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

use crate::transport::{PostHandler, PostResponse, RequestParts, SessionStore};

/// Negotiated MCP protocol version we'll always echo back. Real Copilot CLI
/// v1.0.44 sends "2025-11-25" — we mirror it to avoid downgrade rejection.
const FALLBACK_PROTOCOL_VERSION: &str = "2025-11-25";

/// One MCP tool invocation flowing from a connection's background task into
/// the foreground dispatcher.
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
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
    pub fn spawn(capabilities: Arc<dyn EditorCapabilities>, cx: &mut App) -> Self {
        let (sender, receiver) = unbounded();
        let task = cx.spawn(async move |cx| {
            run_tool_loop(receiver, capabilities, cx).await;
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
    cx: &mut AsyncApp,
) {
    while let Some(call) = receiver.next().await {
        let ToolCall {
            name,
            arguments,
            respond_to,
        } = call;
        log::info!("Copilot /ide tools/call: tool={name}");
        let result = run_tool(&name, arguments, capabilities.clone(), cx).await;
        let _ = respond_to.send(result);
    }
}

async fn run_tool(
    name: &str,
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let payload = match name {
        "get_vscode_info" => tool_get_vscode_info(),
        "get_selection" => tool_get_selection(capabilities, cx).await,
        "get_diagnostics" => tool_get_diagnostics(arguments, capabilities, cx).await?,
        "open_diff" => return tool_open_diff(arguments, capabilities, cx).await,
        "close_diff" => tool_close_diff(arguments).await,
        "update_session_name" => json!({ "success": true }),
        other => return Err(anyhow!("unknown MCP tool: {other}")),
    };
    Ok(make_text_result(&payload))
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
) -> Result<Value> {
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
            return Err(anyhow!(
                "get_diagnostics: path is outside any visible workspace folder"
            ));
        }
    }

    let diagnostics = cx.update(|cx| capabilities.get_diagnostics(path, cx));
    Ok(diagnostics_to_json(diagnostics))
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
            let method = body
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let id = body.get("id").cloned();
            let params = body.get("params").cloned().unwrap_or(Value::Null);

            // Reject batch (array body).
            if body.is_array() {
                return PostResponse::json(
                    StatusCode::BAD_REQUEST,
                    b"batch JSON-RPC not supported".to_vec(),
                );
            }

            // Notification (no id or null id) — 202 Accepted, no body.
            if id.is_none() || matches!(id, Some(Value::Null)) {
                log::debug!("Copilot /ide notification: method={method}");
                return PostResponse::accepted();
            }
            log::debug!("Copilot /ide request method={method}");

            // Request — handle locally or dispatch to foreground tool loop.
            let result = match method.as_str() {
                "initialize" => match self.handle_initialize(&parts, &params) {
                    Ok(value) => return self.initialize_response(id, value, &parts),
                    Err(e) => Err(e),
                },
                "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
                "tools/call" => self.handle_tools_call(params).await,
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
    fn handle_initialize(&self, parts: &RequestParts, params: &Value) -> Result<Value> {
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
        // Tolerate duplicate initialize for the same id by treating it as a
        // re-init (replace the entry). Strictly we should 409, but empirically
        // the CLI may retry; better UX to accept.
        if self.session_store.exists(&session_id) {
            self.session_store.delete(&session_id);
        }
        self.session_store
            .try_create(session_id.clone(), protocol_version.clone(), pid, parent_pid)
            .map_err(|_| anyhow!("session create raced"))?;
        Ok(json!({
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
        }))
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

    async fn handle_tools_call(&self, params: Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("tools/call missing name"))?
            .to_string();
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        let (tx, rx) = oneshot::channel();
        self.tool_call_sender
            .unbounded_send(ToolCall {
                name,
                arguments,
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

