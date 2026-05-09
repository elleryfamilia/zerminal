//! JSON-RPC notification builder + fan-out for Copilot CLI's `/ide` channel.
//!
//! Two notifications:
//! - `selection_changed` — fires on cursor / selection movement in the active
//!   editor. Same JSON shape as `get_selection`'s reply.
//! - `diagnostics_changed` — fires when any file's diagnostics change.
//!   Carries the changed URIs along with the current diagnostics for each.
//!
//! Debouncing is the caller's responsibility. The attachment layer wraps each
//! `EditorCapabilities` observation with a 200ms debounce before invoking
//! `send_*` here, matching vscode-copilot-chat's convention.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_capabilities::{DiagnosticInfo, DiagnosticSeverity, EditorSelection};
use serde_json::{Value, json};

use crate::transport::SessionStore;

/// Builds Copilot-flavored JSON-RPC notifications and pushes them to every
/// session that currently has an attached SSE stream.
#[derive(Clone)]
pub struct Broadcaster {
    sessions: SessionStore,
}

impl Broadcaster {
    pub fn new(sessions: SessionStore) -> Self {
        Self { sessions }
    }

    /// Push `selection_changed` to every attached session. `None` represents
    /// "no active editor"; we still send a frame so the CLI knows the IDE is
    /// alive and just idle.
    pub fn send_selection_changed(&self, selection: Option<&EditorSelection>) {
        let params = match selection {
            Some(sel) => selection_params(sel),
            None => empty_selection_params(),
        };
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "selection_changed",
            "params": params,
        })
        .to_string();
        log::debug!(
            "Copilot /ide broadcaster: selection_changed (has_selection={})",
            selection.is_some()
        );
        self.sessions.broadcast(frame);
    }

    /// Push `diagnostics_changed` to every attached session. `entries`
    /// is the set of paths (with their current full diagnostics) that
    /// changed in this debounced batch.
    pub fn send_diagnostics_changed(&self, entries: Vec<(Arc<Path>, Vec<DiagnosticInfo>)>) {
        let uris: Vec<Value> = entries
            .into_iter()
            .map(|(path, diags)| diagnostics_entry(&path, diags))
            .collect();
        let uri_count = uris.len();
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "diagnostics_changed",
            "params": { "uris": uris },
        })
        .to_string();
        log::debug!("Copilot /ide broadcaster: diagnostics_changed (uri_count={uri_count})");
        self.sessions.broadcast(frame);
    }
}

fn selection_params(sel: &EditorSelection) -> Value {
    let path_str = sel.path.to_string_lossy().into_owned();
    let file_url = url::Url::from_file_path(&*sel.path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{path_str}"));
    let text = sel.text.as_ref().map(|t| t.to_string()).unwrap_or_default();
    let is_empty = sel.start == sel.end;
    json!({
        "text": text,
        "filePath": path_str,
        "fileUrl": file_url,
        "selection": {
            "start": { "line": sel.start.row, "character": sel.start.column },
            "end":   { "line": sel.end.row,   "character": sel.end.column   },
            "isEmpty": is_empty,
        }
    })
}

fn empty_selection_params() -> Value {
    json!({
        "text": "",
        "filePath": "",
        "fileUrl": "",
        "selection": {
            "start": { "line": 0, "character": 0 },
            "end":   { "line": 0, "character": 0 },
            "isEmpty": true,
        }
    })
}

fn diagnostics_entry(path: &Path, diagnostics: Vec<DiagnosticInfo>) -> Value {
    let path_str = path.to_string_lossy().into_owned();
    let file_url = url::Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{path_str}"));
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
    let diags: Vec<Value> = grouped.into_values().flatten().collect();
    json!({
        "uri": file_url,
        "filePath": path_str,
        "diagnostics": diags,
    })
}

fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Information => "information",
        DiagnosticSeverity::Hint => "hint",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use language::Point;
    use std::path::Path;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        smol::block_on(f)
    }

    #[test]
    fn selection_changed_emits_copilot_shape() {
        let sessions = SessionStore::new();
        sessions
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let receiver = sessions.try_attach_sse("s1").unwrap();
        let bcast = Broadcaster::new(sessions.clone());

        let sel = EditorSelection {
            path: Arc::from(Path::new("/tmp/x/main.rs")),
            start: Point::new(2, 4),
            end: Point::new(2, 10),
            text: Some("hello!".into()),
        };
        bcast.send_selection_changed(Some(&sel));

        let frame = block_on(async {
            smol::future::or(
                async { receiver.recv().await.ok() },
                async {
                    smol::Timer::after(std::time::Duration::from_millis(200)).await;
                    None
                },
            )
            .await
        })
        .expect("frame");
        let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "selection_changed");
        assert_eq!(value["params"]["text"], "hello!");
        assert_eq!(value["params"]["filePath"], "/tmp/x/main.rs");
        assert_eq!(value["params"]["fileUrl"], "file:///tmp/x/main.rs");
        assert_eq!(value["params"]["selection"]["start"]["line"], 2);
        assert_eq!(value["params"]["selection"]["isEmpty"], false);
    }

    #[test]
    fn selection_changed_with_none_emits_idle_frame() {
        let sessions = SessionStore::new();
        sessions
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let receiver = sessions.try_attach_sse("s1").unwrap();
        let bcast = Broadcaster::new(sessions);

        bcast.send_selection_changed(None);

        let frame = block_on(receiver.recv()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["params"]["text"], "");
        assert_eq!(value["params"]["filePath"], "");
        assert_eq!(value["params"]["selection"]["isEmpty"], true);
    }

    #[test]
    fn diagnostics_changed_groups_uris() {
        let sessions = SessionStore::new();
        sessions
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let receiver = sessions.try_attach_sse("s1").unwrap();
        let bcast = Broadcaster::new(sessions);

        let path: Arc<Path> = Arc::from(Path::new("/tmp/x/main.rs"));
        let diag = DiagnosticInfo {
            path: path.clone(),
            start: Point::new(0, 0),
            end: Point::new(0, 5),
            severity: DiagnosticSeverity::Error,
            message: "oops".into(),
            source: Some("rustc".into()),
            code: Some("E0001".into()),
        };
        bcast.send_diagnostics_changed(vec![(path, vec![diag])]);

        let frame = block_on(receiver.recv()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["method"], "diagnostics_changed");
        let uris = &value["params"]["uris"];
        assert!(uris.is_array());
        assert_eq!(uris[0]["filePath"], "/tmp/x/main.rs");
        assert_eq!(uris[0]["uri"], "file:///tmp/x/main.rs");
        assert_eq!(uris[0]["diagnostics"][0]["severity"], "error");
        assert_eq!(uris[0]["diagnostics"][0]["message"], "oops");
        assert_eq!(uris[0]["diagnostics"][0]["source"], "rustc");
        assert_eq!(uris[0]["diagnostics"][0]["code"], "E0001");
    }

    #[test]
    fn broadcast_does_not_panic_with_no_attached_sessions() {
        let sessions = SessionStore::new();
        let bcast = Broadcaster::new(sessions);
        bcast.send_selection_changed(None);
        bcast.send_diagnostics_changed(Vec::new());
    }

}
