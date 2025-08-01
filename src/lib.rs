use builder::{error_type_to_code, get_type_definition, DefinitionIndex};
use built_info::PKG_VERSION;
use clap::{Args, Parser, Subcommand};
use dashmap::mapref::entry::Entry;
use debugger::debugger::DapDebugger;
use debugger::tracer::Builder as TraceBuilder;
use forge_fmt::fmt;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solang::sema::ast;
use solang::sema::builtin::{BUILTIN_FUNCTIONS, BUILTIN_METHODS, BUILTIN_VARIABLE};
use solang::sema::builtin_structs::BUILTIN_STRUCTS;
use solang::{parse_and_resolve, Target};
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::{Error, ErrorCode, Result};
use tower_lsp::lsp_types::notification::Notification;
use tower_lsp::lsp_types::request::GotoTypeDefinitionResponse;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::builder::{Builder, Files, GlobalCache};
use crate::builder::{DefinitionType, Hints};
use crate::config::Config;
use crate::debugger::tracer::DebugTrace;
use crate::forge::Forge;
use crate::position_tracker::PositionTracker;
use crate::symbol_indexer::SymbolIndexer;
use pprof::protos::Message;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::OnceLock;

use solang::file_resolver::FileResolver;
use solang_parser::pt;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use tokio::net::TcpStream;

use crate::debugger::dap::Server as DapServer;

// Include the generated-file as a separate module
pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

mod builder;
mod config;
mod debugger;
mod forge;
mod position_tracker;
mod symbol_indexer;

struct Backend {
    client: Client,
    files: Arc<Files>,
    workspace: Mutex<String>,
    global_cache: Arc<Mutex<GlobalCache>>,
    config: Mutex<Config>,
    tx: OnceLock<tokio::sync::mpsc::Sender<ParseRequest>>,
    symbol_indexer: Arc<SymbolIndexer>,
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
        let mut config = Config::default();
        if let Some(opts) = params.initialization_options {
            config = Config::from_json(opts).map_err(|e| Error {
                code: ErrorCode::InvalidParams,
                message: format!("Failed to parse initialization options: {e}").into(),
                data: None,
            })?;
        }

        let mut config_guard = self.config.lock().await;
        *config_guard = config.clone();

        let workspace = params.root_uri.unwrap_or_else(|| {
            // If no root URI is provided, use the current directory as the workspace
            // In E2E tests, this is the case.
            Url::from_file_path(std::env::current_dir().unwrap()).unwrap()
        });
        let workspace_path = workspace.path().to_string();

        let mut workspace_guard = self.workspace.lock().await;
        *workspace_guard = workspace_path.clone();

        self.tx.get_or_init(|| {
            let (tx, rx) = tokio::sync::mpsc::channel(5);
            self.spawn_compiler_routine(workspace_path.clone(), config.clone(), rx);

            tx
        });

        self.symbol_indexer.track(PathBuf::from(workspace_path));

        Ok(InitializeResult {
            server_info: None,
            offset_encoding: None,
            capabilities: ServerCapabilities {
                inlay_hint_provider: Some(OneOf::Left(true)),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![".".to_string()]),
                    all_commit_characters: None,
                    work_done_progress_options: Default::default(),
                    completion_item: None,
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["sol.test.file".to_string(), "sol.debug.file".to_string()],
                    work_done_progress_options: Default::default(),
                }),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: Some(true),
                        work_done_progress_options: Default::default(),
                    },
                )),
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
                document_formatting_provider: Some(OneOf::Left(true)),
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
        let version = params.text_document.version;
        let uri = params.text_document.uri;

        match uri.to_file_path() {
            Ok(path) => {
                self.files
                    .update_text_file(&path, params.text_document.text);
                self.parse_file(uri, version, true).await;
            }
            Err(_) => {
                self.client
                    .log_message(MessageType::ERROR, format!("received invalid URI: {uri}"))
                    .await;
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let version = params.text_document.version;
        let uri = params.text_document.uri.clone();

        match uri.to_file_path() {
            Ok(path) => {
                // Adjust existing hint positions
                let had_hints = self
                    .adjust_cached_hints(&path, &params.content_changes, version)
                    .await;

                if had_hints {
                    // Refresh with adjusted positions
                    self.client.inlay_hint_refresh().await.ok();
                }

                self.files.update_partial_text_file(&path, params);
                self.parse_file(uri, version, false).await;
            }
            Err(_) => {
                self.client
                    .log_message(MessageType::ERROR, format!("received invalid URI: {uri}"))
                    .await;
            }
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        tracing::info!("Saving file");
        let uri = params.text_document.uri;

        if let Some(text) = params.text {
            if let Ok(path) = uri.to_file_path() {
                self.files.update_text_file(&path, text);
            }
        }

        self.parse_file(uri, i32::MAX, true).await;
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

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let source_path = uri.to_file_path().map_err(|_| Error {
            code: ErrorCode::InvalidRequest,
            message: format!("Received invalid URI: {uri}").into(),
            data: None,
        })?;

        if let Some(file_hints) = self.files.hints.get(&source_path) {
            // check if there is any function in scope
            let valid_hints = file_hints
                .hints
                .iter()
                .filter(|hint| self.position_in_range(&hint.position, &params.range))
                .cloned()
                .collect::<Vec<InlayHint>>();
            return Ok(Some(valid_hints));
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

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let def_params: GotoDefinitionParams = GotoDefinitionParams {
            text_document_position_params: params.text_document_position,
            work_done_progress_params: params.work_done_progress_params,
            partial_result_params: Default::default(),
        };
        let Some(reference) = self.get_reference_from_params(def_params).await? else {
            return Ok(None);
        };

        let new_text = params.new_name;

        let ws = self
            .files
            .caches
            .iter()
            .map(|entry| {
                let p = entry.key();
                let cache = entry.value();

                let uri = Url::from_file_path(p).unwrap();
                let text_edits: Vec<_> = cache
                    .references
                    .iter()
                    .filter(|r| r.val == reference)
                    .map(|r| TextEdit {
                        range: get_range_exclusive(r.start, r.stop, &cache.file),
                        new_text: new_text.clone(),
                    })
                    .collect();
                (uri, text_edits)
            })
            .collect::<HashMap<_, _>>();

        Ok(Some(WorkspaceEdit::new(ws)))
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

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        tracing::info!("Formatting requested for {:?}", params.text_document.uri);

        // get parse tree for the input file
        let uri = params.text_document.uri;
        let source_path = uri.to_file_path().map_err(|_| Error {
            code: ErrorCode::InvalidRequest,
            message: format!("Received invalid URI: {uri}").into(),
            data: None,
        })?;

        let source = self
            .files
            .text_buffers
            .get(&source_path)
            .ok_or_else(|| Error {
                code: ErrorCode::InvalidRequest,
                message: format!("File not found: {uri}").into(),
                data: None,
            })?;

        let source_formatted = match fmt(&source) {
            Ok(formatted) => formatted,
            Err(e) => {
                // This most likely is a syntax error on the user part. If we were to return an error
                // it would show up as a modal window in the editor, which is not ideal.
                tracing::debug!("Failed to format file: {}: {}", uri, e);
                return Ok(None);
            }
        };

        // create a `TextEdit` instance that replaces the contents of the file with the formatted text
        let text_edit = TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: u32::MAX,
                    character: u32::MAX,
                },
            },
            new_text: source_formatted,
        };

        Ok(Some(vec![text_edit]))
    }

    async fn code_action_resolve(&self, params: CodeAction) -> Result<CodeAction> {
        tracing::info!("Do code action resolve: {:?}", params);

        let data = params.data.as_ref().ok_or_else(|| Error {
            code: ErrorCode::InvalidParams,
            message: "Code action data is missing".into(),
            data: None,
        })?;

        let metadata: CodeActionMetadata = serde_json::from_value(data.clone()).unwrap();

        let mut params = params.clone();
        params.edit = Some(WorkspaceEdit {
            changes: Some({
                let mut changes = std::collections::HashMap::new();
                changes.insert(
                    metadata.target_file,
                    vec![TextEdit {
                        range: Range {
                            start: Position {
                                line: 0,
                                character: 0,
                            },
                            end: Position {
                                line: 0,
                                character: 0,
                            },
                        },
                        new_text: format!(
                            "import {{{}}} from \"{}\";\n",
                            metadata.unknown_type,
                            metadata.import_location.display(),
                        ),
                    }],
                );
                changes
            }),
            ..Default::default()
        });

        Ok(params)
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let path = uri.clone().to_file_path().unwrap();

        if let Some(cache) = self.files.caches.get(&path) {
            if let Some(reference) = cache
                .unknown_types
                .iter()
                .find(|(loc, _)| loc.start == params.range.start)
            // TODO (check end of range too)
            {
                let unknown_type = &reference.1;

                if let Some(symbol_locations) =
                    self.symbol_indexer.find_symbol_locations(unknown_type)
                {
                    let mut actions = Vec::new();

                    for location in symbol_locations {
                        // we need to create an import location from the uri path to the location.file_path
                        let res = pathdiff::diff_paths(
                            location.clone().file_path,
                            path.parent().expect("parent"),
                        )
                        .expect("Failed to compute relative path");

                        let metadata = CodeActionMetadata {
                            unknown_type: unknown_type.clone(),
                            import_location: res.clone(),
                            target_file: uri.clone(),
                        };

                        // Create the code action
                        let action = CodeAction {
                            title: format!("Import {} from {}", unknown_type, res.display()),
                            kind: Some(CodeActionKind::QUICKFIX),
                            is_preferred: Some(true),
                            data: Some(serde_json::to_value(metadata).unwrap()),
                            ..Default::default()
                        };

                        actions.push(CodeActionOrCommand::CodeAction(action));
                    }

                    if !actions.is_empty() {
                        return Ok(Some(actions));
                    }
                }
            }
        }

        Ok(None)
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

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let path = uri.to_file_path().map_err(|_| Error {
            code: ErrorCode::InvalidRequest,
            message: format!("Received invalid URI: {uri}").into(),
            data: None,
        })?;

        let Some(cache) = self.files.caches.get(&path) else {
            return Ok(None);
        };

        let offset = cache
            .file
            .get_offset(
                params.text_document_position.position.line as _,
                params.text_document_position.position.character as _,
            )
            .unwrap();

        let builtin_functions = BUILTIN_FUNCTIONS
            .iter()
            .filter(|function| function.target.is_empty() || function.target.contains(&Target::EVM))
            .map(|function| (function.name.to_string(), None));
        let builtin_variables = BUILTIN_VARIABLE
            .iter()
            .filter(|var| var.target.is_empty() || var.target.contains(&Target::EVM))
            .map(|var| (var.name.to_string(), None));

        // Get all the code objects available from the lexical scope from which the request was raised.
        let code_objects_in_scope = cache
            .scopes
            .find(offset, offset + 1)
            // get all the enclosing scopes
            .flat_map(|scope| scope.val.iter().cloned())
            // get the top level code objects in the file
            .chain(cache.top_level_code_objects.clone())
            // builtins
            .chain(builtin_functions)
            .chain(builtin_variables)
            .collect::<HashMap<_, _>>();

        let global_cache = self.global_cache.lock().await;

        let suggestions = match params.context {
            Some(CompletionContext {
                trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
                trigger_character: Some(trigger_character),
            }) if trigger_character == "." => {
                let Some(text_buf) = self.files.text_buffers.get(&path) else {
                    return Ok(None);
                };

                let mut builtin_methods =
                    HashMap::<DefinitionType, HashMap<String, Option<DefinitionIndex>>>::new();
                for method in BUILTIN_METHODS.iter().filter(|method| {
                    method.target.is_empty() || method.target.contains(&Target::EVM)
                }) {
                    if let Some(def_type) = get_type_definition(&method.method[0]) {
                        builtin_methods
                            .entry(def_type)
                            .or_default()
                            .insert(method.name.to_string(), None);
                    }
                }

                let builtin_structs = BUILTIN_STRUCTS
                    .iter()
                    .map(|r#struct| {
                        let def_type = DefinitionType::Struct(r#struct.struct_type);
                        let fields = r#struct
                            .struct_decl
                            .fields
                            .iter()
                            .map(|field| (field.name_as_str().to_string(), None))
                            .collect();
                        (def_type, fields)
                    })
                    .collect::<HashMap<_, HashMap<_, _>>>();

                // Extract code object from source code for which `Completion` request was triggered.
                // Extracts all the characters connected to the "." character.
                // This includes all the alphanumeric characters that come before the triggering "."
                // and the interspersed "." characters between the alphanumeric characters.
                let code_object = {
                    let buffer = text_buf.chars().collect_vec();
                    let mut curr: isize = offset as isize - 2;
                    while curr >= 0
                        && (buffer[curr as usize].is_ascii_alphanumeric()
                            || buffer[curr as usize] == '.')
                    {
                        curr -= 1;
                    }
                    curr = isize::max(curr, 0);
                    if !buffer[curr as usize].is_ascii_alphanumeric() {
                        curr += 1;
                    }
                    let name = buffer[curr as usize..offset - 1].iter().collect::<String>();

                    name
                };

                // Get an iterator that iterates over all parts of the code object.
                // The parts are basically a field, a variant or a method defined on the previous part.
                let mut code_object_parts = code_object.split('.');

                // `properties` gives the list of fields, variants and methods defined for the code object in question.
                let properties = code_object_parts.next().and_then(|symbol| {
                    code_objects_in_scope
                        .get(symbol)
                        .and_then(|def_index| def_index.as_ref())
                        .and_then(|def_index| {
                            global_cache
                                .properties
                                .get(def_index)
                                .or_else(|| builtin_methods.get(&def_index.def_type))
                                .or_else(|| builtin_structs.get(&def_index.def_type))
                        })
                });
                let properties = code_object_parts.fold(properties, |acc, prop| {
                    acc.and_then(|properties| properties.get(prop))
                        .and_then(|def_index| def_index.as_ref())
                        .and_then(|def_index| {
                            global_cache
                                .properties
                                .get(def_index)
                                .or_else(|| builtin_methods.get(&def_index.def_type))
                                .or_else(|| builtin_structs.get(&def_index.def_type))
                        })
                });

                // Return a list of suggestions using the `properties` extracted previously by converting them into the expected format.
                properties.map(|properties| {
                    properties
                        .keys()
                        .map(|name| CompletionItem {
                            label: name.clone(),
                            ..Default::default()
                        })
                        .collect_vec()
                })
            }
            Some(CompletionContext {
                trigger_kind: CompletionTriggerKind::INVOKED,
                ..
            }) => {
                let suggestions = code_objects_in_scope
                    .into_keys()
                    .map(|label| CompletionItem {
                        label: label.clone(),
                        ..Default::default()
                    })
                    .collect_vec();
                Some(suggestions)
            }
            _ => None,
        };

        Ok(suggestions.map(CompletionResponse::Array))
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeActionMetadata {
    unknown_type: String,
    import_location: PathBuf,
    target_file: Url,
}

const TEMP_FORGE_DUMP_PATH: &str = "/tmp/debug_trace.json";

struct ParseRequest {
    url: Url,
    version: i32,
    response_tx: tokio::sync::oneshot::Sender<()>,
}

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

        let forge = Forge::new()
            .expect("Forge not found")
            .workspace_path(workspace_path);

        let test_cmd = forge.test(&function_name, &test_path);
        let test_cmd_args = test_cmd.args();

        let output = test_cmd.execute();

        match output {
            Ok(output) => {
                // Send both stdout and stderr to the client
                self.send_log(
                    "Forge Test".to_string(),
                    format!("{} {:?}", test_cmd_args, output.stdout),
                )
                .await;
            }
            Err(e) => {
                self.send_log(
                    "Forge Test Error".to_string(),
                    format!("{test_cmd_args} Failed to execute test: {e}"),
                )
                .await;
            }
        }
    }

    async fn debug_test(&self, test_path: String, function_name: String) {
        let workspace = self.workspace.lock().await;
        let workspace_path = workspace.clone();

        let forge = Forge::new()
            .expect("Forge not found")
            .workspace_path(workspace_path.clone());

        let debug_cmd = forge.debug(&function_name, &test_path, TEMP_FORGE_DUMP_PATH);
        let debug_cmd_args = debug_cmd.args();

        let output = debug_cmd.execute();

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
                    format!("{debug_cmd_args} Failed to execute test: {e}"),
                )
                .await;
            }
        }
    }

    fn position_in_range(&self, position: &Position, range: &Range) -> bool {
        (position.line > range.start.line
            || (position.line == range.start.line && position.character >= range.start.character))
            && (position.line < range.end.line
                || (position.line == range.end.line && position.character <= range.end.character))
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

    async fn adjust_cached_hints(
        &self,
        uri: &PathBuf,
        changes: &[TextDocumentContentChangeEvent],
        version: i32,
    ) -> bool {
        if let Some(mut file_hints) = self.files.hints.get_mut(uri) {
            // Create position tracker for all changes
            let tracker = PositionTracker::new(changes);

            // Collect original positions
            let original_positions: Vec<Position> =
                file_hints.hints.iter().map(|hint| hint.position).collect();

            // Adjust all positions using the tracker
            let adjusted_positions = tracker.adjust_positions(original_positions);

            // Update the hints with new positions
            for (hint, new_position) in file_hints.hints.iter_mut().zip(adjusted_positions.iter()) {
                hint.position = *new_position;
            }

            // Remove any hints that became invalid (tracker returns fewer positions)
            file_hints.hints.truncate(adjusted_positions.len());
            file_hints.version = version;

            true // We had cached data and updated it
        } else {
            false // No cached data for this file
        }
    }

    async fn parse_file(&self, uri: Url, version: i32, wait: bool) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = ParseRequest {
            url: uri.clone(),
            version,
            response_tx: tx,
        };

        if let Some(sender) = self.tx.get() {
            if let Err(e) = sender.send(request).await {
                tracing::error!("Failed to send parse request for: {}: {}", uri, e);
                return;
            }
        } else {
            tracing::error!("Compiler routine not initialized, cannot parse file");
            return;
        }

        // Wait for the response
        if wait {
            let _ = rx.await;
        }
    }

    fn spawn_compiler_routine(
        &self,
        workspace: String,
        config: Config,
        mut rx: tokio::sync::mpsc::Receiver<ParseRequest>,
    ) {
        let files = Arc::clone(&self.files);
        let global_cache = Arc::clone(&self.global_cache);
        let client = self.client.clone();

        tracing::info!("Compiler spawning task started, workspace: {}", workspace);

        let workspace_path = PathBuf::from(&*workspace);

        let lib_remappings = discover_lib_remappings(&workspace_path);
        tracing::info!("Discovered {} library remappings", lib_remappings.len());

        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                tracing::info!(
                    "Received parse request for: {} version {}",
                    req.url,
                    req.version
                );

                let uri = req.url.clone();

                let mut resolver = FileResolver::default();
                for entry in files.text_buffers.iter() {
                    let path = entry.key();
                    let contents = entry.value().clone();

                    resolver.set_file_contents(path.to_str().unwrap(), contents);
                }
                if let Ok(path) = uri.to_file_path() {
                    let dir = path.parent().unwrap();
                    resolver.add_import_path(dir);

                    // Add all precomputed library remappings
                    for (lib_name, lib_path) in &lib_remappings {
                        resolver.add_import_map(lib_name.clone().into(), lib_path.clone());
                    }

                    let mut diags = Vec::new();
                    let os_str = path.file_name().unwrap();

                    let ns = parse_and_resolve(os_str, &mut resolver, Target::EVM);

                    diags.extend(ns.diagnostics.iter().filter_map(|diag| {
                        if diag.loc.file_no() != ns.top_file_no() {
                            // The first file is the one we wanted to parse; others are imported
                            return None;
                        }

                        let diag_ty = diag.ty.clone();

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
                                            uri: Url::from_file_path(
                                                &ns.files[note.loc.file_no()].path,
                                            )
                                            .unwrap(),
                                            range: loc_to_range(
                                                &note.loc,
                                                &ns.files[ns.top_file_no()],
                                            ),
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
                            code: Some(NumberOrString::String(error_type_to_code(diag_ty))),
                            source: Some("solstice".to_string()),
                            ..Default::default()
                        })
                    }));

                    let res = client.publish_diagnostics(uri, diags, None);
                    let (file_caches, sub_global_cache) = Builder::new(&ns).build(&config);

                    for (f, c) in ns.files.iter().zip(file_caches.into_iter()) {
                        if f.cache_no.is_some() {
                            // Update the hints only if the same or higher, if it is the same the hints from the parser
                            // take precedence
                            // Update hints only if this version is newer or equal
                            match files.hints.entry(f.path.clone()) {
                                Entry::Occupied(mut entry) => {
                                    if req.version >= entry.get().version {
                                        entry.insert(Hints {
                                            hints: c.available_hints.clone(),
                                            version: req.version,
                                        });
                                    }
                                }
                                Entry::Vacant(entry) => {
                                    // No existing hints, always insert
                                    entry.insert(Hints {
                                        hints: c.available_hints.clone(),
                                        version: req.version,
                                    });
                                }
                            }

                            files.caches.insert(f.path.clone(), c);
                        }
                    }

                    let mut gc = global_cache.lock().await;
                    gc.extend(sub_global_cache);

                    res.await;

                    // ask again for code lens because now we do it async so the system might be asking for
                    // code lens before the compilation is done
                    let _ = client.code_lens_refresh().await;

                    // Notify inlay hints
                    let _ = client.inlay_hint_refresh().await;

                    // notify that the compilation routine is done
                    let _ = req.response_tx.send(());
                }
            }
        });
    }
}

// Helper function definition
fn discover_lib_remappings(workspace_path: &std::path::Path) -> Vec<(String, PathBuf)> {
    let mut remappings = Vec::new();
    let lib_dir = workspace_path.join("lib");

    if !lib_dir.exists() || !lib_dir.is_dir() {
        tracing::debug!("No lib directory found at {}", lib_dir.display());
        return remappings;
    }

    match std::fs::read_dir(&lib_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        if let Some(lib_name) = entry.file_name().to_str() {
                            let lib_src_path = entry.path().join("src");

                            if lib_src_path.exists() && lib_src_path.is_dir() {
                                match lib_src_path.canonicalize() {
                                    Ok(canonical_path) => {
                                        tracing::debug!(
                                            "Found library: {} -> {}",
                                            lib_name,
                                            canonical_path.display()
                                        );
                                        remappings.push((lib_name.to_string(), canonical_path));
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Could not canonicalize {} library path: {}",
                                            lib_name,
                                            e
                                        );
                                    }
                                }
                            } else {
                                tracing::debug!(
                                    "Library {} has no src directory, skipping",
                                    lib_name
                                );
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!("Could not read lib directory: {}", e);
        }
    }

    tracing::info!(
        "Discovered libraries: {:?}",
        remappings.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    remappings
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

        let _ = Forge::new()
            .expect("Failed to find forge")
            .debug(&self.match_test, &self.match_path, TEMP_FORGE_DUMP_PATH)
            .execute()?;

        let (debug_trace, _) =
            TraceBuilder::new(&workspace_path, TEMP_FORGE_DUMP_PATH)?.generate_trace()?;
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
                    files: Arc::new(Default::default()),
                    workspace: Mutex::new(String::new()),
                    global_cache: Arc::new(Mutex::new(Default::default())),
                    config: Mutex::new(Config::default()),
                    tx: OnceLock::new(),
                    symbol_indexer: Arc::new(SymbolIndexer::new()),
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

        let (debug_trace, _) = TraceBuilder::new(&workspace_path, TEMP_FORGE_DUMP_PATH)
            .unwrap()
            .generate_trace()
            .unwrap();

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
