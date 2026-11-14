use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use luat::analysis::{AnalysisHost, AnalysisSnapshot, CompletionKind, FileRange, TextEdit};
use tokio::sync::RwLock;
use tower_lsp_server::ls_types as lsp;
use tower_lsp_server::{
    Client, LanguageServer, LspService, Server,
    jsonrpc::Result,
    ls_types::{
        CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams,
        CompletionResponse, Diagnostic as LspDiagnostic, DiagnosticSeverity,
        DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
        DidOpenTextDocumentParams, Documentation, GotoDefinitionResponse, Hover, HoverContents,
        HoverParams, HoverProviderCapability, InitializeParams, InitializeResult, Location,
        MarkupContent, MarkupKind, MessageType, OneOf, ParameterLabel, Position, ReferenceParams,
        RenameParams, ServerCapabilities, SignatureHelp as LspSignatureHelp, SignatureHelpParams,
        SignatureInformation as LspSignatureInformation, TextDocumentSyncCapability,
        TextDocumentSyncKind, TextEdit as LspTextEdit, Uri, WorkspaceEdit,
    },
};

#[derive(Clone)]
struct Document {
    path: PathBuf,
    text: Arc<String>,
    version: i32,
    index: Arc<LineIndex>,
}

struct RequestSnapshot {
    analysis: AnalysisSnapshot,
    generation: u64,
}

impl std::ops::Deref for RequestSnapshot {
    type Target = AnalysisSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.analysis
    }
}

struct Backend {
    client: Client,
    host: Arc<RwLock<AnalysisHost>>,
    documents: Arc<RwLock<BTreeMap<Uri, Document>>>,
    generation: Arc<AtomicU64>,
    analysis: Arc<tokio::sync::Mutex<()>>,
    settings: Arc<RwLock<EditorSettings>>,
    inlay_refresh: Arc<std::sync::atomic::AtomicBool>,
    semantic_refresh: Arc<std::sync::atomic::AtomicBool>,
    editor: tokio::sync::Mutex<()>,
    pending: Arc<tokio::sync::Notify>,
}

impl Backend {
    fn new(client: Client) -> Self {
        let backend = Self {
            client,
            host: Arc::new(RwLock::new(AnalysisHost::new())),
            documents: Arc::new(RwLock::new(BTreeMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
            analysis: Arc::new(tokio::sync::Mutex::new(())),
            settings: Arc::new(RwLock::new(EditorSettings::default())),
            inlay_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            semantic_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            editor: tokio::sync::Mutex::new(()),
            pending: Arc::new(tokio::sync::Notify::new()),
        };
        backend.start_analysis_worker();
        tokio::task::spawn_blocking(AnalysisHost::warm_builtins);
        backend
    }

    async fn set_document(&self, uri: Uri, path: PathBuf, text: String, version: i32) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.host.write().await.set_file(path.clone(), text.clone());
        self.documents.write().await.insert(
            uri.clone(),
            Document {
                path: path.clone(),
                index: Arc::new(LineIndex::new(&text)),
                text: Arc::new(text),
                version,
            },
        );
        self.schedule_analysis().await;
    }

    async fn schedule_analysis(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.pending.notify_one();
    }

    fn start_analysis_worker(&self) {
        let shared_host = Arc::clone(&self.host);
        let documents = Arc::clone(&self.documents);
        let generation = Arc::clone(&self.generation);
        let analysis = Arc::clone(&self.analysis);
        let client = self.client.clone();
        let settings = Arc::clone(&self.settings);
        let pending = Arc::clone(&self.pending);
        let inlay_refresh = Arc::clone(&self.inlay_refresh);
        let semantic_refresh = Arc::clone(&self.semantic_refresh);
        tokio::spawn(async move {
            let mut indexed = false;
            loop {
                pending.notified().await;
                while indexed {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(150)) => break,
                        _ = pending.notified() => {}
                    }
                }
                let _analysis = analysis.lock().await;
                let revision = generation.load(Ordering::SeqCst);
                let mut host = shared_host.read().await.clone();
                let cancelled = Arc::clone(&generation);
                let computed = tokio::task::spawn_blocking(move || {
                    let snapshot =
                        host.snapshot_cancellable(&|| cancelled.load(Ordering::SeqCst) != revision);
                    (host, snapshot)
                })
                .await;
                let Ok((host, Some(snapshot))) = computed else {
                    continue;
                };
                indexed = true;
                {
                    let mut current = shared_host.write().await;
                    if generation.load(Ordering::SeqCst) != revision {
                        continue;
                    }
                    *current = host;
                }
                let current = documents.read().await;
                if generation.load(Ordering::SeqCst) != revision {
                    continue;
                }
                let settings = settings.read().await.clone();
                let published = current
                    .iter()
                    .filter_map(|(uri, document)| {
                        if snapshot.source(&document.path) != Some(document.text.as_str()) {
                            return None;
                        }
                        let mut diagnostics: Vec<_> = snapshot
                            .diagnostics(&document.path)
                            .unwrap_or_default()
                            .iter()
                            .map(|diagnostic| LspDiagnostic {
                                range: document
                                    .index
                                    .range(&document.text, diagnostic.source_range.clone()),
                                severity: Some(DiagnosticSeverity::ERROR),
                                code: Some(diagnostic.code.into()),
                                source: Some("LuaT".to_owned()),
                                message: diagnostic.message.clone(),
                                ..Default::default()
                            })
                            .collect();
                        diagnostics.extend(editor_diagnostics(&snapshot, document, &settings));
                        Some((uri.clone(), diagnostics, document.version))
                    })
                    .collect::<Vec<_>>();
                drop(current);
                drop(_analysis);
                for (uri, diagnostics, version) in published {
                    if generation.load(Ordering::SeqCst) != revision {
                        break;
                    }
                    client
                        .publish_diagnostics(uri, diagnostics, Some(version))
                        .await;
                }
                if generation.load(Ordering::SeqCst) != revision {
                    continue;
                }
                if semantic_refresh.load(Ordering::Relaxed) {
                    let _ = client.semantic_tokens_refresh().await;
                }
                if inlay_refresh.load(Ordering::Relaxed) {
                    let _ = client.inlay_hint_refresh().await;
                }
            }
        });
    }

    async fn editor_snapshot(&self, document: &Document) -> Result<Option<RequestSnapshot>> {
        let _editor = self.editor.lock().await;
        let revision = self.generation.load(Ordering::SeqCst);
        let mut host = self.host.read().await.clone();
        let (host, snapshot) = tokio::task::spawn_blocking(move || {
            let snapshot = host.editor_snapshot();
            (host, snapshot)
        })
        .await
        .map_err(|_| tower_lsp_server::jsonrpc::Error::internal_error())?;
        let mut current = self.host.write().await;
        if self.generation.load(Ordering::SeqCst) != revision
            || snapshot.source(&document.path) != Some(document.text.as_str())
        {
            return Err(tower_lsp_server::jsonrpc::Error::content_modified());
        }
        current.adopt_editor_cache(&host);
        Ok(Some(RequestSnapshot {
            analysis: snapshot,
            generation: revision,
        }))
    }

    async fn snapshot(&self) -> Result<Option<RequestSnapshot>> {
        let _analysis = self.analysis.lock().await;
        let revision = self.generation.load(Ordering::SeqCst);
        let mut host = self.host.read().await.clone();
        let cancelled = Arc::clone(&self.generation);
        let (host, snapshot) = tokio::task::spawn_blocking(move || {
            let snapshot =
                host.snapshot_cancellable(&|| cancelled.load(Ordering::SeqCst) != revision);
            (host, snapshot)
        })
        .await
        .map_err(|_| tower_lsp_server::jsonrpc::Error::internal_error())?;
        let mut current = self.host.write().await;
        if self.generation.load(Ordering::SeqCst) != revision {
            return Err(tower_lsp_server::jsonrpc::Error::content_modified());
        }
        *current = host;
        Ok(snapshot.map(|analysis| RequestSnapshot {
            analysis,
            generation: revision,
        }))
    }

    async fn document(&self, uri: &Uri) -> Option<Document> {
        self.documents.read().await.get(uri).cloned()
    }

    async fn snapshot_for_document(&self, document: &Document) -> Result<Option<RequestSnapshot>> {
        let snapshot = self.snapshot().await?;
        if snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.source(&document.path))
            != Some(document.text.as_str())
        {
            return Err(tower_lsp_server::jsonrpc::Error::content_modified());
        }
        Ok(snapshot)
    }

    fn response<T>(&self, snapshot: &RequestSnapshot, value: T) -> Result<T> {
        if self.generation.load(Ordering::SeqCst) != snapshot.generation {
            return Err(tower_lsp_server::jsonrpc::Error::content_modified());
        }
        Ok(value)
    }

    fn location(&self, range: &FileRange, snapshot: &AnalysisSnapshot) -> Option<Location> {
        let uri = Uri::from_file_path(&range.path)?;
        let source = match snapshot.source(&range.path) {
            Some(source) => std::borrow::Cow::Borrowed(source),
            None => std::borrow::Cow::Owned(std::fs::read_to_string(&range.path).ok()?),
        };
        Some(Location::new(
            uri,
            byte_range_to_lsp(&source, range.range.clone()),
        ))
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.semantic_refresh.store(
            params
                .capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.semantic_tokens.as_ref())
                .and_then(|tokens| tokens.refresh_support)
                .unwrap_or(false),
            Ordering::Relaxed,
        );
        if let Some(options) = params.initialization_options {
            *self.settings.write().await = EditorSettings::from_value(&options);
        }
        self.inlay_refresh.store(
            params
                .capabilities
                .workspace
                .and_then(|workspace| workspace.inlay_hint)
                .and_then(|hints| hints.refresh_support)
                .unwrap_or(false),
            Ordering::Relaxed,
        );
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                semantic_tokens_provider: Some(tower_lsp_server::ls_types::SemanticTokensServerCapabilities::SemanticTokensOptions(tower_lsp_server::ls_types::SemanticTokensOptions {
                    legend: tower_lsp_server::ls_types::SemanticTokensLegend {
                        token_types: ["variable", "parameter", "type", "function", "property", "operator"].into_iter().map(tower_lsp_server::ls_types::SemanticTokenType::new).collect(),
                        token_modifiers: ["readonly", "declaration", "defaultLibrary", "deprecated"].into_iter().map(tower_lsp_server::ls_types::SemanticTokenModifier::new).collect(),
                    },
                    full: Some(tower_lsp_server::ls_types::SemanticTokensFullOptions::Bool(true)),
                    ..Default::default()
                })),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_owned(), "@".to_owned(), "-".to_owned(), " ".to_owned()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                signature_help_provider: Some(
                    tower_lsp_server::ls_types::SignatureHelpOptions {
                        trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
                        ..Default::default()
                    },
                ),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(lsp::CodeActionProviderCapability::Options(lsp::CodeActionOptions { code_action_kinds: Some(vec![lsp::CodeActionKind::QUICKFIX]), ..Default::default() })),
                inlay_hint_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: tower_lsp_server::ls_types::InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "LuaT language server initialized")
            .await;
    }

    async fn semantic_tokens_full(
        &self,
        params: tower_lsp_server::ls_types::SemanticTokensParams,
    ) -> Result<Option<tower_lsp_server::ls_types::SemanticTokensResult>> {
        use tower_lsp_server::ls_types::{SemanticToken, SemanticTokens, SemanticTokensResult};
        let Some(document) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        let Some(source) = snapshot.source(&document.path) else {
            return Ok(None);
        };
        let index = &document.index;
        let mut previous = Position::new(0, 0);
        let data = snapshot
            .semantic_tokens(&document.path)
            .into_iter()
            .filter_map(|(range, kind, modifiers)| {
                let start = index.offset_to_position(source, range.start);
                let end = index.offset_to_position(source, range.end);
                if start.line != end.line || end.character <= start.character {
                    return None;
                }
                let delta_line = start.line - previous.line;
                let delta_start = if delta_line == 0 {
                    start.character - previous.character
                } else {
                    start.character
                };
                previous = start;
                Some(SemanticToken {
                    delta_line,
                    delta_start,
                    length: end.character - start.character,
                    token_type: kind,
                    token_modifiers_bitset: modifiers,
                })
            })
            .collect();
        self.response(
            &snapshot,
            Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data,
            })),
        )
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        let Some(path) = document.uri.to_file_path().map(PathBuf::from) else {
            return;
        };
        self.set_document(document.uri, path, document.text, document.version)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(mut document) = self.document(&uri).await else {
            return;
        };
        if params.text_document.version < document.version {
            return;
        }
        for change in params.content_changes {
            if let Some(range) = change.range {
                let index = LineIndex::new(&document.text);
                let start = index.position_to_offset(&document.text, range.start);
                let end = index.position_to_offset(&document.text, range.end);
                if start <= end {
                    Arc::make_mut(&mut document.text).replace_range(start..end, &change.text);
                }
            } else {
                document.text = Arc::new(change.text);
            }
        }
        self.set_document(
            uri,
            document.path,
            document.text.as_ref().clone(),
            params.text_document.version,
        )
        .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        if let Some(document) = self
            .documents
            .write()
            .await
            .remove(&params.text_document.uri)
        {
            self.host.write().await.remove_file(&document.path);
            self.generation.fetch_add(1, Ordering::Relaxed);
            self.schedule_analysis().await;
            self.client
                .publish_diagnostics(params.text_document.uri, Vec::new(), None)
                .await;
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let changed_paths = params
            .changes
            .into_iter()
            .filter_map(|change| {
                change
                    .uri
                    .to_file_path()
                    .map(|path| (PathBuf::from(path), change.typ))
            })
            .collect::<Vec<_>>();
        if changed_paths.is_empty() {
            return;
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        let mut host = self.host.write().await;
        for (path, kind) in changed_paths {
            if kind == lsp::FileChangeType::CHANGED {
                host.invalidate_file(&path);
            } else {
                host.invalidate_path(&path);
            }
        }
        drop(host);
        self.schedule_analysis().await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let position = params.text_document_position;
        let Some(document) = self.document(&position.text_document.uri).await else {
            return Ok(None);
        };
        let offset = document
            .index
            .position_to_offset(&document.text, position.position);
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        let items = snapshot
            .completions(&document.path, offset)
            .into_iter()
            .map(|item| CompletionItem {
                label: item.label,
                kind: Some(match item.kind {
                    CompletionKind::Variable => CompletionItemKind::VARIABLE,
                    CompletionKind::Function => CompletionItemKind::FUNCTION,
                    CompletionKind::Type => CompletionItemKind::TYPE_PARAMETER,
                    CompletionKind::Property => CompletionItemKind::PROPERTY,
                    CompletionKind::Snippet => CompletionItemKind::SNIPPET,
                }),
                detail: Some(item.detail),
                documentation: item.documentation.map(markdown_documentation),
                tags: item
                    .deprecated
                    .then(|| vec![tower_lsp_server::ls_types::CompletionItemTag::DEPRECATED]),
                insert_text_format: item
                    .insert_text
                    .as_ref()
                    .map(|_| tower_lsp_server::ls_types::InsertTextFormat::SNIPPET),
                insert_text: item.insert_text,
                ..Default::default()
            })
            .collect();
        self.response(&snapshot, Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let position = params.text_document_position_params;
        let Some(document) = self.document(&position.text_document.uri).await else {
            return Ok(None);
        };
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        let Some(source) = snapshot.source(&document.path) else {
            return Ok(None);
        };
        let offset = LineIndex::new(source).position_to_offset(source, position.position);
        self.response(
            &snapshot,
            snapshot.hover(&document.path, offset).map(|result| Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: result.contents,
                }),
                range: Some(byte_range_to_lsp(source, result.range)),
            }),
        )
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<LspSignatureHelp>> {
        let position = params.text_document_position_params;
        let Some(document) = self.document(&position.text_document.uri).await else {
            return Ok(None);
        };
        let offset = document
            .index
            .position_to_offset(&document.text, position.position);
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        self.response(
            &snapshot,
            snapshot
                .signature_help(&document.path, offset)
                .map(|help| LspSignatureHelp {
                    signatures: help
                        .signatures
                        .into_iter()
                        .map(|signature| LspSignatureInformation {
                            label: signature.label,
                            parameters: Some(
                                signature
                                    .parameters
                                    .into_iter()
                                    .zip(signature.parameter_documentation)
                                    .map(|(label, documentation)| {
                                        tower_lsp_server::ls_types::ParameterInformation {
                                            label: ParameterLabel::Simple(label),
                                            documentation: documentation
                                                .map(markdown_documentation),
                                        }
                                    })
                                    .collect(),
                            ),
                            documentation: signature.documentation.map(markdown_documentation),
                            active_parameter: None,
                        })
                        .collect(),
                    active_signature: Some(help.active_signature as u32),
                    active_parameter: Some(help.active_parameter as u32),
                }),
        )
    }

    async fn goto_definition(
        &self,
        params: tower_lsp_server::ls_types::GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params;
        let Some(document) = self.document(&position.text_document.uri).await else {
            return Ok(None);
        };
        let offset = document
            .index
            .position_to_offset(&document.text, position.position);
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        let location = match snapshot.definition(&document.path, offset) {
            Some(range) => self.location(&range, &snapshot),
            None => None,
        };
        self.response(&snapshot, location.map(GotoDefinitionResponse::Scalar))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let position = params.text_document_position;
        let Some(document) = self.document(&position.text_document.uri).await else {
            return Ok(None);
        };
        let offset = document
            .index
            .position_to_offset(&document.text, position.position);
        let Some(snapshot) = self.snapshot_for_document(&document).await? else {
            return Ok(None);
        };
        let include = params.context.include_declaration;
        let mut locations = Vec::new();
        for range in snapshot.references(&document.path, offset, include) {
            if let Some(location) = self.location(&range, &snapshot) {
                locations.push(location);
            }
        }
        self.response(&snapshot, Some(locations))
    }

    async fn prepare_rename(
        &self,
        params: tower_lsp_server::ls_types::TextDocumentPositionParams,
    ) -> Result<Option<tower_lsp_server::ls_types::PrepareRenameResponse>> {
        let Some(document) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let offset = document
            .index
            .position_to_offset(&document.text, params.position);
        let Some(snapshot) = self.snapshot_for_document(&document).await? else {
            return Ok(None);
        };
        let Some(hover) = snapshot.hover(&document.path, offset) else {
            return Ok(None);
        };
        let name = document
            .text
            .get(hover.range.clone())
            .unwrap_or_default()
            .to_owned();
        if snapshot.rename(&document.path, offset, &name).is_err() {
            return Ok(None);
        }
        self.response(
            &snapshot,
            Some(
                tower_lsp_server::ls_types::PrepareRenameResponse::RangeWithPlaceholder {
                    range: byte_range_to_lsp(&document.text, hover.range),
                    placeholder: name,
                },
            ),
        )
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let position = params.text_document_position;
        let Some(document) = self.document(&position.text_document.uri).await else {
            return Ok(None);
        };
        let offset = document
            .index
            .position_to_offset(&document.text, position.position);
        let Some(snapshot) = self.snapshot_for_document(&document).await? else {
            return Ok(None);
        };
        let Ok(edit) = snapshot.rename(&document.path, offset, &params.new_name) else {
            return Ok(None);
        };
        let mut changes = std::collections::HashMap::new();
        for (path, edits) in edit.changes {
            let Some(uri) = Uri::from_file_path(&path) else {
                return Ok(None);
            };
            let Some(source) = snapshot.source(&path) else {
                return Ok(None);
            };
            changes.insert(
                uri,
                edits
                    .into_iter()
                    .map(|edit| to_lsp_text_edit(&source, edit))
                    .collect(),
            );
        }
        self.response(
            &snapshot,
            Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
        )
    }

    async fn did_change_configuration(&self, params: lsp::DidChangeConfigurationParams) {
        *self.settings.write().await = EditorSettings::from_value(&params.settings);
        self.schedule_analysis().await;
        if self.inlay_refresh.load(Ordering::Relaxed) {
            let _ = self.client.inlay_hint_refresh().await;
        }
    }

    async fn document_symbol(
        &self,
        params: lsp::DocumentSymbolParams,
    ) -> Result<Option<lsp::DocumentSymbolResponse>> {
        let Some(document) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        self.response(
            &snapshot,
            Some(lsp::DocumentSymbolResponse::Nested(
                snapshot
                    .document_symbols(&document.path)
                    .into_iter()
                    .map(|symbol| document_symbol(&document.text, symbol))
                    .collect(),
            )),
        )
    }

    async fn symbol(
        &self,
        params: lsp::WorkspaceSymbolParams,
    ) -> Result<Option<lsp::WorkspaceSymbolResponse>> {
        let Some(snapshot) = self.snapshot().await? else {
            return Ok(None);
        };
        let mut symbols = Vec::new();
        for (path, symbol) in snapshot.workspace_symbols(&params.query) {
            let Some(source) = snapshot.source(&path) else {
                continue;
            };
            let Some(uri) = Uri::from_file_path(&path) else {
                continue;
            };
            #[allow(deprecated)]
            symbols.push(lsp::SymbolInformation {
                name: symbol.name,
                kind: symbol_kind(symbol.kind),
                tags: None,
                deprecated: None,
                location: Location::new(uri, byte_range_to_lsp(source, symbol.selection)),
                container_name: None,
            });
        }
        self.response(&snapshot, Some(lsp::WorkspaceSymbolResponse::Flat(symbols)))
    }

    async fn inlay_hint(
        &self,
        params: lsp::InlayHintParams,
    ) -> Result<Option<Vec<lsp::InlayHint>>> {
        let Some(document) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let settings = self.settings.read().await.clone();
        if !settings.parameter_names && !settings.variable_types {
            return Ok(Some(Vec::new()));
        }
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        let index = &document.index;
        let start = index.position_to_offset(&document.text, params.range.start);
        let end = index.position_to_offset(&document.text, params.range.end);
        self.response(
            &snapshot,
            Some(
                snapshot
                    .inlay_hints(
                        &document.path,
                        start..end,
                        settings.parameter_names,
                        settings.variable_types,
                    )
                    .into_iter()
                    .map(|hint| lsp::InlayHint {
                        position: index.offset_to_position(&document.text, hint.offset),
                        label: lsp::InlayHintLabel::String(hint.label),
                        kind: Some(if hint.parameter {
                            lsp::InlayHintKind::PARAMETER
                        } else {
                            lsp::InlayHintKind::TYPE
                        }),
                        padding_right: hint.parameter.then_some(true),
                        padding_left: None,
                        text_edits: None,
                        tooltip: None,
                        data: None,
                    })
                    .collect(),
            ),
        )
    }

    async fn code_action(
        &self,
        params: lsp::CodeActionParams,
    ) -> Result<Option<lsp::CodeActionResponse>> {
        if params.context.only.as_ref().is_some_and(|kinds| {
            !kinds
                .iter()
                .any(|kind| kind.as_str() == "" || kind.as_str() == "quickfix")
        }) {
            return Ok(Some(Vec::new()));
        }
        let Some(document) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let Some(snapshot) = self.editor_snapshot(&document).await? else {
            return Ok(None);
        };
        let settings = self.settings.read().await.clone();
        let mut actions = Vec::new();
        for diagnostic in editor_diagnostics(&snapshot, &document, &settings) {
            if diagnostic
                .tags
                .as_ref()
                .is_none_or(|tags| !tags.contains(&lsp::DiagnosticTag::UNNECESSARY))
                || diagnostic.range.end < params.range.start
                || diagnostic.range.start > params.range.end
            {
                continue;
            }
            let offset = LineIndex::new(&document.text)
                .position_to_offset(&document.text, diagnostic.range.start);
            let Some(edit) = snapshot.unused_fix(&document.path, offset) else {
                continue;
            };
            let mut changes = std::collections::HashMap::new();
            for (path, edits) in edit.changes {
                let Some(uri) = Uri::from_file_path(&path) else {
                    continue;
                };
                let Some(source) = snapshot.source(&path) else {
                    continue;
                };
                changes.insert(
                    uri,
                    edits
                        .into_iter()
                        .map(|edit| to_lsp_text_edit(source, edit))
                        .collect(),
                );
            }
            actions.push(lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
                title: "Prefix unused name with '_'".to_owned(),
                kind: Some(lsp::CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
        self.response(&snapshot, Some(actions))
    }
}

#[derive(Clone)]
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn range(&self, source: &str, range: std::ops::Range<usize>) -> lsp::Range {
        lsp::Range::new(
            self.offset_to_position(source, range.start),
            self.offset_to_position(source, range.end),
        )
    }

    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        Self { starts }
    }

    fn position_to_offset(&self, source: &str, position: Position) -> usize {
        let line = (position.line as usize).min(self.starts.len().saturating_sub(1));
        let start = self.starts[line];
        let end = source[start..]
            .find('\n')
            .map_or(source.len(), |offset| start + offset);
        let end = if end > start && source.as_bytes()[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        let wanted = position.character as usize;
        let mut units = 0;
        for (offset, character) in source[start..end].char_indices() {
            if units + character.len_utf16() > wanted {
                return start + offset;
            }
            units += character.len_utf16();
            if units == wanted {
                return start + offset + character.len_utf8();
            }
        }
        end
    }

    fn offset_to_position(&self, source: &str, offset: usize) -> Position {
        let offset = offset.min(source.len());
        let line = self
            .starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = self.starts[line];
        Position::new(
            line as u32,
            source[start..offset].encode_utf16().count() as u32,
        )
    }
}

fn byte_range_to_lsp(
    source: &str,
    range: std::ops::Range<usize>,
) -> tower_lsp_server::ls_types::Range {
    let index = LineIndex::new(source);
    tower_lsp_server::ls_types::Range::new(
        index.offset_to_position(source, range.start),
        index.offset_to_position(source, range.end),
    )
}

fn to_lsp_text_edit(source: &str, edit: TextEdit) -> LspTextEdit {
    LspTextEdit {
        range: byte_range_to_lsp(source, edit.range),
        new_text: edit.new_text,
    }
}

#[tokio::main]
async fn main() {
    let (service, socket) = LspService::new(Backend::new);
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
}

#[cfg(test)]
mod tests {
    use super::LineIndex;
    use tower_lsp_server::ls_types::Position;

    #[test]
    fn utf16_positions_round_trip_with_emoji_turkish_tabs_and_crlf() {
        let source = "🚀\tçağrı\r\nnext";
        let index = LineIndex::new(source);
        let offset = source.find("çağrı").unwrap();
        let position = index.offset_to_position(source, offset);
        assert_eq!(position, Position::new(0, 3));
        assert_eq!(index.position_to_offset(source, position), offset);
        let next = source.find("next").unwrap();
        assert_eq!(
            index.position_to_offset(source, Position::new(1, 4)),
            next + 4
        );
    }
}

fn markdown_documentation(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

#[derive(Clone)]
struct EditorSettings {
    unused_parameters: bool,
    unused_locals: bool,
    parameter_names: bool,
    variable_types: bool,
}
impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            unused_parameters: true,
            unused_locals: true,
            parameter_names: false,
            variable_types: false,
        }
    }
}
impl EditorSettings {
    fn from_value(value: &lsp::LSPAny) -> Self {
        let value = value.get("luat").unwrap_or(value);
        Self {
            unused_parameters: value
                .pointer("/diagnostics/unusedParameters")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            unused_locals: value
                .pointer("/diagnostics/unusedLocals")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            parameter_names: value
                .pointer("/inlayHints/parameterNames/enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            variable_types: value
                .pointer("/inlayHints/variableTypes/enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }
}
fn editor_diagnostics(
    snapshot: &AnalysisSnapshot,
    document: &Document,
    settings: &EditorSettings,
) -> Vec<LspDiagnostic> {
    snapshot
        .editor_diagnostics(&document.path)
        .into_iter()
        .filter(|diagnostic| match diagnostic.code {
            "LTH1001" => settings.unused_parameters,
            "LTH1002" | "LTH1003" => settings.unused_locals,
            _ => true,
        })
        .map(|diagnostic| LspDiagnostic {
            range: byte_range_to_lsp(&document.text, diagnostic.range),
            severity: Some(DiagnosticSeverity::HINT),
            code: Some(diagnostic.code.into()),
            source: Some("LuaT".to_owned()),
            message: diagnostic.message,
            tags: Some(if diagnostic.deprecated {
                vec![lsp::DiagnosticTag::DEPRECATED]
            } else {
                vec![lsp::DiagnosticTag::UNNECESSARY]
            }),
            ..Default::default()
        })
        .collect()
}
fn symbol_kind(kind: luat::analysis::EditorSymbolKind) -> lsp::SymbolKind {
    match kind {
        luat::analysis::EditorSymbolKind::Variable => lsp::SymbolKind::VARIABLE,
        luat::analysis::EditorSymbolKind::Function => lsp::SymbolKind::FUNCTION,
        luat::analysis::EditorSymbolKind::Type => lsp::SymbolKind::STRUCT,
    }
}
fn document_symbol(source: &str, symbol: luat::analysis::EditorSymbol) -> lsp::DocumentSymbol {
    #[allow(deprecated)]
    lsp::DocumentSymbol {
        name: symbol.name,
        detail: None,
        kind: symbol_kind(symbol.kind),
        tags: None,
        deprecated: None,
        range: byte_range_to_lsp(source, symbol.range),
        selection_range: byte_range_to_lsp(source, symbol.selection),
        children: Some(
            symbol
                .children
                .into_iter()
                .map(|child| document_symbol(source, child))
                .collect(),
        ),
    }
}
