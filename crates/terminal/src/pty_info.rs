use alacritty_terminal::tty::Pty;
use gpui::{Context, Task};
use parking_lot::{MappedRwLockReadGuard, Mutex, RwLock, RwLockReadGuard};
#[cfg(target_os = "windows")]
use std::num::NonZeroU32;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(target_os = "windows")]
use windows::Win32::{Foundation::HANDLE, System::Threading::GetProcessId};

use sysinfo::{Pid, Process, ProcessRefreshKind, RefreshKind, System, UpdateKind};

use crate::{Event, Terminal};

#[derive(Clone, Copy)]
pub struct ProcessIdGetter {
    handle: i32,
    fallback_pid: u32,
}

impl ProcessIdGetter {
    pub fn fallback_pid(&self) -> Pid {
        Pid::from_u32(self.fallback_pid)
    }
}

#[cfg(unix)]
impl ProcessIdGetter {
    fn new(pty: &Pty) -> ProcessIdGetter {
        ProcessIdGetter {
            handle: pty.file().as_raw_fd(),
            fallback_pid: pty.child().id(),
        }
    }

    fn pid(&self) -> Option<Pid> {
        // Negative pid means error.
        // Zero pid means no foreground process group is set on the PTY yet.
        // Avoid killing the current process by returning a zero pid.
        let pid = unsafe { libc::tcgetpgrp(self.handle) };
        if pid > 0 {
            return Some(Pid::from_u32(pid as u32));
        }

        if self.fallback_pid > 0 {
            return Some(Pid::from_u32(self.fallback_pid));
        }

        None
    }
}

#[cfg(windows)]
impl ProcessIdGetter {
    fn new(pty: &Pty) -> ProcessIdGetter {
        let child = pty.child_watcher();
        let handle = child.raw_handle();
        let fallback_pid = child.pid().unwrap_or_else(|| unsafe {
            NonZeroU32::new_unchecked(GetProcessId(HANDLE(handle as _)))
        });

        ProcessIdGetter {
            handle: handle as i32,
            fallback_pid: u32::from(fallback_pid),
        }
    }

    fn pid(&self) -> Option<Pid> {
        let pid = unsafe { GetProcessId(HANDLE(self.handle as _)) };
        // the GetProcessId may fail and returns zero, which will lead to a stack overflow issue
        if pid == 0 {
            // in the builder process, there is a small chance, almost negligible,
            // that this value could be zero, which means child_watcher returns None,
            // GetProcessId returns 0.
            if self.fallback_pid == 0 {
                return None;
            }
            return Some(Pid::from_u32(self.fallback_pid));
        }
        Some(Pid::from_u32(pid))
    }
}

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub name: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
}

/// Fetches Zed-relevant Pseudo-Terminal (PTY) process information
pub struct PtyProcessInfo {
    system: RwLock<System>,
    refresh_kind: ProcessRefreshKind,
    pid_getter: ProcessIdGetter,
    pub current: RwLock<Option<ProcessInfo>>,
    task: Mutex<Option<Task<()>>>,
}

impl PtyProcessInfo {
    pub fn new(pty: &Pty) -> PtyProcessInfo {
        let process_refresh_kind = ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always);
        let refresh_kind = RefreshKind::nothing().with_processes(process_refresh_kind);
        let system = System::new_with_specifics(refresh_kind);

        PtyProcessInfo {
            system: RwLock::new(system),
            refresh_kind: process_refresh_kind,
            pid_getter: ProcessIdGetter::new(pty),
            current: RwLock::new(None),
            task: Mutex::new(None),
        }
    }

    pub fn pid_getter(&self) -> &ProcessIdGetter {
        &self.pid_getter
    }

    fn refresh(&self) -> Option<MappedRwLockReadGuard<'_, Process>> {
        let pid = self.pid_getter.pid()?;
        if self.system.write().refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            self.refresh_kind,
        ) == 1
        {
            RwLockReadGuard::try_map(self.system.read(), |system| system.process(pid)).ok()
        } else {
            None
        }
    }

    fn get_child(&self) -> Option<MappedRwLockReadGuard<'_, Process>> {
        let pid = self.pid_getter.fallback_pid();
        RwLockReadGuard::try_map(self.system.read(), |system| system.process(pid)).ok()
    }

    #[cfg(unix)]
    pub(crate) fn kill_current_process(&self) -> bool {
        let Some(pid) = self.pid_getter.pid() else {
            return false;
        };
        unsafe { libc::killpg(pid.as_u32() as i32, libc::SIGKILL) == 0 }
    }

    #[cfg(not(unix))]
    pub(crate) fn kill_current_process(&self) -> bool {
        self.refresh().is_some_and(|process| process.kill())
    }

    pub(crate) fn kill_child_process(&self) -> bool {
        self.get_child().is_some_and(|process| process.kill())
    }

    #[cfg(unix)]
    pub(crate) fn terminate_child_process(&self) -> bool {
        let pid = self.pid_getter.fallback_pid();
        unsafe { libc::killpg(pid.as_u32() as i32, libc::SIGTERM) == 0 }
    }

    #[cfg(not(unix))]
    pub(crate) fn terminate_child_process(&self) -> bool {
        false
    }

    fn load(&self) -> Option<ProcessInfo> {
        let process = self.refresh()?;
        let cwd = process.cwd().map_or(PathBuf::new(), |p| p.to_owned());

        let argv: Vec<String> = process
            .cmd()
            .iter()
            .filter_map(|s| s.to_str().map(ToOwned::to_owned))
            .collect();
        let info = ProcessInfo {
            name: display_name(&argv, process.exe(), process.name())?,
            cwd,
            argv,
        };
        *self.current.write() = Some(info.clone());
        Some(info)
    }

    /// Updates the cached process info, emitting a [`Event::TitleChanged`] event if the Zed-relevant info has changed
    pub fn emit_title_changed_if_changed(self: &Arc<Self>, cx: &mut Context<'_, Terminal>) {
        if self.task.lock().is_some() {
            return;
        }
        let this = self.clone();
        let has_changed = cx.background_executor().spawn(async move {
            // Snapshot the cached info before `load()` overwrites it; otherwise
            // `previous` and `current` always alias the same just-written value
            // and `Event::TitleChanged` never fires.
            let previous = this.current.read().clone();
            let current = this.load();
            match (previous.as_ref(), current.as_ref()) {
                (None, None) => false,
                (Some(prev), Some(now)) => prev.cwd != now.cwd || prev.name != now.name,
                _ => true,
            }
        });
        let this = Arc::downgrade(self);
        *self.task.lock() = Some(cx.spawn(async move |term, cx| {
            if has_changed.await {
                term.update(cx, |_, cx| cx.emit(Event::TitleChanged)).ok();
            }
            if let Some(this) = this.upgrade() {
                this.task.lock().take();
            }
        }));
    }

    pub fn pid(&self) -> Option<Pid> {
        self.pid_getter.pid()
    }
}

/// Derives the foreground process's display name from argv[0] (then the
/// executable path), not from sysinfo's process name: sysinfo only records the
/// name when a pid is first seen, so after an in-place `exec` — e.g. a
/// launcher like loadout's `load` exec()ing an agent CLI — the recorded name
/// keeps naming the launcher while argv and exe refresh to the new program.
/// The leading `-` login-shell convention in argv[0] is stripped so shells
/// still match `is_known_shell`.
fn display_name(argv: &[String], exe: Option<&Path>, sysinfo_name: &OsStr) -> Option<String> {
    if let Some(argv0) = argv.first() {
        let argv0 = argv0.strip_prefix('-').unwrap_or(argv0);
        if let Some(name) = Path::new(argv0).file_name().and_then(OsStr::to_str) {
            return Some(name.to_owned());
        }
    }
    if let Some(name) = exe.and_then(Path::file_name).and_then(OsStr::to_str) {
        return Some(name.to_owned());
    }
    sysinfo_name.to_str().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::display_name;
    use std::ffi::OsStr;
    use std::path::Path;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn prefers_fresh_argv0_over_stale_sysinfo_name() {
        // After a launcher exec()s into another program (same pid), sysinfo
        // keeps reporting the pre-exec name ("load") while argv refreshes.
        assert_eq!(
            display_name(
                &argv(&["claude", "--append-system-prompt", "loadout: refreshed"]),
                Some(Path::new("/Users/me/.local/share/claude/versions/2.1.222")),
                OsStr::new("load"),
            )
            .as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn strips_login_shell_dash_from_argv0() {
        assert_eq!(
            display_name(
                &argv(&["-zsh"]),
                Some(Path::new("/bin/zsh")),
                OsStr::new("zsh")
            )
            .as_deref(),
            Some("zsh")
        );
    }

    #[test]
    fn uses_basename_of_absolute_argv0() {
        assert_eq!(
            display_name(
                &argv(&["/opt/homebrew/bin/fish", "-l"]),
                None,
                OsStr::new("fish")
            )
            .as_deref(),
            Some("fish")
        );
    }

    #[test]
    fn falls_back_to_exe_basename_when_argv_is_empty() {
        assert_eq!(
            display_name(&argv(&[]), Some(Path::new("/bin/cat")), OsStr::new("stale")).as_deref(),
            Some("cat")
        );
    }

    #[test]
    fn falls_back_to_exe_basename_when_argv0_is_empty() {
        assert_eq!(
            display_name(
                &argv(&[""]),
                Some(Path::new("/bin/cat")),
                OsStr::new("stale")
            )
            .as_deref(),
            Some("cat")
        );
    }

    #[test]
    fn falls_back_to_sysinfo_name_when_argv_and_exe_are_missing() {
        assert_eq!(
            display_name(&argv(&[]), None, OsStr::new("kernel_task")).as_deref(),
            Some("kernel_task")
        );
    }
}
