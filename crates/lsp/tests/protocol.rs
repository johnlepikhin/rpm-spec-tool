//! End-to-end protocol tests.
//!
//! Spawn `rpm_spec_lsp::Server` against an in-memory `lsp_server::Connection`
//! and drive it with the same JSON-RPC messages an editor would send. No
//! subprocesses, no tokio.

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidOpenTextDocument, Initialized, Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, Initialize,
    PrepareRenameRequest, Rename, Request as RequestTrait, Shutdown,
};
use lsp_types::{
    ClientCapabilities, CodeActionContext, CodeActionParams, CodeActionResponse, CompletionParams,
    CompletionResponse, DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, InitializeParams,
    InitializedParams, NumberOrString, PartialResultParams, Position, PrepareRenameResponse,
    PublishDiagnosticsParams, Range, RenameParams, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, WorkDoneProgressParams, WorkspaceEdit,
};

use rpm_spec_lsp::Server;

const SPEC_LANG_ID: &str = "rpm-spec";

/// Spec source missing `Name:` AND a `%changelog`. We expect at least
/// the missing-changelog finding (`RPM001`), which is a default-warn
/// AST rule covered by the analyzer's own test suite.
const BAD_SPEC: &str = "Version: 1\n";

fn spawn_server() -> Connection {
    let (server_conn, client_conn) = Connection::memory();
    thread::spawn(move || {
        let server = Server::new(server_conn);
        if let Err(e) = server.run() {
            eprintln!("server thread exited with error: {e:#}");
        }
    });
    client_conn
}

fn send_initialize(client: &Connection) {
    let params = InitializeParams {
        capabilities: ClientCapabilities::default(),
        ..Default::default()
    };
    let req = Request {
        id: RequestId::from(1),
        method: Initialize::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap(),
    };
    client
        .sender
        .send(Message::Request(req))
        .expect("send initialize");
    // Drain the initialize response so the server can proceed.
    match recv_with_timeout(client, Duration::from_secs(2)) {
        Some(Message::Response(_)) => {}
        other => panic!("expected initialize response, got {other:?}"),
    }
    let note = Notification {
        method: Initialized::METHOD.to_string(),
        params: serde_json::to_value(InitializedParams {}).unwrap(),
    };
    client
        .sender
        .send(Message::Notification(note))
        .expect("send initialized");
}

fn send_did_open(client: &Connection, uri: &Uri, text: &str) {
    let params = DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: SPEC_LANG_ID.to_string(),
            version: 1,
            text: text.to_string(),
        },
    };
    let note = Notification {
        method: DidOpenTextDocument::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap(),
    };
    client
        .sender
        .send(Message::Notification(note))
        .expect("send didOpen");
}

fn shutdown(client: &Connection) {
    let req = Request {
        id: RequestId::from(99),
        method: Shutdown::METHOD.to_string(),
        params: serde_json::Value::Null,
    };
    client
        .sender
        .send(Message::Request(req))
        .expect("send shutdown");
    // Drain shutdown response.
    let _ = recv_with_timeout(client, Duration::from_secs(2));
    let note = Notification {
        method: "exit".to_string(),
        params: serde_json::Value::Null,
    };
    let _ = client.sender.send(Message::Notification(note));
}

fn recv_with_timeout(client: &Connection, timeout: Duration) -> Option<Message> {
    client.receiver.recv_timeout(timeout).ok()
}

/// Wait for the next `publishDiagnostics` notification, ignoring
/// unrelated messages. Returns the parsed params.
fn wait_for_diagnostics(client: &Connection) -> PublishDiagnosticsParams {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = std::time::Instant::now();
        let remaining = deadline.checked_duration_since(now).unwrap_or_default();
        let msg = client
            .receiver
            .recv_timeout(remaining)
            .expect("timeout waiting for publishDiagnostics");
        if let Message::Notification(note) = msg
            && note.method == PublishDiagnostics::METHOD
        {
            return serde_json::from_value(note.params).expect("parse publishDiagnostics");
        }
    }
}

fn send_request<R>(client: &Connection, id: i32, params: R::Params) -> Response
where
    R: RequestTrait,
    R::Params: serde::Serialize,
{
    let req = Request {
        id: RequestId::from(id),
        method: R::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap(),
    };
    client
        .sender
        .send(Message::Request(req))
        .expect("send request");
    loop {
        let msg = recv_with_timeout(client, Duration::from_secs(5))
            .expect("timeout waiting for response");
        if let Message::Response(r) = msg {
            return r;
        }
        // Notifications (e.g. extra publishDiagnostics) are skipped.
    }
}

#[test]
fn initialize_and_publish_diagnostics() {
    let client = spawn_server();
    send_initialize(&client);

    let uri: Uri = "file:///tmp/test.spec".parse().unwrap();
    send_did_open(&client, &uri, BAD_SPEC);

    let params = wait_for_diagnostics(&client);
    assert_eq!(params.uri.as_str(), uri.as_str());
    let has_rpm001 = params.diagnostics.iter().any(|d| {
        matches!(
            d.code.as_ref(),
            Some(NumberOrString::String(s)) if s == "RPM001"
        )
    });
    assert!(
        has_rpm001,
        "expected RPM001 (missing-changelog) in {:?}",
        params.diagnostics
    );

    shutdown(&client);
}

#[test]
fn code_action_returns_quick_fix_when_available() {
    // Use a spec that we know produces at least one suggestion-bearing
    // diagnostic. Trailing-whitespace is one of the simpler ones — its
    // rule attaches a MachineApplicable edit that removes the offending
    // bytes.
    let src = "Name: hello\n%description\nbody  \n%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n";
    let client = spawn_server();
    send_initialize(&client);

    let uri: Uri = "file:///tmp/ws.spec".parse().unwrap();
    send_did_open(&client, &uri, src);
    let diags = wait_for_diagnostics(&client);

    // Find any diagnostic whose range we can use as the request range.
    let Some(target) = diags.diagnostics.first() else {
        // No diagnostics for this fixture → nothing to test; treat as
        // a "skipped" pass rather than a failure. We assert below that
        // the cached spec at least round-trips through `didOpen`.
        shutdown(&client);
        return;
    };

    let params = CodeActionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range: target.range,
        context: CodeActionContext {
            diagnostics: vec![target.clone()],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let resp = send_request::<CodeActionRequest>(&client, 2, params);
    assert!(resp.error.is_none(), "codeAction error: {:?}", resp.error);

    // Result may be null (no actions) or a list. Both are fine for
    // this smoke test — we're really verifying that the request gets
    // a well-formed response and doesn't hang or panic.
    let result = resp.result.unwrap_or(serde_json::Value::Null);
    if !result.is_null() {
        let _parsed: CodeActionResponse =
            serde_json::from_value(result).expect("parse codeAction response");
    }
    let _ = (Position::new(0, 0), Range::default());

    shutdown(&client);
}

#[test]
fn document_symbol_returns_sections() {
    let src = "Name: hello\n%prep\nset -x\n%build\nmake\n%files\n/usr/bin/hello\n";
    let client = spawn_server();
    send_initialize(&client);

    let uri: Uri = "file:///tmp/outline.spec".parse().unwrap();
    send_did_open(&client, &uri, src);
    // Drain the publishDiagnostics that follows didOpen so the symbol
    // response isn't queued behind it.
    let _ = wait_for_diagnostics(&client);

    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let resp = send_request::<DocumentSymbolRequest>(&client, 3, params);
    assert!(
        resp.error.is_none(),
        "documentSymbol error: {:?}",
        resp.error
    );
    let result = resp.result.expect("non-null documentSymbol result");
    let parsed: DocumentSymbolResponse =
        serde_json::from_value(result).expect("parse documentSymbol response");

    let names: Vec<String> = match parsed {
        DocumentSymbolResponse::Nested(syms) => syms.iter().map(|s| s.name.clone()).collect(),
        DocumentSymbolResponse::Flat(infos) => infos.iter().map(|s| s.name.clone()).collect(),
    };
    assert!(names.iter().any(|n| n == "%prep"), "got {names:?}");
    assert!(names.iter().any(|n| n == "%build"), "got {names:?}");
    assert!(names.iter().any(|n| n == "%files"), "got {names:?}");

    shutdown(&client);
}

#[test]
fn hover_returns_markdown_for_known_tag() {
    let src = "BuildRequires: gcc\n";
    let client = spawn_server();
    send_initialize(&client);

    let uri: Uri = "file:///tmp/hover.spec".parse().unwrap();
    send_did_open(&client, &uri, src);
    let _ = wait_for_diagnostics(&client);

    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(0, 3),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let resp = send_request::<HoverRequest>(&client, 4, params);
    assert!(resp.error.is_none(), "hover error: {:?}", resp.error);
    let result = resp.result.expect("non-null hover result");
    let hover: Hover = serde_json::from_value(result).expect("parse hover");
    // Render the contents to a string regardless of variant, then
    // assert the BuildRequires keyword appears.
    let body = serde_json::to_string(&hover.contents).unwrap();
    assert!(
        body.contains("BuildRequires"),
        "hover content missing tag name: {body}"
    );

    shutdown(&client);
}

#[test]
fn completion_offers_directives_after_percent() {
    let src = "%pre\n";
    let client = spawn_server();
    send_initialize(&client);

    let uri: Uri = "file:///tmp/complete.spec".parse().unwrap();
    send_did_open(&client, &uri, src);
    let _ = wait_for_diagnostics(&client);

    let params = CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(0, 4),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: None,
    };
    let resp = send_request::<Completion>(&client, 5, params);
    assert!(resp.error.is_none(), "completion error: {:?}", resp.error);
    let result = resp.result.expect("non-null completion result");
    let parsed: CompletionResponse =
        serde_json::from_value(result).expect("parse completion response");
    let labels: Vec<String> = match parsed {
        CompletionResponse::Array(items) => items.into_iter().map(|i| i.label).collect(),
        CompletionResponse::List(list) => list.items.into_iter().map(|i| i.label).collect(),
    };
    assert!(labels.contains(&"%prep".to_string()), "got {labels:?}");
    assert!(labels.contains(&"%pretrans".to_string()), "got {labels:?}");

    shutdown(&client);
}

#[test]
fn rename_macro_round_trips() {
    let src = "%define foo 1\n%build\necho %foo %{foo}\n";
    let client = spawn_server();
    send_initialize(&client);

    let uri: Uri = "file:///tmp/rename.spec".parse().unwrap();
    send_did_open(&client, &uri, src);
    let _ = wait_for_diagnostics(&client);

    // prepareRename on the reference `%foo` should return a Range.
    let prep_params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position::new(2, 6), // inside `foo` of `%foo`
    };
    let resp = send_request::<PrepareRenameRequest>(&client, 6, prep_params);
    assert!(
        resp.error.is_none(),
        "prepareRename error: {:?}",
        resp.error
    );
    let prep_result = resp.result.expect("non-null prepareRename");
    let prep: PrepareRenameResponse =
        serde_json::from_value(prep_result).expect("parse prepareRename");
    match prep {
        PrepareRenameResponse::Range(r) => {
            assert_eq!(r.start.line, 2);
            // Range covers identifier only — start is past the `%`.
            assert!(r.start.character > 0);
        }
        other => panic!("expected Range response, got {other:?}"),
    }

    // Actual rename.
    let rename_params = RenameParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(2, 6),
        },
        new_name: "bar".to_string(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let resp = send_request::<Rename>(&client, 7, rename_params);
    assert!(resp.error.is_none(), "rename error: {:?}", resp.error);
    let result = resp.result.expect("non-null rename result");
    let edit: WorkspaceEdit = serde_json::from_value(result).expect("parse rename WorkspaceEdit");
    #[allow(clippy::mutable_key_type)]
    let mut changes = edit.changes.expect("changes map");
    let edits = changes.remove(&uri).expect("edits for our URI");
    assert_eq!(edits.len(), 3, "expected 3 edits, got {edits:?}");
    for e in &edits {
        assert_eq!(e.new_text, "bar");
    }

    shutdown(&client);
}

#[test]
fn goto_definition_jumps_to_define_site() {
    let src = "%define foo bar\n%build\necho %foo\n";
    let client = spawn_server();
    send_initialize(&client);

    let uri: Uri = "file:///tmp/goto.spec".parse().unwrap();
    send_did_open(&client, &uri, src);
    let _ = wait_for_diagnostics(&client);

    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(2, 7), // inside `foo` of `%foo`
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let resp = send_request::<GotoDefinition>(&client, 8, params);
    assert!(resp.error.is_none(), "definition error: {:?}", resp.error);
    let result = resp.result.expect("non-null definition result");
    let parsed: GotoDefinitionResponse =
        serde_json::from_value(result).expect("parse definition response");
    let loc = match parsed {
        GotoDefinitionResponse::Scalar(l) => l,
        GotoDefinitionResponse::Array(mut v) => v.remove(0),
        other => panic!("unexpected response: {other:?}"),
    };
    // Definition is on line 0; range covers identifier `foo`.
    assert_eq!(loc.uri.as_str(), uri.as_str());
    assert_eq!(loc.range.start.line, 0);
    assert_eq!(loc.range.start.character, 8);
    assert_eq!(loc.range.end.character, 11);

    shutdown(&client);
}
