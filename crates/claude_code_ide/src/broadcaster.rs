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
                // Empty-string encoding is load-bearing for the
                // `opened_file_in_ide` path: Claude's `xY8` builder fires when
                // `filePath && !text`, and JS treats `""` as falsy. If this is
                // ever changed to null-encode None, the file-hint path silently
                // breaks (synthetic terminal paths with no scrollback rely on
                // this).
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

#[cfg(test)]
mod tests {
    use super::*;
    use editor_capabilities::EditorSelection;
    use futures::StreamExt as _;
    use language::Point;
    use std::path::Path;
    use std::sync::Arc as StdArc;

    /// Drains one frame from a freshly-subscribed broadcaster after firing
    /// `f`, parsed as JSON.
    fn drain_one(f: impl FnOnce(&Broadcaster)) -> serde_json::Value {
        let broadcaster = Broadcaster::new();
        let (sender, receiver) = futures::channel::mpsc::unbounded::<String>();
        broadcaster.subscribe(sender);
        f(&broadcaster);
        let frame = futures::executor::block_on(async {
            let mut receiver = receiver;
            receiver.next().await.expect("frame")
        });
        serde_json::from_str(&frame).expect("valid JSON-RPC frame")
    }

    #[test]
    fn selection_changed_with_text_emits_filled_params() {
        let value = drain_one(|b| {
            let selection = EditorSelection {
                path: StdArc::from(Path::new("/tmp/zerminal-test/src/main.rs")),
                start: Point::new(2, 4),
                end: Point::new(2, 10),
                text: Some("hello!".into()),
            };
            b.send_selection_changed(Some(&selection));
        });

        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "selection_changed");
        let params = &value["params"];
        assert_eq!(params["text"], "hello!");
        assert_eq!(params["filePath"], "/tmp/zerminal-test/src/main.rs");
        assert_eq!(params["fileUrl"], "file:///tmp/zerminal-test/src/main.rs");
        assert_eq!(params["selection"]["start"]["line"], 2);
        assert_eq!(params["selection"]["start"]["character"], 4);
        assert_eq!(params["selection"]["end"]["line"], 2);
        assert_eq!(params["selection"]["end"]["character"], 10);
        assert_eq!(params["selection"]["isEmpty"], false);
    }

    #[test]
    fn selection_changed_with_empty_text_uses_empty_string_for_xy8() {
        // Hint-only path (terminal focus, or editor cursor with no
        // selection): Claude's `xY8` builder fires `opened_file_in_ide`
        // when `filePath && !text`. JS treats `""` as falsy, so the empty
        // string IS the right encoding — must not become `null` or be
        // omitted.
        let value = drain_one(|b| {
            let selection = EditorSelection {
                path: StdArc::from(Path::new("/tmp/zerminal-test/Terminal")),
                start: Point::new(0, 0),
                end: Point::new(0, 0),
                text: None,
            };
            b.send_selection_changed(Some(&selection));
        });

        let params = &value["params"];
        assert_eq!(params["text"], "");
        assert!(
            params.get("text").is_some(),
            "text key must be present for the xY8 builder to evaluate filePath && !text"
        );
        assert_eq!(params["filePath"], "/tmp/zerminal-test/Terminal");
        assert_eq!(params["selection"]["isEmpty"], true);
    }

    #[test]
    fn selection_changed_with_none_emits_idle_frame() {
        let value = drain_one(|b| b.send_selection_changed(None));
        let params = &value["params"];
        assert_eq!(params["text"], "");
        assert_eq!(params["filePath"], "");
        assert_eq!(params["selection"]["isEmpty"], true);
    }
}
