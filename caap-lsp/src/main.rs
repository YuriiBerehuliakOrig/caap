//! CAAP Language Server — surface-syntax aware.
//!
//! Wires `caap-core`'s real parser into a synchronous LSP loop. Bootstrap
//! TextMate grammar handles initial colorization in the client; this server
//! provides authoritative semantic tokens, diagnostics, document symbols,
//! hover, and go-to-definition based on the actual parsed surface forms.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use caap_core::lsp::BootstrapSession;
use caap_core::{parse_forms_with_source_path, ParsedForm};

use caap_lsp::{analyze, doc, index, semantic_tokens, structure, symbols};

use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument, Exit,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare, CodeLensRequest,
    Completion, DocumentHighlightRequest, DocumentSymbolRequest, FoldingRangeRequest, Formatting,
    GotoDefinition, HoverRequest, InlayHintRequest, PrepareRenameRequest, References,
    RegisterCapability, Rename, Request as _, SelectionRangeRequest, SemanticTokensFullRequest,
    SignatureHelpRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall,
    CallHierarchyServerCapability, CodeLens, CodeLensOptions, Command, CompletionItem,
    CompletionItemKind, CompletionOptions, CompletionResponse, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DocumentHighlight, DocumentSymbolResponse,
    FileChangeType, FileSystemWatcher, FoldingRange, FoldingRangeProviderCapability, GlobPattern,
    GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability, InitializeParams,
    InlayHint, Location, MarkupContent, MarkupKind, OneOf, Position, PrepareRenameResponse,
    PublishDiagnosticsParams, Range, Registration, RegistrationParams, RenameOptions,
    SelectionRange, SelectionRangeProviderCapability, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensServerCapabilities,
    ServerCapabilities, SignatureHelp, SignatureHelpOptions, SymbolKind,
    TextDocumentContentChangeEvent, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
    Uri, WorkspaceEdit, WorkspaceSymbol, WorkspaceSymbolResponse,
};
use std::str::FromStr;

use analyze::Analysis;
use caap_lsp::analyze::DefinitionKind;
use caap_lsp::index::{build_workspace_index, workspace_occurrences, IndexedSymbol};
use doc::Document;
use semantic_tokens::token_legend;

/// Where a `(module name)` or named `(surface name)` is declared.
struct ModuleLocation {
    path: PathBuf,
    /// 0-based line of the `(module ...)` declaration.
    line: u32,
}

/// A per-file call-graph cache entry, keyed by canonical path and invalidated
/// when the file's modification time changes.
struct CachedCallGraph {
    mtime: Option<std::time::SystemTime>,
    infos: Vec<structure::FunctionInfo>,
}

struct State {
    docs: HashMap<Uri, Document>,
    bootstrap: Option<Arc<BootstrapSession>>,
    /// module name → declaring file, for import go-to-definition.
    module_index: HashMap<String, ModuleLocation>,
    workspace_roots: Vec<PathBuf>,
    /// Lazily-built workspace-wide definition index for `workspace/symbol`.
    workspace_index: Option<Vec<IndexedSymbol>>,
    /// Per-file call graphs for cross-file call hierarchy: grammar-extended files
    /// are bootstrap-augmented once and cached until their mtime changes.
    call_graph_cache: HashMap<PathBuf, CachedCallGraph>,
}

impl State {
    fn new() -> Self {
        Self {
            docs: HashMap::new(),
            bootstrap: None,
            module_index: HashMap::new(),
            workspace_roots: Vec::new(),
            workspace_index: None,
            call_graph_cache: HashMap::new(),
        }
    }

    /// The call graph for one on-disk file, cached by mtime. Plain files use the
    /// base AST; grammar-extended files (base parse fails) are bootstrap-augmented
    /// so their bodies' functions and call sites become visible across files.
    fn cached_call_graph(
        &mut self,
        path: &Path,
        session: Option<&BootstrapSession>,
    ) -> Vec<structure::FunctionInfo> {
        let cpath = canonical(path);
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some(cached) = self.call_graph_cache.get(&cpath) {
            if cached.mtime == mtime {
                return cached.infos.clone();
            }
        }
        let infos = compute_call_graph(path, session);
        self.call_graph_cache.insert(
            cpath,
            CachedCallGraph {
                mtime,
                infos: infos.clone(),
            },
        );
        infos
    }

    /// Build (once) and return the workspace symbol index.
    fn workspace_symbols(&mut self) -> &[IndexedSymbol] {
        if self.workspace_index.is_none() {
            self.workspace_index = Some(build_workspace_index(&self.workspace_roots));
        }
        self.workspace_index.as_deref().unwrap_or_default()
    }
}

fn server_capabilities() -> ServerCapabilities {
    let SemanticTokensLegend {
        token_types,
        token_modifiers,
    } = token_legend();
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..Default::default()
        }),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), " ".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        inlay_hint_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types,
                    token_modifiers,
                },
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: None,
                ..Default::default()
            },
        )),
        ..Default::default()
    }
}

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    eprintln!("caap-lsp: starting");
    let (connection, io_threads) = Connection::stdio();
    let capabilities = serde_json::to_value(server_capabilities())?;
    let initialization_params = match connection.initialize(capabilities) {
        Ok(params) => params,
        Err(err) => {
            if err.channel_is_disconnected() {
                io_threads.join()?;
            }
            return Err(err.into());
        }
    };
    let params: InitializeParams = serde_json::from_value(initialization_params)?;

    let mut state = State::new();
    let roots = collect_workspace_roots(&params);
    state.bootstrap = build_bootstrap_session(&roots);
    state.module_index = build_module_index(&roots);
    state.workspace_roots = roots.clone();
    eprintln!(
        "caap_lsp: indexed {} module declaration(s)",
        state.module_index.len()
    );
    // Ask the client to watch `.caap` files so we can refresh the workspace and
    // module indexes when files are created/changed/deleted on disk (otherwise
    // they go stale for the rest of the session).
    register_file_watchers(&connection, &params);
    let result = main_loop(&connection, &mut state);
    // Drop the connection (and with it the writer-channel sender) *before*
    // joining the IO threads: the lsp-server writer/dropper threads only exit
    // once every `Sender<Message>` clone is gone. Joining while `connection`
    // is still alive would hang the process on every client-initiated
    // shutdown.
    drop(connection);
    io_threads.join()?;
    result?;
    eprintln!("caap-lsp: stopped");
    Ok(())
}

fn main_loop(conn: &Connection, state: &mut State) -> Result<(), Box<dyn Error + Sync + Send>> {
    let mut shutting_down = false;
    // A message pulled out of the channel while coalescing a change-burst, to be
    // processed on the next iteration instead of blocking on `recv`.
    let mut pending: Option<Message> = None;
    loop {
        let msg = match pending.take() {
            Some(msg) => msg,
            None => match conn.receiver.recv() {
                Ok(msg) => msg,
                Err(_) => break,
            },
        };
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req)? {
                    shutting_down = true;
                    continue;
                }
                handle_request(conn, state, req)?;
            }
            Message::Notification(notif) => {
                if notif.method == Exit::METHOD {
                    return Ok(());
                }
                // Coalesce a burst of `didChange` edits: re-running the bootstrap
                // frontend on every keystroke is expensive, so we collapse queued
                // changes to the latest text per document and analyze once.
                if notif.method == DidChangeTextDocument::METHOD {
                    pending = coalesce_changes(conn, state, notif)?;
                    continue;
                }
                handle_notification(conn, state, notif)?;
            }
            Message::Response(_) => {}
        }
        if shutting_down {
            // After shutdown, only `exit` is meaningful. Continue to drain the
            // queue but skip further work; the next `exit` will break the loop.
        }
    }
    Ok(())
}

/// Collapse a run of queued `didChange` notifications into the latest text per
/// document, analyzing each affected document once. Drains the channel
/// non-blockingly until a non-change message arrives (returned to the caller for
/// normal processing) or the queue empties.
// `Uri` map key is effectively immutable; the idiomatic LSP pattern.
#[allow(clippy::mutable_key_type)]
fn coalesce_changes(
    conn: &Connection,
    state: &mut State,
    first: Notification,
) -> Result<Option<Message>, Box<dyn Error + Sync + Send>> {
    let mut order: Vec<Uri> = Vec::new();
    let mut latest: HashMap<Uri, String> = HashMap::new();
    apply_notification_changes(state, first, &mut order, &mut latest);

    let leftover = loop {
        match conn.receiver.try_recv() {
            Ok(Message::Notification(n)) if n.method == DidChangeTextDocument::METHOD => {
                apply_notification_changes(state, n, &mut order, &mut latest);
            }
            Ok(other) => break Some(other),
            // Empty or disconnected: stop draining and analyze what we have.
            Err(_) => break None,
        }
    };

    for uri in order {
        if let Some(text) = latest.remove(&uri) {
            refresh_document(conn, state, uri, text)?;
        }
    }
    Ok(leftover)
}

/// Apply one `didChange`'s content changes onto the document's evolving text,
/// preserving first-seen document order across a burst. With INCREMENTAL sync
/// each change is a delta (a `range` to splice), so the working copy starts from
/// the previously-coalesced text for this URI, or the last analyzed text in
/// `state.docs`, and every queued change is applied in order before re-analysis.
// `Uri` is effectively immutable; the `HashMap<Uri, _>` key is the idiomatic LSP
// pattern and is not mutated through interior cells.
#[allow(clippy::mutable_key_type)]
fn apply_notification_changes(
    state: &State,
    notif: Notification,
    order: &mut Vec<Uri>,
    latest: &mut HashMap<Uri, String>,
) {
    let Ok(params) = cast_notification::<DidChangeTextDocument>(notif) else {
        return;
    };
    let uri = params.text_document.uri;
    let text = latest.entry(uri.clone()).or_insert_with(|| {
        order.push(uri.clone());
        state
            .docs
            .get(&uri)
            .map(|doc| doc.text.clone())
            .unwrap_or_default()
    });
    for change in params.content_changes {
        apply_content_change(text, change);
    }
}

/// Apply a single LSP content change to `text`: splice `change.text` into the
/// `change.range` (INCREMENTAL), or replace the whole document when no range is
/// given (a client that still sends full updates).
fn apply_content_change(text: &mut String, change: TextDocumentContentChangeEvent) {
    match change.range {
        Some(range) => {
            let start = position_to_byte(text, range.start);
            let end = position_to_byte(text, range.end).max(start);
            text.replace_range(start..end, &change.text);
        }
        None => *text = change.text,
    }
}

/// Convert an LSP `Position` (0-based line, UTF-16 code-unit column) to a byte
/// offset into `text`. Positions past the end of a line or the document clamp to
/// the nearest valid boundary so a malformed range can never panic the splice.
fn position_to_byte(text: &str, pos: Position) -> usize {
    // Advance to the first byte of the target line.
    let mut line = 0u32;
    let mut idx = 0usize;
    while line < pos.line {
        match text[idx..].find('\n') {
            Some(rel) => {
                idx += rel + 1;
                line += 1;
            }
            None => return text.len(),
        }
    }
    // Within the line, advance `pos.character` UTF-16 units (stopping at EOL).
    let mut utf16 = 0u32;
    for ch in text[idx..].chars() {
        if utf16 >= pos.character || ch == '\n' {
            break;
        }
        utf16 += ch.len_utf16() as u32;
        idx += ch.len_utf8();
    }
    idx
}

// `lsp_types::Uri` is effectively immutable; using it as a `HashMap` key for
// workspace edits is the idiomatic LSP pattern.
#[allow(clippy::mutable_key_type)]
/// Try each request type in turn: on the first match, run its handler body
/// (which has `conn`, `state`, and the bound `id`/`params` in scope) and return;
/// otherwise pass the still-encoded request to the next arm. Collapses the ~20
/// identical `match cast_request::<T>(req)` scaffolds down to their bodies.
macro_rules! dispatch_requests {
    ($req:expr, { $($ty:ty => ($id:ident, $params:ident) $body:block)* }) => {{
        let mut req = $req;
        $(
            req = match cast_request::<$ty>(req) {
                Ok(($id, $params)) => {
                    $body
                    return Ok(());
                }
                Err(req) => req,
            };
        )*
        let _ = req;
    }};
}

fn handle_request(
    conn: &Connection,
    state: &mut State,
    req: Request,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    dispatch_requests!(req, {
        SemanticTokensFullRequest => (id, params) {
            let uri = params.text_document.uri;
            let result = state.docs.get(&uri).and_then(|doc| {
                let analysis = doc.analysis.as_ref()?;
                Some(semantic_tokens::full_tokens(analysis, &doc.text))
            });
            respond(conn, id, result)?;
        }
        DocumentSymbolRequest => (id, params) {
            let uri = params.text_document.uri;
            let result: Option<DocumentSymbolResponse> = state
                .docs
                .get(&uri)
                .and_then(|doc| doc.analysis.as_ref())
                .map(|analysis| {
                    DocumentSymbolResponse::Nested(symbols::document_symbols(analysis))
                });
            respond(conn, id, result)?;
        }
        HoverRequest => (id, params) {
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let result: Option<Hover> = state
                .docs
                .get(&uri)
                .and_then(|doc| {
                    let analysis = doc.analysis.as_ref()?;
                    symbols::hover_at(analysis, &doc.text, pos)
                })
                .map(|markdown| Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: markdown,
                    }),
                    range: None,
                });
            respond(conn, id, result)?;
        }
        GotoDefinition => (id, params) {
            let uri = params
                .text_document_position_params
                .text_document
                .uri
                .clone();
            let pos = params.text_document_position_params.position;
            let result: Option<GotoDefinitionResponse> =
                resolve_definition(state, &uri, pos).map(GotoDefinitionResponse::Scalar);
            respond(conn, id, result)?;
        }
        DocumentHighlightRequest => (id, params) {
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let result: Option<Vec<DocumentHighlight>> = state.docs.get(&uri).and_then(|doc| {
                let analysis = doc.analysis.as_ref()?;
                symbols::document_highlights(analysis, &doc.text, pos)
            });
            respond(conn, id, result)?;
        }
        References => (id, params) {
            let uri = params.text_document_position.text_document.uri.clone();
            let pos = params.text_document_position.position;
            let result: Option<Vec<Location>> =
                workspace_symbol_occurrences(state, &uri, pos).map(|occ| {
                    occ.into_iter()
                        .map(|(uri, range)| Location { uri, range })
                        .collect()
                });
            respond(conn, id, result)?;
        }
        Formatting => (id, params) {
            let uri = params.text_document.uri;
            let result: Option<Vec<TextEdit>> = state
                .docs
                .get(&uri)
                .and_then(|doc| caap_lsp::format::format_document(&doc.text));
            respond(conn, id, result)?;
        }
        Completion => (id, params) {
            let uri = params.text_document_position.text_document.uri;
            // Local definitions + keywords first; then merge in workspace-wide
            // symbols (skipping names already offered locally).
            let mut items: Vec<CompletionItem> = state
                .docs
                .get(&uri)
                .and_then(|doc| doc.analysis.as_ref())
                .map(symbols::completions)
                .unwrap_or_default();
            let mut seen: std::collections::HashSet<String> =
                items.iter().map(|i| i.label.clone()).collect();
            for sym in state.workspace_symbols() {
                if seen.insert(sym.name.clone()) {
                    let file = sym
                        .path
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    items.push(CompletionItem {
                        label: sym.name.clone(),
                        kind: Some(completion_item_kind(sym.kind)),
                        detail: Some(file),
                        ..Default::default()
                    });
                }
            }
            let result = Some(CompletionResponse::Array(items));
            respond(conn, id, result)?;
        }
        PrepareRenameRequest => (id, params) {
            let uri = params.text_document.uri;
            let pos = params.position;
            let result: Option<PrepareRenameResponse> = state
                .docs
                .get(&uri)
                .and_then(|doc| {
                    let analysis = doc.analysis.as_ref()?;
                    symbols::prepare_rename(analysis, &doc.text, pos)
                })
                .map(PrepareRenameResponse::Range);
            respond(conn, id, result)?;
        }
        Rename => (id, params) {
            let uri = params.text_document_position.text_document.uri.clone();
            let pos = params.text_document_position.position;
            let new_name = params.new_name;
            // Workspace-wide rename: rewrite every occurrence across files.
            // (Text-based, so VS Code's rename preview is the safety net.)
            let result: Option<WorkspaceEdit> =
                workspace_symbol_occurrences(state, &uri, pos).map(|occ| {
                    // `Uri` reports interior mutability (an interned-string cache)
                    // but is used immutably as a key here, as `WorkspaceEdit`
                    // itself requires — so the lint is a false positive.
                    #[allow(clippy::mutable_key_type)]
                    let mut changes: std::collections::HashMap<Uri, Vec<TextEdit>> =
                        std::collections::HashMap::new();
                    for (uri, range) in occ {
                        changes.entry(uri).or_default().push(TextEdit {
                            range,
                            new_text: new_name.clone(),
                        });
                    }
                    WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }
                });
            respond(conn, id, result)?;
        }
        SignatureHelpRequest => (id, params) {
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let result: Option<SignatureHelp> = state
                .docs
                .get(&uri)
                .and_then(|doc| doc.analysis.as_ref())
                .and_then(|analysis| symbols::signature_help(analysis, pos));
            respond(conn, id, result)?;
        }
        WorkspaceSymbolRequest => (id, params) {
            let query = params.query.to_lowercase();
            let symbols = workspace_symbols(state, &query);
            respond(conn, id, Some(WorkspaceSymbolResponse::Nested(symbols)))?;
        }
        InlayHintRequest => (id, params) {
            let uri = params.text_document.uri;
            let range = params.range;
            let result: Option<Vec<InlayHint>> = state
                .docs
                .get(&uri)
                .and_then(|doc| doc.analysis.as_ref())
                .map(|analysis| symbols::inlay_hints(analysis, range));
            respond(conn, id, result)?;
        }
        CodeLensRequest => (id, params) {
            let uri = params.text_document.uri;
            let result: Option<Vec<CodeLens>> = code_lenses(state, &uri);
            respond(conn, id, result)?;
        }
        FoldingRangeRequest => (id, params) {
            let uri = params.text_document.uri;
            let result: Option<Vec<FoldingRange>> = state.docs.get(&uri).and_then(|doc| {
                let analysis = doc.analysis.as_ref()?;
                Some(structure::folding_ranges(analysis, &doc.text))
            });
            respond(conn, id, result)?;
        }
        SelectionRangeRequest => (id, params) {
            let uri = params.text_document.uri;
            let positions = params.positions;
            let result: Option<Vec<SelectionRange>> = state.docs.get(&uri).and_then(|doc| {
                let analysis = doc.analysis.as_ref()?;
                Some(structure::selection_ranges(analysis, &doc.text, &positions))
            });
            respond(conn, id, result)?;
        }
        CallHierarchyPrepare => (id, params) {
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let result: Option<Vec<CallHierarchyItem>> =
                prepare_call_hierarchy(state, &uri, pos).map(|item| vec![item]);
            respond(conn, id, result)?;
        }
        CallHierarchyIncomingCalls => (id, params) {
            let result = Some(incoming_calls(state, &params.item));
            respond(conn, id, result)?;
        }
        CallHierarchyOutgoingCalls => (id, params) {
            let result = Some(outgoing_calls(state, &params.item));
            respond(conn, id, result)?;
        }
    });
    let _ = req;
    Ok(())
}

/// A reference-count CodeLens over every definition in the document: the
/// workspace-wide occurrence count of the name, minus the definition itself.
/// The command title is the only payload (clicking is a no-op), so it reads as
/// an inline "N references" annotation.
fn code_lenses(state: &State, uri: &Uri) -> Option<Vec<CodeLens>> {
    let doc = state.docs.get(uri)?;
    let analysis = doc.analysis.as_ref()?;
    let current_path = uri_to_path(uri);
    let mut out = Vec::new();
    for def in &analysis.definitions {
        // Current-doc matches use the fresh in-memory text; other files come
        // from disk (the current file is skipped to avoid double counting).
        let in_doc = symbols::occurrences(&doc.text, &def.name).len();
        let in_workspace =
            workspace_occurrences(&state.workspace_roots, &def.name, current_path.as_deref()).len();
        let refs = (in_doc + in_workspace).saturating_sub(1);
        out.push(CodeLens {
            range: analyze::span_to_range(&def.name_span),
            command: Some(Command {
                title: format!("{refs} reference{}", if refs == 1 { "" } else { "s" }),
                command: String::new(),
                arguments: None,
            }),
            data: None,
        });
    }
    Some(out)
}

/// Build a call-hierarchy item for a workspace function (range = whole defining
/// form, selection = the name token).
fn function_item(path: &Path, info: &structure::FunctionInfo) -> Option<CallHierarchyItem> {
    Some(CallHierarchyItem {
        name: info.name.clone(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: path.file_name().map(|f| f.to_string_lossy().into_owned()),
        uri: path_to_uri(path)?,
        range: analyze::span_to_range(&info.form_span),
        selection_range: analyze::span_to_range(&info.name_span),
        data: None,
    })
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Per-file callables across the workspace for call hierarchy. Open documents
/// contribute their (bootstrap-augmented, fresh) analyses; every other on-disk
/// `.caap` contributes its cached call graph (grammar-extended files augmented
/// once and cached by mtime). This makes call hierarchy precise across files,
/// including grammar-extended ones that aren't currently open.
fn workspace_function_infos(state: &mut State) -> Vec<(PathBuf, structure::FunctionInfo)> {
    // Open docs first: authoritative and fresh (reflect unsaved edits).
    let mut open_paths: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<(PathBuf, structure::FunctionInfo)> = Vec::new();
    for (uri, doc) in &state.docs {
        if let (Some(analysis), Some(path)) = (doc.analysis.as_ref(), uri_to_path(uri)) {
            open_paths.insert(canonical(&path));
            for info in structure::analysis_function_infos(analysis) {
                out.push((path.clone(), info));
            }
        }
    }
    // On-disk files (cached). Take the session Arc out first so the cache can be
    // mutated without holding a borrow on `state`.
    let session = state.bootstrap.clone();
    for path in index::workspace_caap_files(&state.workspace_roots) {
        if open_paths.contains(&canonical(&path)) {
            continue;
        }
        for info in state.cached_call_graph(&path, session.as_deref()) {
            out.push((path.clone(), info));
        }
    }
    out
}

/// Compute one file's call graph: plain s-expr files from the base AST;
/// grammar-extended files (base parse fails) via bootstrap augmentation so their
/// surface-DSL bodies surface functions and call sites.
fn compute_call_graph(
    path: &Path,
    session: Option<&BootstrapSession>,
) -> Vec<structure::FunctionInfo> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Some(path_str) = path.to_str() else {
        return Vec::new();
    };
    match Analysis::from_source(path_str, &text) {
        Ok(analysis) => structure::analysis_function_infos(&analysis),
        Err(_) => {
            let mut analysis = Analysis::from_leading_forms(path_str, &text);
            if let Some(session) = session {
                analysis.augment_from_bootstrap(session, path);
            }
            structure::analysis_function_infos(&analysis)
        }
    }
}

/// Resolve the function under the cursor (a definition or a call site) to a
/// call-hierarchy item — current document first (precise, bootstrap-augmented),
/// then the workspace.
fn prepare_call_hierarchy(
    state: &mut State,
    uri: &Uri,
    pos: Position,
) -> Option<CallHierarchyItem> {
    let doc = state.docs.get(uri)?;
    let analysis = doc.analysis.as_ref()?;
    let word = symbols::symbol_at_cursor(analysis, &doc.text, pos)?;

    if let Some(path) = uri_to_path(uri) {
        if let Some(info) = structure::analysis_function_infos(analysis)
            .into_iter()
            .find(|f| f.name == word)
        {
            if let Some(item) = function_item(&path, &info) {
                return Some(item);
            }
        }
    }
    workspace_function_infos(state)
        .into_iter()
        .find(|(_, f)| f.name == word)
        .and_then(|(path, info)| function_item(&path, &info))
}

/// Callers of `item`: every workspace function whose body calls it.
fn incoming_calls(state: &mut State, item: &CallHierarchyItem) -> Vec<CallHierarchyIncomingCall> {
    let mut out = Vec::new();
    for (path, info) in workspace_function_infos(state) {
        let from_ranges: Vec<Range> = info
            .calls
            .iter()
            .filter(|c| c.callee == item.name)
            .map(|c| analyze::span_to_range(&c.span))
            .collect();
        if from_ranges.is_empty() {
            continue;
        }
        if let Some(from) = function_item(&path, &info) {
            out.push(CallHierarchyIncomingCall { from, from_ranges });
        }
    }
    out
}

/// Callees of `item`: every workspace function its body calls (control forms and
/// unresolved names are dropped).
fn outgoing_calls(state: &mut State, item: &CallHierarchyItem) -> Vec<CallHierarchyOutgoingCall> {
    let funcs = workspace_function_infos(state);
    let target_path = uri_to_path(&item.uri).map(|p| canonical(&p));
    // Prefer the function in the item's own file, else the first by name.
    // Paths in `funcs` are canonical for open docs; canonicalize both sides.
    let source = funcs
        .iter()
        .find(|(p, f)| f.name == item.name && Some(canonical(p)) == target_path)
        .or_else(|| funcs.iter().find(|(_, f)| f.name == item.name));
    let Some((_, src)) = source else {
        return Vec::new();
    };

    // Group call sites by callee, preserving source order.
    let mut order: Vec<String> = Vec::new();
    let mut by_callee: HashMap<String, Vec<Range>> = HashMap::new();
    for call in &src.calls {
        let ranges = by_callee.entry(call.callee.clone()).or_default();
        if ranges.is_empty() {
            order.push(call.callee.clone());
        }
        ranges.push(analyze::span_to_range(&call.span));
    }

    let mut out = Vec::new();
    for callee in order {
        if let Some((path, info)) = funcs.iter().find(|(_, f)| f.name == callee) {
            if let Some(to) = function_item(path, info) {
                let from_ranges = by_callee.remove(&callee).unwrap_or_default();
                out.push(CallHierarchyOutgoingCall { to, from_ranges });
            }
        }
    }
    out
}

/// Map the workspace definition index to DAP `WorkspaceSymbol`s, filtered by a
/// case-insensitive substring query (empty query returns a capped set).
fn workspace_symbols(state: &mut State, query: &str) -> Vec<WorkspaceSymbol> {
    const LIMIT: usize = 512;
    let mut out = Vec::new();
    for sym in state.workspace_symbols() {
        if !query.is_empty() && !sym.name.to_lowercase().contains(query) {
            continue;
        }
        let Some(uri) = path_to_uri(&sym.path) else {
            continue;
        };
        out.push(WorkspaceSymbol {
            name: sym.name.clone(),
            kind: def_symbol_kind(sym.kind),
            tags: None,
            container_name: None,
            location: OneOf::Left(Location {
                uri,
                range: analyze::span_to_range(&sym.name_span),
            }),
            data: None,
        });
        if out.len() >= LIMIT {
            break;
        }
    }
    out
}

/// All occurrences of the symbol under the cursor across the workspace: the
/// current document (fresh in-memory text) plus every other `.caap` file on
/// disk. Backs workspace-wide references and rename.
fn workspace_symbol_occurrences(
    state: &State,
    uri: &Uri,
    pos: Position,
) -> Option<Vec<(Uri, Range)>> {
    let doc = state.docs.get(uri)?;
    let analysis = doc.analysis.as_ref()?;
    let (word, scope) = symbols::occurrence_scope(analysis, &doc.text, pos)?;

    // A local / parameter is renamed and listed only within its own definition
    // in this file — never across the workspace, where a same-named binding is
    // unrelated.
    if let symbols::OccurrenceScope::Local { form_span } = scope {
        return Some(
            symbols::occurrences_in_span(&doc.text, &word, &form_span)
                .into_iter()
                .map(|range| (uri.clone(), range))
                .collect(),
        );
    }

    let mut out: Vec<(Uri, Range)> = symbols::occurrences(&doc.text, &word)
        .into_iter()
        .map(|range| (uri.clone(), range))
        .collect();

    let current_path = uri_to_path(uri);
    for (path, range) in
        workspace_occurrences(&state.workspace_roots, &word, current_path.as_deref())
    {
        if let Some(file_uri) = path_to_uri(&path) {
            out.push((file_uri, range));
        }
    }
    Some(out)
}

fn def_symbol_kind(kind: DefinitionKind) -> SymbolKind {
    match kind {
        DefinitionKind::Function => SymbolKind::FUNCTION,
        DefinitionKind::Macro => SymbolKind::FUNCTION,
        DefinitionKind::Class => SymbolKind::CLASS,
        DefinitionKind::Interface => SymbolKind::INTERFACE,
        DefinitionKind::Module => SymbolKind::MODULE,
        DefinitionKind::Variable => SymbolKind::VARIABLE,
    }
}

fn completion_item_kind(kind: DefinitionKind) -> CompletionItemKind {
    match kind {
        DefinitionKind::Function => CompletionItemKind::FUNCTION,
        DefinitionKind::Macro => CompletionItemKind::FUNCTION,
        DefinitionKind::Class => CompletionItemKind::CLASS,
        DefinitionKind::Interface => CompletionItemKind::INTERFACE,
        DefinitionKind::Module => CompletionItemKind::MODULE,
        DefinitionKind::Variable => CompletionItemKind::VARIABLE,
    }
}

fn handle_notification(
    conn: &Connection,
    state: &mut State,
    notif: Notification,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let notif = match cast_notification::<DidOpenTextDocument>(notif) {
        Ok(params) => {
            let uri = params.text_document.uri;
            let text = params.text_document.text;
            // A newly opened file may not be in the (init-time) workspace index;
            // drop it so the next workspace-symbol/completion rebuild sees it.
            state.workspace_index = None;
            refresh_document(conn, state, uri, text)?;
            return Ok(());
        }
        Err(notif) => notif,
    };
    let notif = match cast_notification::<DidChangeWatchedFiles>(notif) {
        Ok(params) => {
            handle_watched_files(state, params);
            return Ok(());
        }
        Err(notif) => notif,
    };
    let notif = match cast_notification::<DidChangeTextDocument>(notif) {
        Ok(params) => {
            // The main loop routes `didChange` through `coalesce_changes`, so this
            // arm is a defensive fallback. Apply deltas onto the stored text the
            // same way (INCREMENTAL sync), starting from the last analyzed text.
            let uri = params.text_document.uri;
            let mut text = state
                .docs
                .get(&uri)
                .map(|doc| doc.text.clone())
                .unwrap_or_default();
            for change in params.content_changes {
                apply_content_change(&mut text, change);
            }
            refresh_document(conn, state, uri, text)?;
            return Ok(());
        }
        Err(notif) => notif,
    };
    let notif = match cast_notification::<DidCloseTextDocument>(notif) {
        Ok(params) => {
            state.docs.remove(&params.text_document.uri);
            // The on-disk version is now authoritative again; rebuild the index
            // lazily so it reflects the saved file rather than stale in-memory.
            state.workspace_index = None;
            return Ok(());
        }
        Err(notif) => notif,
    };
    let _ = notif;
    Ok(())
}

/// React to on-disk `.caap` changes: invalidate the lazily-built workspace
/// definition index, rebuild the module index (so new `(module …)` declarations
/// resolve), and purge any deleted files from the call-graph cache.
fn handle_watched_files(state: &mut State, params: DidChangeWatchedFilesParams) {
    if params.changes.is_empty() {
        return;
    }
    state.workspace_index = None;
    state.module_index = build_module_index(&state.workspace_roots);
    for change in &params.changes {
        if change.typ == FileChangeType::DELETED {
            if let Some(path) = uri_to_path(&change.uri) {
                state.call_graph_cache.remove(&canonical(&path));
            }
        }
    }
    eprintln!(
        "caap-lsp: refreshed indexes after {} watched-file change(s)",
        params.changes.len()
    );
}

fn refresh_document(
    conn: &Connection,
    state: &mut State,
    uri: Uri,
    text: String,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let base = Analysis::from_source(uri.as_str(), &text);
    // Hold the base-parse error aside: the plain s-expr parser cannot read
    // grammar-extended surface syntax, so its error is only meaningful when
    // the authoritative bootstrap frontend *also* fails to analyze the file.
    let base_err = base.as_ref().err().cloned();
    // Start from the base parse when it succeeded; otherwise recover the
    // leading s-expr header (module / import forms) so the bootstrap path has
    // somewhere to write body definitions and the header stays navigable.
    let mut analysis = base.unwrap_or_else(|_| Analysis::from_leading_forms(uri.as_str(), &text));

    let mut bootstrap_ok = false;
    if let Some(session) = state.bootstrap.as_ref() {
        if let Some(path) = uri_to_path(&uri) {
            if path.exists() {
                eprintln!("caap-lsp: invoking analyze-source on {}", path.display());
                // Appends real semantic diagnostics (with spans) to
                // analysis.diagnostics; returns whether the file was analyzed.
                if analysis.augment_from_bootstrap(session, &path) {
                    bootstrap_ok = true;
                    // Tighten bootstrap-reported spans to the exact name token
                    // using the in-memory source (precise outline +
                    // go-to-definition for grammar-extended files).
                    analysis.refine_definition_spans(&text);
                }
            }
        }
    }

    // Collect diagnostics AFTER augmentation so the compiler's semantic
    // diagnostics are included.
    let mut diagnostics = analysis.diagnostics.clone();

    // Surface the raw base-parse error only when the bootstrap frontend did
    // not produce an analysis — otherwise it's a false positive on a valid
    // grammar-extended file the s-expr parser simply can't read.
    if let Some(err) = base_err {
        if !bootstrap_ok {
            diagnostics.push(*err);
        }
    }

    let doc = Document {
        text,
        analysis: Some(analysis),
    };
    state.docs.insert(uri.clone(), doc);
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    conn.sender.send(Message::Notification(Notification {
        method: PublishDiagnostics::METHOD.to_string(),
        params: serde_json::to_value(params)?,
    }))?;
    Ok(())
}

fn respond<T: serde::Serialize>(
    conn: &Connection,
    id: RequestId,
    result: T,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let resp = Response {
        id,
        result: Some(serde_json::to_value(result)?),
        error: None,
    };
    conn.sender.send(Message::Response(resp))?;
    Ok(())
}

/// Resolve go-to-definition: first a local definition under the cursor, then
/// an import reference (`(import-symbols ...)`, `(syntax-import ...)`, …)
/// resolved to its declaring module file.
fn resolve_definition(state: &mut State, uri: &Uri, pos: Position) -> Option<Location> {
    let doc = state.docs.get(uri)?;
    let analysis = doc.analysis.as_ref()?;
    if let Some(loc) = symbols::definition_at(analysis, &doc.text, uri, pos) {
        return Some(loc);
    }
    // Import form under the cursor → its declaring module file.
    if let Some(target) = symbols::import_target_at(analysis, pos) {
        if let Some(module) = state.module_index.get(&target.module) {
            if let Some(module_uri) = path_to_uri(&module.path) {
                // With a specific imported symbol under the cursor, try to land
                // on its definition inside the target file; else the module decl.
                if let Some(symbol) = &target.symbol {
                    if let Some(range) = find_symbol_range_in_file(&module.path, symbol) {
                        return Some(Location {
                            uri: module_uri,
                            range,
                        });
                    }
                }
                let start = Position::new(module.line, 0);
                return Some(Location {
                    uri: module_uri,
                    range: Range { start, end: start },
                });
            }
        }
    }
    // Fallback: a bare reference (e.g. a call) to a symbol defined in another
    // workspace file. Resolve via the workspace definition index, preferring a
    // non-variable definition (function/class/…) on name collisions.
    let word = symbols::symbol_at_cursor(analysis, &doc.text, pos)?;
    let symbols = state.workspace_symbols();
    let chosen = symbols
        .iter()
        .find(|s| s.name == word && s.kind != DefinitionKind::Variable)
        .or_else(|| symbols.iter().find(|s| s.name == word))?;
    Some(Location {
        uri: path_to_uri(&chosen.path)?,
        range: analyze::span_to_range(&chosen.name_span),
    })
}

/// Best-effort lookup of a top-level definition's name range inside a plain
/// s-expr file. Returns `None` if the file cannot be parsed (e.g. it is itself
/// grammar-extended) or the symbol is not a top-level definition there.
fn find_symbol_range_in_file(path: &std::path::Path, symbol: &str) -> Option<Range> {
    let text = std::fs::read_to_string(path).ok()?;
    let analysis = Analysis::from_source(path.to_str()?, &text).ok()?;
    let def = analysis.definition_for(symbol)?;
    Some(analyze::span_to_range(&def.name_span))
}

/// Build a `file:` URI from an absolute filesystem path.
fn path_to_uri(path: &std::path::Path) -> Option<Uri> {
    let path = path.to_str()?;
    Uri::from_str(&format!("file://{path}")).ok()
}

/// Convert an LSP `Uri` to a filesystem path. Supports only the `file:` scheme;
/// other schemes (untitled, inmemory) return `None`.
fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let rest = s.strip_prefix("file://")?;
    // `file:///path` → strip leading `/` only if followed by another path
    // segment; on POSIX the path is `/abs/path`. On Windows it would be
    // `/C:/...`; we don't decode percent-escapes (good enough for Phase 1).
    Some(PathBuf::from(rest))
}

/// Look for a workspace-level stdlib bootstrap and build a long-lived
/// `BootstrapSession`. Looks for `<root>/stdlib/bootstrap.caap` relative to the
/// workspace root URI. Returns `None` if no stdlib was found — the LSP then
/// falls back to base-parse-only analysis.
// The LSP runs single-threaded; `BootstrapSession` holds `Rc`-based runtime
// values so it is intentionally `!Send`/`!Sync`. The `Arc` only provides shared
// ownership within this one thread, never cross-thread sharing.
#[allow(clippy::arc_with_non_send_sync)]
fn build_bootstrap_session(roots: &[PathBuf]) -> Option<Arc<BootstrapSession>> {
    // stdlib's `caap.session.commands` capability map serves the LSP analyze
    // command; its `bootstrap.caap` is the session this LSP boots.
    for root in roots {
        let candidate = root.join("stdlib").join("bootstrap.caap");
        if !candidate.exists() {
            continue;
        }
        match BootstrapSession::new(vec![candidate], vec![root.clone()]) {
            Ok(session) => {
                eprintln!(
                    "caap-lsp: bootstrap session configured from stdlib for workspace root {}",
                    root.display()
                );
                return Some(Arc::new(session));
            }
            Err(error) => {
                eprintln!(
                    "caap-lsp: failed to construct stdlib bootstrap session at {}: {}",
                    root.display(),
                    error.message()
                );
            }
        }
    }
    None
}

/// Register a `.caap` file watcher with the client (when it supports dynamic
/// registration), so on-disk creates/changes/deletes invalidate our indexes.
/// Best-effort: a failure to send is logged and ignored — the `didOpen`/`didClose`
/// fallback still keeps open files fresh.
fn register_file_watchers(conn: &Connection, params: &InitializeParams) {
    let supported = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|w| w.did_change_watched_files.as_ref())
        .and_then(|d| d.dynamic_registration)
        .unwrap_or(false);
    if !supported {
        eprintln!("caap-lsp: client lacks dynamic file-watch registration; index refresh limited to open files");
        return;
    }
    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![FileSystemWatcher {
            glob_pattern: GlobPattern::String("**/*.caap".to_string()),
            kind: None, // create | change | delete
        }],
    };
    let registration = Registration {
        id: "caap-watch-files".to_string(),
        method: DidChangeWatchedFiles::METHOD.to_string(),
        register_options: serde_json::to_value(options).ok(),
    };
    let params = RegistrationParams {
        registrations: vec![registration],
    };
    match serde_json::to_value(params) {
        Ok(value) => {
            let req = Request {
                id: RequestId::from("caap-register-watchers".to_string()),
                method: RegisterCapability::METHOD.to_string(),
                params: value,
            };
            if let Err(err) = conn.sender.send(Message::Request(req)) {
                eprintln!("caap-lsp: failed to register file watchers: {err}");
            }
        }
        Err(err) => eprintln!("caap-lsp: failed to encode watcher registration: {err}"),
    }
}

fn collect_workspace_roots(params: &InitializeParams) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(folders) = &params.workspace_folders {
        for folder in folders {
            if let Some(path) = uri_to_path(&folder.uri) {
                roots.push(path);
            }
        }
    }
    #[allow(deprecated)]
    if let Some(root_uri) = &params.root_uri {
        if let Some(path) = uri_to_path(root_uri) {
            if !roots.contains(&path) {
                roots.push(path);
            }
        }
    }
    roots
}

/// Scan the workspace roots for `.caap` files and record each `(module
/// "name")` declaration, so import forms can be resolved to their declaring
/// file. First declaration wins on collisions.
fn build_module_index(roots: &[PathBuf]) -> HashMap<String, ModuleLocation> {
    let mut index = HashMap::new();
    for root in roots {
        collect_module_declarations(root, &mut index, 0);
    }
    index
}

fn collect_module_declarations(
    dir: &std::path::Path,
    index: &mut HashMap<String, ModuleLocation>,
    depth: usize,
) {
    // Guard against pathological trees; the stdlib is only a few levels deep.
    if depth > 32 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "node_modules" | ".git" | ".caap_build"
            ) {
                continue;
            }
            collect_module_declarations(&path, index, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("caap") {
            if let Some((module, line)) = read_module_declaration(&path) {
                index.entry(module).or_insert(ModuleLocation {
                    path: path.clone(),
                    line,
                });
            }
        }
    }
}

/// Find the first module identity declaration in a file, returning the module
/// name and its 0-based line.
fn read_module_declaration(path: &std::path::Path) -> Option<(String, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let prefix_len = leading_parenthesized_forms_prefix_len(&text);
    if prefix_len == 0 {
        return None;
    }
    let uri = path.to_string_lossy();
    let parsed = parse_forms_with_source_path(&text[..prefix_len], &uri).ok()?;
    parsed.forms.iter().find_map(module_declaration_from_form)
}

fn module_declaration_from_form(form: &ParsedForm) -> Option<(String, u32)> {
    let ParsedForm::List { items, span } = form else {
        return None;
    };
    match items.as_slice() {
        [ParsedForm::Symbol { text: head, .. }, name, ..] if head == "module" => {
            module_name_arg(name).map(|name| (name, span.start_line.saturating_sub(1) as u32))
        }
        [ParsedForm::Symbol { text: head, .. }, _, name, ..] if head == "surface" => {
            module_name_arg(name).map(|name| (name, span.start_line.saturating_sub(1) as u32))
        }
        _ => None,
    }
}

fn module_name_arg(form: &ParsedForm) -> Option<String> {
    match form {
        ParsedForm::Symbol { text, .. } | ParsedForm::String { value: text, .. } => {
            Some(text.clone())
        }
        _ => None,
    }
}

/// Byte length of the leading run of top-level parenthesized forms in `text`.
/// This lets workspace indexing read `(module ...)` and `(surface ...)` headers
/// from files whose grammar-extended bodies cannot be parsed by the base reader.
fn leading_parenthesized_forms_prefix_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut last_end = 0;
    loop {
        i = skip_header_trivia(bytes, i);
        if i >= n || bytes[i] != b'(' {
            break;
        }
        match matching_header_paren_end(bytes, i) {
            Some(end) => {
                last_end = end + 1;
                i = end + 1;
            }
            None => break,
        }
    }
    last_end
}

fn skip_header_trivia(bytes: &[u8], mut i: usize) -> usize {
    let n = bytes.len();
    loop {
        while i < n && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i < n && bytes[i] == b';' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < n && bytes[i] == b'#' && bytes[i + 1] == b'|' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'|' && bytes[i + 1] == b'#') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        break;
    }
    i
}

fn matching_header_paren_end(bytes: &[u8], start: usize) -> Option<usize> {
    let n = bytes.len();
    let mut depth = 0i32;
    let mut i = start;
    let mut in_string = false;
    while i < n {
        let c = bytes[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b';' => {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn cast_request<R>(req: Request) -> Result<(RequestId, R::Params), Request>
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
{
    match req.extract::<R::Params>(R::METHOD) {
        Ok(value) => Ok(value),
        Err(ExtractError::MethodMismatch(req)) => Err(req),
        Err(ExtractError::JsonError { method, error }) => {
            eprintln!("caap-lsp: invalid params for {method}: {error}");
            Err(Request {
                id: RequestId::from(0),
                method,
                params: serde_json::Value::Null,
            })
        }
    }
}

fn cast_notification<N>(notif: Notification) -> Result<N::Params, Notification>
where
    N: lsp_types::notification::Notification,
    N::Params: serde::de::DeserializeOwned,
{
    match notif.extract::<N::Params>(N::METHOD) {
        Ok(value) => Ok(value),
        Err(ExtractError::MethodMismatch(notif)) => Err(notif),
        Err(ExtractError::JsonError { method, error }) => {
            eprintln!("caap-lsp: invalid params for {method}: {error}");
            Err(Notification {
                method,
                params: serde_json::Value::Null,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Range;

    fn change(range: Option<Range>, text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range,
            range_length: None,
            text: text.to_string(),
        }
    }

    fn rng(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: Position::new(sl, sc),
            end: Position::new(el, ec),
        }
    }

    #[test]
    fn incremental_change_splices_a_single_line() {
        let mut text = String::from("(foo 1)\n(bar 2)\n");
        // Replace `bar` (line 1, cols 1..4) with `baz`.
        apply_content_change(&mut text, change(Some(rng(1, 1, 1, 4)), "baz"));
        assert_eq!(text, "(foo 1)\n(baz 2)\n");
    }

    #[test]
    fn incremental_change_spanning_lines_and_insert() {
        let mut text = String::from("a\nb\nc\n");
        // Delete from line 0 col 1 through line 2 col 0 → "a" + "c\n".
        apply_content_change(&mut text, change(Some(rng(0, 1, 2, 0)), ""));
        assert_eq!(text, "ac\n");
    }

    #[test]
    fn rangeless_change_replaces_whole_document() {
        let mut text = String::from("old contents");
        apply_content_change(&mut text, change(None, "new"));
        assert_eq!(text, "new");
    }

    #[test]
    fn position_columns_count_utf16_code_units() {
        // `é` is one UTF-16 unit (2 UTF-8 bytes); the column after it is 1.
        let text = "é=1\n";
        // Insert `x` at column 1 (right after `é`).
        let mut t = text.to_string();
        apply_content_change(&mut t, change(Some(rng(0, 1, 0, 1)), "x"));
        assert_eq!(t, "éx=1\n");
    }

    #[test]
    fn out_of_range_position_clamps_without_panicking() {
        let mut text = String::from("short\n");
        // A range past the document end must clamp, not panic.
        apply_content_change(&mut text, change(Some(rng(99, 99, 99, 99)), "!"));
        assert_eq!(text, "short\n!");
    }

    #[test]
    fn module_declaration_accepts_name_directive() {
        let path =
            std::env::temp_dir().join(format!("caap-lsp-module-name-{}.caap", std::process::id()));
        std::fs::write(&path, "\n(module demo.app)\n(defn main () int 0)\n").unwrap();
        let result = read_module_declaration(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, Some(("demo.app".to_string(), 1)));
    }

    #[test]
    fn module_declaration_accepts_named_surface_header() {
        let path =
            std::env::temp_dir().join(format!("caap-lsp-surface-name-{}.caap", std::process::id()));
        std::fs::write(
            &path,
            "(surface stdlib.frontend.clike demo.rounds)\ntwice (n i32) i32 = { n * 2; }\n",
        )
        .unwrap();
        let result = read_module_declaration(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, Some(("demo.rounds".to_string(), 0)));
    }
}
