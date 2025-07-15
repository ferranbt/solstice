use alloy_primitives::FixedBytes;
use dap::types::PresentationHint;
use dap::types::StackFramePresentationhint;
use dap::types::Thread;
use std::cell::RefCell;
use std::collections::HashMap;

use crate::dap::requests::{
    ContinueArguments, LaunchRequestArguments, SetBreakpointsArguments, StepInArguments,
    StepOutArguments, VariablesArguments,
};
use crate::dap::responses::{SetBreakpointsResponse, ThreadsResponse, VariablesResponse};
use crate::dap::Client;
use crate::dap::Service;
use crate::state::{StateReference, Type};
use crate::tracer::VariableLocation;
use crate::tracer::{Assignment, DebugTrace, DebugTraceStep, StepKind, Variable};

pub struct DapDebugger {
    debug_trace: RefCell<Debugger>,
    client: Client,
}

impl DapDebugger {
    pub fn new(client: Client, debug_trace: DebugTrace) -> Self {
        Self {
            client,
            debug_trace: RefCell::new(Debugger::new(debug_trace)),
        }
    }
}

impl Service for DapDebugger {
    fn launch(&self, _body: LaunchRequestArguments) {
        // stop right away since we have already loaded the trace
        self.client.send_event(dap::prelude::Event::Stopped(
            dap::events::StoppedEventBody {
                reason: dap::types::StoppedEventReason::Step,
                description: None,
                thread_id: Some(1),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(true),
                hit_breakpoint_ids: None,
            },
        ));
    }

    fn threads(&self) -> ThreadsResponse {
        ThreadsResponse {
            threads: vec![Thread {
                id: 1,
                name: "Main Thread".to_string(),
            }],
        }
    }

    fn step_in(&self, _body: StepInArguments) {
        self.debug_trace.borrow_mut().step_in();

        self.client.send_event(dap::prelude::Event::Stopped(
            dap::events::StoppedEventBody {
                reason: dap::types::StoppedEventReason::Step,
                description: None,
                thread_id: Some(1),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(true),
                hit_breakpoint_ids: None,
            },
        ));
    }

    fn step_out(&self, _body: StepOutArguments) {
        self.debug_trace.borrow_mut().step_out();

        self.client.send_event(dap::prelude::Event::Stopped(
            dap::events::StoppedEventBody {
                reason: dap::types::StoppedEventReason::Step,
                description: None,
                thread_id: Some(1),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(true),
                hit_breakpoint_ids: None,
            },
        ));
    }

    fn step_back(&self, _body: dap::requests::StepBackArguments) {
        self.debug_trace.borrow_mut().prev();

        self.client.send_event(dap::prelude::Event::Stopped(
            dap::events::StoppedEventBody {
                reason: dap::types::StoppedEventReason::Step,
                description: None,
                thread_id: Some(1),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(true),
                hit_breakpoint_ids: None,
            },
        ));
    }

    fn next(&self, _body: dap::requests::NextArguments) {
        self.debug_trace.borrow_mut().next();

        // just stop right away
        self.client.send_event(dap::prelude::Event::Stopped(
            dap::events::StoppedEventBody {
                reason: dap::types::StoppedEventReason::Step,
                description: None,
                thread_id: Some(1),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(true),
                hit_breakpoint_ids: None,
            },
        ));
    }

    fn cont(&self, _body: ContinueArguments) {
        self.debug_trace.borrow_mut().cont();

        self.client.send_event(dap::prelude::Event::Stopped(
            dap::events::StoppedEventBody {
                reason: dap::types::StoppedEventReason::Step,
                description: None,
                thread_id: Some(1),
                preserve_focus_hint: None,
                text: None,
                all_threads_stopped: Some(true),
                hit_breakpoint_ids: None,
            },
        ));
    }

    fn scopes(&self, _body: dap::requests::ScopesArguments) -> dap::responses::ScopesResponse {
        let variables_in_scope = self.debug_trace.borrow().scope();

        dap::responses::ScopesResponse {
            scopes: variables_in_scope
                .into_iter()
                .map(|var| dap::types::Scope {
                    name: var.name,
                    presentation_hint: None,
                    variables_reference: var.id as i64,
                    ..Default::default()
                })
                .collect(), // Add appropriate scopes here
        }
    }

    fn stack_trace(
        &self,
        _body: dap::requests::StackTraceArguments,
    ) -> dap::responses::StackTraceResponse {
        let concrete_trace = self.debug_trace.borrow().trace();
        let len_frames = concrete_trace.stack_frames.len();

        let traces = concrete_trace
            .stack_frames
            .into_iter()
            .enumerate()
            .map(|(i, trace)| {
                let source_location = trace.location;

                dap::types::StackFrame {
                    id: i as i64,
                    name: format!("Frame {i}"),
                    line: source_location.line as i64,
                    column: (source_location.column + 1) as i64,
                    end_line: source_location.end_line.map(|l| l as i64),
                    end_column: source_location.end_column.map(|c| (c + 1) as i64),
                    source: Some(dap::types::Source {
                        name: Some(format!("Frame {i}")),
                        path: Some(trace.path.clone()),
                        presentation_hint: Some(PresentationHint::Normal),
                        source_reference: None,
                        origin: None,
                        sources: None,
                        adapter_data: None,
                        checksums: None,
                    }),
                    presentation_hint: Some(StackFramePresentationhint::Normal), // Add presentation hint
                    ..Default::default()
                }
            })
            .collect();

        dap::responses::StackTraceResponse {
            stack_frames: traces,
            total_frames: Some(len_frames as i64),
        }
    }

    fn set_breakpoints(&self, breakpoints: SetBreakpointsArguments) -> SetBreakpointsResponse {
        let source = breakpoints.source.path.unwrap();

        let breakpoints = breakpoints.breakpoints.unwrap_or_default();
        for breakpoint in &breakpoints {
            self.debug_trace
                .borrow_mut()
                .set_breakpoint(source.clone(), breakpoint.line as usize);
        }

        let resp_breakpoints = breakpoints
            .iter()
            .map(|bp| dap::types::Breakpoint {
                line: Some(bp.line),
                ..Default::default()
            })
            .collect();

        SetBreakpointsResponse {
            breakpoints: resp_breakpoints,
        }
    }

    fn variables(&self, body: VariablesArguments) -> VariablesResponse {
        tracing::info!(
            "Variables request received, id {}",
            body.variables_reference
        );

        let res = self
            .debug_trace
            .borrow()
            .get_variable(body.variables_reference as u64);

        match res {
            Ok(Some(response)) => {
                tracing::info!(
                    "Variables response found for id {}",
                    body.variables_reference
                );
                return response;
            }
            Ok(None) => {
                tracing::warn!("No variable found for id {}", body.variables_reference);
            }
            Err(err) => {
                tracing::error!(
                    "Error retrieving variable for id {}: {}",
                    body.variables_reference,
                    err
                );
            }
        };

        VariablesResponse { variables: vec![] }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Debugger {
    pub trace: DebugTrace,
    pub indx: usize,
    pub breakpoints: Vec<Breakpoint>,
}

#[derive(Debug, Clone, Default)]
pub struct Breakpoint {
    pub source: String,
    pub line: usize,
}

impl Debugger {
    pub fn new(trace: DebugTrace) -> Self {
        Self {
            trace,
            indx: 0,
            breakpoints: vec![],
        }
    }

    pub fn set_breakpoint(&mut self, source: String, line: usize) {
        self.breakpoints.push(Breakpoint { source, line });
    }

    pub fn prev(&mut self) -> Option<usize> {
        // go back to the previous statement
        while self.indx > 0 {
            self.indx -= 1;
            if !matches!(
                self.trace.steps[self.indx].kind,
                StepKind::FunctionDefinition(_)
            ) {
                return Some(self.indx);
            }
        }
        None
    }

    pub fn next(&mut self) -> Option<usize> {
        // continue until the next statement in the same function
        let call_trace_length = self.trace.steps[self.indx].call_trace.len();

        while self.indx < self.trace.steps.len() - 1 {
            self.indx += 1;
            let step = &self.trace.steps[self.indx];

            if matches!(step.kind, StepKind::FunctionDefinition(_)) {
                continue;
            }
            if step.call_trace.len() != call_trace_length {
                continue;
            }
            return Some(self.indx);
        }
        None
    }

    pub fn cont(&mut self) -> Option<usize> {
        // continue until we hit a breakpoint
        while self.indx < self.trace.steps.len() - 1 {
            self.indx += 1;
            let step = &self.trace.steps[self.indx];

            if self.is_breakpoint(step.path.clone(), step.location.line) {
                return Some(self.indx);
            }
        }
        None
    }

    pub fn is_breakpoint(&self, path: String, line: usize) -> bool {
        self.breakpoints
            .iter()
            .any(|bp| bp.source == path && bp.line == line)
    }

    pub fn step_out(&mut self) -> Option<usize> {
        // return to the function caller, pick the latest element from the call trace
        match self.trace.steps[self.indx].call_trace.clone().last() {
            Some(call_trace) => {
                self.indx = *call_trace;
                Some(self.indx)
            }
            None => None,
        }
    }

    pub fn step_in(&mut self) -> Option<usize> {
        // if next item is a function call, go inside it.
        // otherwise, return the next item in the current function
        if matches!(self.trace.steps[self.indx].kind, StepKind::FunctionCall) {
            while self.indx < self.trace.steps.len() - 1 {
                self.indx += 1;

                let step = &self.trace.steps[self.indx];
                if matches!(step.kind, StepKind::FunctionDefinition(_)) {
                    continue;
                }
                return Some(self.indx);
            }
        }
        self.next()
    }

    pub fn scope(&self) -> Vec<Variable> {
        self.trace.scope(self.indx)
    }

    pub fn trace(&self) -> DebugTraceStep {
        self.trace.trace(self.indx)
    }

    pub fn get_variable(&self, var_id: u64) -> eyre::Result<Option<VariablesResponse>> {
        let assignment = self.trace.assignments.get(&var_id);

        let assignment: Assignment = if let Some(assignment) = assignment {
            assignment.clone()
        } else {
            tracing::warn!("No assignment found for id {}", var_id);
            return Ok(None);
        };

        let step = &self.trace.steps[self.indx];

        let variable = if let Some(variable) = self.trace.variables.get(&var_id) {
            variable
        } else {
            tracing::warn!("No variable found for id {}", var_id);
            return Ok(None);
        };

        match assignment {
            Assignment::Storage(storage_position) => {
                let contract_storage = &step.state_snapshot.storage;

                let contract_storage: HashMap<FixedBytes<32>, FixedBytes<32>> = contract_storage
                    .iter()
                    .map(|(k, v)| {
                        (
                            FixedBytes::right_padding_from(k),
                            FixedBytes::right_padding_from(v),
                        )
                    })
                    .collect();

                let state_resolver = StateReference::new(contract_storage);
                let typ = self.trace.variable_types.get(&var_id).cloned().unwrap();
                let value = state_resolver.resolve_type(typ.clone(), storage_position);

                let var = dap::types::Variable {
                    name: variable.name.clone(),
                    value: value.to_string(),
                    ..Default::default()
                };

                return Ok(Some(VariablesResponse {
                    variables: vec![var],
                }));
            }
            Assignment::Stack(index) => {
                tracing::info!("Variable is in stack, index: {:?}", index);

                let value = match step.state_snapshot.stack.get(index as usize).cloned() {
                    Some(value) => value,
                    None => {
                        return Err(eyre::eyre!("No value found in stack at index {}", index));
                    }
                };

                match variable.location {
                    VariableLocation::Stack => {
                        tracing::info!("Variable is in stack, value: {:?}", value);

                        let typ = self
                            .trace
                            .variable_types
                            .get(&variable.id)
                            .cloned()
                            .unwrap();
                        let typ_size = typ.get_bytes();

                        // fixed bytes pads to the left and the other elements pads to the right
                        let value_bytes = match typ {
                            Type::FixedBytes(_) => value[0..typ_size as usize].to_vec(),
                            _ => value[32 - typ_size as usize..].to_vec(),
                        };

                        let value = typ.decode_bytes(&value_bytes).map_err(|e| {
                            eyre::eyre!(
                                "Failed to decode variable bytes {:?} with type {:?}: {}",
                                value_bytes,
                                typ,
                                e
                            )
                        })?;

                        let var = dap::types::Variable {
                            name: variable.name.clone(),
                            value: value.to_string(),
                            ..Default::default()
                        };

                        return Ok(Some(VariablesResponse {
                            variables: vec![var],
                        }));
                    }
                    _ => unreachable!("Variable storage handled in the other case"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracer::{DebugStep, SourceLocation};

    #[test]
    fn test_debugger_breakpoints_continue() {
        let debug_trace = DebugTrace {
            steps: vec![
                DebugStep {
                    path: "test.sol".to_string(),
                    ..Default::default()
                },
                DebugStep {
                    path: "test.sol".to_string(),
                    ..Default::default()
                },
                DebugStep {
                    path: "test.sol".to_string(),
                    location: SourceLocation {
                        line: 1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                DebugStep {
                    path: "test.sol".to_string(),
                    ..Default::default()
                },
                DebugStep {
                    path: "test.sol".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let mut debugger = Debugger::new(debug_trace);
        debugger.set_breakpoint("test.sol".to_string(), 1);

        assert_eq!(debugger.cont(), Some(2));

        assert_eq!(debugger.next(), Some(3));
        assert_eq!(debugger.next(), Some(4));

        assert_eq!(debugger.prev(), Some(3));

        // there are no more breakpoints
        assert_eq!(debugger.cont(), None);
    }

    #[test]
    fn test_debugger_step_in_out() {
        let debug_trace = DebugTrace {
            steps: vec![
                DebugStep {
                    ..Default::default()
                },
                DebugStep {
                    path: "test.sol".to_string(),
                    location: SourceLocation {
                        line: 1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                DebugStep {
                    kind: StepKind::FunctionCall,
                    ..Default::default()
                },
                DebugStep {
                    kind: StepKind::FunctionDefinition("test".to_string()),
                    ..Default::default()
                },
                DebugStep {
                    call_trace: vec![2],
                    ..Default::default()
                },
                DebugStep {
                    call_trace: vec![2],
                    ..Default::default()
                },
                DebugStep {
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let mut debugger = Debugger::new(debug_trace);
        debugger.set_breakpoint("test.sol".to_string(), 1);

        assert_eq!(debugger.cont(), Some(1));

        // step in should act as a step over if no function call is found
        assert_eq!(debugger.step_in(), Some(2));

        // step over should skip the function call
        assert_eq!(debugger.next(), Some(6));

        // prev goes back in the execution call to the instructions in the nested
        // function call
        assert_eq!(debugger.prev(), Some(5));
        assert_eq!(debugger.prev(), Some(4));
        assert_eq!(debugger.prev(), Some(2)); // 2 since we skip functionCall

        // step in goes inside the function call
        assert_eq!(debugger.step_in(), Some(4));
        assert_eq!(debugger.next(), Some(5));

        // step out goes out of the function call to the caller
        assert_eq!(debugger.step_out(), Some(2));
        // again, step over should skip the function call
        assert_eq!(debugger.next(), Some(6));
    }
}
