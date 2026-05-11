//! MCP message-layer dispatcher for Copilot CLI's `/ide` connection.
//!
//! Two pieces:
//!
//! - [`McpDispatcher`] runs on the GPUI foreground (because
//!   [`EditorCapabilities`] is `!Send`). Owns a `mpsc` receiver that drains
//!   tool calls and resolves them via the capabilities trait.
//!
//! - [`McpPostHandler`] is the [`PostHandler`] implementation passed to
//!   [`Server`]. It runs on the per-connection background task and:
//!     * parses the JSON-RPC request,
//!     * handles synchronous methods (`initialize`, `tools/list`,
//!       `notifications/initialized`) directly using only [`SessionStore`]
//!       state,
//!     * forwards `tools/call` to the foreground dispatcher via a channel
//!       and awaits the result.
//!
//! Splitting the two halves keeps wire-layer work off the foreground and
//! limits the foreground hop to actual EditorCapabilities calls.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use editor_capabilities::{
    DiagnosticInfo, DiagnosticSeverity, DiffDecision, EditorCapabilities, EditorSelection,
};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::future::BoxFuture;
use gpui::{App, AppContext as _, AsyncApp, Task};
use http::{HeaderName, HeaderValue, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::router::TerminalRouter;
use crate::transport::{CreateError, PostHandler, PostResponse, RequestParts, SessionStore};

/// Negotiated MCP protocol version we'll always echo back. Real Copilot CLI
/// v1.0.44 sends "2025-11-25" — we mirror it to avoid downgrade rejection.
const FALLBACK_PROTOCOL_VERSION: &str = "2025-11-25";

/// Result of attempting to create a session via `initialize`.
enum InitializeOutcome {
    Created(Value),
    DuplicateSession,
}

/// One MCP tool invocation flowing from a connection's background task into
/// the foreground dispatcher.
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
    /// `mcp-session-id` (or `x-copilot-session-id`) header from the originating
    /// POST. Per-terminal tool handlers (e.g. `update_session_name`) use this
    /// to route back to the spawning Copilot terminal via
    /// [`crate::router::TerminalRouter`]. `None` only on POSTs that arrived
    /// without either header — the live CLI always supplies one.
    pub session_id: Option<String>,
    pub respond_to: oneshot::Sender<Result<Value>>,
}

pub type ToolCallSender = UnboundedSender<ToolCall>;

/// Routes MCP `tools/call` requests into [`EditorCapabilities`]. Spawned on
/// the GPUI foreground because the trait holds entities that aren't `Send`.
pub struct McpDispatcher {
    sender: ToolCallSender,
    _task: Task<()>,
}

impl McpDispatcher {
    pub fn spawn(
        capabilities: Arc<dyn EditorCapabilities>,
        router: Rc<dyn TerminalRouter>,
        cx: &mut App,
    ) -> Self {
        let (sender, receiver) = unbounded();
        let task = cx.spawn(async move |cx| {
            run_tool_loop(receiver, capabilities, router, cx).await;
        });
        Self {
            sender,
            _task: task,
        }
    }

    pub fn sender(&self) -> ToolCallSender {
        self.sender.clone()
    }
}

async fn run_tool_loop(
    mut receiver: UnboundedReceiver<ToolCall>,
    capabilities: Arc<dyn EditorCapabilities>,
    router: Rc<dyn TerminalRouter>,
    cx: &mut AsyncApp,
) {
    while let Some(call) = receiver.next().await {
        let ToolCall {
            name,
            arguments,
            session_id,
            respond_to,
        } = call;
        log::info!(
            "Copilot /ide tools/call: tool={name} session_id={}",
            session_id.as_deref().unwrap_or("<none>")
        );
        let result = run_tool(
            &name,
            arguments,
            session_id,
            capabilities.clone(),
            router.clone(),
            cx,
        )
        .await;
        let _ = respond_to.send(result);
    }
}

async fn run_tool(
    name: &str,
    arguments: Value,
    session_id: Option<String>,
    capabilities: Arc<dyn EditorCapabilities>,
    router: Rc<dyn TerminalRouter>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    // Tool-level failures (bad arguments, path-traversal rejection, "unknown
    // tool") should surface as `result.isError = true` per MCP convention, not
    // as JSON-RPC -32603 errors. Reserve `Err` here for genuine protocol-level
    // failures (channel disconnect, GPUI window dropped, etc.).
    let payload = match name {
        "get_vscode_info" => tool_get_vscode_info(),
        "get_selection" => tool_get_selection(capabilities, cx).await,
        "get_diagnostics" => return Ok(tool_get_diagnostics(arguments, capabilities, cx).await),
        "open_diff" => return tool_open_diff(arguments, capabilities, cx).await,
        "close_diff" => tool_close_diff(arguments).await,
        "update_session_name" => tool_update_session_name(&arguments, session_id.as_deref(), router, cx).await,
        other => return Ok(make_text_error(&format!("unknown MCP tool: {other}"))),
    };
    Ok(make_text_result(&payload))
}

/// Resolve the spawning terminal for this MCP session and (eventually) rename
/// its tab. v1 wires the routing groundwork — the actual tab-title rewrite
/// lands in ZR-4. We resolve here so we have a trace that routing works
/// end-to-end on a live CLI even before the rename code exists.
async fn tool_update_session_name(
    arguments: &Value,
    session_id: Option<&str>,
    router: Rc<dyn TerminalRouter>,
    cx: &mut AsyncApp,
) -> Value {
    let new_name = arguments.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let resolved = match session_id {
        Some(sid) => cx.update(|cx| router.terminal_for_session(sid, cx)),
        None => None,
    };
    log::info!(
        "Copilot /ide update_session_name: name={new_name:?} session_id={} target_terminal={resolved:?}",
        session_id.unwrap_or("<none>"),
    );
    json!({ "success": true })
}

/// Wrap any tool result value in MCP's `{content: [{type: "text", text: <json>}]}`
/// envelope. Matches `makeTextResult` in vscode-copilot-chat's tool/utils.ts.
fn make_text_result(value: &Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_default(),
        }]
    })
}

/// Variant of `make_text_result` that also marks the result as an error.
fn make_text_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn tool_get_vscode_info() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "appName": "Zerminal",
        "appRoot": std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_default(),
        "language": "en",
        "machineId": "zerminal",
        "sessionId": "",
        "uriScheme": "zerminal",
        "shell": std::env::var("SHELL").unwrap_or_default(),
    })
}

async fn tool_get_selection(
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Value {
    let selection = cx.update(|cx| capabilities.current_selection(cx));
    match selection {
        Some(sel) => selection_to_json(&sel, true),
        None => Value::Null,
    }
}

fn selection_to_json(sel: &EditorSelection, current: bool) -> Value {
    let path_str = sel.path.to_string_lossy().into_owned();
    let file_url = url::Url::from_file_path(&*sel.path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{path_str}"));
    let text = sel
        .text
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_default();
    let is_empty = sel.start == sel.end;
    json!({
        "text": text,
        "filePath": path_str,
        "fileUrl": file_url,
        "selection": {
            "start": { "line": sel.start.row, "character": sel.start.column },
            "end": { "line": sel.end.row, "character": sel.end.column },
            "isEmpty": is_empty,
        },
        "current": current,
    })
}

#[derive(Deserialize)]
struct DiagnosticsArgs {
    #[serde(default)]
    uri: Option<String>,
}

async fn tool_get_diagnostics(
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Value {
    let args: DiagnosticsArgs = serde_json::from_value(arguments).unwrap_or(DiagnosticsArgs {
        uri: None,
    });
    let path: Option<Arc<Path>> = args.uri.as_ref().map(|raw| {
        let stripped = raw.strip_prefix("file://").unwrap_or(raw.as_str());
        Arc::from(PathBuf::from(stripped).as_path())
    });

    if let Some(target) = path.as_ref() {
        let folders = cx.update(|cx| capabilities.list_workspace_folders(cx));
        if !path_is_within_workspace(target, &folders) {
            log::warn!(
                "Copilot /ide get_diagnostics: rejecting path outside any visible worktree: {}",
                target.display()
            );
            return make_text_error(
                "get_diagnostics: path is outside any visible workspace folder",
            );
        }
    }

    let diagnostics = cx.update(|cx| capabilities.get_diagnostics(path, cx));
    make_text_result(&diagnostics_to_json(diagnostics))
}

fn diagnostics_to_json(diagnostics: Vec<DiagnosticInfo>) -> Value {
    use std::collections::BTreeMap;
    let mut grouped: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
    for entry in diagnostics {
        let mut diag = json!({
            "range": {
                "start": { "line": entry.start.row, "character": entry.start.column },
                "end":   { "line": entry.end.row,   "character": entry.end.column   },
            },
            "severity": severity_label(entry.severity),
            "message": entry.message.to_string(),
        });
        if let Some(obj) = diag.as_object_mut() {
            if let Some(source) = entry.source.as_ref() {
                obj.insert("source".into(), Value::String(source.to_string()));
            }
            if let Some(code) = entry.code.as_ref() {
                obj.insert("code".into(), Value::String(code.to_string()));
            }
        }
        grouped.entry(entry.path.to_path_buf()).or_default().push(diag);
    }
    let entries: Vec<Value> = grouped
        .into_iter()
        .map(|(path, diags)| {
            let path_str = path.to_string_lossy().into_owned();
            let file_url = url::Url::from_file_path(&path)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| format!("file://{path_str}"));
            json!({
                "uri": file_url,
                "filePath": path_str,
                "diagnostics": diags,
            })
        })
        .collect();
    Value::Array(entries)
}

fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Information => "information",
        DiagnosticSeverity::Hint => "hint",
    }
}

#[derive(Deserialize)]
struct OpenDiffArgs {
    original_file_path: PathBuf,
    new_file_contents: String,
    tab_name: String,
}

async fn tool_open_diff(
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let args: OpenDiffArgs = match serde_json::from_value(arguments) {
        Ok(v) => v,
        Err(e) => return Ok(make_text_error(&format!("invalid arguments: {e}"))),
    };

    let folders = cx.update(|cx| capabilities.list_workspace_folders(cx));
    if !path_is_within_workspace(&args.original_file_path, &folders) {
        log::warn!(
            "Copilot /ide open_diff: rejecting path outside any visible worktree: {}",
            args.original_file_path.display()
        );
        return Ok(make_text_error(&format!(
            "open_diff: path is outside any visible workspace folder: {}",
            args.original_file_path.display()
        )));
    }

    let read_path = args.original_file_path.clone();
    let read_task = cx.background_spawn(async move { std::fs::read_to_string(&read_path) });
    let original_text = match read_task.await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Ok(make_text_error(&format!(
                "open_diff: failed to read {}: {error}",
                args.original_file_path.display()
            )));
        }
    };

    let path: Arc<Path> = Arc::from(args.original_file_path.as_path());
    let task = cx.update(|cx| {
        capabilities.open_diff_for_review(path, original_text, args.new_file_contents, cx)
    });
    let decision = task.await?;

    let payload = open_diff_response(&args.tab_name, &args.original_file_path, decision);
    Ok(make_text_result(&payload))
}

fn open_diff_response(tab_name: &str, file_path: &Path, decision: DiffDecision) -> Value {
    match decision {
        DiffDecision::Accept { .. } => json!({
            "success": true,
            "result": "SAVED",
            "trigger": "user_accepted",
            "tab_name": tab_name,
            "message": format!("User accepted changes for {}", file_path.display()),
        }),
        DiffDecision::Reject => json!({
            "success": true,
            "result": "REJECTED",
            "trigger": "user_rejected",
            "tab_name": tab_name,
            "message": format!("User rejected changes for {}", file_path.display()),
        }),
        DiffDecision::Cancelled => json!({
            "success": true,
            "result": "REJECTED",
            "trigger": "tab_closed",
            "tab_name": tab_name,
            "message": format!("Diff tab closed without decision for {}", file_path.display()),
        }),
    }
}

#[derive(Deserialize)]
struct CloseDiffArgs {
    tab_name: String,
}

async fn tool_close_diff(arguments: Value) -> Value {
    let args: CloseDiffArgs = match serde_json::from_value(arguments) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "success": true,
                "already_closed": true,
                "tab_name": "",
                "message": "no tab_name supplied; treating as already closed",
            });
        }
    };
    // v1: we don't track open AgentDiffPanes by tab_name yet, so the best
    // we can do is acknowledge. The spawn_diff_review pane closes itself
    // when the user picks Accept/Reject, and dropping the pane fires our
    // `Cancelled` decision. The model would receive a delayed `open_diff`
    // result with `trigger: "tab_closed"` instead of an immediate close.
    json!({
        "success": true,
        "already_closed": true,
        "tab_name": args.tab_name,
        "message": "close_diff: tracking not yet implemented; diff will close on user action",
    })
}

fn path_is_within_workspace(path: &Path, workspace_folders: &[Arc<Path>]) -> bool {
    if workspace_folders.is_empty() {
        return false;
    }
    workspace_folders.iter().any(|root| path.starts_with(root.as_ref()))
}

/// Tool descriptors returned for `tools/list`. These mirror the inputSchema
/// shapes vscode-copilot-chat declares — Copilot CLI is tool-agnostic and
/// just forwards what we advertise to the model.
fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "get_vscode_info",
            "description": "Get information about the current Zerminal instance.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "get_selection",
            "description": "Get the active editor's file path and current text selection. Always call this when the user asks 'what file am I in', 'what's open', 'what am I looking at', or any question about their current editor state — file-path context is only refreshed in your prompt when the user has text selected, so for cursor-only states you must call this tool to learn the active file.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "get_diagnostics",
            "description": "Gets language diagnostics (errors, warnings, hints) from Zerminal.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uri": { "type": "string", "description": "Optional file URI to filter to." }
                }
            },
        }),
        json!({
            "name": "open_diff",
            "description": "Open a diff view for the user to review proposed edits. Blocks until the user accepts, rejects, or closes the tab.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "original_file_path": { "type": "string" },
                    "new_file_contents":  { "type": "string" },
                    "tab_name":           { "type": "string" },
                },
                "required": ["original_file_path", "new_file_contents", "tab_name"],
            },
        }),
        json!({
            "name": "close_diff",
            "description": "Close a diff tab by tab_name.",
            "inputSchema": {
                "type": "object",
                "properties": { "tab_name": { "type": "string" } },
                "required": ["tab_name"],
            },
        }),
        json!({
            "name": "update_session_name",
            "description": "Update the display name for the current CLI session.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"],
            },
        }),
    ]
}

// ---------------------------------------------------------------------------
// PostHandler: glues the wire layer to the dispatcher.
// ---------------------------------------------------------------------------

pub struct McpPostHandler {
    session_store: SessionStore,
    tool_call_sender: ToolCallSender,
}

impl McpPostHandler {
    pub fn new(session_store: SessionStore, tool_call_sender: ToolCallSender) -> Self {
        Self {
            session_store,
            tool_call_sender,
        }
    }
}

impl PostHandler for McpPostHandler {
    fn handle_post(self: Arc<Self>, parts: RequestParts) -> BoxFuture<'static, PostResponse> {
        Box::pin(async move {
            // Parse JSON-RPC envelope.
            let body: Value = match serde_json::from_slice(&parts.body) {
                Ok(v) => v,
                Err(_) => {
                    return PostResponse::json(
                        StatusCode::BAD_REQUEST,
                        b"invalid JSON".to_vec(),
                    );
                }
            };

            // Reject batch (array body) before any per-request field lookups —
            // `body.get(...)` on an array silently returns None, which would
            // otherwise let array bodies fall through to the notification path.
            if body.is_array() {
                return PostResponse::json(
                    StatusCode::BAD_REQUEST,
                    b"batch JSON-RPC not supported".to_vec(),
                );
            }

            let method = body
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let id = body.get("id").cloned();
            let params = body.get("params").cloned().unwrap_or(Value::Null);

            // Notification (no id or null id) — 202 Accepted, no body.
            if id.is_none() || matches!(id, Some(Value::Null)) {
                log::debug!("Copilot /ide notification: method={method}");
                return PostResponse::accepted();
            }
            log::debug!("Copilot /ide request method={method}");

            // Request — handle locally or dispatch to foreground tool loop.
            let result = match method.as_str() {
                "initialize" => match self.handle_initialize(&parts, &params) {
                    Ok(InitializeOutcome::Created(value)) => {
                        return self.initialize_response(id, value, &parts);
                    }
                    Ok(InitializeOutcome::DuplicateSession) => {
                        log::warn!(
                            "Copilot /ide initialize: rejecting duplicate session id"
                        );
                        return duplicate_session_response(id);
                    }
                    Err(e) => Err(e),
                },
                "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
                "tools/call" => self.handle_tools_call(&parts, params).await,
                _ => return jsonrpc_error_response(id, -32601, "Method not found"),
            };

            match result {
                Ok(value) => jsonrpc_result_response(id, value),
                Err(error) => {
                    log::warn!("Copilot /ide method {method} failed: {error:#}");
                    jsonrpc_error_response(id, -32603, &error.to_string())
                }
            }
        })
    }
}

impl McpPostHandler {
    fn handle_initialize(
        &self,
        parts: &RequestParts,
        params: &Value,
    ) -> Result<InitializeOutcome> {
        let session_id = extract_session_id(parts)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let protocol_version = params
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(FALLBACK_PROTOCOL_VERSION)
            .to_string();
        let pid = parts
            .headers
            .get("x-copilot-pid")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok());
        let parent_pid = parts
            .headers
            .get("x-copilot-parent-pid")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok());
        // Per MCP Streamable HTTP, a duplicate initialize for an already-known
        // session id is a 409 Conflict. We must NOT silently replace the prior
        // entry — that would tear down any attached SSE stream out from under
        // a legitimate consumer.
        match self.session_store.try_create(
            session_id.clone(),
            protocol_version.clone(),
            pid,
            parent_pid,
        ) {
            Ok(()) => {}
            Err(CreateError::AlreadyExists) => return Ok(InitializeOutcome::DuplicateSession),
        }
        Ok(InitializeOutcome::Created(json!({
            "protocolVersion": protocol_version,
            "serverInfo": {
                "name": "Zerminal",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": { "listChanged": false }
            },
            // Echo the session id back in the JSON-RPC body too for clients
            // that don't read the header. vscode-copilot-chat does the same.
            "_zerminalSessionId": session_id,
        })))
    }

    fn initialize_response(
        self: Arc<Self>,
        id: Option<Value>,
        result: Value,
        parts: &RequestParts,
    ) -> PostResponse {
        let session_id = extract_session_id(parts)
            .or_else(|| {
                result
                    .get("_zerminalSessionId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .unwrap_or_default();
        let mut response = PostResponse::json(StatusCode::OK, body);
        if !session_id.is_empty() {
            // Echo both header conventions — the real CLI uses
            // `X-Copilot-Session-Id` but vscode-copilot-chat also sets
            // `mcp-session-id` for forward-compat with the MCP spec.
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(b"x-copilot-session-id"),
                HeaderValue::from_str(&session_id),
            ) {
                response = response.with_header(name, value);
            }
            if let Ok(value) = HeaderValue::from_str(&session_id) {
                response = response.with_header(http::HeaderName::from_static("mcp-session-id"), value);
            }
        }
        response
    }

    async fn handle_tools_call(&self, parts: &RequestParts, params: Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("tools/call missing name"))?
            .to_string();
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        let session_id = extract_session_id(parts);
        let (tx, rx) = oneshot::channel();
        self.tool_call_sender
            .unbounded_send(ToolCall {
                name,
                arguments,
                session_id,
                respond_to: tx,
            })
            .context("forwarding tool call to foreground dispatcher")?;
        rx.await
            .map_err(|_| anyhow!("foreground dispatcher dropped before responding"))?
    }
}

fn extract_session_id(parts: &RequestParts) -> Option<String> {
    parts
        .headers
        .get("x-copilot-session-id")
        .or_else(|| parts.headers.get("mcp-session-id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn jsonrpc_result_response(id: Option<Value>, result: Value) -> PostResponse {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .unwrap_or_default();
    PostResponse::json(StatusCode::OK, body)
}

fn jsonrpc_error_response(id: Option<Value>, code: i32, message: &str) -> PostResponse {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_default();
    PostResponse::json(StatusCode::OK, body)
}

/// 409 Conflict response for a duplicate `initialize` against an existing
/// session. Body carries a JSON-RPC error envelope so clients that don't read
/// the HTTP status still see something useful.
fn duplicate_session_response(id: Option<Value>) -> PostResponse {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32600,
            "message": "session already initialized",
        },
    }))
    .unwrap_or_default();
    PostResponse::json(StatusCode::CONFLICT, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderMap, HeaderValue, Method};

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        smol::block_on(f)
    }

    fn handler() -> Arc<McpPostHandler> {
        let sessions = SessionStore::new();
        let (tx, _rx) = unbounded();
        Arc::new(McpPostHandler::new(sessions, tx))
    }

    fn handler_with_receiver() -> (Arc<McpPostHandler>, UnboundedReceiver<ToolCall>) {
        let sessions = SessionStore::new();
        let (tx, rx) = unbounded();
        (Arc::new(McpPostHandler::new(sessions, tx)), rx)
    }

    fn tools_call_parts(session_id: Option<&str>) -> RequestParts {
        let mut headers = HeaderMap::new();
        if let Some(sid) = session_id {
            headers.insert(
                "x-copilot-session-id",
                HeaderValue::from_str(sid).unwrap(),
            );
        }
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "get_vscode_info", "arguments": {} },
        }))
        .unwrap();
        RequestParts {
            method: Method::POST,
            path: "/mcp".to_string(),
            headers,
            body,
        }
    }

    fn initialize_parts(session_id: &str) -> RequestParts {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-copilot-session-id",
            HeaderValue::from_str(session_id).unwrap(),
        );
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25" },
        }))
        .unwrap();
        RequestParts {
            method: Method::POST,
            path: "/mcp".to_string(),
            headers,
            body,
        }
    }

    #[test]
    fn duplicate_initialize_returns_409() {
        let handler = handler();
        let first = block_on(handler.clone().handle_post(initialize_parts("dup-session")));
        assert_eq!(first.status, StatusCode::OK);

        let second = block_on(handler.clone().handle_post(initialize_parts("dup-session")));
        assert_eq!(second.status, StatusCode::CONFLICT);

        let body: Value = serde_json::from_slice(&second.body).unwrap();
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert_eq!(body["error"]["code"], -32600);
        assert!(body["error"]["message"].as_str().unwrap().contains("session"));
    }

    #[test]
    fn tools_call_carries_session_id_into_dispatcher() {
        // Verifies the wire-to-dispatcher plumbing for routing per-terminal
        // tools (`update_session_name`, `close_diff`) — the foreground side
        // resolves session_id → terminal via TerminalRouter.
        let (handler, mut rx) = handler_with_receiver();
        let parts = tools_call_parts(Some("sid-alpha"));

        // handle_post awaits the dispatcher response, so spawn it and pull
        // the ToolCall off the receiver synchronously before responding.
        let post = smol::spawn(async move { handler.handle_post(parts).await });
        let call = block_on(async { rx.next().await.expect("dispatcher received call") });
        assert_eq!(call.name, "get_vscode_info");
        assert_eq!(call.session_id.as_deref(), Some("sid-alpha"));

        // Unblock handle_post so the spawn doesn't outlive the test.
        let _ = call.respond_to.send(Ok(json!({ "ok": true })));
        block_on(post);
    }

    #[test]
    fn tools_call_without_session_header_yields_none() {
        let (handler, mut rx) = handler_with_receiver();
        let parts = tools_call_parts(None);

        let post = smol::spawn(async move { handler.handle_post(parts).await });
        let call = block_on(async { rx.next().await.expect("dispatcher received call") });
        assert!(call.session_id.is_none());

        let _ = call.respond_to.send(Ok(json!({ "ok": true })));
        block_on(post);
    }

    #[test]
    fn batch_request_rejected_before_notification_path() {
        let handler = handler();
        let body = serde_json::to_vec(&json!([
            { "jsonrpc": "2.0", "id": 1, "method": "tools/list" },
            { "jsonrpc": "2.0", "id": 2, "method": "tools/list" }
        ]))
        .unwrap();
        let parts = RequestParts {
            method: Method::POST,
            path: "/mcp".to_string(),
            headers: HeaderMap::new(),
            body,
        };
        let response = block_on(handler.handle_post(parts));
        // Must be a 400, NOT a 202 Accepted (which is what the previous bug
        // produced because the array body's `get("id")` returned None).
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
    }
}
