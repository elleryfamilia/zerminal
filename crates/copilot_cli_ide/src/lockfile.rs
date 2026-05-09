use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// JSON payload Copilot CLI reads to discover running IDE servers. Field
/// names mirror the protocol contract in microsoft/vscode-copilot-chat
/// (file `src/extension/chatSessions/copilotcli/vscode-node/lockFile.ts`).
/// Do not rename without verifying the CLI bundle still parses the new shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub socket_path: String,
    /// Always `"unix"` — this crate is `#[cfg(unix)]`. The `"pipe"` value
    /// reserved by vscode-copilot-chat for Windows named pipes is not produced
    /// here; that path lands when Zerminal ships on Windows.
    pub scheme: String,
    /// Headers the CLI must send with every request. We populate this with
    /// the single `Authorization: Nonce <uuid>` header that gates the
    /// loopback channel.
    pub headers: BTreeMap<String, String>,
    pub pid: u32,
    pub ide_name: String,
    /// Unix epoch milliseconds (`Date.now()` equivalent).
    pub timestamp: u64,
    pub workspace_folders: Vec<PathBuf>,
    pub is_trusted: bool,
}

impl Lockfile {
    pub fn new(socket_path: String, nonce: &str, workspace_folders: Vec<PathBuf>) -> Self {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), format!("Nonce {nonce}"));
        Self {
            socket_path,
            scheme: "unix".to_string(),
            headers,
            pid: std::process::id(),
            ide_name: "Zerminal".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            workspace_folders,
            // No workspace-trust concept in Zerminal yet. Defensible v1: always
            // true. The auth-token-protected loopback channel is the real gate;
            // `isTrusted` is a feature-gate hint the CLI may surface to the
            // model, not a security boundary.
            is_trusted: true,
        }
    }
}

/// Resolves the Copilot CLI state directory and ensures it exists with
/// mode 0o700. Mirrors `getCopilotCliStateDir` in vscode-copilot-chat:
/// honors `XDG_STATE_HOME` if set, otherwise falls back to `~/.copilot/ide`.
/// (Note: this is `XDG_STATE_HOME`-only — `COPILOT_HOME` is **not** honored
/// by the IDE side of the protocol; verified against `@github/copilot`
/// v0.0.374 bundle.)
pub fn copilot_state_dir() -> Result<PathBuf> {
    let dir = if let Some(xdg_state) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(xdg_state).join(".copilot").join("ide")
    } else {
        let home = dirs::home_dir().context("could not resolve home directory")?;
        home.join(".copilot").join("ide")
    };
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating Copilot lockfile directory {}", dir.display()))?;
    use std::os::unix::fs::PermissionsExt as _;
    // 0o700: lockfiles contain auth nonces. Match vscode-copilot-chat.
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    Ok(dir)
}

/// Atomically writes a lockfile under `dir/<uuid>.lock` via temp + rename.
/// Returns a guard that unlinks on drop.
///
/// On Unix the file is restricted to mode 0o600 *before* the rename. The
/// file's `headers` field carries the auth nonce; default umask would yield
/// 0o644, exposing the credential to every other local user — they could
/// dial the Unix socket and impersonate the CLI.
pub fn write_atomic(dir: &Path, lockfile: &Lockfile) -> Result<LockfileGuard> {
    let uuid = Uuid::new_v4();
    let final_path = dir.join(format!("{uuid}.lock"));
    let mut tempfile = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating temp lockfile in {}", dir.display()))?;
    let json = serde_json::to_vec_pretty(lockfile).context("serializing lockfile")?;
    tempfile.write_all(&json).context("writing temp lockfile")?;
    use std::os::unix::fs::PermissionsExt as _;
    let permissions = fs::Permissions::from_mode(0o600);
    tempfile
        .as_file()
        .set_permissions(permissions)
        .with_context(|| {
            format!(
                "restricting permissions on temp lockfile in {}",
                dir.display()
            )
        })?;
    tempfile
        .persist(&final_path)
        .map_err(|err| anyhow!("renaming lockfile to {}: {}", final_path.display(), err.error))?;
    Ok(LockfileGuard { path: final_path })
}

/// Holds an exclusive claim on a Copilot lockfile. Drop unlinks the file.
/// Failures during unlink are logged but not propagated — the file may have
/// been removed by a concurrent sweep on another process startup.
pub struct LockfileGuard {
    path: PathBuf,
}

impl LockfileGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockfileGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "failed to remove Copilot /ide lockfile {}: {}",
                    self.path.display(),
                    error
                );
            }
        }
    }
}

/// Scan `dir` for `*.lock` files, parse each, and remove entries whose owner
/// process is no longer running (probe via `kill(pid, 0)`). Returns the paths
/// that were removed. Errors on individual entries are logged and skipped —
/// a parse failure or fs error on one file should not block sweeping the rest.
pub fn sweep_stale(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!("skipping unreadable Copilot lockfile entry: {error}");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
            continue;
        }
        let pid = match fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str::<Lockfile>(&content).ok())
        {
            Some(parsed) => parsed.pid,
            None => {
                log::debug!("unparseable Copilot lockfile {}, skipping", path.display());
                continue;
            }
        };
        if is_process_alive(pid) {
            continue;
        }
        if let Err(error) = fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "failed to remove stale Copilot lockfile {}: {}",
                    path.display(),
                    error
                );
            }
            continue;
        }
        removed.push(path);
    }
    Ok(removed)
}

fn is_process_alive(pid: u32) -> bool {
    // `kill(0, 0)` and `kill(-1, 0)` are POSIX-special (process group / all
    // processes); only positive pids that fit in `pid_t` are real probes.
    // Anything else we treat as dead so the lockfile is swept.
    let pid_t: libc::pid_t = match libc::pid_t::try_from(pid) {
        Ok(p) if p > 0 => p,
        _ => return false,
    };
    // SAFETY: `kill(2)` with `sig=0` is a side-effect-free probe of process
    // existence and signal permission. EPERM means the process exists but
    // we don't own it — it's still alive for our purposes.
    let result = unsafe { libc::kill(pid_t, 0) };
    if result == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_round_trips_through_camel_case_json() {
        let lockfile = Lockfile::new(
            "/tmp/x/sock".to_string(),
            "abcd-nonce",
            vec![PathBuf::from("/tmp/foo")],
        );
        let serialized = serde_json::to_string(&lockfile).expect("serialize lockfile");
        // Field naming must match the Copilot CLI / vscode-copilot-chat schema.
        assert!(serialized.contains("\"socketPath\""));
        assert!(serialized.contains("\"scheme\""));
        assert!(serialized.contains("\"workspaceFolders\""));
        assert!(serialized.contains("\"ideName\":\"Zerminal\""));
        assert!(serialized.contains("\"isTrusted\":true"));
        // The Authorization header value must use the "Nonce <token>" form.
        assert!(serialized.contains("\"Nonce abcd-nonce\""));
        let parsed: Lockfile = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(parsed.socket_path, "/tmp/x/sock");
        assert_eq!(parsed.workspace_folders, lockfile.workspace_folders);
        assert_eq!(
            parsed.headers.get("Authorization").map(String::as_str),
            Some("Nonce abcd-nonce")
        );
    }

    #[test]
    fn write_atomic_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("create tempdir");
        let lockfile = Lockfile::new(
            "/tmp/x/sock".to_string(),
            "perm-test",
            vec![PathBuf::from("/tmp/foo")],
        );
        let guard = write_atomic(dir.path(), &lockfile).expect("write lockfile");
        let mode = fs::metadata(guard.path())
            .expect("stat lockfile")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "lockfile must not be readable by other users");
    }

    #[test]
    fn guard_drop_unlinks_lockfile() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let lockfile = Lockfile::new(
            "/tmp/sock".to_string(),
            "drop-test",
            Vec::new(),
        );
        let path = {
            let guard = write_atomic(dir.path(), &lockfile).expect("write lockfile");
            assert!(guard.path().exists(), "lockfile must exist while guard is live");
            guard.path().to_path_buf()
        };
        assert!(!path.exists(), "lockfile must be removed when guard drops");
    }

    #[test]
    fn sweep_removes_dead_pid_entries_keeps_live_ones() {
        let dir = tempfile::tempdir().expect("create tempdir");

        // Live entry: this process's own pid.
        let live = Lockfile::new(
            "/tmp/live/sock".to_string(),
            "live",
            vec![PathBuf::from("/tmp/foo")],
        );
        let live_guard = write_atomic(dir.path(), &live).expect("write live lockfile");

        // Dead entry: spawn a child, wait for it to exit. The reaped pid is
        // genuinely dead until the OS reuses it (negligible odds in this test
        // window on a healthy system). u32::MAX-style sentinels don't work
        // because `pid_t` is `i32` — they wrap to `-1`, which has POSIX
        // special semantics in `kill(2)`.
        let dead_pid = {
            let mut child = std::process::Command::new("true")
                .spawn()
                .expect("spawn `true`");
            let pid = child.id();
            child.wait().expect("wait on `true`");
            pid
        };
        let dead = Lockfile {
            pid: dead_pid,
            ..Lockfile::new(
                "/tmp/dead/sock".to_string(),
                "dead",
                vec![PathBuf::from("/tmp/foo")],
            )
        };
        let dead_path = dir.path().join("dead.lock");
        fs::write(&dead_path, serde_json::to_vec(&dead).expect("serialize"))
            .expect("write dead lockfile");

        let removed = sweep_stale(dir.path()).expect("sweep");
        assert!(
            removed.iter().any(|p| p == &dead_path),
            "dead lockfile must be removed; got removed={removed:?}"
        );
        assert!(
            !removed.iter().any(|p| p == live_guard.path()),
            "live lockfile must be kept; got removed={removed:?}"
        );
        assert!(live_guard.path().exists(), "live lockfile still on disk");
        assert!(!dead_path.exists(), "dead lockfile removed from disk");
    }

    #[test]
    fn sweep_skips_non_lock_files() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let unrelated = dir.path().join("notes.txt");
        fs::write(&unrelated, b"hello").expect("write unrelated file");

        let removed = sweep_stale(dir.path()).expect("sweep");
        assert!(removed.is_empty(), "non-.lock files must be ignored");
        assert!(unrelated.exists(), "non-.lock files must be left alone");
    }
}
