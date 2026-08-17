//! Background scanner that walks a directory tree looking for large files.
//!
//! Runs on its own thread so the UI never blocks. Progress and results are
//! shared through an `Arc<Mutex<Scan>>`; the worker asks egui to repaint as it
//! makes progress.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// How many of the largest files to keep and show.
const KEEP: usize = 60;

/// Directory names treated as regenerable build artifacts. Matched by exact
/// name during the walk and re-validated against this list before a delete.
const ARTIFACT_NAMES: &[&str] = &["node_modules", "__pycache__", ".venv", "target"];

/// How many of the largest artifact directories to keep and show.
const KEEP_ARTIFACTS: usize = 40;

/// Artifact directories smaller than this are noise; skip them.
const ARTIFACT_MIN: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct BigFile {
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ArtifactKind {
    NodeModules,
    PyCache,
    Venv,
    CargoTarget,
}

impl ArtifactKind {
    pub fn label(self) -> &'static str {
        match self {
            ArtifactKind::NodeModules => "Node packages",
            ArtifactKind::PyCache => "Python cache",
            ArtifactKind::Venv => "Python venv",
            ArtifactKind::CargoTarget => "Cargo build",
        }
    }
}

/// A build/dependency directory found by the scan, sized as one unit.
#[derive(Clone)]
pub struct ArtifactDir {
    pub path: String,
    pub size: u64,
    pub kind: ArtifactKind,
}

pub struct Scan {
    pub running: bool,
    pub done: bool,
    pub deleting: bool, // an artifact delete worker is active
    pub scanned: u64,   // files visited
    pub matches: u64,   // files >= threshold
    pub top: Vec<BigFile>,
    pub artifacts: Vec<ArtifactDir>,
    pub root: String,
    pub threshold: u64,
    pub last_error: Option<String>, // e.g. a failed delete
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            running: false,
            done: false,
            deleting: false,
            scanned: 0,
            matches: 0,
            top: Vec::new(),
            artifacts: Vec::new(),
            root: String::new(),
            threshold: 100 * 1024 * 1024,
            last_error: None,
        }
    }
}

pub struct Scanner {
    pub state: Arc<Mutex<Scan>>,
    cancel: Arc<AtomicBool>,
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(Scan::default())),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_running(&self) -> bool {
        self.state.lock().map(|s| s.running).unwrap_or(false)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Permanently delete one scanned file and drop it from the results. On
    /// failure (e.g. permissions), records the message in `last_error`.
    pub fn delete(&self, path: &str) {
        let res = std::fs::remove_file(path);
        if let Ok(mut s) = self.state.lock() {
            match res {
                Ok(()) => {
                    if let Some(pos) = s.top.iter().position(|f| f.path == path) {
                        s.top.remove(pos);
                        s.matches = s.matches.saturating_sub(1);
                    }
                    s.last_error = None;
                }
                Err(e) => s.last_error = Some(format!("Couldn't delete {path}: {e}")),
            }
        }
    }

    /// Kick off a fresh scan of `root`, reporting files at least `threshold` bytes.
    pub fn start(&self, root: PathBuf, threshold: u64, ctx: egui::Context) {
        if self.is_running() {
            return;
        }
        self.cancel.store(false, Ordering::Relaxed);
        if let Ok(mut s) = self.state.lock() {
            *s = Scan {
                running: true,
                root: root.to_string_lossy().to_string(),
                threshold,
                ..Default::default()
            };
        }

        let state = Arc::clone(&self.state);
        let cancel = Arc::clone(&self.cancel);
        std::thread::spawn(move || {
            let mut top: Vec<BigFile> = Vec::new();
            let mut artifacts: Vec<ArtifactDir> = Vec::new();
            let mut scanned: u64 = 0;
            let mut matches: u64 = 0;
            let mut stack: Vec<PathBuf> = vec![root];
            let mut since_flush: u64 = 0;

            let sort_trim = |v: &mut Vec<BigFile>| {
                v.sort_by_key(|f| std::cmp::Reverse(f.size));
                v.truncate(KEEP);
            };
            let sort_trim_art = |v: &mut Vec<ArtifactDir>| {
                v.sort_by_key(|f| std::cmp::Reverse(f.size));
                v.truncate(KEEP_ARTIFACTS);
            };

            'outer: while let Some(dir) = stack.pop() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let rd = match std::fs::read_dir(&dir) {
                    Ok(r) => r,
                    Err(_) => continue, // unreadable dir (permissions) — skip
                };
                for entry in rd.flatten() {
                    if cancel.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    let path = entry.path();
                    // symlink_metadata: never follow links (avoids loops & double counting).
                    let meta = match std::fs::symlink_metadata(&path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if meta.file_type().is_symlink() {
                        continue;
                    }
                    if meta.is_dir() {
                        let name = entry.file_name();
                        let kind = artifact_kind(&name.to_string_lossy(), || {
                            dir.join("Cargo.toml").is_file()
                        });
                        if let Some(kind) = kind {
                            // Aggregate the whole subtree as one entry and don't
                            // descend — its files never join the big-file list.
                            let (size, files) = dir_size(&path, &cancel);
                            scanned += files;
                            since_flush += files;
                            if size >= ARTIFACT_MIN {
                                artifacts.push(ArtifactDir {
                                    path: path.to_string_lossy().to_string(),
                                    size,
                                    kind,
                                });
                            }
                        } else {
                            stack.push(path);
                        }
                    } else if meta.is_file() {
                        scanned += 1;
                        since_flush += 1;
                        let size = meta.len();
                        if size >= threshold {
                            matches += 1;
                            top.push(BigFile {
                                path: path.to_string_lossy().to_string(),
                                size,
                            });
                            if top.len() > KEEP * 4 {
                                sort_trim(&mut top);
                            }
                        }
                    }
                    if since_flush >= 3000 {
                        since_flush = 0;
                        let mut snapshot = top.clone();
                        sort_trim(&mut snapshot);
                        let mut snap_art = artifacts.clone();
                        sort_trim_art(&mut snap_art);
                        if let Ok(mut s) = state.lock() {
                            s.scanned = scanned;
                            s.matches = matches;
                            s.top = snapshot;
                            s.artifacts = snap_art;
                        }
                        ctx.request_repaint();
                    }
                }
            }

            sort_trim(&mut top);
            sort_trim_art(&mut artifacts);
            if let Ok(mut s) = state.lock() {
                s.scanned = scanned;
                s.matches = matches;
                s.top = top;
                s.artifacts = artifacts;
                s.running = false;
                s.done = !cancel.load(Ordering::Relaxed);
            }
            ctx.request_repaint();
        });
    }

    /// Permanently delete scanned artifact directories on a background thread
    /// — a multi-GB `target/` takes seconds to remove, which must never
    /// freeze the UI. Rows disappear from the results as each delete lands.
    /// Every path is re-validated at delete time (see `delete_one_artifact`)
    /// so a stale or forged path can never escape. Fail closed.
    pub fn delete_artifacts(&self, paths: Vec<String>, ctx: egui::Context) {
        if paths.is_empty() {
            return;
        }
        {
            let Ok(mut s) = self.state.lock() else { return };
            if s.deleting || s.running {
                return;
            }
            s.deleting = true;
            s.last_error = None;
        }
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let mut failed = 0usize;
            let mut first_err: Option<String> = None;
            for path in paths {
                let (root, listed) = match state.lock() {
                    Ok(s) => (s.root.clone(), s.artifacts.iter().any(|a| a.path == path)),
                    Err(_) => break,
                };
                let res = delete_one_artifact(&root, listed, &path);
                if let Ok(mut s) = state.lock() {
                    match res {
                        Ok(()) => s.artifacts.retain(|a| a.path != path),
                        Err(e) => {
                            failed += 1;
                            if first_err.is_none() {
                                first_err = Some(format!("Couldn't delete {path}: {e}"));
                            }
                        }
                    }
                }
                ctx.request_repaint();
            }
            if let Ok(mut s) = state.lock() {
                s.deleting = false;
                s.last_error = first_err.map(|e| {
                    if failed > 1 {
                        format!("{e} (and {} more failed)", failed - 1)
                    } else {
                        e
                    }
                });
            }
            ctx.request_repaint();
        });
    }
}

/// The fail-closed per-directory checks: the path must still be a current
/// scan result, sit under the scanned root with no `..`, end in a known
/// artifact name, and be a real non-symlink directory (`target` additionally
/// re-checks its sibling `Cargo.toml`) — only then is it removed.
fn delete_one_artifact(root: &str, listed: bool, path: &str) -> Result<(), String> {
    if !listed {
        return Err("not in the current scan results".into());
    }
    if !artifact_delete_allowed(root, path) {
        return Err("path failed safety checks".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("not a real directory".into());
    }
    let p = Path::new(path);
    if p.file_name().and_then(|n| n.to_str()) == Some("target") {
        // A `target` is only claimable next to a Cargo.toml; re-check now.
        let has_manifest = p
            .parent()
            .map(|d| d.join("Cargo.toml").is_file())
            .unwrap_or(false);
        if !has_manifest {
            return Err("no Cargo.toml next to this target directory".into());
        }
    }
    std::fs::remove_dir_all(path).map_err(|e| e.to_string())
}

/// Classify a directory name as a build artifact. `target` is ambiguous, so it
/// only matches when the caller confirms a sibling `Cargo.toml` exists (passed
/// as a closure to keep this testable without a filesystem).
pub(crate) fn artifact_kind(
    name: &str,
    has_cargo_toml: impl FnOnce() -> bool,
) -> Option<ArtifactKind> {
    match name {
        "node_modules" => Some(ArtifactKind::NodeModules),
        "__pycache__" => Some(ArtifactKind::PyCache),
        ".venv" => Some(ArtifactKind::Venv),
        "target" if has_cargo_toml() => Some(ArtifactKind::CargoTarget),
        _ => None,
    }
}

/// True only for an absolute, `..`-free path under `root` whose final
/// component is a known artifact directory name.
pub(crate) fn artifact_delete_allowed(root: &str, path: &str) -> bool {
    if root.is_empty() {
        return false;
    }
    let p = Path::new(path);
    if !p.is_absolute() || !p.starts_with(root) {
        return false;
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| ARTIFACT_NAMES.contains(&n))
        .unwrap_or(false)
}

/// Total size and file count of a subtree, with the same rules as the main
/// walk: never follow symlinks, skip unreadable directories, honor cancel.
fn dir_size(root: &Path, cancel: &AtomicBool) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_matches_known_names() {
        assert!(matches!(
            artifact_kind("node_modules", || false),
            Some(ArtifactKind::NodeModules)
        ));
        assert!(matches!(
            artifact_kind("__pycache__", || false),
            Some(ArtifactKind::PyCache)
        ));
        assert!(matches!(
            artifact_kind(".venv", || false),
            Some(ArtifactKind::Venv)
        ));
    }

    #[test]
    fn cargo_target_requires_manifest() {
        assert!(matches!(
            artifact_kind("target", || true),
            Some(ArtifactKind::CargoTarget)
        ));
        assert!(artifact_kind("target", || false).is_none());
    }

    #[test]
    fn artifact_kind_rejects_near_misses() {
        assert!(artifact_kind("Target", || true).is_none());
        assert!(artifact_kind("node_modules2", || false).is_none());
        assert!(artifact_kind("venv", || false).is_none());
        assert!(artifact_kind("", || true).is_none());
    }

    #[test]
    fn delete_allowed_happy_paths() {
        assert!(artifact_delete_allowed(
            "/home/u",
            "/home/u/proj/node_modules"
        ));
        assert!(artifact_delete_allowed("/home/u", "/home/u/rs/target"));
    }

    #[test]
    fn delete_allowed_fails_closed() {
        // Relative, outside root, traversal, wrong component, empty root.
        assert!(!artifact_delete_allowed("/home/u", "proj/node_modules"));
        assert!(!artifact_delete_allowed("/home/u", "/etc/node_modules"));
        assert!(!artifact_delete_allowed(
            "/home/u",
            "/home/u/../etc/node_modules"
        ));
        assert!(!artifact_delete_allowed("/home/u", "/home/u/proj/src"));
        assert!(!artifact_delete_allowed("", "/home/u/proj/node_modules"));
    }
}
