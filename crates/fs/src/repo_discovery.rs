use std::path::{Path, PathBuf};

use crate::Fs;

pub const DEFAULT_MAX_WALKUP_DEPTH: usize = 16;

/// Walks up from `start` and returns the nearest ancestor that contains a
/// `.git` file or directory (including `start` itself). Returns `None` if no
/// such ancestor is found within `max_depth`, or if the walk reaches a
/// filesystem root or `stop_boundary` first.
///
/// `stop_boundary` should be a pre-canonicalized path (e.g. `$HOME`). The walk
/// stops *before* checking `stop_boundary` itself — the boundary is never
/// treated as a repo root, even if it happens to contain `.git`.
///
/// The caller is responsible for canonicalizing `start` beforehand if it
/// wants symlink-aware walk-up.
pub async fn find_repo_root(
    start: &Path,
    fs: &dyn Fs,
    stop_boundary: Option<&Path>,
    max_depth: usize,
) -> Option<PathBuf> {
    let mut current: &Path = start;
    for _ in 0..=max_depth {
        if matches!(stop_boundary, Some(boundary) if current == boundary) {
            return None;
        }
        match fs.metadata(&current.join(git::DOT_GIT)).await {
            Ok(Some(_)) => return Some(current.to_path_buf()),
            Ok(None) => {}
            Err(_) => return None,
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => return None,
        }
    }
    None
}

/// Detects a bare git repository at `path`: no `.git` subtree, but both `HEAD`
/// and `config` exist at the root. This check does NOT walk — it only inspects
/// `path` itself.
pub async fn is_bare_repo_root(path: &Path, fs: &dyn Fs) -> bool {
    if fs.metadata(&path.join(git::DOT_GIT)).await.ok().flatten().is_some() {
        return false;
    }
    let head = fs.metadata(&path.join("HEAD")).await.ok().flatten().is_some();
    let config = fs.metadata(&path.join("config")).await.ok().flatten().is_some();
    head && config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use util::path;

    #[gpui::test]
    async fn find_repo_root_at_start(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/repo"), json!({ ".git": {}, "src": { "main.rs": "" } }))
            .await;

        let root = find_repo_root(
            Path::new(path!("/repo")),
            fs.as_ref(),
            None,
            DEFAULT_MAX_WALKUP_DEPTH,
        )
        .await;
        assert_eq!(root.as_deref(), Some(Path::new(path!("/repo"))));
    }

    #[gpui::test]
    async fn find_repo_root_walk_up(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/repo"),
            json!({ ".git": {}, "a": { "b": { "c": { "file.rs": "" } } } }),
        )
        .await;

        let root = find_repo_root(
            Path::new(path!("/repo/a/b/c")),
            fs.as_ref(),
            None,
            DEFAULT_MAX_WALKUP_DEPTH,
        )
        .await;
        assert_eq!(root.as_deref(), Some(Path::new(path!("/repo"))));
    }

    #[gpui::test]
    async fn find_repo_root_gitlink_file(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/wt"),
            json!({ ".git": "gitdir: /other/.git/worktrees/wt", "src": {} }),
        )
        .await;

        let root = find_repo_root(
            Path::new(path!("/wt/src")),
            fs.as_ref(),
            None,
            DEFAULT_MAX_WALKUP_DEPTH,
        )
        .await;
        assert_eq!(root.as_deref(), Some(Path::new(path!("/wt"))));
    }

    #[gpui::test]
    async fn find_repo_root_no_match(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/loose"), json!({ "a": { "b": { "c.txt": "" } } }))
            .await;

        let root = find_repo_root(
            Path::new(path!("/loose/a/b")),
            fs.as_ref(),
            None,
            DEFAULT_MAX_WALKUP_DEPTH,
        )
        .await;
        assert_eq!(root, None);
    }

    #[gpui::test]
    async fn find_repo_root_stops_at_boundary(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/home"),
            json!({ "me": { ".git": {}, "proj": { "src": {} } } }),
        )
        .await;

        let boundary = Path::new(path!("/home/me"));
        let root = find_repo_root(
            Path::new(path!("/home/me/proj/src")),
            fs.as_ref(),
            Some(boundary),
            DEFAULT_MAX_WALKUP_DEPTH,
        )
        .await;
        assert_eq!(root, None, "walk must stop before checking $HOME");
    }

    #[gpui::test]
    async fn find_repo_root_depth_cap(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/repo"),
            json!({
                ".git": {},
                "a": { "b": { "c": { "d": { "e": { "f": { "g": { "h": { "i": { "j": { "file.rs": "" } } } } } } } } } }
            }),
        )
        .await;
        let deep = Path::new(path!("/repo/a/b/c/d/e/f/g/h/i/j"));

        let found = find_repo_root(deep, fs.as_ref(), None, DEFAULT_MAX_WALKUP_DEPTH).await;
        assert_eq!(found.as_deref(), Some(Path::new(path!("/repo"))));

        let capped = find_repo_root(deep, fs.as_ref(), None, 3).await;
        assert_eq!(capped, None, "depth cap should prevent finding the root");
    }

    #[gpui::test]
    async fn is_bare_repo_detected(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/bare"),
            json!({ "HEAD": "ref: refs/heads/main\n", "config": "[core]\n", "objects": {}, "refs": {} }),
        )
        .await;
        fs.insert_tree(path!("/normal"), json!({ ".git": {}, "src": {} }))
            .await;
        fs.insert_tree(path!("/neither"), json!({ "a": "" })).await;

        assert!(is_bare_repo_root(Path::new(path!("/bare")), fs.as_ref()).await);
        assert!(!is_bare_repo_root(Path::new(path!("/normal")), fs.as_ref()).await);
        assert!(!is_bare_repo_root(Path::new(path!("/neither")), fs.as_ref()).await);
    }
}
