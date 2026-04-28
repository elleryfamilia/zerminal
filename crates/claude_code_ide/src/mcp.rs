use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use editor_capabilities::{EditorCapabilities, OpenEditorInfo};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gpui::{App, AsyncApp, Task};
use serde::Deserialize;
use serde_json::{Value, json};

/// One MCP method invocation flowing from a connection's background task into
/// the foreground dispatcher.
pub struct McpCall {
    pub method: String,
    pub params: Value,
    pub respond_to: oneshot::Sender<Result<Value>>,
}

pub type McpCallSender = UnboundedSender<McpCall>;

/// Routes MCP method calls into [`EditorCapabilities`]. Lives on the GPUI
/// foreground because `dyn EditorCapabilities` holds entities that are not
/// `Send`. Connection background tasks send [`McpCall`]s over the returned
/// sender; this dispatcher resolves each call and replies via the call's
/// `respond_to` oneshot.
pub struct McpDispatcher {
    sender: McpCallSender,
    _task: Task<()>,
}

impl McpDispatcher {
    pub fn spawn(capabilities: Arc<dyn EditorCapabilities>, cx: &mut App) -> Self {
        let (sender, receiver) = unbounded();
        let task = cx.spawn(async move |cx| {
            run_dispatch_loop(receiver, capabilities, cx).await;
        });
        Self {
            sender,
            _task: task,
        }
    }

    pub fn sender(&self) -> McpCallSender {
        self.sender.clone()
    }
}

async fn run_dispatch_loop(
    mut receiver: UnboundedReceiver<McpCall>,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) {
    while let Some(call) = receiver.next().await {
        let McpCall {
            method,
            params,
            respond_to,
        } = call;
        let result = match dispatch(&method, params, capabilities.clone(), cx).await {
            Ok(value) => Ok(value),
            Err(error) => {
                log::warn!("Claude /ide MCP {method} failed: {error:#}");
                Err(error)
            }
        };
        let _ = respond_to.send(result);
    }
}

async fn dispatch(
    method: &str,
    params: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "Zerminal", "version": env!("CARGO_PKG_VERSION") }
        })),
        "notifications/initialized" => Ok(Value::Null),
        "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
        "tools/call" => dispatch_tool_call(params, capabilities, cx).await,
        other => Err(anyhow!("unknown MCP method: {other}")),
    }
}

#[derive(Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

async fn dispatch_tool_call(
    params: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let ToolCallParams { name, arguments } = serde_json::from_value(params)?;

    let payload: Value = match name.as_str() {
        "openFile" => tool_open_file(arguments, capabilities, cx).await?,
        "getCurrentSelection" => tool_current_selection(capabilities, cx).await?,
        "getLatestSelection" => tool_current_selection(capabilities, cx).await?,
        "getOpenEditors" => tool_open_editors(capabilities, cx).await?,
        "getWorkspaceFolders" => tool_workspace_folders(capabilities, cx).await?,
        "getDiagnostics" => tool_diagnostics(arguments, capabilities, cx).await?,
        "saveDocument" => tool_save_document(arguments, capabilities, cx).await?,
        "checkDocumentDirty" => tool_check_dirty(arguments, capabilities, cx).await?,
        "openDiff" => return Err(anyhow!(
            "openDiff is not implemented in this Zerminal build; the Accept/Reject UI is pending"
        )),
        "close_tab" | "closeAllDiffTabs" => json!({ "closed": 0 }),
        "executeCode" => return Err(anyhow!("executeCode is not supported by Zerminal")),
        other => return Err(anyhow!("unknown MCP tool: {other}")),
    };

    Ok(json!({
        "content": [{ "type": "text", "text": payload.to_string() }]
    }))
}

#[derive(Deserialize)]
struct OpenFileArgs {
    #[serde(rename = "filePath")]
    file_path: PathBuf,
    #[serde(default)]
    preview: bool,
}

async fn tool_open_file(
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let OpenFileArgs { file_path, preview } = serde_json::from_value(arguments)?;
    let task = cx.update(|cx| capabilities.open_file(Arc::from(file_path.as_path()), !preview, cx));
    task.await?;
    Ok(json!({ "ok": true }))
}

async fn tool_current_selection(
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let selection = cx.update(|cx| capabilities.current_selection(cx));
    Ok(match selection {
        None => json!({ "selection": null }),
        Some(selection) => {
            let text = selection
                .text
                .as_ref()
                .map(|text| text.to_string())
                .unwrap_or_default();
            json!({
                "selection": {
                    "filePath": selection.path.to_string_lossy(),
                    "start": { "line": selection.start.row, "character": selection.start.column },
                    "end": { "line": selection.end.row, "character": selection.end.column },
                    "text": text,
                }
            })
        }
    })
}

async fn tool_open_editors(
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let editors = cx.update(|cx| capabilities.list_open_editors(cx));
    Ok(json!({ "editors": editors.into_iter().map(open_editor_to_json).collect::<Vec<_>>() }))
}

fn open_editor_to_json(info: OpenEditorInfo) -> Value {
    json!({
        "filePath": info.path.to_string_lossy(),
        "isDirty": info.is_dirty,
        "isActive": info.is_active,
    })
}

async fn tool_workspace_folders(
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let folders = cx.update(|cx| capabilities.list_workspace_folders(cx));
    Ok(json!({
        "folders": folders.into_iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>()
    }))
}

#[derive(Deserialize)]
struct DiagnosticsArgs {
    #[serde(default)]
    uri: Option<String>,
}

async fn tool_diagnostics(
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let DiagnosticsArgs { uri } = serde_json::from_value(arguments).unwrap_or(DiagnosticsArgs { uri: None });
    let path = uri.and_then(|uri| {
        let stripped = uri.strip_prefix("file://").unwrap_or(&uri);
        Some(Arc::from(PathBuf::from(stripped).as_path()))
    });
    let diagnostics = cx.update(|cx| capabilities.get_diagnostics(path, cx));
    let entries: Vec<Value> = diagnostics
        .into_iter()
        .map(|diagnostic| {
            json!({
                "filePath": diagnostic.path.to_string_lossy(),
                "start": { "line": diagnostic.start.row, "character": diagnostic.start.column },
                "end": { "line": diagnostic.end.row, "character": diagnostic.end.column },
                "severity": format!("{:?}", diagnostic.severity),
                "message": diagnostic.message.to_string(),
                "source": diagnostic.source.as_ref().map(|source| source.to_string()),
            })
        })
        .collect();
    Ok(json!({ "diagnostics": entries }))
}

#[derive(Deserialize)]
struct PathArg {
    #[serde(rename = "filePath")]
    file_path: PathBuf,
}

async fn tool_save_document(
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let PathArg { file_path } = serde_json::from_value(arguments)?;
    let task = cx.update(|cx| capabilities.save_document(Arc::from(file_path.as_path()), cx));
    task.await?;
    Ok(json!({ "ok": true }))
}

async fn tool_check_dirty(
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let PathArg { file_path } = serde_json::from_value(arguments)?;
    let is_dirty = cx.update(|cx| capabilities.check_dirty(Arc::from(file_path.as_path()), cx));
    Ok(json!({ "isDirty": is_dirty }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_capabilities::{
        DiagnosticInfo, DiffDecision, EditorCapabilities, EditorSelection, OpenEditorInfo,
        SelectionCallback,
    };
    use futures::channel::oneshot;
    use gpui::{Subscription, TestAppContext};
    use language::Point;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    /// A trivial in-memory `EditorCapabilities` implementation that returns
    /// fixed synthetic data. Used to verify the MCP dispatcher routes calls
    /// correctly without needing a real workspace.
    struct MockCapabilities {
        workspace_root: Arc<Path>,
        selection: Option<EditorSelection>,
        open_editors: Vec<OpenEditorInfo>,
    }

    impl EditorCapabilities for MockCapabilities {
        fn list_workspace_folders(&self, _cx: &gpui::App) -> Vec<Arc<Path>> {
            vec![self.workspace_root.clone()]
        }
        fn list_open_editors(&self, _cx: &gpui::App) -> Vec<OpenEditorInfo> {
            self.open_editors.clone()
        }
        fn current_selection(&self, _cx: &gpui::App) -> Option<EditorSelection> {
            self.selection.clone()
        }
        fn open_file(
            &self,
            _path: Arc<Path>,
            _focus: bool,
            _cx: &mut gpui::App,
        ) -> gpui::Task<Result<()>> {
            gpui::Task::ready(Ok(()))
        }
        fn save_document(
            &self,
            _path: Arc<Path>,
            _cx: &mut gpui::App,
        ) -> gpui::Task<Result<()>> {
            gpui::Task::ready(Ok(()))
        }
        fn check_dirty(&self, _path: Arc<Path>, _cx: &gpui::App) -> bool {
            false
        }
        fn get_diagnostics(
            &self,
            _path: Option<Arc<Path>>,
            _cx: &gpui::App,
        ) -> Vec<DiagnosticInfo> {
            Vec::new()
        }
        fn open_diff_for_review(
            &self,
            _path: Arc<Path>,
            _old_text: String,
            _new_text: String,
            _cx: &mut gpui::App,
        ) -> gpui::Task<Result<DiffDecision>> {
            gpui::Task::ready(Err(anyhow!("not used in test")))
        }
        fn observe_selection(
            &self,
            _callback: SelectionCallback,
            _cx: &mut gpui::App,
        ) -> Subscription {
            unreachable!("observe_selection not exercised in dispatcher unit tests")
        }
    }

    fn editor_info(path: &str, is_dirty: bool, is_active: bool) -> OpenEditorInfo {
        OpenEditorInfo {
            path: Arc::from(Path::new(path)),
            is_dirty,
            is_active,
        }
    }

    fn build_mock() -> Arc<MockCapabilities> {
        Arc::new(MockCapabilities {
            workspace_root: Arc::from(Path::new("/tmp/zerminal-test")),
            selection: Some(EditorSelection {
                path: Arc::from(Path::new("/tmp/zerminal-test/src/main.rs")),
                start: Point::new(2, 4),
                end: Point::new(2, 10),
                text: Some("hello!".into()),
            }),
            open_editors: vec![
                editor_info("/tmp/zerminal-test/src/main.rs", false, true),
                editor_info("/tmp/zerminal-test/src/lib.rs", true, false),
            ],
        })
    }

    async fn call(sender: &McpCallSender, method: &str, params: Value) -> Result<Value> {
        let (respond_to, response) = oneshot::channel();
        sender
            .unbounded_send(McpCall {
                method: method.to_string(),
                params,
                respond_to,
            })
            .map_err(|err| anyhow!("dispatcher closed: {err}"))?;
        response
            .await
            .map_err(|err| anyhow!("dispatcher dropped response: {err}"))?
    }

    #[gpui::test]
    async fn initialize_returns_server_info(cx: &mut TestAppContext) {
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), cx));
        let response = call(&dispatcher.sender(), "initialize", Value::Null)
            .await
            .expect("initialize");
        assert_eq!(response["serverInfo"]["name"], "Zerminal");
    }

    #[gpui::test]
    async fn tools_list_includes_known_tools(cx: &mut TestAppContext) {
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), cx));
        let response = call(&dispatcher.sender(), "tools/list", Value::Null)
            .await
            .expect("tools/list");
        let tool_names: Vec<&str> = response["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(tool_names.contains(&"openFile"));
        assert!(tool_names.contains(&"getCurrentSelection"));
        assert!(tool_names.contains(&"getWorkspaceFolders"));
    }

    #[gpui::test]
    async fn tools_call_get_current_selection(cx: &mut TestAppContext) {
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), cx));
        let response = call(
            &dispatcher.sender(),
            "tools/call",
            json!({ "name": "getCurrentSelection" }),
        )
        .await
        .expect("tools/call");

        let inner_text = response["content"][0]["text"]
            .as_str()
            .expect("text content");
        let payload: Value = serde_json::from_str(inner_text).expect("inner json");
        assert_eq!(
            payload["selection"]["filePath"],
            "/tmp/zerminal-test/src/main.rs"
        );
        assert_eq!(payload["selection"]["text"], "hello!");
    }

    #[gpui::test]
    async fn tools_call_get_workspace_folders(cx: &mut TestAppContext) {
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), cx));
        let response = call(
            &dispatcher.sender(),
            "tools/call",
            json!({ "name": "getWorkspaceFolders" }),
        )
        .await
        .expect("tools/call");
        let payload: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().expect("text"))
                .expect("inner json");
        assert_eq!(payload["folders"][0], "/tmp/zerminal-test");
    }

    #[gpui::test]
    async fn unknown_tool_returns_error(cx: &mut TestAppContext) {
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), cx));
        let result = call(
            &dispatcher.sender(),
            "tools/call",
            json!({ "name": "doesNotExist" }),
        )
        .await;
        assert!(result.is_err(), "unknown tool should error");
    }

    #[gpui::test]
    async fn open_diff_returns_not_implemented_error(cx: &mut TestAppContext) {
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), cx));
        let result = call(
            &dispatcher.sender(),
            "tools/call",
            json!({
                "name": "openDiff",
                "arguments": {
                    "old_file_path": "/tmp/a.rs",
                    "new_file_path": "/tmp/a.rs",
                    "new_file_contents": "x",
                    "tab_name": "diff"
                }
            }),
        )
        .await;
        assert!(result.is_err(), "openDiff should error until UI lands");
    }
}

fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "openFile",
            "description": "Open a file in the editor.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string" },
                    "preview": { "type": "boolean", "default": false }
                },
                "required": ["filePath"]
            }
        }),
        json!({
            "name": "getCurrentSelection",
            "description": "Return the active editor's current selection.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "getLatestSelection",
            "description": "Alias for getCurrentSelection.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "getOpenEditors",
            "description": "List all open editor tabs across all panes.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "getWorkspaceFolders",
            "description": "List visible worktree roots.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "getDiagnostics",
            "description": "Return diagnostics for a file (or all open files if uri omitted).",
            "inputSchema": {
                "type": "object",
                "properties": { "uri": { "type": "string" } }
            }
        }),
        json!({
            "name": "saveDocument",
            "description": "Save a document to disk.",
            "inputSchema": {
                "type": "object",
                "properties": { "filePath": { "type": "string" } },
                "required": ["filePath"]
            }
        }),
        json!({
            "name": "checkDocumentDirty",
            "description": "Return whether the document has unsaved changes.",
            "inputSchema": {
                "type": "object",
                "properties": { "filePath": { "type": "string" } },
                "required": ["filePath"]
            }
        }),
    ]
}
