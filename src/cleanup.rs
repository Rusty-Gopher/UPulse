//! System cleaner for the Cleanup tab: measures reclaimable space in well-known
//! safe caches/logs and clears the selected ones.
//!
//! Sizing runs on a background thread (`du`, `apt-get -s autoremove`,
//! `journalctl --disk-usage`). Cleaning runs the safe commands — user-owned
//! paths as the current user, system paths once under `pkexec` — streaming
//! output into a bounded live log, the same pattern as `apps`/`updates`.
//!
//! Deliberately conservative: it only ever touches package caches, auto-removable
//! packages, old journald logs, crash reports, the thumbnail cache, and trash —
//! never real user documents.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const LOG_CAP: usize = 600;

/// (key, label, detail, needs_root)
const TARGETS: &[(&str, &str, &str, bool)] = &[
    (
        "apt",
        "Apt package cache",
        "Downloaded .deb files in /var/cache/apt",
        true,
    ),
    (
        "autoremove",
        "Unused packages",
        "Auto-removable dependencies & old kernels",
        true,
    ),
    (
        "journal",
        "System logs",
        "Old journald logs (keeps the last 3 days)",
        true,
    ),
    (
        "crash",
        "Crash reports",
        "Saved crash dumps in /var/crash",
        true,
    ),
    (
        "thumbnails",
        "Thumbnail cache",
        "~/.cache/thumbnails (regenerated on demand)",
        false,
    ),
    ("trash", "Trash", "~/.local/share/Trash", false),
];

#[derive(Clone)]
pub struct CleanTarget {
    pub key: &'static str,
    pub label: &'static str,
    pub detail: String,
    pub size: u64,    // reclaimable bytes
    pub items: usize, // countable units (snap revisions, flatpak refs); 0 elsewhere
}

#[derive(Default)]
pub struct CleanupState {
    pub scanning: bool,
    pub scanned: bool,
    pub targets: Vec<CleanTarget>,
    pub error: Option<String>,

    // A running clean.
    pub busy: bool,
    pub log: Vec<String>,
    pub done: bool,
    pub ok: bool,
}

impl CleanupState {
    pub fn total(&self) -> u64 {
        self.targets.iter().map(|t| t.size).sum()
    }
}

pub struct Cleanup {
    pub state: Arc<Mutex<CleanupState>>,
}

impl Cleanup {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CleanupState::default())),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.lock().map(|s| s.busy).unwrap_or(false)
    }

    fn is_scanning(&self) -> bool {
        self.state.lock().map(|s| s.scanning).unwrap_or(false)
    }

    /// Measure how much each target could reclaim, on a background thread.
    pub fn scan(&self, ctx: egui::Context) {
        if self.is_scanning() || self.is_busy() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.scanning = true;
            s.error = None;
        }
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let targets = measure_all();
            if let Ok(mut s) = state.lock() {
                s.targets = targets;
                s.scanning = false;
                s.scanned = true;
            }
            ctx.request_repaint();
        });
    }

    /// Clean the selected targets (by key). User-owned paths run as the current
    /// user; system paths run once under `pkexec`. Rescans when finished.
    pub fn clean(&self, keys: Vec<String>, ctx: egui::Context) {
        if self.is_busy() || self.is_scanning() || keys.is_empty() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.busy = true;
            s.done = false;
            s.ok = false;
            s.log.clear();
            s.log.push("Cleaning…".to_string());
            s.error = None;
        }
        ctx.request_repaint();

        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            let mut user_cmds: Vec<String> = Vec::new();
            let mut root_cmds: Vec<String> = Vec::new();
            for key in &keys {
                if let Some((cmd, root)) = clean_command(key, &home) {
                    if root {
                        root_cmds.push(cmd);
                    } else {
                        user_cmds.push(cmd);
                    }
                }
            }

            let mut ok = true;

            // User-owned cleans first (no password prompt).
            if !user_cmds.is_empty() {
                let script = format!("{} 2>&1", user_cmds.join(" ; "));
                let mut c = Command::new("/bin/sh");
                c.arg("-c").arg(&script);
                ok &= run_streaming(c, &state, &ctx);
            }

            // System cleans: one pkexec for the whole batch.
            if !root_cmds.is_empty() {
                push_log(&state, "Requesting administrator access…".to_string());
                ctx.request_repaint();
                let script = format!("{} 2>&1", root_cmds.join(" ; "));
                let mut c = Command::new("pkexec");
                c.arg("/bin/sh").arg("-c").arg(&script);
                ok &= run_streaming(c, &state, &ctx);
            }

            // Refresh sizes and report.
            let targets = measure_all();
            if let Ok(mut s) = state.lock() {
                s.targets = targets;
                s.busy = false;
                s.done = true;
                s.ok = ok;
                if !ok && s.error.is_none() {
                    s.error = Some("Some items couldn't be cleaned. See the log above.".into());
                }
            }
            ctx.request_repaint();
        });
    }
}

/// Run a command, streaming its (already `2>&1`-merged) output into the log.
fn run_streaming(mut cmd: Command, state: &Arc<Mutex<CleanupState>>, ctx: &egui::Context) -> bool {
    let child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            push_log(state, format!("Couldn't start: {e}"));
            ctx.request_repaint();
            return false;
        }
    };
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            push_log(state, line);
            ctx.request_repaint();
        }
    }
    let mut err_txt = String::new();
    if let Some(mut es) = child.stderr.take() {
        use std::io::Read;
        let _ = es.read_to_string(&mut err_txt);
    }
    match child.wait() {
        Ok(st) if st.success() => true,
        _ => {
            let detail = err_txt.trim();
            if !detail.is_empty() {
                push_log(state, detail.lines().last().unwrap_or(detail).to_string());
            }
            false
        }
    }
}

fn push_log(state: &Arc<Mutex<CleanupState>>, line: String) {
    if let Ok(mut s) = state.lock() {
        s.log.push(line);
        let len = s.log.len();
        if len > LOG_CAP {
            s.log.drain(0..len - LOG_CAP);
        }
    }
}

/// The shell command that cleans one target, and whether it needs root.
fn clean_command(key: &str, home: &str) -> Option<(String, bool)> {
    Some(match key {
        "apt" => ("apt-get clean".into(), true),
        "autoremove" => ("apt-get -y autoremove --purge".into(), true),
        "journal" => (
            "journalctl --rotate ; journalctl --vacuum-time=3d".into(),
            true,
        ),
        "crash" => ("rm -rf /var/crash/*".into(), true),
        "thumbnails" => (format!("rm -rf '{home}/.cache/thumbnails'"), false),
        "trash" => (
            format!("rm -rf '{home}/.local/share/Trash/files' '{home}/.local/share/Trash/info'"),
            false,
        ),
        "snap" => {
            // Re-list at clean time so we only remove what snapd currently
            // marks disabled — never a stale scan result. Every name/rev has
            // passed the validators, so interpolation is safe.
            let revs = snap_old_revisions();
            if revs.is_empty() {
                return None;
            }
            (
                revs.iter()
                    .map(|(n, r)| format!("snap remove {n} --revision={r}"))
                    .collect::<Vec<_>>()
                    .join(" ; "),
                true,
            )
        }
        "flatpak" => (
            // As the user, not root: pkexec would point $HOME at /root and
            // miss user installations; system refs raise flatpak's own polkit
            // prompt. The unused set is recomputed by flatpak itself here.
            "flatpak uninstall --unused -y --noninteractive".into(),
            false,
        ),
        _ => return None,
    })
}

// --- snap -------------------------------------------------------------------

/// Disabled (old) snap revisions as validated (name, revision) pairs.
fn snap_old_revisions() -> Vec<(String, String)> {
    let mut cmd = Command::new("snap");
    cmd.args(["list", "--all"]);
    cmd.env("LC_ALL", "C");
    match crate::util::output_timeout(cmd, 15) {
        Ok(out) => parse_snap_disabled(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Vec::new(),
    }
}

/// Parse `snap list --all` (columns: Name Version Rev Tracking Publisher
/// Notes), keeping rows whose comma-joined Notes carry a `disabled` token.
/// Rows failing name/revision validation are dropped (fail closed).
pub(crate) fn parse_snap_disabled(out: &str) -> Vec<(String, String)> {
    out.lines()
        .skip(1) // header
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 6 {
                return None;
            }
            let (name, rev, notes) = (cols[0], cols[2], cols[cols.len() - 1]);
            let disabled = notes.split(',').any(|t| t == "disabled");
            (disabled && valid_snap_name(name) && valid_snap_rev(rev))
                .then(|| (name.to_string(), rev.to_string()))
        })
        .collect()
}

/// Snap names are lowercase alphanumerics and dashes (the store enforces it).
pub(crate) fn valid_snap_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 100
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Revisions are numbers ("1443") or `x`-prefixed for sideloads ("x1").
pub(crate) fn valid_snap_rev(s: &str) -> bool {
    !s.is_empty() && s.len() < 20 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Size the disabled revisions by stat-ing their seed files under
/// `/var/lib/snapd/snaps` — file *metadata* is readable without root even
/// though the contents aren't. `items` keeps the row honest (and selectable)
/// if some stats fail.
fn measure_snap() -> CleanTarget {
    let revs = snap_old_revisions();
    let size = revs
        .iter()
        .map(|(n, r)| {
            std::fs::metadata(format!("/var/lib/snapd/snaps/{n}_{r}.snap"))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();
    CleanTarget {
        key: "snap",
        label: "Old Snap revisions",
        detail: match revs.len() {
            0 => "No disabled revisions — snapd is tidy".to_string(),
            n => format!("{n} old disabled revision(s) kept by snapd"),
        },
        size,
        items: revs.len(),
    }
}

// --- flatpak ----------------------------------------------------------------

/// The refs `flatpak uninstall --unused` would remove, without removing them:
/// the confirmation prompt is answered with an explicit "n" (stdin is nulled
/// by `output_timeout`, and relying on an EOF default would be a gamble), so
/// this sizing pass can never mutate anything.
fn flatpak_unused() -> Vec<(String, String)> {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "printf 'n\\n' | flatpak uninstall --unused"]);
    cmd.env("LC_ALL", "C");
    // The aborted run exits nonzero by design; parse stdout regardless.
    match crate::util::output_timeout(cmd, 20) {
        Ok(out) => parse_flatpak_unused(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Vec::new(),
    }
}

/// Parse the numbered ref list from an aborted `flatpak uninstall --unused`
/// transcript: rows shaped ` 1. org.gnome.Platform 45 r` (tab- or
/// space-aligned) → (id, branch) pairs.
pub(crate) fn parse_flatpak_unused(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let mut toks = line.split_whitespace();
            let digits = toks.next()?.strip_suffix('.')?;
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let id = toks.next()?;
            let branch = toks.next()?;
            (id.contains('.') && valid_flatpak_part(id) && valid_flatpak_part(branch))
                .then(|| (id.to_string(), branch.to_string()))
        })
        .collect()
}

fn valid_flatpak_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 200
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Parse `flatpak list --columns=ref,size` (tab-separated when not a tty) →
/// (id, branch, bytes). Refs come as `org.x.Y/arch/branch` or with a leading
/// `app/`/`runtime/` kind segment; both are handled.
pub(crate) fn parse_flatpak_sizes(out: &str) -> Vec<(String, String, u64)> {
    out.lines()
        .filter_map(|line| {
            let (r, sz) = line.split_once('\t')?;
            let segs: Vec<&str> = r.trim().split('/').collect();
            let (id, branch) = match segs.len() {
                4 => (segs[1], segs[3]),
                3 => (segs[0], segs[2]),
                _ => return None,
            };
            Some((
                id.to_string(),
                branch.to_string(),
                parse_size_unit(sz.trim()),
            ))
        })
        .collect()
}

fn measure_flatpak() -> CleanTarget {
    let unused = flatpak_unused();
    let size = if unused.is_empty() {
        0
    } else {
        let mut cmd = Command::new("flatpak");
        cmd.args(["list", "--columns=ref,size"]);
        cmd.env("LC_ALL", "C");
        let sizes = match crate::util::output_timeout(cmd, 20) {
            Ok(out) => parse_flatpak_sizes(&String::from_utf8_lossy(&out.stdout)),
            Err(_) => Vec::new(),
        };
        unused
            .iter()
            .map(|(id, branch)| {
                sizes
                    .iter()
                    .find(|(i, b, _)| i == id && b == branch)
                    .map(|&(_, _, s)| s)
                    .unwrap_or(0)
            })
            .sum()
    };
    CleanTarget {
        key: "flatpak",
        label: "Flatpak unused runtimes",
        detail: match unused.len() {
            0 => "Every installed runtime is still needed".to_string(),
            n => format!("{n} runtime(s) no installed app needs"),
        },
        size,
        items: unused.len(),
    }
}

// --- sizing -----------------------------------------------------------------

fn measure_all() -> Vec<CleanTarget> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let mut targets: Vec<CleanTarget> = TARGETS
        .iter()
        .map(|&(key, label, detail, _needs_root)| CleanTarget {
            key,
            label,
            detail: detail.to_string(),
            size: measure(key, &home),
            items: 0,
        })
        .collect();
    if crate::util::has_bin("snap") {
        targets.push(measure_snap());
    }
    if crate::util::has_bin("flatpak") {
        targets.push(measure_flatpak());
    }
    targets
}

fn measure(key: &str, home: &str) -> u64 {
    match key {
        "apt" => du_bytes("/var/cache/apt/archives"),
        "crash" => du_bytes("/var/crash"),
        "thumbnails" => du_bytes(&format!("{home}/.cache/thumbnails")),
        "trash" => du_bytes(&format!("{home}/.local/share/Trash")),
        "autoremove" => apt_autoremove_bytes(),
        "journal" => journal_bytes(),
        _ => 0,
    }
}

/// `du -sb <path>` → bytes (0 on any error / missing path).
fn du_bytes(path: &str) -> u64 {
    let mut cmd = Command::new("du");
    cmd.args(["-sb", path]);
    match crate::util::output_timeout(cmd, 20) {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Parse the reclaimable size from a simulated `apt-get autoremove`.
fn apt_autoremove_bytes() -> u64 {
    let mut cmd = Command::new("apt-get");
    cmd.args(["-s", "autoremove", "--purge"]);
    let out = match crate::util::output_timeout(cmd, 20) {
        Ok(o) => o,
        Err(_) => return 0,
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // "After this operation, 234 MB disk space will be freed."
        if let Some(idx) = line.find("will be freed") {
            let head = &line[..idx];
            if let Some(comma) = head.rfind(',') {
                return parse_size_unit(head[comma + 1..].trim());
            }
        }
    }
    0
}

/// Parse `journalctl --disk-usage` → bytes.
fn journal_bytes() -> u64 {
    let mut cmd = Command::new("journalctl");
    cmd.arg("--disk-usage");
    let out = match crate::util::output_timeout(cmd, 15) {
        Ok(o) => o,
        Err(_) => return 0,
    };
    // "Archived and active journals take up 1.2G in the file system."
    let text = String::from_utf8_lossy(&out.stdout);
    for tok in text.split_whitespace() {
        let b = parse_journal_size(tok);
        if b > 0 {
            return b;
        }
    }
    0
}

/// "234 MB" / "1.5 GB" / "512 kB" → bytes.
fn parse_size_unit(s: &str) -> u64 {
    let mut it = s.split_whitespace();
    let num: f64 = it.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
    let mult = match it.next().unwrap_or("") {
        "B" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        _ => 1.0,
    };
    (num * mult) as u64
}

/// journalctl's compact sizes: "1.2G", "240.0M", "512.0K", "900B".
fn parse_journal_size(tok: &str) -> u64 {
    let tok = tok.trim_end_matches('.');
    let (num_str, mult) = if let Some(n) = tok.strip_suffix('G') {
        (n, 1024.0 * 1024.0 * 1024.0)
    } else if let Some(n) = tok.strip_suffix('M') {
        (n, 1024.0 * 1024.0)
    } else if let Some(n) = tok.strip_suffix('K') {
        (n, 1024.0)
    } else if let Some(n) = tok.strip_suffix('B') {
        (n, 1.0)
    } else {
        return 0;
    };
    num_str
        .parse::<f64>()
        .map(|v| (v * mult) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from a real `snap list --all` on Ubuntu 24.04.
    const SNAP_FIXTURE: &str = "\
Name       Version          Rev    Tracking       Publisher       Notes
bare       1.0              5      latest/stable  canonical**     base
bitwarden  2026.4.0         159    latest/stable  bitwarden**     -
bitwarden  2026.3.1         157    latest/stable  bitwarden**     disabled
code       4fe60c8b         248    latest/stable  vscode**        disabled,classic
core20     20260211         2769   latest/stable  canonical**     base,disabled
firefox    153.0-1          8664   latest/stable  mozilla**       -
";

    #[test]
    fn snap_parser_finds_disabled_rows_only() {
        let revs = parse_snap_disabled(SNAP_FIXTURE);
        assert_eq!(
            revs,
            vec![
                ("bitwarden".to_string(), "157".to_string()),
                ("code".to_string(), "248".to_string()),
                ("core20".to_string(), "2769".to_string()),
            ]
        );
    }

    #[test]
    fn snap_parser_drops_malformed_rows() {
        // Too few columns / injection-looking name — both dropped.
        let out = "Name Version Rev Tracking Publisher Notes\n\
                   short row disabled\n\
                   bad;name 1.0 12 latest/stable pub** disabled\n";
        assert!(parse_snap_disabled(out).is_empty());
    }

    #[test]
    fn snap_validators_reject_shell_metacharacters() {
        assert!(valid_snap_name("core20"));
        assert!(valid_snap_name("firmware-updater"));
        assert!(!valid_snap_name("a;b"));
        assert!(!valid_snap_name("a b"));
        assert!(!valid_snap_name("$(reboot)"));
        assert!(!valid_snap_name(""));
        assert!(!valid_snap_name("Upper"));
        assert!(valid_snap_rev("1443"));
        assert!(valid_snap_rev("x1"));
        assert!(!valid_snap_rev("1443; rm -rf /"));
        assert!(!valid_snap_rev(""));
    }

    #[test]
    fn flatpak_unused_parser_reads_numbered_rows() {
        let out = "\
Found 2 unused refs:
        ID                          Branch    Op
 1.     org.gnome.Platform          45        r
 2.     org.freedesktop.Platform    23.08     r

Proceed with these changes to the system installation? [Y/n]: ";
        assert_eq!(
            parse_flatpak_unused(out),
            vec![
                ("org.gnome.Platform".to_string(), "45".to_string()),
                ("org.freedesktop.Platform".to_string(), "23.08".to_string()),
            ]
        );
    }

    #[test]
    fn flatpak_unused_parser_ignores_prose_and_junk() {
        assert!(parse_flatpak_unused("Nothing unused to uninstall\n").is_empty());
        // Numbered row whose id has no dot, or with shell junk → dropped.
        let out = " 1. noDotHere 45 r\n 2. org.x.Y$(id) 45 r\n";
        assert!(parse_flatpak_unused(out).is_empty());
    }

    #[test]
    fn flatpak_sizes_parser_handles_both_ref_shapes() {
        let out = "runtime/org.gnome.Platform/x86_64/45\t1.2 GB\n\
                   org.mozilla.firefox/x86_64/stable\t310.2 MB\n\
                   not-a-ref\t1 MB\n";
        let sizes = parse_flatpak_sizes(out);
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes[0].0, "org.gnome.Platform");
        assert_eq!(sizes[0].1, "45");
        assert_eq!(sizes[0].2, 1_200_000_000);
        assert_eq!(sizes[1].0, "org.mozilla.firefox");
        assert_eq!(sizes[1].1, "stable");
        assert_eq!(sizes[1].2, 310_200_000);
    }

    #[test]
    fn size_unit_parser_regressions() {
        assert_eq!(parse_size_unit("234 MB"), 234_000_000);
        assert_eq!(parse_size_unit("1.5 GB"), 1_500_000_000);
        assert_eq!(parse_size_unit("512 kB"), 512_000);
        assert_eq!(parse_size_unit("0 B"), 0);
        assert_eq!(parse_size_unit("garbage"), 0);
    }

    #[test]
    fn journal_size_parser_regressions() {
        assert_eq!(
            parse_journal_size("1.2G"),
            (1.2 * 1024.0 * 1024.0 * 1024.0) as u64
        );
        assert_eq!(
            parse_journal_size("240.0M"),
            (240.0 * 1024.0 * 1024.0) as u64
        );
        assert_eq!(parse_journal_size("512.0K"), 512 * 1024);
        assert_eq!(parse_journal_size("900B"), 900);
        assert_eq!(parse_journal_size("words"), 0);
    }
}
