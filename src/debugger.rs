use alloy_primitives::Bytes;
use alloy_primitives::FixedBytes;
use alloy_primitives::U256;
use dap::types::PresentationHint;
use dap::types::StackFramePresentationhint;
use dap::types::Thread;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::dap::requests::{
    ContinueArguments, LaunchRequestArguments, SetBreakpointsArguments, StepInArguments,
    StepOutArguments, VariablesArguments,
};
use crate::dap::responses::{SetBreakpointsResponse, ThreadsResponse, VariablesResponse};
use crate::dap::Client;
use crate::dap::Service;
use crate::state::resolve_memory_assignment;
use crate::state::Location;
use crate::state::StateReference;
use crate::state::StoragePosition;
use crate::state::Type;
use crate::tracer::SourceLocation;
use crate::tracer::{DebugTrace, DebugTraceStep, StepKind, Variable};
use rand::Rng;
use std::hash::Hasher;

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
                    name: format!("Frame {}", i),
                    line: source_location.line as i64,
                    column: (source_location.column + 1) as i64,
                    end_line: source_location.end_line.map(|l| l as i64),
                    end_column: source_location.end_column.map(|c| (c + 1) as i64),
                    source: Some(dap::types::Source {
                        name: Some(format!("Frame {}", i)),
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
                line: Some(bp.line as i64),
                ..Default::default()
            })
            .collect();

        SetBreakpointsResponse {
            breakpoints: resp_breakpoints,
        }
    }

    fn variables(&self, body: VariablesArguments) -> VariablesResponse {
        println!("variables arguments {:?}", body);

        let variable = self
            .debug_trace
            .borrow()
            .get_variable(body.variables_reference);

        println!("variables {:?}", variable);

        variable
    }
}

#[derive(Debug, Clone, Default)]
pub struct Debugger {
    pub trace: DebugTrace,
    pub indx: usize,
    pub breakpoints: Vec<Breakpoint>,

    // i64 because DAP uses that
    pub variable_cache: Arc<Mutex<HashMap<i64, serde_json::Map<String, serde_json::Value>>>>,
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
            variable_cache: Arc::new(Mutex::new(HashMap::new())),
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

    pub fn get_variable(&self, id: i64) -> VariablesResponse {
        println!("query variable {:?}", id);

        // Try to use first the values from the cache
        let cache = self.variable_cache.lock().unwrap();
        if let Some(value) = cache.get(&id) {
            println!("it is nested");

            let vars = value
                .iter()
                .map(|(k, v)| dap::types::Variable {
                    name: k.clone(),
                    value: v.to_string(),
                    ..Default::default()
                })
                .collect();

            return VariablesResponse { variables: vars };
        }
        drop(cache); // Explicitly drop the lock before proceeding

        let id_u64 = id as u64;

        let step = &self.trace.steps[self.indx];
        let val = self.trace.variables.get(&id_u64).unwrap();

        println!("get_variable {:?} location {:?}", id, val.state_location);

        match val.state_location {
            Location::Storage { slot, index } => {
                let binding = HashMap::new();
                let contract_storage = step.storage.get(&step.contract_address).unwrap_or(&binding);

                let contract_storage: HashMap<FixedBytes<32>, FixedBytes<32>> = contract_storage
                    .iter()
                    .map(|(k, v)| (FixedBytes::from_slice(k), FixedBytes::from_slice(v)))
                    .collect();

                println!("contract storage {:?}", contract_storage);

                let state_resolver = StateReference::new(contract_storage);
                let value = state_resolver.resolve_type(
                    val.typ.clone(),
                    StoragePosition {
                        slot,
                        index_in_slot: index,
                    },
                );

                println!("value {:?}", value);
                let mut rng = rand::thread_rng();

                let var = match value {
                    serde_json::Value::Object(obj) => {
                        // Create a deterministic ID based on the variable's properties
                        let id = {
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            hasher.write(val.name.as_bytes());
                            hasher.write_u64(self.indx as u64); // Include the current step index
                            (hasher.finish() % 1_000_000) as i64
                            // Using big i64 values creates some issues
                        };

                        println!("storing nested object {:?}", id);

                        // Store the nested object in the cache
                        let mut cache = self.variable_cache.lock().unwrap();
                        cache.insert(id, obj.clone());
                        drop(cache);

                        // Calculate the number of named variables for VS Code
                        let named_variables = Some(obj.len() as i64);

                        dap::types::Variable {
                            name: val.name.clone(),
                            value: "".to_string(),
                            variables_reference: id,
                            named_variables,
                            ..Default::default()
                        }
                    }
                    _ => dap::types::Variable {
                        name: val.name.clone(),
                        value: value.to_string(),
                        ..Default::default()
                    },
                };

                VariablesResponse {
                    variables: vec![var],
                }
            }
            Location::Memory | Location::Stack => {
                println!("memory or slack variable {:?}", id);

                let stack_location = match self.trace.stack_positions.get(&id_u64) {
                    Some(stack_location) => stack_location,
                    None => {
                        panic!("stack location not found");
                    }
                };

                println!("stack location {:?}", stack_location);
                println!("stack: {:?}", step.stack);

                let offset = step.stack.get(*stack_location + 1).unwrap();

                match val.state_location {
                    Location::Memory => {
                        println!("memory: {:?}", step.memory);

                        let offset =
                            U256::from_be_bytes(<[u8; 32]>::try_from(offset.as_ref()).unwrap());
                        let offset_bytes = offset.as_limbs()[0] as usize;

                        let value = resolve_memory_assignment(
                            val.typ.clone(),
                            offset_bytes,
                            step.memory.clone(),
                        );

                        println!("Memory: {:?}", value);

                        let var = dap::types::Variable {
                            name: val.name.clone(),
                            value: value.to_string(),
                            ..Default::default()
                        };

                        VariablesResponse {
                            variables: vec![var],
                        }
                    }
                    Location::Stack => {
                        let value_bytes = offset.as_ref();
                        let typ_size = val.typ.get_bytes();

                        // fixed bytes pads to the left and the other elements pads to the right
                        let value_bytes = match val.typ {
                            Type::FixedBytes(_) => value_bytes[0..typ_size as usize].to_vec(),
                            _ => value_bytes[32 - typ_size as usize..].to_vec(),
                        };

                        println!("Type: {:?}", val.typ);
                        let value = val.typ.decode_bytes(&value_bytes).unwrap();
                        println!("Value: {:?}", value);

                        println!("Stack: {:?}", value);

                        let var = dap::types::Variable {
                            name: val.name.clone(),
                            value: value.to_string(),
                            ..Default::default()
                        };

                        VariablesResponse {
                            variables: vec![var],
                        }
                    }
                    _ => !unreachable!(),
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
