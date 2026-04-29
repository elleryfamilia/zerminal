use std::sync::Arc;

use editor_capabilities::EditorSelection;
use futures::channel::mpsc::UnboundedSender;
use parking_lot::Mutex;
use serde_json::json;

/// Fan-out channel for unsolicited JSON-RPC notifications pushed from the IDE
/// to all connected Claude `/ide` clients.
///
/// Claude's `/ide` integration relies on the editor PUSHING editor state;
/// listing read-side tools in `tools/list` is not enough to make Claude treat
/// the IDE as exposing editor state. The two notifications Claude consumes
/// today are `selection_changed` and `at_mentioned` (claudecode.nvim's
/// PROTOCOL.md). Today we only emit `selection_changed`.
#[derive(Clone)]
pub struct Broadcaster {
    subscribers: Arc<Mutex<Vec<UnboundedSender<String>>>>,
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Broadcaster {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a per-connection outgoing channel. Frames sent via [`broadcast`]
    /// are forwarded to every registered sender. Senders that fail are pruned
    /// on the next broadcast — there's no explicit unsubscribe.
    pub fn subscribe(&self, sender: UnboundedSender<String>) {
        self.subscribers.lock().push(sender);
    }

    fn broadcast_frame(&self, frame: String) {
        let mut subscribers = self.subscribers.lock();
        let before = subscribers.len();
        subscribers.retain(|sender| sender.unbounded_send(frame.clone()).is_ok());
        let after = subscribers.len();
        if before != after {
            log::debug!(
                "Claude /ide broadcaster pruned {} dead subscribers (now {})",
                before - after,
                after
            );
        }
    }

    /// Send a `selection_changed` notification to every connected client.
    /// `selection.is_none()` represents "no active editor / no selection" —
    /// emitted as an empty selection so Claude knows we're alive but idle.
    pub fn send_selection_changed(&self, selection: Option<&EditorSelection>) {
        let params = match selection {
            Some(sel) => {
                let path = sel.path.display().to_string();
                let url = format!("file://{path}");
                let text = sel.text.as_ref().map(|t| t.to_string()).unwrap_or_default();
                let is_empty = sel.start == sel.end;
                json!({
                    "text": text,
                    "filePath": path,
                    "fileUrl": url,
                    "selection": {
                        "start": { "line": sel.start.row, "character": sel.start.column },
                        "end": { "line": sel.end.row, "character": sel.end.column },
                        "isEmpty": is_empty,
                    }
                })
            }
            None => json!({
                "text": "",
                "filePath": "",
                "fileUrl": "",
                "selection": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 0 },
                    "isEmpty": true,
                }
            }),
        };
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "selection_changed",
            "params": params,
        })
        .to_string();
        log::info!(
            "Claude /ide broadcasting selection_changed (subscribers={}, has_selection={})",
            self.subscribers.lock().len(),
            selection.is_some()
        );
        self.broadcast_frame(frame);
    }
}
