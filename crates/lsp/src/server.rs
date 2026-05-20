//! LSP server core: state, message dispatch, analysis pipeline.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest,
    FoldingRangeRequest, GotoDefinition, HoverRequest, InlayHintRequest, PrepareRenameRequest,
    References, RegisterCapability, Rename, Request as RequestTrait,
};
use lsp_types::{
    CodeActionOptions, CodeActionParams, CodeActionProviderCapability, CodeActionResponse,
    CompletionItem, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic as LspDiagnostic, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentHighlight, DocumentHighlightParams, DocumentSymbolParams,
    DocumentSymbolResponse, FileSystemWatcher, FoldingRange, FoldingRangeParams,
    FoldingRangeProviderCapability, GlobPattern, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverParams, HoverProviderCapability, InitializeParams, InlayHint, InlayHintParams,
    InlayHintServerCapabilities, Location, OneOf, PositionEncodingKind, PrepareRenameResponse,
    PublishDiagnosticsParams, ReferenceParams, Registration, RegistrationParams, RenameOptions,
    RenameParams, ServerCapabilities, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri, WorkDoneProgressOptions, WorkspaceEdit,
};
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::config_cache::{ConfigCache, default_config_path};
use rpm_spec_analyzer::error_format::format_error_chain;
use rpm_spec_analyzer::profile::Profile;
use rpm_spec_analyzer::{Diagnostic as AnalyzerDiagnostic, analyze_with_profile_at};

use crate::code_actions;
use crate::completion;
use crate::diagnostics::to_lsp;
use crate::document::Document;
use crate::encoding::PositionEncoding;
use crate::folding;
use crate::hover;
use crate::inlay;
use crate::outline;
use crate::rename as rename_impl;
use crate::xref;

/// Per-document analysis result the server caches so `codeAction`
/// requests don't have to re-run the analyzer.
#[derive(Debug, Default)]
struct AnalysisCache {
    analyzer: Vec<AnalyzerDiagnostic>,
    /// Map lint_id → first LSP diagnostic emitted. Used by `codeAction`
    /// to attribute fixes back to a specific marker.
    lsp_by_lint: HashMap<String, LspDiagnostic>,
    /// Profile that was active for the latest analysis pass. Inlay
    /// hints look it up to expand macros; storing it here avoids a
    /// second `.rpmspec.toml` walk per hint request.
    profile: Option<Arc<Profile>>,
}

/// Server runtime state.
pub struct Server {
    connection: Connection,
    encoding: PositionEncoding,
    documents: HashMap<Uri, Document>,
    analyses: HashMap<Uri, AnalysisCache>,
    /// `.rpmspec.toml` cache, shared across documents.
    config_cache: ConfigCache,
    /// Resolved profile per config base directory. Mirrors the
    /// CLI memoization so showrc parsing happens at most once per
    /// `(config, base_dir)`.
    profile_cache: HashMap<std::path::PathBuf, Arc<Profile>>,
}

impl Server {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            encoding: PositionEncoding::Utf16,
            documents: HashMap::new(),
            analyses: HashMap::new(),
            config_cache: ConfigCache::new(xdg_default_config()),
            profile_cache: HashMap::new(),
        }
    }

    /// Run the initialize handshake + main message loop. Returns when
    /// the client sends `shutdown` followed by `exit`.
    pub fn run(mut self) -> Result<()> {
        let server_caps = serde_json::to_value(Self::capabilities())?;
        let init_params = self
            .connection
            .initialize(server_caps)
            .context("LSP initialize handshake failed")?;
        let init_params: InitializeParams = serde_json::from_value(init_params)?;
        self.encoding = pick_encoding(&init_params);
        tracing::info!(?self.encoding, "client initialized");

        if let Err(e) = self.register_config_watcher() {
            // Watcher registration is best-effort: clients that
            // don't support dynamic registration just keep the
            // old behaviour (manual server restart on config edit).
            tracing::debug!(
                "config watcher registration skipped: {}",
                format_error_chain(e.as_ref())
            );
        }

        self.main_loop()?;
        Ok(())
    }

    /// Ask the client to forward us `workspace/didChangeWatchedFiles`
    /// notifications for every `.rpmspec.toml` it sees. The
    /// registration is one-shot; we don't track the response.
    fn register_config_watcher(&self) -> Result<()> {
        let opts = DidChangeWatchedFilesRegistrationOptions {
            watchers: vec![FileSystemWatcher {
                glob_pattern: GlobPattern::String("**/.rpmspec.toml".to_string()),
                kind: None,
            }],
        };
        let reg = Registration {
            id: "rpm-spec-lsp/watch-config".to_string(),
            method: DidChangeWatchedFiles::METHOD.to_string(),
            register_options: Some(serde_json::to_value(opts)?),
        };
        let params = RegistrationParams {
            registrations: vec![reg],
        };
        let req = Request {
            id: RequestId::from("rpm-spec-lsp/register-watchers".to_string()),
            method: RegisterCapability::METHOD.to_string(),
            params: serde_json::to_value(params)?,
        };
        self.connection.sender.send(Message::Request(req))?;
        Ok(())
    }

    fn capabilities() -> ServerCapabilities {
        ServerCapabilities {
            position_encoding: Some(PositionEncodingKind::UTF16),
            text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
            code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                code_action_kinds: Some(vec![lsp_types::CodeActionKind::QUICKFIX]),
                work_done_progress_options: WorkDoneProgressOptions::default(),
                resolve_provider: Some(false),
            })),
            document_symbol_provider: Some(OneOf::Left(true)),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            completion_provider: Some(CompletionOptions {
                // `%` opens a directive context; other characters
                // (letters in a tag name) fire via the editor's
                // built-in word-completion trigger.
                trigger_characters: Some(vec!["%".to_string()]),
                all_commit_characters: None,
                resolve_provider: Some(false),
                work_done_progress_options: WorkDoneProgressOptions::default(),
                completion_item: None,
            }),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            document_highlight_provider: Some(OneOf::Left(true)),
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                lsp_types::InlayHintOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                },
            ))),
            ..Default::default()
        }
    }

    fn main_loop(&mut self) -> Result<()> {
        // Clone the receiver up front so we can iterate it while
        // sending messages through `self.connection.sender`.
        let receiver = self.connection.receiver.clone();
        for msg in receiver.iter() {
            match msg {
                Message::Request(req) => {
                    if self.connection.handle_shutdown(&req)? {
                        // `handle_shutdown` replied for us and is now
                        // expecting an `exit` notification, after which
                        // the channel closes and this loop ends.
                        return Ok(());
                    }
                    self.dispatch_request(req)?;
                }
                Message::Notification(note) => {
                    self.dispatch_notification(note)?;
                }
                Message::Response(_) => {
                    // We don't issue server→client requests yet; any
                    // response we receive is stray and safely ignored.
                }
            }
        }
        Ok(())
    }

    fn dispatch_request(&mut self, req: Request) -> Result<()> {
        let id = req.id.clone();
        if let Some(params) = cast_request::<CodeActionRequest>(&req) {
            let resp = self.on_code_action(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<DocumentSymbolRequest>(&req) {
            let resp = self.on_document_symbol(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<HoverRequest>(&req) {
            let resp = self.on_hover(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<Completion>(&req) {
            let resp = self.on_completion(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<PrepareRenameRequest>(&req) {
            let resp = self.on_prepare_rename(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<Rename>(&req) {
            let resp = self.on_rename(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<GotoDefinition>(&req) {
            let resp = self.on_goto_definition(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<References>(&req) {
            let resp = self.on_references(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<DocumentHighlightRequest>(&req) {
            let resp = self.on_document_highlight(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<FoldingRangeRequest>(&req) {
            let resp = self.on_folding_range(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        if let Some(params) = cast_request::<InlayHintRequest>(&req) {
            let resp = self.on_inlay_hint(params);
            self.respond(id, resp)?;
            return Ok(());
        }
        // Unknown request: reply with MethodNotFound so the client
        // doesn't hang. lsp-server returns the matching error code.
        let resp = Response::new_err(
            id,
            lsp_server::ErrorCode::MethodNotFound as i32,
            format!("method not implemented: {}", req.method),
        );
        self.connection.sender.send(Message::Response(resp))?;
        Ok(())
    }

    fn dispatch_notification(&mut self, note: Notification) -> Result<()> {
        if let Some(params) = cast_notification::<DidOpenTextDocument>(&note) {
            self.on_did_open(params)?;
        } else if let Some(params) = cast_notification::<DidChangeTextDocument>(&note) {
            self.on_did_change(params)?;
        } else if let Some(params) = cast_notification::<DidCloseTextDocument>(&note) {
            self.on_did_close(params);
        } else if let Some(params) = cast_notification::<DidChangeWatchedFiles>(&note) {
            self.on_did_change_watched_files(params)?;
        } else {
            tracing::trace!(method = %note.method, "unhandled notification");
        }
        Ok(())
    }

    fn on_did_change_watched_files(&mut self, params: DidChangeWatchedFilesParams) -> Result<()> {
        tracing::info!(
            count = params.changes.len(),
            "rpmspec.toml changed; reloading config + re-analyzing"
        );
        // Tear down everything keyed by the old config: the cache
        // itself, profile resolutions, and per-document analysis
        // (severities and bridged parser lints may shift).
        self.config_cache = ConfigCache::new(xdg_default_config());
        self.profile_cache.clear();
        self.analyses.clear();
        // Re-run analysis for every open document so editors see
        // fresh diagnostics without needing a restart or a save.
        let uris: Vec<Uri> = self.documents.keys().cloned().collect();
        for uri in uris {
            self.analyze_and_publish(&uri)?;
        }
        Ok(())
    }

    // ---- notification handlers ------------------------------------

    fn on_did_open(&mut self, params: DidOpenTextDocumentParams) -> Result<()> {
        let td = params.text_document;
        let doc = Document::new(td.uri.clone(), td.text, td.version);
        self.documents.insert(td.uri.clone(), doc);
        self.analyze_and_publish(&td.uri)?;
        Ok(())
    }

    fn on_did_change(&mut self, params: DidChangeTextDocumentParams) -> Result<()> {
        // FULL sync: every change carries one entry with the whole text.
        let uri = params.text_document.uri.clone();
        let version = params.text_document.version;
        let Some(new_text) = params.content_changes.into_iter().next().map(|c| c.text) else {
            tracing::warn!(uri = uri.as_str(), "didChange with no content");
            return Ok(());
        };
        if let Some(doc) = self.documents.get_mut(&uri) {
            doc.replace(new_text, version);
        } else {
            // Client sent didChange before didOpen — synthesize.
            let doc = Document::new(uri.clone(), new_text, version);
            self.documents.insert(uri.clone(), doc);
        }
        self.analyze_and_publish(&uri)?;
        Ok(())
    }

    fn on_did_close(&mut self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        self.analyses.remove(&uri);
        // Spec courtesy: clear diagnostics for the closed file so they
        // don't linger in the client's problem list.
        let _ = self.publish_diagnostics(&uri, Vec::new(), None);
    }

    // ---- request handlers -----------------------------------------

    fn on_code_action(&self, params: CodeActionParams) -> CodeActionResponse {
        let uri = &params.text_document.uri;
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        let Some(cache) = self.analyses.get(uri) else {
            return Vec::new();
        };
        code_actions::collect(
            uri,
            &doc.text,
            &doc.line_index,
            self.encoding,
            &cache.analyzer,
            &cache.lsp_by_lint,
            params.range,
        )
    }

    fn on_document_symbol(&self, params: DocumentSymbolParams) -> Option<DocumentSymbolResponse> {
        let doc = self.documents.get(&params.text_document.uri)?;
        let outcome = doc.parsed();
        let symbols = outline::build(&outcome.spec, &doc.text, &doc.line_index, self.encoding);
        Some(DocumentSymbolResponse::Nested(symbols))
    }

    fn on_hover(&self, params: HoverParams) -> Option<Hover> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let profile = self.analyses.get(uri).and_then(|c| c.profile.as_deref());
        hover::lookup(&doc.text, pos, profile)
    }

    fn on_completion(&self, params: CompletionParams) -> Option<CompletionResponse> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri)?;
        let profile = self.analyses.get(uri).and_then(|c| c.profile.as_deref());
        let items: Vec<CompletionItem> = completion::complete(&doc.text, pos, profile);
        if items.is_empty() {
            None
        } else {
            Some(CompletionResponse::Array(items))
        }
    }

    fn on_prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Option<PrepareRenameResponse> {
        let doc = self.documents.get(&params.text_document.uri)?;
        let range =
            rename_impl::prepare(&doc.text, params.position, &doc.line_index, self.encoding)?;
        Some(PrepareRenameResponse::Range(range))
    }

    fn on_rename(&self, params: RenameParams) -> Option<WorkspaceEdit> {
        let uri = &params.text_document_position.text_document.uri;
        let doc = self.documents.get(uri)?;
        rename_impl::rename(
            uri,
            &doc.text,
            &doc.line_index,
            self.encoding,
            &params.text_document_position,
            &params.new_name,
        )
    }

    fn on_goto_definition(&self, params: GotoDefinitionParams) -> Option<GotoDefinitionResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let loc = xref::goto_definition(uri, &doc.text, &doc.line_index, self.encoding, pos)?;
        Some(GotoDefinitionResponse::Scalar(loc))
    }

    fn on_references(&self, params: ReferenceParams) -> Option<Vec<Location>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri)?;
        let locs = xref::references(
            uri,
            &doc.text,
            &doc.line_index,
            self.encoding,
            pos,
            params.context.include_declaration,
        );
        if locs.is_empty() { None } else { Some(locs) }
    }

    fn on_document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Option<Vec<DocumentHighlight>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let hl = xref::document_highlight(&doc.text, &doc.line_index, self.encoding, pos);
        if hl.is_empty() { None } else { Some(hl) }
    }

    fn on_folding_range(&self, params: FoldingRangeParams) -> Option<Vec<FoldingRange>> {
        let doc = self.documents.get(&params.text_document.uri)?;
        let ranges = folding::build(&doc.parsed().spec);
        if ranges.is_empty() {
            None
        } else {
            Some(ranges)
        }
    }

    fn on_inlay_hint(&self, params: InlayHintParams) -> Option<Vec<InlayHint>> {
        let doc = self.documents.get(&params.text_document.uri)?;
        // Inlay hints only fire after the first analysis pass (which
        // resolves the profile). Until then we have nothing to expand.
        let cache = self.analyses.get(&params.text_document.uri)?;
        let profile = cache.profile.as_ref()?;
        let span =
            inlay::range_to_byte_span(&doc.text, params.range, &doc.line_index, self.encoding);
        let hints = inlay::build(&doc.text, &doc.line_index, self.encoding, profile, span);
        if hints.is_empty() { None } else { Some(hints) }
    }

    // ---- analysis pipeline ----------------------------------------

    fn analyze_and_publish(&mut self, uri: &Uri) -> Result<()> {
        // Snapshot the borrow-sensitive bits of the document up front
        // so subsequent `&mut self` calls (config cache, profile
        // resolver) don't conflict with the immutable doc borrow.
        let (doc_path, doc_version) = match self.documents.get(uri) {
            Some(d) => (d.path.clone(), d.version),
            None => return Ok(()),
        };

        let path_for_discovery = doc_path
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let (config, base_dir) = match self
            .config_cache
            .load_for_with_base_dir(&path_for_discovery)
        {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(
                    uri = uri.as_str(),
                    "failed to load .rpmspec.toml; using defaults: {}",
                    format_error_chain(&e)
                );
                (Arc::new(Config::default()), path_for_discovery.clone())
            }
        };
        let profile = self.resolve_profile(&config, &base_dir);

        // Re-acquire the doc borrow now that all `&mut self` work is
        // done. `documents` is never mutated between the snapshot and
        // here, so the document we saw is still present.
        let doc = self
            .documents
            .get(uri)
            .expect("document removed mid-analysis");

        let (_outcome, diags) =
            analyze_with_profile_at(&doc.text, doc_path.as_deref(), &config, (*profile).clone());

        // Build LSP diagnostics + lookup table for code actions.
        let mut lsp_diags = Vec::with_capacity(diags.len());
        let mut lsp_by_lint = HashMap::new();
        for d in &diags {
            let lsp = to_lsp(d, uri, &doc.text, &doc.line_index, self.encoding);
            lsp_by_lint
                .entry(d.lint_id.to_string())
                .or_insert_with(|| lsp.clone());
            lsp_diags.push(lsp);
        }

        self.analyses.insert(
            uri.clone(),
            AnalysisCache {
                analyzer: diags,
                lsp_by_lint,
                profile: Some(Arc::clone(&profile)),
            },
        );

        self.publish_diagnostics(uri, lsp_diags, Some(doc_version))?;
        Ok(())
    }

    fn resolve_profile(&mut self, config: &Config, base_dir: &Path) -> Arc<Profile> {
        if let Some(p) = self.profile_cache.get(base_dir) {
            return Arc::clone(p);
        }
        let resolved = match config.resolve_profile(
            base_dir,
            rpm_spec_analyzer::profile::ResolveOptions::default(),
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "profile resolution failed; using default Profile: {}",
                    format_error_chain(&e)
                );
                Profile::default()
            }
        };
        let arc = Arc::new(resolved);
        self.profile_cache
            .insert(base_dir.to_path_buf(), Arc::clone(&arc));
        arc
    }

    fn publish_diagnostics(
        &self,
        uri: &Uri,
        diagnostics: Vec<LspDiagnostic>,
        version: Option<i32>,
    ) -> Result<()> {
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics,
            version,
        };
        let note = Notification {
            method: PublishDiagnostics::METHOD.to_string(),
            params: serde_json::to_value(params)?,
        };
        self.connection.sender.send(Message::Notification(note))?;
        Ok(())
    }

    fn respond<T: serde::Serialize>(&self, id: RequestId, result: T) -> Result<()> {
        let resp = Response::new_ok(id, result);
        self.connection.sender.send(Message::Response(resp))?;
        Ok(())
    }
}

/// Pick the position encoding to advertise back to the client.
///
/// LSP 3.17: the server picks any encoding present in the client's
/// `general.positionEncodings` list. UTF-8 is preferred when offered
/// (matches the analyzer's native byte spans → no codepoint walks).
/// Default is UTF-16 for compatibility with clients that omit the list.
fn pick_encoding(init: &InitializeParams) -> PositionEncoding {
    let Some(general) = init.capabilities.general.as_ref() else {
        return PositionEncoding::Utf16;
    };
    let Some(list) = general.position_encodings.as_ref() else {
        return PositionEncoding::Utf16;
    };
    if list.iter().any(|k| k == &PositionEncodingKind::UTF8) {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}

fn cast_request<R>(req: &Request) -> Option<R::Params>
where
    R: RequestTrait,
    R::Params: serde::de::DeserializeOwned,
{
    if req.method != R::METHOD {
        return None;
    }
    match serde_json::from_value::<R::Params>(req.params.clone()) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(method = R::METHOD, err = %e, "failed to deserialize request params");
            None
        }
    }
}

fn cast_notification<N>(note: &Notification) -> Option<N::Params>
where
    N: NotificationTrait,
    N::Params: serde::de::DeserializeOwned,
{
    if note.method != N::METHOD {
        return None;
    }
    match serde_json::from_value::<N::Params>(note.params.clone()) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(method = N::METHOD, err = %e, "failed to deserialize notification params");
            None
        }
    }
}

/// Resolve the XDG-default config path for the LSP server's
/// [`ConfigCache`]. Returns `Some(path)` only when the file actually
/// exists — passing a path to a non-existent file would make every
/// `load_for` call fail loudly; the empty `None` cache degrades to
/// built-in defaults silently, which is what editors expect when a
/// project has no `rpmspec.toml`.
///
/// Mirrors the CLI's `cli_config::make_config_cache` resolution so a
/// spec linted in the editor matches what `rpm-spec-tool lint` would
/// produce from the shell.
fn xdg_default_config() -> Option<std::path::PathBuf> {
    let path = default_config_path()?;
    if path.is_file() { Some(path) } else { None }
}
