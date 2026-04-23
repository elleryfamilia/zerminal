use anyhow::{Context as _, Result};
use fs::{
    Fs,
    repo_discovery::{DEFAULT_MAX_WALKUP_DEPTH, find_repo_root, is_bare_repo_root},
};
use futures::future::join_all;
use gpui::{AsyncApp, PromptLevel, WindowHandle};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use util::paths::home_dir;

use crate::{MultiWorkspace, Toast, notifications::NotificationId};

/// Outcome of validating a set of open-requested paths against the
/// "workspace must be a git repo" rule.
#[derive(Debug)]
pub enum GitProjectResolution {
    /// Caller should proceed with these paths. Any normalization (walk-up to
    /// repo root, `git init`) has already happened.
    Proceed {
        resolved_roots: Vec<PathBuf>,
        files_to_reveal: Vec<PathBuf>,
    },
    /// User cancelled, an error blocked progress, or no paths remain after
    /// rejection of invalid entries. Caller must abort the open.
    Abort,
}

/// Inspect each path, walk up to nearest repo root, prompt-to-init any
/// non-git folders, and return resolved repo roots plus any files to reveal
/// post-open. See `crates/workspace/src/git_project_guard.rs` doc comments in
/// the plan for full semantics.
pub async fn resolve(
    paths: Vec<PathBuf>,
    fs: Arc<dyn Fs>,
    requesting_window: Option<WindowHandle<MultiWorkspace>>,
    bypass: bool,
    cx: &mut AsyncApp,
) -> Result<GitProjectResolution> {
    if bypass || paths.is_empty() {
        return Ok(GitProjectResolution::Proceed {
            resolved_roots: paths,
            files_to_reveal: Vec::new(),
        });
    }

    let home = home_dir().clone();
    let canonical_home = fs.canonicalize(&home).await.ok();

    let mut classifications = Vec::with_capacity(paths.len());
    for path in &paths {
        classifications.push(classify_path(path, fs.as_ref(), canonical_home.as_deref()).await);
    }

    let mut resolved_roots: Vec<PathBuf> = Vec::new();
    let mut files_to_reveal: Vec<PathBuf> = Vec::new();
    let mut non_git_folders: Vec<PathBuf> = Vec::new();
    let mut normalization_toasts: Vec<PathBuf> = Vec::new();
    let mut error_toasts: Vec<String> = Vec::new();

    for (original, classification) in paths.into_iter().zip(classifications) {
        match classification {
            Classification::AlreadyRepoRoot(root) => {
                push_unique(&mut resolved_roots, root);
            }
            Classification::NormalizedToRoot { root, reveal_file } => {
                push_unique(&mut resolved_roots, root.clone());
                if let Some(file) = reveal_file {
                    files_to_reveal.push(file);
                }
                normalization_toasts.push(root);
            }
            Classification::NonGitFolder(path) => {
                non_git_folders.push(path);
            }
            Classification::BareRepo(path) => {
                error_toasts.push(format!(
                    "{} is a bare git repository; working tree required.",
                    display_name(&path)
                ));
            }
            Classification::FileOutsideRepo(path) => {
                error_toasts.push(format!(
                    "{} is not inside a git repository. Zerminal projects require git.",
                    display_name(&path)
                ));
            }
            Classification::HomeDirectory => {
                error_toasts.push(format!(
                    "{} cannot be opened as a project.",
                    home.display()
                ));
            }
            Classification::PassThrough(path) => {
                resolved_roots.push(path);
            }
            Classification::ReadError { path, error } => {
                error_toasts.push(format!("Couldn't read {}: {error}", display_name(&path)));
            }
        }
        let _ = original;
    }

    if !non_git_folders.is_empty() {
        let window = match requesting_window {
            Some(window) => window,
            None => {
                log::warn!(
                    "git_project_guard: skipping prompt for {} non-git folder(s); no window available",
                    non_git_folders.len()
                );
                show_toasts(&requesting_window, error_toasts, cx);
                return Ok(GitProjectResolution::Abort);
            }
        };

        let outcome = prompt_and_init(&non_git_folders, fs.clone(), window, cx).await?;
        match outcome {
            PromptOutcome::Initialized => {
                for path in &non_git_folders {
                    push_unique(&mut resolved_roots, path.clone());
                }
            }
            PromptOutcome::Cancelled => {
                return Ok(GitProjectResolution::Abort);
            }
            PromptOutcome::InitFailed(msg) => {
                error_toasts.push(msg);
                show_toasts(&requesting_window, error_toasts, cx);
                return Ok(GitProjectResolution::Abort);
            }
        }
    }

    show_toasts(&requesting_window, error_toasts, cx);
    for root in &normalization_toasts {
        show_toast_msg(
            &requesting_window,
            format!("Opened project {}", display_name(root)),
            cx,
        );
    }

    if resolved_roots.is_empty() {
        return Ok(GitProjectResolution::Abort);
    }

    Ok(GitProjectResolution::Proceed {
        resolved_roots,
        files_to_reveal,
    })
}

enum Classification {
    AlreadyRepoRoot(PathBuf),
    NormalizedToRoot {
        root: PathBuf,
        reveal_file: Option<PathBuf>,
    },
    NonGitFolder(PathBuf),
    BareRepo(PathBuf),
    FileOutsideRepo(PathBuf),
    HomeDirectory,
    PassThrough(PathBuf),
    ReadError {
        path: PathBuf,
        error: String,
    },
}

async fn classify_path(
    original: &Path,
    fs: &dyn Fs,
    canonical_home: Option<&Path>,
) -> Classification {
    // Fast path: if the input already has a `.git` at its root, keep the
    // caller's original (non-canonical) path and skip any further work.
    // Downstream (Workspace::new_local) canonicalizes on its own; returning
    // the original path here avoids introducing path-shape differences for
    // callers that do their own lookups keyed on the original path.
    if fs
        .metadata(&original.join(git::DOT_GIT))
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return Classification::AlreadyRepoRoot(original.to_path_buf());
    }

    let canonical = match fs.canonicalize(original).await {
        Ok(p) => p,
        Err(err) => {
            let kind = err
                .downcast_ref::<std::io::Error>()
                .map(|e| e.kind())
                .unwrap_or(std::io::ErrorKind::Other);
            if kind == std::io::ErrorKind::NotFound {
                return Classification::PassThrough(original.to_path_buf());
            }
            return Classification::ReadError {
                path: original.to_path_buf(),
                error: format!("{err}"),
            };
        }
    };

    let meta = match fs.metadata(&canonical).await {
        Ok(Some(m)) => m,
        Ok(None) => return Classification::PassThrough(original.to_path_buf()),
        Err(err) => {
            return Classification::ReadError {
                path: original.to_path_buf(),
                error: format!("{err}"),
            };
        }
    };

    if meta.is_dir {
        if matches!(canonical_home, Some(home) if home == canonical) {
            return Classification::HomeDirectory;
        }

        if fs
            .metadata(&canonical.join(git::DOT_GIT))
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return Classification::AlreadyRepoRoot(canonical);
        }

        if is_bare_repo_root(&canonical, fs).await {
            return Classification::BareRepo(canonical);
        }

        if let Some(root) = find_repo_root(
            &canonical,
            fs,
            canonical_home,
            DEFAULT_MAX_WALKUP_DEPTH,
        )
        .await
        {
            return Classification::NormalizedToRoot {
                root,
                reveal_file: None,
            };
        }

        Classification::NonGitFolder(canonical)
    } else {
        let Some(parent) = canonical.parent() else {
            return Classification::FileOutsideRepo(canonical);
        };
        match find_repo_root(parent, fs, canonical_home, DEFAULT_MAX_WALKUP_DEPTH).await {
            Some(root) => Classification::NormalizedToRoot {
                root,
                reveal_file: Some(canonical),
            },
            None => Classification::FileOutsideRepo(canonical),
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
    if !list.contains(&path) {
        list.push(path);
    }
}

enum PromptOutcome {
    Initialized,
    Cancelled,
    InitFailed(String),
}

async fn prompt_and_init(
    non_git_folders: &[PathBuf],
    fs: Arc<dyn Fs>,
    window: WindowHandle<MultiWorkspace>,
    cx: &mut AsyncApp,
) -> Result<PromptOutcome> {
    let (title, detail, answers) = build_prompt_copy(non_git_folders);

    let receiver = window.update(cx, |_, window, cx| {
        window.prompt(PromptLevel::Info, &title, Some(&detail), &answers, cx)
    })?;
    let idx = receiver.await?;

    if idx != 0 {
        return Ok(PromptOutcome::Cancelled);
    }

    let inits = non_git_folders.iter().map(|path| {
        let fs = fs.clone();
        let path = path.clone();
        async move {
            fs.git_init(&path, fallback_branch_name())
                .await
                .with_context(|| format!("git init {}", path.display()))
        }
    });
    let results = join_all(inits).await;
    let failures: Vec<String> = results
        .into_iter()
        .filter_map(|r| r.err().map(|e| format!("{e:#}")))
        .collect();

    if !failures.is_empty() {
        let detail = failures.join("\n");
        let _ = window.update(cx, |_, window, cx| {
            window.prompt(
                PromptLevel::Critical,
                "Couldn't initialize git repository",
                Some(&detail),
                &["OK"],
                cx,
            )
        });
        return Ok(PromptOutcome::InitFailed(format!(
            "Failed to initialize git repository: {}",
            failures.join("; ")
        )));
    }

    let msg = if non_git_folders.len() == 1 {
        format!(
            "Initialized git repository in {}",
            display_name(&non_git_folders[0])
        )
    } else {
        format!("Initialized {} git repositories", non_git_folders.len())
    };
    show_toast_msg(&Some(window), msg, cx);
    Ok(PromptOutcome::Initialized)
}

fn fallback_branch_name() -> String {
    "main".into()
}

const DETAIL_CAP_CHARS: usize = 500;
const LIST_DISPLAY_CAP: usize = 5;

fn build_prompt_copy(non_git_folders: &[PathBuf]) -> (String, String, Vec<&'static str>) {
    if non_git_folders.len() == 1 {
        let name = display_name(&non_git_folders[0]);
        let abs = non_git_folders[0].display().to_string();
        let mut detail = format!(
            "Zerminal projects are git repositories. \"{name}\" is not tracked by git. Initialize it now to open as a project?\n\n{abs}"
        );
        truncate_detail(&mut detail);
        (
            "Initialize as a git repository?".to_string(),
            detail,
            vec!["Initialize & Open", "Cancel"],
        )
    } else {
        let title = format!(
            "Initialize {} folders as git repositories?",
            non_git_folders.len()
        );
        let mut lines: Vec<String> = non_git_folders
            .iter()
            .take(LIST_DISPLAY_CAP)
            .map(|p| format!("• {}", display_name(p)))
            .collect();
        if non_git_folders.len() > LIST_DISPLAY_CAP {
            lines.push(format!(
                "…and {} more",
                non_git_folders.len() - LIST_DISPLAY_CAP
            ));
        }
        let mut detail = lines.join("\n");
        truncate_detail(&mut detail);
        (title, detail, vec!["Initialize All & Open", "Cancel"])
    }
}

fn truncate_detail(detail: &mut String) {
    if detail.len() > DETAIL_CAP_CHARS {
        detail.truncate(DETAIL_CAP_CHARS);
        detail.push('…');
    }
}

fn show_toasts(
    window: &Option<WindowHandle<MultiWorkspace>>,
    msgs: Vec<String>,
    cx: &mut AsyncApp,
) {
    for msg in msgs {
        show_toast_msg(window, msg, cx);
    }
}

fn show_toast_msg(
    window: &Option<WindowHandle<MultiWorkspace>>,
    msg: String,
    cx: &mut AsyncApp,
) {
    let Some(window) = window else { return };
    let _ = window.update(cx, |multi_workspace, _, cx| {
        let workspace = multi_workspace.workspace().clone();
        workspace.update(cx, |workspace, cx| {
            let toast = Toast::new(NotificationId::unique::<GitProjectGuardToast>(), msg);
            workspace.show_toast(toast, cx);
        });
    });
}

struct GitProjectGuardToast;

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use util::path;

    async fn classify(cx: &mut TestAppContext, path: &str, tree_root: &str, tree: serde_json::Value) -> Classification {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(tree_root, tree).await;
        classify_path(Path::new(path), fs.as_ref(), None).await
    }

    #[gpui::test]
    async fn classify_already_repo_root(cx: &mut TestAppContext) {
        let c = classify(
            cx,
            path!("/repo"),
            path!("/repo"),
            json!({ ".git": {}, "src": {} }),
        )
        .await;
        assert!(matches!(c, Classification::AlreadyRepoRoot(_)));
    }

    #[gpui::test]
    async fn classify_subfolder_walks_up(cx: &mut TestAppContext) {
        let c = classify(
            cx,
            path!("/repo/src"),
            path!("/repo"),
            json!({ ".git": {}, "src": {} }),
        )
        .await;
        match c {
            Classification::NormalizedToRoot { root, reveal_file } => {
                assert_eq!(root, Path::new(path!("/repo")));
                assert!(reveal_file.is_none());
            }
            other => panic!("expected NormalizedToRoot, got {:?}", discriminant(&other)),
        }
    }

    #[gpui::test]
    async fn classify_file_inside_repo(cx: &mut TestAppContext) {
        let c = classify(
            cx,
            path!("/repo/src/main.rs"),
            path!("/repo"),
            json!({ ".git": {}, "src": { "main.rs": "" } }),
        )
        .await;
        match c {
            Classification::NormalizedToRoot { root, reveal_file } => {
                assert_eq!(root, Path::new(path!("/repo")));
                assert_eq!(
                    reveal_file.as_deref(),
                    Some(Path::new(path!("/repo/src/main.rs")))
                );
            }
            other => panic!("expected NormalizedToRoot, got {:?}", discriminant(&other)),
        }
    }

    #[gpui::test]
    async fn classify_non_git_folder(cx: &mut TestAppContext) {
        let c = classify(
            cx,
            path!("/loose"),
            path!("/loose"),
            json!({ "a.txt": "" }),
        )
        .await;
        assert!(matches!(c, Classification::NonGitFolder(_)));
    }

    #[gpui::test]
    async fn classify_file_outside_repo(cx: &mut TestAppContext) {
        let c = classify(
            cx,
            path!("/loose/a.txt"),
            path!("/loose"),
            json!({ "a.txt": "" }),
        )
        .await;
        assert!(matches!(c, Classification::FileOutsideRepo(_)));
    }

    #[gpui::test]
    async fn classify_bare_repo(cx: &mut TestAppContext) {
        let c = classify(
            cx,
            path!("/bare"),
            path!("/bare"),
            json!({
                "HEAD": "ref: refs/heads/main\n",
                "config": "[core]\n",
                "objects": {},
                "refs": {}
            }),
        )
        .await;
        assert!(matches!(c, Classification::BareRepo(_)));
    }

    #[gpui::test]
    async fn classify_linked_worktree(cx: &mut TestAppContext) {
        let c = classify(
            cx,
            path!("/wt"),
            path!("/wt"),
            json!({ ".git": "gitdir: /other/.git/worktrees/wt" }),
        )
        .await;
        assert!(matches!(c, Classification::AlreadyRepoRoot(_)));
    }

    #[gpui::test]
    async fn classify_nonexistent_passes_through(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let c = classify_path(Path::new(path!("/nope")), fs.as_ref(), None).await;
        assert!(matches!(c, Classification::PassThrough(_)));
    }

    #[gpui::test]
    async fn resolve_bypass(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let mut async_app = cx.to_async();
        let out = resolve(
            vec![PathBuf::from(path!("/nonexistent"))],
            fs.clone(),
            None,
            true,
            &mut async_app,
        )
        .await
        .unwrap();
        match out {
            GitProjectResolution::Proceed {
                resolved_roots,
                files_to_reveal,
            } => {
                assert_eq!(resolved_roots, vec![PathBuf::from(path!("/nonexistent"))]);
                assert!(files_to_reveal.is_empty());
            }
            GitProjectResolution::Abort => panic!("bypass should not abort"),
        }
    }

    #[gpui::test]
    async fn resolve_no_window_aborts_non_git(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/loose"), json!({ "a.txt": "" })).await;
        let mut async_app = cx.to_async();
        let out = resolve(
            vec![PathBuf::from(path!("/loose"))],
            fs.clone(),
            None,
            false,
            &mut async_app,
        )
        .await
        .unwrap();
        assert!(matches!(out, GitProjectResolution::Abort));
    }

    fn discriminant(c: &Classification) -> &'static str {
        match c {
            Classification::AlreadyRepoRoot(_) => "AlreadyRepoRoot",
            Classification::NormalizedToRoot { .. } => "NormalizedToRoot",
            Classification::NonGitFolder(_) => "NonGitFolder",
            Classification::BareRepo(_) => "BareRepo",
            Classification::FileOutsideRepo(_) => "FileOutsideRepo",
            Classification::HomeDirectory => "HomeDirectory",
            Classification::PassThrough(_) => "PassThrough",
            Classification::ReadError { .. } => "ReadError",
        }
    }
}
