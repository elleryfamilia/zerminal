//! Per-call routing from an MCP `tools/call` back to the terminal that
//! spawned the connecting Copilot CLI.
//!
//! The flow is `mcp-session-id → x-copilot-pid → ancestor-walk → registered
//! terminal EntityId`. Tools like `update_session_name` and `close_diff`
//! need to know which terminal a given call originated from so they target
//! the right tab — the protocol's session id alone is opaque, and the CLI
//! itself has no concept of "which IDE terminal am I in."
//!
//! ## Why a process-tree walk
//!
//! `SessionStore` records the CLI's `x-copilot-pid` (the `copilot` process
//! itself) and `x-copilot-parent-pid` (typically the spawning shell). We
//! register terminals against the *PTY child PID* (the shell that the PTY
//! spawned). For the common `zsh -c "copilot ..."` case, the CLI's parent
//! is the shell, which is the registered PID — one hop. Activation-script
//! chains, function wrappers, or future Copilot versions that fork an MCP
//! worker push the registered ancestor a few hops up. We walk
//! [`ProcessTree::parent_of`] until we find a registered PID, hit init, or
//! exceed [`MAX_PARENT_HOPS`].
//!
//! ## Foreground-only
//!
//! The router holds a `RefCell<HashMap<u32, EntityId>>` and is consumed by
//! the GPUI foreground dispatcher (`run_tool` in [`crate::mcp`]). It is not
//! `Send` or `Sync` — neither trait nor impl carries those bounds. Holders
//! pass it via `Rc`, never `Arc`.
//!
//! ## Not a security boundary
//!
//! Any local user-process can craft a request to the lockfile-published
//! Unix socket; the lockfile nonce is the only auth gate. PID-based routing
//! is a UX-disambiguation aid for the legitimate, rendezvoused-via-the-same-
//! lockfile case. Don't treat a "found ancestor" answer as proof the caller
//! is the legitimate Copilot CLI.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{App, EntityId};

use crate::transport::SessionStore;

/// Maximum process-tree depth the router walks looking for a registered
/// ancestor. Realistic chains are 1-3 hops; 16 is generous slack for
/// activation-script wrappers and language-shim layers without becoming a
/// runaway walk on a deep launchd-managed tree.
pub const MAX_PARENT_HOPS: u8 = 16;

/// Process-tree abstraction so the router can be tested without booting
/// `sysinfo`. Real impl is [`SysinfoProcessTree`].
pub trait ProcessTree: 'static {
    /// Returns the parent PID of `pid`, refreshing the underlying snapshot
    /// as needed. Returns `None` if the process is unknown to the OS, has
    /// no parent (init / launchd), or is invalid.
    fn parent_of(&mut self, pid: u32) -> Option<u32>;
}

/// Routing surface the dispatcher calls to map a tool-call back to the
/// originating terminal. Implementations are foreground-only.
pub trait TerminalRouter: 'static {
    /// Resolve the spawning terminal's `EntityId` for the given MCP session
    /// id, if known. Returns `None` if the session id is unknown, no
    /// ancestor PID is registered, or the walk hits init / max-hops.
    fn terminal_for_session(&self, session_id: &str, cx: &App) -> Option<EntityId>;
}

pub struct CopilotTerminalRouter {
    sessions: SessionStore,
    by_pty_child_pid: RefCell<HashMap<u32, EntityId>>,
    process_tree: RefCell<Box<dyn ProcessTree>>,
}

impl CopilotTerminalRouter {
    pub fn new(sessions: SessionStore) -> Self {
        Self::with_process_tree(sessions, Box::new(SysinfoProcessTree::new()))
    }

    /// Test seam: build the router with a deterministic [`ProcessTree`]
    /// stand-in instead of the real `sysinfo`-backed walker.
    pub fn with_process_tree(sessions: SessionStore, tree: Box<dyn ProcessTree>) -> Self {
        Self {
            sessions,
            by_pty_child_pid: RefCell::new(HashMap::new()),
            process_tree: RefCell::new(tree),
        }
    }

    /// Register `pid` as the PTY-child PID owned by `entity_id` (a terminal
    /// view). Overwrites any prior entry for the same PID — the OS may have
    /// reissued a recently-freed PID, in which case the old terminal is
    /// already gone and we want the new mapping to win.
    pub fn register(&self, pid: u32, entity_id: EntityId) {
        self.by_pty_child_pid.borrow_mut().insert(pid, entity_id);
    }

    /// Remove `pid`'s mapping only if it still points at `entity_id`. Guards
    /// against this sequence: terminal A (pid 4242) closes → OS reissues
    /// 4242 to a new terminal B → B registers (pid 4242, B's id) → A's
    /// CloseTerminal subscription finally fires and calls `unregister(4242,
    /// A's id)`. Without this verification we'd evict B's correct mapping.
    pub fn unregister(&self, pid: u32, entity_id: EntityId) {
        let mut map = self.by_pty_child_pid.borrow_mut();
        if map.get(&pid) == Some(&entity_id) {
            map.remove(&pid);
        }
    }

    fn walk_to_registered(&self, start_pid: u32) -> Option<EntityId> {
        let map = self.by_pty_child_pid.borrow();
        let mut tree = self.process_tree.borrow_mut();
        let mut current = start_pid;
        for _ in 0..MAX_PARENT_HOPS {
            if let Some(&entity_id) = map.get(&current) {
                return Some(entity_id);
            }
            match tree.parent_of(current) {
                // pid 1 (init / launchd) means we've crossed out of the
                // user's session tree; stop.
                Some(parent) if parent > 1 && parent != current => current = parent,
                _ => return None,
            }
        }
        log::warn!(
            "Copilot /ide router: parent walk exceeded {MAX_PARENT_HOPS} hops from start_pid={start_pid}"
        );
        None
    }
}

impl TerminalRouter for CopilotTerminalRouter {
    fn terminal_for_session(&self, session_id: &str, _cx: &App) -> Option<EntityId> {
        // Start from the CLI's own PID. If a future Copilot version registers
        // its own PID directly (which would need explicit IDE-side support
        // anyway), zero hops finds it. The common case is one hop up — to
        // the spawning shell, which IS the PTY-child PID we registered.
        let start = self.sessions.client_pid(session_id)?;
        self.walk_to_registered(start)
    }
}

// ---------------------------------------------------------------------------
// Real ProcessTree backed by `sysinfo`.
// ---------------------------------------------------------------------------

/// `sysinfo`-backed [`ProcessTree`]. Holds one `System` snapshot and refreshes
/// just the PIDs it walks rather than the whole process table — full refreshes
/// are ~10ms on a busy machine, which adds up on every `tools/call`.
pub struct SysinfoProcessTree {
    system: sysinfo::System,
    /// We only need pid + parent fields, not cmd/cwd/exe — `nothing()` is
    /// the cheapest refresh kind that still populates `parent()`.
    refresh_kind: sysinfo::ProcessRefreshKind,
}

impl SysinfoProcessTree {
    pub fn new() -> Self {
        let refresh_kind = sysinfo::ProcessRefreshKind::nothing();
        let system = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::nothing().with_processes(refresh_kind),
        );
        Self {
            system,
            refresh_kind,
        }
    }
}

impl Default for SysinfoProcessTree {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTree for SysinfoProcessTree {
    fn parent_of(&mut self, pid: u32) -> Option<u32> {
        let pid_obj = sysinfo::Pid::from_u32(pid);
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid_obj]),
            true,
            self.refresh_kind,
        );
        self.system
            .process(pid_obj)
            .and_then(|p| p.parent())
            .map(|p| p.as_u32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic [`ProcessTree`] built from a fixed `child_pid -> parent_pid`
    /// map. Tests register terminals at known PIDs and seed this map to make
    /// the walk follow a chosen chain.
    struct MockProcessTree {
        parents: HashMap<u32, u32>,
    }

    impl MockProcessTree {
        fn new(parents: HashMap<u32, u32>) -> Self {
            Self { parents }
        }
    }

    impl ProcessTree for MockProcessTree {
        fn parent_of(&mut self, pid: u32) -> Option<u32> {
            self.parents.get(&pid).copied()
        }
    }

    fn entity_id(raw: u64) -> EntityId {
        EntityId::from(raw)
    }

    fn router_with_chain(parents: Vec<(u32, u32)>) -> CopilotTerminalRouter {
        let parents: HashMap<_, _> = parents.into_iter().collect();
        CopilotTerminalRouter::with_process_tree(
            SessionStore::new(),
            Box::new(MockProcessTree::new(parents)),
        )
    }

    fn create_session(store: &SessionStore, id: &str, client_pid: u32) {
        store
            .try_create(id.into(), "2025-11-25".into(), Some(client_pid), None)
            .expect("create session");
    }

    #[test]
    fn register_then_lookup_via_walk_succeeds() {
        let router = router_with_chain(vec![
            (1234, 5678), // copilot pid → shell pid
            (5678, 4242), // shell pid → pty-child pid (registered)
        ]);
        router.register(4242, entity_id(7));
        create_session(&router.sessions, "sid-A", 1234);
        let start = router.sessions.client_pid("sid-A").expect("session");
        assert_eq!(router.walk_to_registered(start), Some(entity_id(7)));
    }

    #[test]
    fn lookup_at_zero_hops_returns_immediately() {
        // Pathological case: client_pid IS the registered terminal pid.
        let router = router_with_chain(vec![]);
        router.register(9999, entity_id(3));
        create_session(&router.sessions, "sid-self", 9999);
        let start = router.sessions.client_pid("sid-self").expect("session");
        assert_eq!(router.walk_to_registered(start), Some(entity_id(3)));
    }

    #[test]
    fn unregister_with_matching_entity_removes() {
        let router = router_with_chain(vec![]);
        router.register(100, entity_id(1));
        router.unregister(100, entity_id(1));
        assert!(router.by_pty_child_pid.borrow().is_empty());
    }

    #[test]
    fn unregister_with_mismatched_entity_keeps_entry() {
        // Simulates the close-after-PID-reuse race: terminal A's late
        // CloseTerminal handler must not evict terminal B's mapping when B
        // happened to grab A's PID.
        let router = router_with_chain(vec![]);
        router.register(100, entity_id(99)); // B registered pid=100 → B
        router.unregister(100, entity_id(7)); // A's late unregister with stale id
        assert_eq!(
            router.by_pty_child_pid.borrow().get(&100),
            Some(&entity_id(99))
        );
    }

    #[test]
    fn unknown_session_returns_none() {
        let router = router_with_chain(vec![]);
        // Unknown session id has no client_pid → no walk to perform.
        assert!(router.sessions.client_pid("ghost").is_none());
    }

    #[test]
    fn walk_terminates_at_pid_1() {
        let router = router_with_chain(vec![(50, 25), (25, 1)]);
        create_session(&router.sessions, "sid-orphan", 50);
        let start = router.sessions.client_pid("sid-orphan").expect("session");
        assert_eq!(router.walk_to_registered(start), None);
    }

    #[test]
    fn walk_terminates_at_max_hops() {
        let mut chain = Vec::new();
        for i in 0..(MAX_PARENT_HOPS as u32 + 5) {
            chain.push((i + 100, i + 101));
        }
        let registered_pid = 100 + MAX_PARENT_HOPS as u32 + 4;
        let router = router_with_chain(chain);
        router.register(registered_pid, entity_id(42));
        create_session(&router.sessions, "sid-deep", 100);
        let start = router.sessions.client_pid("sid-deep").expect("session");
        // Walk hits the hop cap before reaching `registered_pid`.
        assert_eq!(router.walk_to_registered(start), None);
    }

    #[test]
    fn walk_terminates_on_self_parent() {
        // Defensive: `parent == current` would otherwise loop forever.
        let router = router_with_chain(vec![(7, 7)]);
        create_session(&router.sessions, "sid-loop", 7);
        let start = router.sessions.client_pid("sid-loop").expect("session");
        assert_eq!(router.walk_to_registered(start), None);
    }
}
