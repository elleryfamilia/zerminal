//! MCP Streamable HTTP session bookkeeping.
//!
//! The CLI keeps a session id (`X-Copilot-Session-Id`) for the duration of
//! an `/ide` connection. Each session has:
//! - the negotiated MCP protocol version
//! - an optional SSE stream for server→client notifications (attached via GET
//!   `/mcp`, detached on stream drop, never split across multiple streams in
//!   v1 — second GET returns 409)
//! - bookkeeping headers (`X-Copilot-PID`, `X-Copilot-Parent-PID`)
//!
//! Empirical: real `@github/copilot` v1.0.44 supplies the session id itself
//! on initialize. We accept and use the client's id (no re-issue) and echo
//! it back in the response. If the client somehow doesn't send one, we fall
//! back to generating a UUID — purely defensive, the live CLI never hits
//! this path.

use std::collections::HashMap;
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use parking_lot::Mutex;

/// JSON-RPC notification frame (already serialized) destined for an attached
/// SSE stream.
pub type SseFrame = String;

#[derive(Debug)]
pub enum CreateError {
    /// Session id already in store. → HTTP 409 Conflict.
    AlreadyExists,
}

#[derive(Debug)]
pub enum AttachError {
    /// Session id not in store. → HTTP 404 Not Found.
    NotFound,
    /// Session already has an attached SSE stream. → HTTP 409 Conflict.
    AlreadyAttached,
}

#[derive(Default, Clone)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<String, SessionEntry>>>,
}

struct SessionEntry {
    protocol_version: String,
    sse_sender: Option<Sender<SseFrame>>,
    client_pid: Option<u32>,
    client_parent_pid: Option<u32>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a session keyed by `id`. Fails if `id` is already in the store
    /// (the CLI sent a duplicate `initialize` for an existing session).
    pub fn try_create(
        &self,
        id: String,
        protocol_version: String,
        client_pid: Option<u32>,
        client_parent_pid: Option<u32>,
    ) -> Result<(), CreateError> {
        let mut sessions = self.inner.lock();
        if sessions.contains_key(&id) {
            return Err(CreateError::AlreadyExists);
        }
        sessions.insert(
            id,
            SessionEntry {
                protocol_version,
                sse_sender: None,
                client_pid,
                client_parent_pid,
            },
        );
        Ok(())
    }

    pub fn exists(&self, id: &str) -> bool {
        self.inner.lock().contains_key(id)
    }

    /// Returns the protocol version negotiated at initialize, if the session
    /// exists.
    pub fn protocol_version(&self, id: &str) -> Option<String> {
        self.inner
            .lock()
            .get(id)
            .map(|e| e.protocol_version.clone())
    }

    pub fn client_pid(&self, id: &str) -> Option<u32> {
        self.inner.lock().get(id).and_then(|e| e.client_pid)
    }

    pub fn client_parent_pid(&self, id: &str) -> Option<u32> {
        self.inner.lock().get(id).and_then(|e| e.client_parent_pid)
    }

    /// Attach an SSE channel to the session. Returns a receiver the GET
    /// handler will drain to write SSE frames out the wire.
    ///
    /// Fails with `NotFound` if the session id doesn't exist (caller maps to
    /// 404), or `AlreadyAttached` if a stream is already live for this
    /// session (caller maps to 409 — v1 simplification, the spec allows
    /// multiple streams per session but we don't fan out yet).
    pub fn try_attach_sse(&self, id: &str) -> Result<Receiver<SseFrame>, AttachError> {
        let mut sessions = self.inner.lock();
        let entry = sessions.get_mut(id).ok_or(AttachError::NotFound)?;
        if entry.sse_sender.as_ref().is_some_and(|s| !s.is_closed()) {
            return Err(AttachError::AlreadyAttached);
        }
        let (sender, receiver) = async_channel::unbounded();
        entry.sse_sender = Some(sender);
        Ok(receiver)
    }

    /// Detach the SSE stream for `id` if any. Called when the GET handler
    /// detects the underlying socket has closed; broadcasts to this session
    /// will become no-ops until a new GET attaches.
    pub fn detach_sse(&self, id: &str) {
        if let Some(entry) = self.inner.lock().get_mut(id) {
            entry.sse_sender = None;
        }
    }

    /// Remove the session entirely. Closes any attached SSE channel via the
    /// sender's drop. Returns whether the session existed.
    pub fn delete(&self, id: &str) -> bool {
        self.inner.lock().remove(id).is_some()
    }

    /// Send `frame` to every session that currently has an attached SSE
    /// stream. Closed-channel sends are tolerated (the receiving GET handler
    /// has gone away; a follow-up `detach_sse` will clean state).
    pub fn broadcast(&self, frame: SseFrame) {
        let senders: Vec<Sender<SseFrame>> = self
            .inner
            .lock()
            .values()
            .filter_map(|e| e.sse_sender.clone())
            .collect();
        for sender in senders {
            // try_send so a slow consumer can never block the broadcaster.
            // Frame loss on full / closed channel is logged at the call site.
            let _ = sender.try_send(frame.clone());
        }
    }

    /// Send `frame` to one specific session. Returns whether the session was
    /// found and the send was queued (not whether the receiver got it).
    pub fn send_to(&self, id: &str, frame: SseFrame) -> bool {
        let sender = match self.inner.lock().get(id) {
            Some(entry) => entry.sse_sender.clone(),
            None => return false,
        };
        match sender {
            Some(s) => s.try_send(frame).is_ok(),
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        smol::block_on(f)
    }

    #[test]
    fn create_then_exists() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), Some(42), Some(7))
            .unwrap();
        assert!(store.exists("s1"));
        assert_eq!(store.protocol_version("s1").as_deref(), Some("2025-11-25"));
        assert_eq!(store.client_pid("s1"), Some(42));
        assert_eq!(store.client_parent_pid("s1"), Some(7));
    }

    #[test]
    fn duplicate_create_returns_already_exists() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let err = store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap_err();
        assert!(matches!(err, CreateError::AlreadyExists));
    }

    #[test]
    fn attach_sse_returns_receiver_for_known_session() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let receiver = store.try_attach_sse("s1").unwrap();
        store.send_to("s1", "hello".into());
        let got = block_on(receiver.recv()).unwrap();
        assert_eq!(got, "hello");
    }

    #[test]
    fn attach_sse_unknown_session_is_not_found() {
        let store = SessionStore::new();
        let err = store.try_attach_sse("nope").unwrap_err();
        assert!(matches!(err, AttachError::NotFound));
    }

    #[test]
    fn second_attach_is_already_attached() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let _first = store.try_attach_sse("s1").unwrap();
        let err = store.try_attach_sse("s1").unwrap_err();
        assert!(matches!(err, AttachError::AlreadyAttached));
    }

    #[test]
    fn detach_then_reattach_works() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let first = store.try_attach_sse("s1").unwrap();
        drop(first);
        store.detach_sse("s1");
        let second = store.try_attach_sse("s1").unwrap();
        store.send_to("s1", "after-reattach".into());
        let got = block_on(second.recv()).unwrap();
        assert_eq!(got, "after-reattach");
    }

    #[test]
    fn dropped_receiver_does_not_block_reattach_via_is_closed_check() {
        // Even without an explicit detach_sse call, the store recognizes a
        // closed channel and lets a second attach succeed. This protects
        // against missed cleanup hooks.
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let first = store.try_attach_sse("s1").unwrap();
        drop(first);
        let second = store.try_attach_sse("s1");
        assert!(second.is_ok());
    }

    #[test]
    fn delete_removes_session() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        assert!(store.delete("s1"));
        assert!(!store.exists("s1"));
        assert!(!store.delete("s1"), "second delete returns false");
    }

    #[test]
    fn delete_closes_attached_sse() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let receiver = store.try_attach_sse("s1").unwrap();
        store.delete("s1");
        // Sender was dropped along with the SessionEntry; receiver should
        // observe the close.
        let result = block_on(receiver.recv());
        assert!(result.is_err(), "receiver must error after sender drop");
    }

    #[test]
    fn broadcast_reaches_only_attached_sessions() {
        let store = SessionStore::new();
        store
            .try_create("attached".into(), "2025-11-25".into(), None, None)
            .unwrap();
        store
            .try_create("idle".into(), "2025-11-25".into(), None, None)
            .unwrap();
        let receiver = store.try_attach_sse("attached").unwrap();

        store.broadcast("ping".into());

        let got = block_on(receiver.recv()).unwrap();
        assert_eq!(got, "ping");
        // The "idle" session has no sender; the broadcast was a no-op for it.
        // We can't read from it, but we can confirm no sender slot was filled.
        let err = store.try_attach_sse("idle").unwrap();
        assert!(
            block_on(async {
                async_io_timeout(std::time::Duration::from_millis(20), err.recv()).await
            })
            .is_err(),
            "idle session must have no queued frames"
        );
    }

    #[test]
    fn send_to_unknown_session_returns_false() {
        let store = SessionStore::new();
        assert!(!store.send_to("ghost", "frame".into()));
    }

    #[test]
    fn protocol_version_after_delete_is_none() {
        let store = SessionStore::new();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        store.delete("s1");
        assert!(store.protocol_version("s1").is_none());
    }

    #[test]
    fn store_is_clone_shareable() {
        let store = SessionStore::new();
        let alias = store.clone();
        store
            .try_create("s1".into(), "2025-11-25".into(), None, None)
            .unwrap();
        assert!(alias.exists("s1"));
    }

    /// Tiny smol-based timeout helper so we don't need a separate workspace
    /// dep just for tests.
    async fn async_io_timeout<F, T>(timeout: std::time::Duration, fut: F) -> Result<T, ()>
    where
        F: std::future::Future<Output = T>,
    {
        use futures::FutureExt as _;
        let timer = smol::Timer::after(timeout);
        futures::select! {
            res = fut.fuse() => Ok(res),
            _ = timer.fuse() => Err(()),
        }
    }
}
