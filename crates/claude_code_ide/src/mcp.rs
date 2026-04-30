use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use editor_capabilities::{DiagnosticSeverity, DiffDecision, EditorCapabilities, OpenEditorInfo};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gpui::{App, AppContext as _, AsyncApp, Task};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::broadcaster::Broadcaster;

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
    pub fn spawn(
        capabilities: Arc<dyn EditorCapabilities>,
        broadcaster: Broadcaster,
        cx: &mut App,
    ) -> Self {
        let (sender, receiver) = unbounded();
        let task = cx.spawn(async move |cx| {
            run_dispatch_loop(receiver, capabilities, broadcaster, cx).await;
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
    broadcaster: Broadcaster,
    cx: &mut AsyncApp,
) {
    while let Some(call) = receiver.next().await {
        let McpCall {
            method,
            params,
            respond_to,
        } = call;
        log::info!("Claude /ide MCP call: method={method}");
        let result = match dispatch(&method, params, capabilities.clone(), cx).await {
            Ok(value) => Ok(value),
            Err(error) => {
                log::warn!("Claude /ide MCP {method} failed: {error:#}");
                Err(error)
            }
        };
        let _ = respond_to.send(result);
        // We push initial selection after `tools/list`, not after
        // `notifications/initialized`: Claude's selection_changed handler is
        // registered in a useEffect that fires after the connection-state
        // settle, so an earlier push lands before the handler is wired.
        if method == "tools/list" {
            push_initial_selection(capabilities.clone(), &broadcaster, cx).await;
        }
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

async fn push_initial_selection(
    capabilities: Arc<dyn EditorCapabilities>,
    broadcaster: &Broadcaster,
    cx: &mut AsyncApp,
) {
    let selection = cx.update(|cx| capabilities.current_selection(cx));
    broadcaster.send_selection_changed(selection.as_ref());
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
    log::info!("Claude /ide tools/call: tool={name}");

    // openDiff returns a multi-item content array (FILE_SAVED/TAB_CLOSED/DIFF_REJECTED),
    // unlike every other tool which folds to a single text content. Short-circuit
    // before the universal text wrap.
    if name == "openDiff" {
        return tool_open_diff(arguments, capabilities, cx).await;
    }

    let payload: Value = match name.as_str() {
        "openFile" => tool_open_file(arguments, capabilities, cx).await?,
        "getCurrentSelection" => tool_current_selection(capabilities, cx).await?,
        "getLatestSelection" => tool_current_selection(capabilities, cx).await?,
        "getOpenEditors" => tool_open_editors(capabilities, cx).await?,
        "getWorkspaceFolders" => tool_workspace_folders(capabilities, cx).await?,
        "getDiagnostics" => tool_diagnostics(arguments, capabilities, cx).await?,
        "checkDocumentDirty" => tool_check_dirty(arguments, capabilities, cx).await?,
        "close_tab" | "closeAllDiffTabs" => json!({ "closed": 0 }),
        // Writing to disk would bypass the user's normal save gesture and is
        // not behavior anyone explicitly asked Zerminal to perform on Claude's
        // behalf — return an error rather than silently saving.
        "saveDocument" => return Err(anyhow!("saveDocument is not supported by Zerminal")),
        "executeCode" => return Err(anyhow!("executeCode is not supported by Zerminal")),
        other => return Err(anyhow!("unknown MCP tool: {other}")),
    };

    Ok(json!({
        "content": [{ "type": "text", "text": payload.to_string() }]
    }))
}

#[derive(Deserialize)]
struct OpenDiffArgs {
    old_file_path: PathBuf,
    #[serde(default)]
    #[allow(dead_code)]
    new_file_path: Option<PathBuf>,
    new_file_contents: String,
    #[serde(default)]
    #[allow(dead_code)]
    tab_name: Option<String>,
}

async fn tool_open_diff(
    arguments: Value,
    capabilities: Arc<dyn EditorCapabilities>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let OpenDiffArgs {
        old_file_path,
        new_file_contents,
        ..
    } = serde_json::from_value(arguments)?;

    let read_path = old_file_path.clone();
    let read_task = cx.background_spawn(async move { std::fs::read_to_string(&read_path) });
    let old_text = match read_task.await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(anyhow!(
                "Claude /ide openDiff: failed to read {}: {error}",
                old_file_path.display()
            ));
        }
    };

    let path: Arc<std::path::Path> = Arc::from(old_file_path.as_path());
    let task = cx.update(|cx| {
        capabilities.open_diff_for_review(path, old_text, new_file_contents, cx)
    });
    let decision = task.await?;
    Ok(open_diff_response(decision))
}

fn open_diff_response(decision: DiffDecision) -> Value {
    let content: Vec<Value> = match decision {
        DiffDecision::Accept { final_text } => vec![
            json!({ "type": "text", "text": "FILE_SAVED" }),
            json!({ "type": "text", "text": final_text }),
        ],
        DiffDecision::Reject => vec![json!({ "type": "text", "text": "DIFF_REJECTED" })],
        DiffDecision::Cancelled => vec![json!({ "type": "text", "text": "TAB_CLOSED" })],
    };
    json!({ "content": content })
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
    let workspace_folders = cx.update(|cx| capabilities.list_workspace_folders(cx));
    if !path_is_within_workspace(&file_path, &workspace_folders) {
        log::warn!(
            "Claude /ide openFile: rejecting path outside any visible worktree: {}",
            file_path.display()
        );
        return Err(anyhow!(
            "openFile: path is outside any visible workspace folder: {}",
            file_path.display()
        ));
    }
    let task = cx.update(|cx| capabilities.open_file(Arc::from(file_path.as_path()), !preview, cx));
    task.await?;
    Ok(json!({ "ok": true }))
}

/// Defense-in-depth scope check for paths Claude asks us to act on. The
/// auth-token-protected loopback channel should be enough, but a leaked
/// token (or a future MCP-injected prompt that convinces Claude to call
/// `openFile("/etc/shadow")`) shouldn't be able to make Zerminal open
/// arbitrary files outside the user's project. Returns true when `path` is
/// equal to, or a descendant of, any visible worktree root.
fn path_is_within_workspace(path: &Path, workspace_folders: &[Arc<Path>]) -> bool {
    if workspace_folders.is_empty() {
        return false;
    }
    workspace_folders
        .iter()
        .any(|root| path.starts_with(root.as_ref()))
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
    let DiagnosticsArgs { uri } = serde_json::from_value(arguments)
        .context("parsing getDiagnostics arguments")?;
    let path: Option<Arc<Path>> = uri.as_ref().map(|raw| {
        let stripped = raw.strip_prefix("file://").unwrap_or(raw.as_str());
        if !stripped.starts_with('/') {
            log::warn!(
                "Claude /ide getDiagnostics: uri lacks file:// prefix or is not absolute: {raw}"
            );
        }
        Arc::from(PathBuf::from(stripped).as_path())
    });
    if let Some(target) = path.as_ref() {
        let workspace_folders = cx.update(|cx| capabilities.list_workspace_folders(cx));
        if !path_is_within_workspace(target, &workspace_folders) {
            log::warn!(
                "Claude /ide getDiagnostics: rejecting path outside any visible worktree: {}",
                target.display()
            );
            return Err(anyhow!(
                "getDiagnostics: path is outside any visible workspace folder: {}",
                target.display()
            ));
        }
    }
    let diagnostics = cx.update(|cx| capabilities.get_diagnostics(path, cx));

    let mut grouped: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
    for entry in diagnostics {
        let mut diag = json!({
            "range": {
                "start": { "line": entry.start.row, "character": entry.start.column },
                "end": { "line": entry.end.row, "character": entry.end.column },
            },
            "severity": severity_label(entry.severity),
            "message": entry.message.to_string(),
        });
        if let Some(map) = diag.as_object_mut() {
            if let Some(source) = entry.source.as_ref() {
                map.insert("source".into(), Value::String(source.to_string()));
            }
            if let Some(code) = entry.code.as_ref() {
                map.insert("code".into(), Value::String(code.to_string()));
            }
        }
        grouped
            .entry(entry.path.to_path_buf())
            .or_default()
            .push(diag);
    }
    let payload: Vec<Value> = grouped
        .into_iter()
        .map(|(path, diagnostics)| {
            json!({
                "uri": format!("file://{}", path.to_string_lossy()),
                "diagnostics": diagnostics,
            })
        })
        .collect();
    Ok(Value::Array(payload))
}

fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "Error",
        DiagnosticSeverity::Warning => "Warning",
        // Claude's getSeveritySymbol keys on "Info", not "Information".
        DiagnosticSeverity::Information => "Info",
        DiagnosticSeverity::Hint => "Hint",
    }
}

#[derive(Deserialize)]
struct PathArg {
    #[serde(rename = "filePath")]
    file_path: PathBuf,
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
        DiagnosticInfo, DiagnosticSeverity, DiffDecision, EditorCapabilities, EditorSelection,
        OpenEditorInfo, SelectionCallback,
    };
    use futures::channel::oneshot;
    use gpui::{Subscription, TestAppContext};
    use language::Point;
    use parking_lot::Mutex;
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
        diagnostics: Vec<DiagnosticInfo>,
        diff_decision: Mutex<Option<DiffDecision>>,
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
            path: Option<Arc<Path>>,
            _cx: &gpui::App,
        ) -> Vec<DiagnosticInfo> {
            self.diagnostics
                .iter()
                .filter(|entry| match path.as_deref() {
                    Some(target) => entry.path.as_ref() == target,
                    None => true,
                })
                .map(|entry| DiagnosticInfo {
                    path: entry.path.clone(),
                    start: entry.start,
                    end: entry.end,
                    severity: entry.severity,
                    message: entry.message.clone(),
                    source: entry.source.clone(),
                    code: entry.code.clone(),
                })
                .collect()
        }
        fn open_diff_for_review(
            &self,
            _path: Arc<Path>,
            _old_text: String,
            _new_text: String,
            _cx: &mut gpui::App,
        ) -> gpui::Task<Result<DiffDecision>> {
            match self.diff_decision.lock().take() {
                Some(decision) => gpui::Task::ready(Ok(decision)),
                None => gpui::Task::ready(Err(anyhow!("no diff decision configured"))),
            }
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
            diagnostics: Vec::new(),
            diff_decision: Mutex::new(None),
        })
    }

    fn build_mock_with_diagnostics(diagnostics: Vec<DiagnosticInfo>) -> Arc<MockCapabilities> {
        Arc::new(MockCapabilities {
            workspace_root: Arc::from(Path::new("/tmp/zerminal-test")),
            selection: None,
            open_editors: Vec::new(),
            diagnostics,
            diff_decision: Mutex::new(None),
        })
    }

    fn build_mock_with_diff_decision(decision: DiffDecision) -> Arc<MockCapabilities> {
        Arc::new(MockCapabilities {
            workspace_root: Arc::from(Path::new("/tmp/zerminal-test")),
            selection: None,
            open_editors: Vec::new(),
            diagnostics: Vec::new(),
            diff_decision: Mutex::new(Some(decision)),
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
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), Broadcaster::new(), cx));
        let response = call(&dispatcher.sender(), "initialize", Value::Null)
            .await
            .expect("initialize");
        assert_eq!(response["serverInfo"]["name"], "Zerminal");
    }

    #[gpui::test]
    async fn tools_list_includes_known_tools(cx: &mut TestAppContext) {
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), Broadcaster::new(), cx));
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
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), Broadcaster::new(), cx));
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
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), Broadcaster::new(), cx));
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
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(build_mock(), Broadcaster::new(), cx));
        let result = call(
            &dispatcher.sender(),
            "tools/call",
            json!({ "name": "doesNotExist" }),
        )
        .await;
        assert!(result.is_err(), "unknown tool should error");
    }

    fn diagnostic(
        path: &str,
        row: u32,
        col: u32,
        severity: DiagnosticSeverity,
        message: &str,
        source: Option<&str>,
        code: Option<&str>,
    ) -> DiagnosticInfo {
        DiagnosticInfo {
            path: Arc::from(Path::new(path)),
            start: Point::new(row, col),
            end: Point::new(row, col + 1),
            severity,
            message: message.to_owned().into(),
            source: source.map(|s| s.to_owned().into()),
            code: code.map(|c| c.to_owned().into()),
        }
    }

    #[gpui::test]
    async fn tools_call_get_diagnostics_groups_by_uri(cx: &mut TestAppContext) {
        let mock = build_mock_with_diagnostics(vec![
            diagnostic(
                "/tmp/zerminal-test/src/main.rs",
                3,
                7,
                DiagnosticSeverity::Error,
                "mismatched types",
                Some("rustc"),
                Some("E0308"),
            ),
            diagnostic(
                "/tmp/zerminal-test/src/main.rs",
                10,
                0,
                DiagnosticSeverity::Information,
                "unused import",
                Some("rustc"),
                None,
            ),
            diagnostic(
                "/tmp/zerminal-test/src/lib.rs",
                1,
                4,
                DiagnosticSeverity::Hint,
                "consider renaming",
                None,
                None,
            ),
        ]);

        let dispatcher = cx.update(|cx| McpDispatcher::spawn(mock, Broadcaster::new(), cx));
        let response = call(
            &dispatcher.sender(),
            "tools/call",
            json!({ "name": "getDiagnostics", "arguments": {} }),
        )
        .await
        .expect("tools/call");

        let inner_text = response["content"][0]["text"]
            .as_str()
            .expect("text content");
        let payload: Value = serde_json::from_str(inner_text).expect("inner json");
        let files = payload.as_array().expect("payload is array");
        assert_eq!(files.len(), 2, "expected one entry per file");

        let main_entry = files
            .iter()
            .find(|entry| entry["uri"] == "file:///tmp/zerminal-test/src/main.rs")
            .expect("main.rs grouped entry");
        let main_diags = main_entry["diagnostics"].as_array().expect("array");
        assert_eq!(main_diags.len(), 2);
        let first = &main_diags[0];
        assert_eq!(first["range"]["start"]["line"], 3);
        assert_eq!(first["range"]["start"]["character"], 7);
        assert_eq!(first["severity"], "Error");
        assert_eq!(first["message"], "mismatched types");
        assert_eq!(first["source"], "rustc");
        assert_eq!(first["code"], "E0308");

        let info_entry = &main_diags[1];
        assert_eq!(
            info_entry["severity"], "Info",
            "Information must be serialized as 'Info' (Claude getSeveritySymbol key)"
        );
        assert!(
            info_entry.get("code").is_none(),
            "code must be omitted when None, not emitted as null"
        );

        let lib_entry = files
            .iter()
            .find(|entry| entry["uri"] == "file:///tmp/zerminal-test/src/lib.rs")
            .expect("lib.rs grouped entry");
        let lib_diag = &lib_entry["diagnostics"][0];
        assert_eq!(lib_diag["severity"], "Hint");
        assert!(
            lib_diag.get("source").is_none(),
            "source must be omitted when None"
        );
    }

    #[gpui::test]
    async fn tools_call_get_diagnostics_filters_by_uri(cx: &mut TestAppContext) {
        let mock = build_mock_with_diagnostics(vec![
            diagnostic(
                "/tmp/zerminal-test/src/main.rs",
                0,
                0,
                DiagnosticSeverity::Warning,
                "main warning",
                None,
                None,
            ),
            diagnostic(
                "/tmp/zerminal-test/src/lib.rs",
                0,
                0,
                DiagnosticSeverity::Error,
                "lib error",
                None,
                None,
            ),
        ]);

        let dispatcher = cx.update(|cx| McpDispatcher::spawn(mock, Broadcaster::new(), cx));
        let response = call(
            &dispatcher.sender(),
            "tools/call",
            json!({
                "name": "getDiagnostics",
                "arguments": { "uri": "file:///tmp/zerminal-test/src/lib.rs" }
            }),
        )
        .await
        .expect("tools/call");

        let payload: Value = serde_json::from_str(
            response["content"][0]["text"].as_str().expect("text"),
        )
        .expect("inner json");
        let files = payload.as_array().expect("array");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["uri"], "file:///tmp/zerminal-test/src/lib.rs");
        assert_eq!(files[0]["diagnostics"][0]["message"], "lib error");
    }

    fn open_diff_args() -> Value {
        json!({
            "name": "openDiff",
            "arguments": {
                "old_file_path": "/tmp/zerminal-test/missing.rs",
                "new_file_path": "/tmp/zerminal-test/missing.rs",
                "new_file_contents": "fn main() { println!(\"hi\"); }",
                "tab_name": "diff"
            }
        })
    }

    #[gpui::test]
    async fn open_diff_accept_returns_file_saved(cx: &mut TestAppContext) {
        let mock = build_mock_with_diff_decision(DiffDecision::Accept {
            final_text: "fn main() { println!(\"edited\"); }".to_string(),
        });
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(mock, Broadcaster::new(), cx));
        let response = call(&dispatcher.sender(), "tools/call", open_diff_args())
            .await
            .expect("tools/call");
        let content = response["content"].as_array().expect("content array");
        assert_eq!(content.len(), 2, "Accept emits FILE_SAVED + text payload");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "FILE_SAVED");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "fn main() { println!(\"edited\"); }");
    }

    #[gpui::test]
    async fn open_diff_reject_returns_diff_rejected(cx: &mut TestAppContext) {
        let mock = build_mock_with_diff_decision(DiffDecision::Reject);
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(mock, Broadcaster::new(), cx));
        let response = call(&dispatcher.sender(), "tools/call", open_diff_args())
            .await
            .expect("tools/call");
        let content = response["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "DIFF_REJECTED");
    }

    #[gpui::test]
    async fn open_diff_cancelled_returns_tab_closed(cx: &mut TestAppContext) {
        let mock = build_mock_with_diff_decision(DiffDecision::Cancelled);
        let dispatcher = cx.update(|cx| McpDispatcher::spawn(mock, Broadcaster::new(), cx));
        let response = call(&dispatcher.sender(), "tools/call", open_diff_args())
            .await
            .expect("tools/call");
        let content = response["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "TAB_CLOSED");
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
