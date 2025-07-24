use builder::DefinitionIndex;
use built_info::PKG_VERSION;
use clap::{Args, Parser, Subcommand};
use debugger::DapDebugger;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solang::sema::ast;
use solang::{parse_and_resolve, Target};
use tower_lsp::jsonrpc::{Error, ErrorCode, Result};
use tower_lsp::lsp_types::notification::Notification;
use tower_lsp::lsp_types::request::GotoTypeDefinitionResponse;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use tracer::{execute_command, generate_trace};

use crate::builder::DefinitionType;
use crate::builder::{Builder, Files, GlobalCache};
use crate::tracer::{DebugTrace, Forge};
use pprof::protos::Message;
use std::io::Write;

use solang::file_resolver::FileResolver;
use solang_parser::pt;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::dap::Server as DapServer;

// Include the generated-file as a separate module
pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

// mod dap;
mod builder;
mod dap;
mod debugger;
mod metrics;
mod state;
mod tracer;

struct Backend {
    client: Client,
    files: Files,
    workspace: Mutex<String>,
    global_cache: Mutex<GlobalCache>,
}

#[derive(Debug)]
pub enum CustomNotification {}

impl Notification for CustomNotification {
    type Params = LogParams;
    const METHOD: &'static str = "custom/logToChannel";
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogParams {
    channel: String,
    message: String,
}

#[derive(Debug)]
pub enum CustomNotification2 {}

impl Notification for CustomNotification2 {
    type Params = Value;
    const METHOD: &'static str = "custom/logToChannel2";
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let workspace = params.root_uri.unwrap();
        let workspace_path = workspace.path().to_string();

        let mut workspace_guard = self.workspace.lock().await;
        *workspace_guard = workspace_path;

        Ok(InitializeResult {
            server_info: None,
            offset_encoding: None,
            capabilities: ServerCapabilities {
                inlay_hint_provider: Some(OneOf::Left(false)),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                completion_provider: None,
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["sol.test.file".to_string(), "sol.debug.file".to_string()],
                    work_done_progress_options: Default::default(),
                }),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(true),
                }),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
                }),
                semantic_tokens_provider: None,
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "initialized!")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;

        match uri.to_file_path() {
            Ok(path) => {
                self.files
                    .update_text_file(&path, params.text_document.text);
                self.parse_file(uri).await;
            }
            Err(_) => {
                self.client
                    .log_message(MessageType::ERROR, format!("received invalid URI: {uri}"))
                    .await;
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();

        match uri.to_file_path() {
            Ok(path) => {
                self.files.update_partial_text_file(&path, params);
                self.parse_file(uri).await;
            }
            Err(_) => {
                self.client
                    .log_message(MessageType::ERROR, format!("received invalid URI: {uri}"))
                    .await;
            }
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;

        if let Some(text) = params.text {
            if let Ok(path) = uri.to_file_path() {
                self.files.update_text_file(&path, text);
            }
        }

        self.parse_file(uri).await;
    }

    async fn did_close(&self, _: DidCloseTextDocumentParams) {
        self.client
            .log_message(MessageType::INFO, "file closed!")
            .await;
    }

    async fn did_change_configuration(&self, _: DidChangeConfigurationParams) {
        self.client
            .log_message(MessageType::INFO, "configuration changed!")
            .await;
    }

    async fn did_change_workspace_folders(&self, _: DidChangeWorkspaceFoldersParams) {
        self.client
            .log_message(MessageType::INFO, "workspace folders changed!")
            .await;
    }

    async fn did_change_watched_files(&self, _: DidChangeWatchedFilesParams) {
        self.client
            .log_message(MessageType::INFO, "watched files have changed!")
            .await;
    }

    async fn hover(&self, hverparam: HoverParams) -> Result<Option<Hover>> {
        let txtdoc = hverparam.text_document_position_params.text_document;
        let pos = hverparam.text_document_position_params.position;

        let uri = txtdoc.uri;

        if let Ok(path) = uri.to_file_path() {
            if let Some(cache) = self.files.caches.get(&path) {
                if let Some(offset) = cache
                    .file
                    .get_offset(pos.line as usize, pos.character as usize)
                {
                    // The shortest hover for the position will be most informative
                    if let Some(hover) = cache
                        .hovers
                        .find(offset, offset + 1)
                        .min_by(|a, b| (a.stop - a.start).cmp(&(b.stop - b.start)))
                    {
                        let range = get_range_exclusive(hover.start, hover.stop, &cache.file);

                        return Ok(Some(Hover {
                            contents: HoverContents::Scalar(MarkedString::from_markdown(
                                hover.val.to_string(),
                            )),
                            range: Some(range),
                        }));
                    }
                }
            }
        }

        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        // fetch the `DefinitionIndex` of the code object
        let Some(reference) = self.get_reference_from_params(params).await? else {
            return Ok(None);
        };

        // get the location of the definition of the code object in source code
        let definitions = &self.global_cache.lock().await.definitions;
        let location = definitions
            .get(&reference)
            .map(|range| {
                let uri = Url::from_file_path(&reference.def_path).unwrap();
                Location { uri, range: *range }
            })
            .map(GotoTypeDefinitionResponse::Scalar);

        Ok(location)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        // fetch the `DefinitionIndex` of the code object in question
        let def_params: GotoDefinitionParams = GotoDefinitionParams {
            text_document_position_params: params.text_document_position,
            work_done_progress_params: params.work_done_progress_params,
            partial_result_params: params.partial_result_params,
        };
        let Some(reference) = self.get_reference_from_params(def_params).await? else {
            return Ok(None);
        };

        // fetch all the locations in source code where the code object is referenced
        // this includes the definition location of the code object
        let mut locations: Vec<_> = self
            .files
            .caches
            .iter()
            .flat_map(|entry| {
                let p = entry.key();
                let cache = entry.value();

                let uri = Url::from_file_path(p).unwrap();
                cache
                    .references
                    .iter()
                    .filter(|r| r.val == reference)
                    .map(move |r| Location {
                        uri: uri.clone(),
                        range: get_range_exclusive(r.start, r.stop, &cache.file),
                    })
                    .collect::<Vec<_>>() // Collect inner iterator to break the reference chain
            })
            .collect();

        // remove the definition location if `include_declaration` is `false`
        if !params.context.include_declaration {
            let definitions = &self.global_cache.lock().await.definitions;
            let uri = Url::from_file_path(&reference.def_path).unwrap();
            if let Some(range) = definitions.get(&reference) {
                let def = Location { uri, range: *range };
                locations.retain(|loc| loc != &def);
            }
        }

        // return `None` if the list of locations is empty
        let locations = if locations.is_empty() {
            None
        } else {
            Some(locations)
        };

        Ok(locations)
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        self.client
            .log_message(MessageType::INFO, "code lens requested!")
            .await;

        let uri = params.text_document.uri;
        let path = uri.to_file_path().unwrap();

        let declarations = &self.global_cache.lock().await.definitions;

        if let Some(cache) = self.files.caches.get(&path) {
            let file_path = uri.path();

            #[allow(clippy::unnecessary_filter_map)]
            let functions: Vec<_> = cache
                .top_level_code_objects
                .iter()
                .filter(|(name, def_type)| {
                    def_type
                        .as_ref()
                        .map(|def_index| {
                            matches!(def_index.def_type, DefinitionType::Function(_))
                                && name.starts_with("test_")
                        })
                        .unwrap_or(false)
                })
                .filter_map(|(name, def_type)| {
                    let range = declarations.get(def_type.as_ref().unwrap());
                    Some((name, range))
                })
                .collect();

            let code_lens = functions
                .iter()
                .flat_map(|(name, range)| {
                    let name = *name;
                    let range = range.unwrap();

                    vec![
                        CodeLens {
                            range: *range,
                            command: Some(Command {
                                title: "run test".to_string(),
                                command: "sol.test.file".to_string(),
                                arguments: Some(vec![
                                    Value::String(file_path.to_string()),
                                    Value::String(name.to_string()),
                                ]),
                            }),
                            data: None,
                        },
                        CodeLens {
                            range: *range,
                            command: Some(Command {
                                title: "debug test".to_string(),
                                command: "sol.debug.file".to_string(),
                                arguments: Some(vec![
                                    Value::String(file_path.to_string()),
                                    Value::String(name.clone()),
                                ]),
                            }),
                            data: None,
                        },
                    ]
                })
                .collect();

            return Ok(Some(code_lens));
        }

        Ok(None)
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        tracing::info!(
            "execute command {:?} {:?}",
            params.command,
            params.arguments
        );

        if params.command == "sol.test.file" {
            self.run_test(
                params.arguments[0].as_str().unwrap().to_string(),
                params.arguments[1].as_str().unwrap().to_string(),
            )
            .await;
        } else if params.command == "sol.debug.file" {
            self.debug_test(
                params.arguments[0].as_str().unwrap().to_string(),
                params.arguments[1].as_str().unwrap().to_string(),
            )
            .await;
        }

        Ok(None)
    }
}

const TEMP_FORGE_DUMP_PATH: &str = "/tmp/debug_trace.json";

impl Backend {
    async fn send_log(&self, channel: String, message: String) {
        let params = LogParams { channel, message };
        self.client
            .send_notification::<CustomNotification>(params)
            .await;
    }

    async fn run_test(&self, test_path: String, function_name: String) {
        let workspace = self.workspace.lock().await;
        let workspace_path = workspace.clone();

        let debug_cmd = Forge::test(&function_name, &test_path);
        let output = execute_command(&workspace_path, debug_cmd.clone());

        match output {
            Ok(output) => {
                // Send both stdout and stderr to the client
                self.send_log(
                    "Forge Test".to_string(),
                    format!("{} {:?}", debug_cmd, output.stdout),
                )
                .await;
            }
            Err(e) => {
                self.send_log(
                    "Forge Test Error".to_string(),
                    format!("{debug_cmd} Failed to execute test: {e}"),
                )
                .await;
            }
        }
    }

    async fn debug_test(&self, test_path: String, function_name: String) {
        let workspace = self.workspace.lock().await;
        let workspace_path = workspace.clone();

        let debug_cmd = Forge::debug(&function_name, &test_path, TEMP_FORGE_DUMP_PATH);
        let output = execute_command(&workspace_path, debug_cmd.clone());

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);

                // Send both stdout and stderr to the client
                self.send_log("Forge Test".to_string(), format!("{stdout}"))
                    .await;

                // spawn the dap server
                run_dap_server(&workspace_path);

                // Create debug configuration
                let debug_config = json!({
                    "type": "mock",
                    "name": "Debug Test",
                    "request": "launch",
                    "program": test_path,
                    "stopOnEntry": true
                });

                self.client
                    .send_notification::<CustomNotification2>(debug_config)
                    .await;
            }
            Err(e) => {
                self.send_log(
                    "Forge Test Error".to_string(),
                    format!("{debug_cmd} Failed to execute test: {e}"),
                )
                .await;
            }
        }
    }

    /// Common code for goto_{definitions, implementations, declarations, type_definitions}
    async fn get_reference_from_params(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<DefinitionIndex>> {
        let uri = params.text_document_position_params.text_document.uri;
        let path = uri.to_file_path().map_err(|_| Error {
            code: ErrorCode::InvalidRequest,
            message: format!("Received invalid URI: {uri}").into(),
            data: None,
        })?;

        if let Some(cache) = self.files.caches.get(&path) {
            let f = &cache.file;
            if let Some(offset) = f.get_offset(
                params.text_document_position_params.position.line as _,
                params.text_document_position_params.position.character as _,
            ) {
                if let Some(reference) = cache
                    .references
                    .find(offset, offset + 1)
                    .min_by(|a, b| (a.stop - a.start).cmp(&(b.stop - b.start)))
                {
                    return Ok(Some(reference.val.clone()));
                }
            }
        }
        Ok(None)
    }

    async fn parse_file(&self, uri: Url) {
        let workspace = self.workspace.lock().await;
        tracing::debug!("workspace in parse file: {}", workspace);

        let workspace_lib = PathBuf::from(&*workspace)
            .join("lib/forge-std/src")
            .canonicalize()
            .unwrap();

        tracing::debug!("workspace_lib ...: {}", workspace_lib.to_str().unwrap());

        let mut resolver = FileResolver::default();
        for entry in self.files.text_buffers.iter() {
            let path = entry.key();
            let contents = entry.value().clone();

            resolver.set_file_contents(path.to_str().unwrap(), contents);
        }
        if let Ok(path) = uri.to_file_path() {
            let dir = path.parent().unwrap();
            resolver.add_import_path(dir);

            // Add the lib path to import all the libraries from Forge.
            resolver.add_import_map("forge-std".into(), workspace_lib);

            let mut diags = Vec::new();
            let os_str = path.file_name().unwrap();

            let ns = parse_and_resolve(os_str, &mut resolver, Target::EVM);

            diags.extend(ns.diagnostics.iter().filter_map(|diag| {
                if diag.loc.file_no() != ns.top_file_no() {
                    // The first file is the one we wanted to parse; others are imported
                    return None;
                }

                let severity = match diag.level {
                    ast::Level::Info => Some(DiagnosticSeverity::INFORMATION),
                    ast::Level::Warning => Some(DiagnosticSeverity::WARNING),
                    ast::Level::Error => Some(DiagnosticSeverity::ERROR),
                    ast::Level::Debug => {
                        return None;
                    }
                };

                let related_information = if diag.notes.is_empty() {
                    None
                } else {
                    Some(
                        diag.notes
                            .iter()
                            .map(|note| DiagnosticRelatedInformation {
                                message: note.message.to_string(),
                                location: Location {
                                    uri: Url::from_file_path(&ns.files[note.loc.file_no()].path)
                                        .unwrap(),
                                    range: loc_to_range(&note.loc, &ns.files[ns.top_file_no()]),
                                },
                            })
                            .collect(),
                    )
                };

                let range = loc_to_range(&diag.loc, &ns.files[ns.top_file_no()]);

                Some(Diagnostic {
                    range,
                    message: diag.message.to_string(),
                    severity,
                    related_information,
                    ..Default::default()
                })
            }));

            let res = self.client.publish_diagnostics(uri, diags, None);
            let (file_caches, global_cache) = Builder::new(&ns).build();

            for (f, c) in ns.files.iter().zip(file_caches.into_iter()) {
                if f.cache_no.is_some() {
                    self.files.caches.insert(f.path.clone(), c);
                }
            }

            let mut gc = self.global_cache.lock().await;
            gc.extend(global_cache);

            res.await;
        }
    }
}

/// Calculate the line and column from the Loc offset received from the parser
fn loc_to_range(loc: &pt::Loc, file: &ast::File) -> Range {
    get_range(loc.start(), loc.end(), file)
}

fn get_range(start: usize, end: usize, file: &ast::File) -> Range {
    let (line, column) = file.offset_to_line_column(start);
    let start = Position::new(line as u32, column as u32);
    let (line, column) = file.offset_to_line_column(end);
    let end = Position::new(line as u32, column as u32);

    Range::new(start, end)
}

fn get_range_exclusive(start: usize, end: usize, file: &ast::File) -> Range {
    get_range(start, end - 1, file)
}

/// Start the LSP server
#[derive(Args, Debug)]
pub struct ServerArgs {
    /// Name of the person to greet
    #[arg(short, long)]
    socket: u64,
}

/// Trace a concrete Solidity test file
#[derive(Args, Debug)]
pub struct TraceArgs {
    /// Path to the file to trace
    #[arg(long)]
    pub match_test: String,

    /// Path to the file to trace
    #[arg(long)]
    pub match_path: String,

    /// Path to the workspace  
    /// If not provided, the current directory will be used
    #[arg(long)]
    pub workspace: Option<String>,

    /// Path to store the output of the trace
    #[arg(long)]
    pub dump: Option<String>,

    /// Path to store the pprof output
    #[arg(long)]
    pub pprof: Option<String>,

    /// Path to store the flamegraph output
    #[arg(long)]
    pub flamegraph: Option<String>,
}

impl TraceArgs {
    pub fn execute_trace(&self) -> eyre::Result<DebugTrace> {
        let workspace_path = self.workspace.clone().unwrap_or_else(|| {
            // Use the current directory as the workspace path
            std::env::current_dir()
                .expect("Failed to get current directory")
                .to_str()
                .expect("Failed to convert path to string")
                .to_string()
        });

        let debug_cmd = Forge::debug(&self.match_test, &self.match_path, TEMP_FORGE_DUMP_PATH);
        let _ = execute_command(&workspace_path, debug_cmd.clone())?;

        let (debug_trace, _) = generate_trace(&workspace_path, TEMP_FORGE_DUMP_PATH)?;
        Ok(debug_trace)
    }

    pub fn run(&self) -> eyre::Result<()> {
        let pprof = if self.pprof.is_some() || self.flamegraph.is_some() {
            tracing::info!("Starting pprof profiler");
            let guard = pprof::ProfilerGuard::new(100).unwrap();
            Some(guard)
        } else {
            None
        };

        let debug_trace = self.execute_trace()?;
        if let Some(dump_path) = &self.dump {
            std::fs::write(dump_path, serde_json::to_string(&debug_trace)?)?;
        }

        tracing::info!("Debug trace generated successfully");
        debug_trace.metrics.iter().for_each(|(action, duration)| {
            tracing::info!("{:?}: {:?}", action, duration);
        });

        if let Some(guard) = pprof {
            if let Ok(report) = guard.report().build() {
                if let Some(flamegraph_path) = &self.flamegraph {
                    tracing::info!("Generating flamegraph at {:?}", flamegraph_path);
                    let file = std::fs::File::create(flamegraph_path).unwrap();
                    report.flamegraph(file).unwrap();
                }

                if let Some(pprof_path) = &self.pprof {
                    tracing::info!("Generating pprof at {:?}", pprof_path);
                    let profile = report.pprof().unwrap();
                    let mut file = std::fs::File::create(pprof_path).unwrap();
                    let mut content = Vec::new();
                    profile.encode(&mut content).unwrap();
                    file.write_all(&content).unwrap();
                }
            }
        }

        Ok(())
    }
}

#[derive(Parser, Debug)]
#[command(version(PKG_VERSION), about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Server(ServerArgs),
    Trace(TraceArgs),
}

impl Cli {
    pub async fn run(&self) {
        match &self.command {
            Commands::Server(args) => {
                let (service, socket) = LspService::build(|client| Backend {
                    client,
                    files: Default::default(),
                    workspace: Mutex::new(String::new()),
                    global_cache: Mutex::new(Default::default()),
                })
                .finish();

                // bind to the pipe to create an async stdin/stdout
                tracing::info!("Pipe: {}", args.socket);

                let stream = TcpStream::connect("127.0.0.1:1111").await.unwrap();
                let (read, write) = tokio::io::split(stream);

                Server::new(read, write, socket).serve(service).await;
            }
            Commands::Trace(args) => {
                if let Err(e) = args.run() {
                    eprintln!("Error running trace: {e}");
                }
            }
        }
    }
}

fn run_dap_server(workspace_path: &str) -> u64 {
    tracing::info!("Starting DAP server");
    let port = 50051; // Replace with your desired port number

    // Bind the listener before spawning the task
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    tracing::info!("==> Server listening on port {}", port);

    let workspace_path = String::from(workspace_path);

    tokio::spawn(async move {
        let (stream, _) = listener.accept().unwrap();
        tracing::info!("==> New connection: {}", stream.peer_addr().unwrap());

        let input = BufReader::new(stream.try_clone().unwrap());
        let output = BufWriter::new(stream);

        let (debug_trace, _) = generate_trace(&workspace_path, TEMP_FORGE_DUMP_PATH).unwrap();

        std::fs::write(
            "/tmp/full_trace_dump.json",
            serde_json::to_string(&debug_trace).unwrap(),
        )
        .unwrap();

        let mut server = DapServer::new(input, output);
        server.serve(|client| DapDebugger::new(client, debug_trace));
    });

    port
}
