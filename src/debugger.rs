use alloy_primitives::FixedBytes;
use alloy_primitives::U256;
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
use crate::state::{resolve_memory_assignment, StateReference, Type};
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
            .rev()
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

type DebugLocation = usize;

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

    pub fn debug_location(&self) -> DebugLocation {
        let step = &self.trace.steps[self.indx];
        step.location.line
    }

    pub fn prev(&mut self) -> Option<DebugLocation> {
        // go back to the previous statement
        while self.indx > 0 {
            self.indx -= 1;
            if !matches!(
                self.trace.steps[self.indx].kind,
                StepKind::FunctionDefinition(_)
            ) {
                return Some(self.debug_location());
            }
        }
        None
    }

    pub fn next(&mut self) -> Option<DebugLocation> {
        // continue until the next statement in the same function
        let call_trace_length = self.trace.steps[self.indx].call_trace.len();
        let current_line = self.debug_location();

        while self.indx < self.trace.steps.len() - 1 {
            self.indx += 1;
            let step = &self.trace.steps[self.indx];

            if step.location.line == current_line {
                // if the line is the same, skip it, this happens if you have a function call
                // because we might store steps for the function call and the statement in which
                // the call is made. In this case, we do not want to stop again in the statement (same line)
                // but to jump directly to the next one.
                continue;
            }
            if matches!(step.kind, StepKind::FunctionDefinition(_)) {
                continue;
            }
            if step.call_trace.len() > call_trace_length {
                // this signal it is a nested step, we should not enter any nested steps
                // with the 'next' function, we can only go up.
                continue;
            }
            return Some(self.debug_location());
        }
        None
    }

    pub fn cont(&mut self) -> Option<DebugLocation> {
        // continue until we hit a breakpoint
        while self.indx < self.trace.steps.len() - 1 {
            self.indx += 1;
            let step = &self.trace.steps[self.indx];

            if self.is_breakpoint(step.path.clone(), step.location.line) {
                return Some(self.debug_location());
            }
        }
        None
    }

    pub fn is_breakpoint(&self, path: String, line: usize) -> bool {
        self.breakpoints
            .iter()
            .any(|bp| bp.source == path && bp.line == line)
    }

    pub fn step_out(&mut self) -> Option<DebugLocation> {
        // return to the function caller, pick the latest element from the call trace
        match self.trace.steps[self.indx].call_trace.clone().last() {
            Some(call_trace) => {
                self.indx = *call_trace;
                Some(self.debug_location())
            }
            None => None,
        }
    }

    pub fn step_in(&mut self) -> Option<DebugLocation> {
        // if next item is a function call, go inside it.
        // otherwise, return the next item in the current function
        if matches!(self.trace.steps[self.indx].kind, StepKind::FunctionCall) {
            while self.indx < self.trace.steps.len() - 1 {
                self.indx += 1;

                let step = &self.trace.steps[self.indx];
                if matches!(step.kind, StepKind::FunctionDefinition(_)) {
                    continue;
                }
                return Some(self.debug_location());
            }
        }
        self.next()
    }

    #[cfg(test)]
    pub fn last(&mut self) {
        // go to the last step in the trace
        self.indx = self.trace.steps.len() - 1;
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

                Ok(Some(VariablesResponse {
                    variables: vec![var],
                }))
            }
            Assignment::Stack(index) => {
                let value = match step.state_snapshot.stack.get(index).cloned() {
                    Some(value) => value,
                    None => {
                        return Err(eyre::eyre!("No value found in stack at index {}", index));
                    }
                };

                let typ = self
                    .trace
                    .variable_types
                    .get(&variable.id)
                    .cloned()
                    .unwrap();

                match variable.location {
                    VariableLocation::Stack => {
                        tracing::info!("Variable is in stack, value: {:?}", value);
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

                        Ok(Some(VariablesResponse {
                            variables: vec![var],
                        }))
                    }
                    VariableLocation::Memory => {
                        let offset =
                            U256::from_be_bytes(<[u8; 32]>::try_from(value.as_ref()).unwrap());
                        let offset_bytes = offset.as_limbs()[0] as usize;

                        let value = resolve_memory_assignment(
                            typ.clone(),
                            offset_bytes,
                            step.state_snapshot.memory.clone(),
                        );

                        let var = dap::types::Variable {
                            name: variable.name.clone(),
                            value: value.to_string(),
                            ..Default::default()
                        };

                        Ok(Some(VariablesResponse {
                            variables: vec![var],
                        }))
                    }
                    _ => unreachable!("Unexpected variable location: {:?}", variable.location),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracer::testing::trace_function;
    use std::sync::Mutex;

    // Run the test sequentially because of https://github.com/ferranbt/solstice/issues/50
    // we have to remove the out folder every time we run the tests so it is not thread safe.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_debugger_breakpoints_continue() -> eyre::Result<()> {
        let _guard = TEST_MUTEX.lock().unwrap();

        let debug_trace = trace_function(
            "test_debugger_breakpoints_continue",
            "function test() public {
           uint256 a = 1; // line 6
           uint256 b = 2; // line 7
           uint256 c = 3; // line 8
       }",
        )?;

        let mut debugger = Debugger::new(debug_trace);

        // get the name of the file from the trace because we need
        // the absolute path to set the breakpoint
        let abs_path = debugger.trace.steps[0].path.clone();
        debugger.set_breakpoint(abs_path.to_string(), 7);

        assert_eq!(debugger.cont(), Some(7));
        assert_eq!(debugger.next(), Some(8));
        assert_eq!(debugger.prev(), Some(7));
        assert_eq!(debugger.prev(), Some(6));
        assert_eq!(debugger.cont(), Some(7));
        assert_eq!(debugger.cont(), None);

        Ok(())
    }

    #[test]
    fn test_debugger_next_skips_function_calls() -> eyre::Result<()> {
        let _guard = TEST_MUTEX.lock().unwrap();

        // When using 'next' on a function call, it should skip over the entire
        // function execution and go to the next statement, not enter the function.

        let debug_trace = trace_function(
            "test_debugger_next_skips_function_calls",
            "function helper() public returns (uint256) { // line 5
           uint256 a = 10; // line 6
           return a; // line 7
       }
       function test() public {
           uint256 x = 5; // line 10
           uint256 y = helper(); // line 11
           uint256 z = 15; // line 12
       }",
        )?;

        let mut debugger = Debugger::new(debug_trace);

        // Navigate through test function using only 'next'
        assert_eq!(debugger.next(), Some(10)); // uint256 x = 5
        assert_eq!(debugger.next(), Some(11)); // uint256 y = helper()
        assert_eq!(debugger.next(), Some(12)); // uint256 z = 15 (skipped helper internals)

        // Should never see lines 5, 6, or 7 (inside helper function)
        // because we never called step_in()

        Ok(())
    }

    #[test]
    fn test_debugger_next_skips_function_calls_within_statement() -> eyre::Result<()> {
        let _guard = TEST_MUTEX.lock().unwrap();

        // When a statement contains multiple function calls, 'next' should skip over
        // all the individual function call steps and land on the next statement.
        //
        // Example: `uint256 y = call() + call();` generates these debug steps:
        // 1. [CALL] call() - line 10
        // 2. [CALL] call() - line 10
        // 3. [STMT] assignment - line 10
        // 4. [STMT] next statement - line 11
        //
        // Using 'next' from step 1 should jump directly to step 4, skipping the
        // internal function calls and statement completion within the same line.

        let debug_trace = trace_function(
            "test_debugger_next_skips_function_calls_within_statement",
            "function call() public returns (uint256) { // line 5
                return 42;
            }
            function test() public {
                uint256 x = 10; // line 9
                uint256 y = call() + call(); // line 10
                uint256 z = 20; // line 11
            }",
        )?;

        let mut debugger = Debugger::new(debug_trace);
        assert_eq!(debugger.next(), Some(9));
        assert_eq!(debugger.next(), Some(10));
        assert_eq!(debugger.next(), Some(11));

        Ok(())
    }

    #[test]
    fn test_debugger_next_skips_standalone_function_calls() -> eyre::Result<()> {
        let _guard = TEST_MUTEX.lock().unwrap();

        // When using 'next' on a standalone function call (no assignment),
        // it should skip over the entire function execution and go to the next statement.

        let debug_trace = trace_function(
            "test_debugger_next_skips_standalone_function_calls",
            "function helper() public { // line 5
           uint256 a = 10; // line 6
       }
       function test() public {
           uint256 x = 5; // line 9
           helper(); // line 10 - standalone call
           uint256 z = 15; // line 11
       }",
        )?;

        let mut debugger = Debugger::new(debug_trace);

        assert_eq!(debugger.next(), Some(9)); // uint256 x = 5
        assert_eq!(debugger.next(), Some(10)); // helper()
        assert_eq!(debugger.next(), Some(11)); // uint256 z = 15 (skipped helper internals)

        // Should never see lines 5 or 6 (inside helper function)
        // because we never called step_in()

        Ok(())
    }

    #[test]
    fn test_debugger_next_stays_within_current_function_scope() -> eyre::Result<()> {
        let _guard = TEST_MUTEX.lock().unwrap();

        // When inside a function (after step-in), 'next' should advance within
        // that function's scope, not return to the caller.
        //
        // Example: stepping into `call()` then using 'next':
        // 1. [CALL] call() - line 10 (in test function)
        // 2. [FUNC] call - line 5 (stepped into call function)
        // 3. [STMT] return 42 - line 6 (inside call function)
        // 4. [STMT] assignment completion - line 10 (back in test function)
        // 5. [STMT] next statement - line 11 (in test function)
        //
        // Using 'next' from step 3 should stay in call() function scope.
        // Only when call() completes should we return to test() function.

        let debug_trace = trace_function(
            "test_debugger_next_stays_within_current_function_scope",
            "function call() public returns (uint256) { // line 5
            uint256 x = 10; // line 6
            return 42; // line 7
        }
        function test() public {
            uint256 x = 10; // line 10
            uint256 y = call(); // line 11
            call(); // line 12 - standalone call
            uint256 z = 20; // line 13
        }",
        )?;

        let mut debugger = Debugger::new(debug_trace);

        assert_eq!(debugger.next(), Some(10)); // uint256 x = 10;
        assert_eq!(debugger.next(), Some(11)); // uint256 y = call()
        assert_eq!(debugger.step_in(), Some(6)); // uint256 x = 10 (step into call function)
        assert_eq!(debugger.next(), Some(7)); // return 42
        assert_eq!(debugger.next(), Some(11)); // uint256 y = call()
        assert_eq!(debugger.next(), Some(12)); // call()
        assert_eq!(debugger.step_in(), Some(6)); // uint256 x = 10
        assert_eq!(debugger.next(), Some(7)); // return 42
        assert_eq!(debugger.next(), Some(13)); // uint256 z = 20
        assert_eq!(debugger.next(), None); // No more steps

        Ok(())
    }

    #[test]
    fn test_debugger_step_in_step_out_navigation() -> eyre::Result<()> {
        let _guard = TEST_MUTEX.lock().unwrap();

        // Tests the complete step-in/step-out workflow:
        // 1. step_in on function call enters the function
        // 2. step_in on non-function-call acts like next (step over)
        // 3. step_out exits current function scope back to caller
        // 4. Navigation works correctly across function boundaries

        let debug_trace = trace_function(
            "test_debugger_step_in_step_out_navigation",
            "function helper() public returns (uint256) { // line 5
            uint256 a = 10; // line 6
            return a; // line 7
        }
        function test() public {
            uint256 x = 5; // line 10
            uint256 y = helper(); // line 11
            uint256 z = 15; // line 12
        }",
        )?;

        let mut debugger = Debugger::new(debug_trace);

        // Start at first statement
        assert_eq!(debugger.next(), Some(10)); // uint256 x = 5

        // step_in on non-function-call should act like next
        assert_eq!(debugger.step_in(), Some(11)); // uint256 y = helper()

        // step_in on function call should enter the function
        assert_eq!(debugger.step_in(), Some(6)); // function helper() definition
        assert_eq!(debugger.next(), Some(7)); // return a

        // step_out should exit function scope back to caller
        assert_eq!(debugger.step_out(), Some(11)); // uint256 y = helper(); (back in test function)
        assert_eq!(debugger.prev(), Some(10)); // uint256 x = 5;

        // step_in again to verify it still works
        assert_eq!(debugger.next(), Some(11)); // uint256 y = helper()
        assert_eq!(debugger.step_in(), Some(6)); // Re-enter helper function

        // Test that next from call site skips the function
        assert_eq!(debugger.prev(), Some(11)); // Back to call site
        assert_eq!(debugger.next(), Some(12)); // Should skip helper() execution

        Ok(())
    }
}
