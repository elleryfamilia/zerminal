use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};

/// JSON payload Claude CLI reads from `~/.claude/ide/<port>.lock` to discover
/// running editor IDE servers. Field names mirror the Claude CLI's expectations
/// — do not rename without updating Claude side.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub pid: u32,
    pub workspace_folders: Vec<PathBuf>,
    pub ide_name: String,
    pub transport: String,
    pub auth_token: String,
    #[serde(default)]
    pub running_in_windows: bool,
}

impl Lockfile {
    pub fn new(workspace_folders: Vec<PathBuf>, auth_token: String) -> Self {
        Self {
            pid: std::process::id(),
            workspace_folders,
            ide_name: "Zerminal".to_string(),
            transport: "ws".to_string(),
            auth_token,
            running_in_windows: cfg!(target_os = "windows"),
        }
    }
}

/// Returns `<home>/.claude/ide`. Creates the directory if it doesn't exist.
pub fn lockfile_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    let dir = home.join(".claude").join("ide");
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating lockfile directory {}", dir.display()))?;
    Ok(dir)
}

/// Atomically writes the lockfile via temp + rename. Returns a guard that
/// unlinks the file on drop so an attachment cleanup always cleans up.
///
/// The file contains the WebSocket auth token, so on Unix we restrict
/// permissions to `0o600` BEFORE the rename. Default umask would yield `0o644`,
/// which would expose the bearer credential to every other local user — they
/// could dial loopback and impersonate Claude over the `/ide` channel. Set the
/// mode while we still have an exclusive handle to the temp file.
pub fn write_atomic(port: u16, lockfile: &Lockfile) -> Result<LockfileGuard> {
    let dir = lockfile_dir()?;
    let final_path = dir.join(format!("{port}.lock"));
    let mut tempfile = tempfile::NamedTempFile::new_in(&dir)
        .with_context(|| format!("creating temp lockfile in {}", dir.display()))?;
    let json = serde_json::to_vec(lockfile).context("serializing lockfile")?;
    tempfile
        .write_all(&json)
        .context("writing temp lockfile")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let permissions = std::fs::Permissions::from_mode(0o600);
        tempfile
            .as_file()
            .set_permissions(permissions)
            .with_context(|| {
                format!(
                    "restricting permissions on temp lockfile in {}",
                    dir.display()
                )
            })?;
    }
    tempfile
        .persist(&final_path)
        .map_err(|err| anyhow!("renaming lockfile to {}: {}", final_path.display(), err.error))?;
    Ok(LockfileGuard { path: final_path })
}

/// Holds an exclusive claim on a lockfile. Drop unlinks the file. Failures
/// during unlink are logged but not propagated — the file may have already
/// been removed by another process or by a startup sweep.
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
                    "failed to remove Claude /ide lockfile {}: {}",
                    self.path.display(),
                    error
                );
            }
        }
    }
}

/// Probe each `*.lock` file in the lockfile directory by attempting a TCP
/// connect to its port. Files whose port doesn't accept a connection within a
/// short timeout are unlinked — they are leftovers from crashed editor
/// processes. Returns the set of files that were removed.
pub fn sweep_stale_lockfiles() -> Result<Vec<PathBuf>> {
    let dir = match lockfile_dir() {
        Ok(dir) => dir,
        Err(_) => return Ok(Vec::new()),
    };

    let mut removed = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!("skipping unreadable lockfile entry: {error}");
                continue;
            }
        };
        let path = entry.path();
        let Some(port) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u16>().ok())
        else {
            continue;
        };

        if probe_port(port) {
            continue;
        }

        if let Err(error) = fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("failed to remove stale lockfile {}: {}", path.display(), error);
            }
            continue;
        }
        removed.push(path);
    }

    Ok(removed)
}

fn probe_port(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn write_atomic_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let tempdir = tempfile::tempdir().expect("create tempdir");
        // Force the lockfile_dir() helper to write under our tempdir by
        // shadowing $HOME.
        // SAFETY: tests are single-threaded per process by default; this only
        // runs the duration of the test.
        let original_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", tempdir.path()) };

        let lockfile = Lockfile::new(
            vec![PathBuf::from("/tmp/foo")],
            "auth-token-perms".to_string(),
        );
        let guard = write_atomic(45678, &lockfile).expect("write lockfile");
        let mode = fs::metadata(guard.path())
            .expect("stat lockfile")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "lockfile must not be readable by other users");

        match original_home {
            Some(home) => unsafe { std::env::set_var("HOME", home) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn lockfile_round_trips_through_json() {
        let lockfile = Lockfile::new(
            vec![PathBuf::from("/tmp/foo")],
            "auth-token-1234".to_string(),
        );
        let serialized = serde_json::to_string(&lockfile).expect("serialize lockfile");
        // Field naming must match Claude CLI exactly.
        assert!(serialized.contains("\"workspaceFolders\""));
        assert!(serialized.contains("\"ideName\":\"Zerminal\""));
        assert!(serialized.contains("\"transport\":\"ws\""));
        assert!(serialized.contains("\"authToken\":\"auth-token-1234\""));
        let parsed: Lockfile = serde_json::from_str(&serialized).expect("deserialize lockfile");
        assert_eq!(parsed.workspace_folders, lockfile.workspace_folders);
        assert_eq!(parsed.auth_token, lockfile.auth_token);
    }
}
