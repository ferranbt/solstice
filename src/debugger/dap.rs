use dap::prelude::Server as InnerServer;
use dap::{
    requests::{
        ContinueArguments, DisconnectArguments, LaunchRequestArguments, NextArguments,
        ScopesArguments, SetBreakpointsArguments, SetExceptionBreakpointsArguments,
        StackTraceArguments, StepBackArguments, StepInArguments, StepOutArguments,
        VariablesArguments,
    },
    responses::{
        ScopesResponse, SetBreakpointsResponse, SetExceptionBreakpointsResponse,
        StackTraceResponse, ThreadsResponse, VariablesResponse,
    },
};
use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::mpsc;

pub mod requests {
    pub use dap::requests::*;
}

pub mod responses {
    pub use dap::responses::*;
}

pub struct Server<R: Read, W: Write> {
    inner: InnerServer<R, W>,
}

pub trait Service {
    fn launch(&self, _body: LaunchRequestArguments);

    fn threads(&self) -> ThreadsResponse;

    fn disconnect(&self, _body: DisconnectArguments) {}

    fn scopes(&self, _body: ScopesArguments) -> ScopesResponse {
        ScopesResponse { scopes: vec![] }
    }

    fn stack_trace(&self, _body: StackTraceArguments) -> StackTraceResponse {
        StackTraceResponse {
            stack_frames: vec![],
            total_frames: None,
        }
    }

    fn variables(&self, _body: VariablesArguments) -> VariablesResponse {
        VariablesResponse { variables: vec![] }
    }

    fn next(&self, _body: NextArguments) {}

    fn step_back(&self, _body: StepBackArguments) {}

    fn step_in(&self, _body: StepInArguments) {}

    fn step_out(&self, _body: StepOutArguments) {}

    fn cont(&self, _body: ContinueArguments) {}

    fn set_breakpoints(&self, _breakpoints: SetBreakpointsArguments) -> SetBreakpointsResponse {
        SetBreakpointsResponse {
            breakpoints: vec![],
        }
    }

    fn set_exception_breakpoints(
        &self,
        _breakpoints: SetExceptionBreakpointsArguments,
    ) -> SetExceptionBreakpointsResponse {
        SetExceptionBreakpointsResponse { breakpoints: None }
    }
}

pub struct Client {
    tx: mpsc::Sender<dap::prelude::Event>,
}

impl Client {
    pub fn send_event(&self, event: dap::prelude::Event) {
        let _ = self.tx.send(event).map_err(|e| {
            tracing::error!("Failed to send event: {}", e);
        });
    }
}

impl<R: Read + Send, W: Write + Send> Server<R, W> {
    /// Construct a new Server using the given input and output streams.
    pub fn new(input: BufReader<R>, output: BufWriter<W>) -> Self {
        Self {
            inner: InnerServer::new(input, output),
        }
    }

    pub fn serve<T: Service, F>(&mut self, launch: F)
    where
        F: FnOnce(Client) -> T,
    {
        let (tx, rx) = mpsc::channel();
        let service = launch(Client { tx });

        let server = &mut self.inner;

        let req = match server.poll_request() {
            Ok(Some(req)) => req,
            Ok(None) => {
                tracing::warn!("No request received");
                return;
            }
            Err(e) => {
                tracing::error!("Failed to poll request: {}", e);
                return;
            }
        };

        if let dap::prelude::Command::Initialize(_) = req.command {
            let rsp = req.success(dap::prelude::ResponseBody::Initialize(
                dap::prelude::types::Capabilities {
                    supports_step_back: Some(true),
                    supports_step_in_targets_request: Some(true),
                    ..Default::default()
                },
            ));

            if let Err(e) = server.respond(rsp) {
                tracing::error!("Failed to respond to Initialize command: {}", e);
            }
            if let Err(e) = server.send_event(dap::prelude::Event::Initialized) {
                tracing::error!("Failed to send Initialized event: {}", e);
            }
        } else {
            tracing::error!("Unexpected request: {:?}", req);
            return;
        }

        loop {
            while let Ok(event) = rx.try_recv() {
                if let Err(e) = server.send_event(event) {
                    tracing::error!("Failed to send event: {}", e);
                }
            }

            let req = match server.poll_request() {
                Ok(Some(req)) => req,
                Ok(None) => {
                    tracing::warn!("No request received");
                    return;
                }
                Err(e) => {
                    tracing::error!("Failed to poll request: {}", e);
                    return;
                }
            };

            match req.command.clone() {
                dap::prelude::Command::Launch(body) => {
                    service.launch(body);
                    let rsp = req.success(dap::prelude::ResponseBody::Launch);
                    if let Err(e) = server.respond(rsp) {
                        tracing::error!("Failed to respond to Launch command: {}", e);
                    }
                }
                dap::prelude::Command::SetBreakpoints(body) => {
                    let rsp = service.set_breakpoints(body);
                    if let Err(e) =
                        server.respond(req.success(dap::prelude::ResponseBody::SetBreakpoints(rsp)))
                    {
                        tracing::error!("Failed to respond to SetBreakpoints command: {}", e);
                    }
                }
                dap::prelude::Command::SetExceptionBreakpoints(body) => {
                    let rsp = service.set_exception_breakpoints(body);
                    if let Err(e) = server.respond(
                        req.success(dap::prelude::ResponseBody::SetExceptionBreakpoints(rsp)),
                    ) {
                        tracing::error!(
                            "Failed to respond to SetExceptionBreakpoints command: {}",
                            e
                        );
                    }
                }
                dap::prelude::Command::Disconnect(body) => {
                    service.disconnect(body);
                    break;
                }
                dap::prelude::Command::Threads => {
                    let rsp = req.success(dap::prelude::ResponseBody::Threads(service.threads()));
                    if let Err(e) = server.respond(rsp) {
                        tracing::error!("Failed to respond to Threads command: {}", e);
                    }
                }
                dap::prelude::Command::StepIn(body) => {
                    service.step_in(body);
                }
                dap::prelude::Command::StepOut(body) => {
                    service.step_out(body);
                }
                dap::prelude::Command::StepBack(body) => {
                    service.step_back(body);
                }
                dap::prelude::Command::Next(body) => {
                    service.next(body);
                }
                dap::prelude::Command::Continue(body) => {
                    service.cont(body);
                }
                dap::prelude::Command::Scopes(body) => {
                    let rsp = service.scopes(body);
                    let rsp = req.success(dap::prelude::ResponseBody::Scopes(rsp));
                    if let Err(e) = server.respond(rsp) {
                        tracing::error!("Failed to respond to Scopes command: {}", e);
                    }
                }
                dap::prelude::Command::Variables(body) => {
                    let rsp = service.variables(body);
                    let rsp = req.success(dap::prelude::ResponseBody::Variables(rsp));
                    if let Err(e) = server.respond(rsp) {
                        tracing::error!("Failed to respond to Variables command: {}", e);
                    }
                }
                dap::prelude::Command::StackTrace(body) => {
                    let rsp = service.stack_trace(body);
                    let rsp = req.success(dap::prelude::ResponseBody::StackTrace(rsp));
                    if let Err(e) = server.respond(rsp) {
                        tracing::error!("Failed to respond to StackTrace command: {}", e);
                    }
                }
                _ => {
                    tracing::warn!("Untracked request: {:?}", req);
                }
            };
        }
    }
}
