//! System-update checking, in-app installing, and power actions for the
//! Updates tab.
//!
//! Detection runs `apt list --upgradable` on a background thread (no root
//! needed — it reads APT's already-downloaded lists) and looks for Ubuntu's
//! `reboot-required` flag. Installing runs `apt-get dist-upgrade` under
//! `pkexec`, which pops the desktop's graphical PolicyKit password dialog —
//! so upgrades happen from inside the app, no terminal, with apt's output
//! streamed live into the UI. Power actions are fire-and-forget.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Keep the live upgrade log bounded so a chatty apt run can't grow forever.
const LOG_CAP: usize = 600;

#[derive(Default)]
pub struct Updates {
    pub checking: bool,
    pub checked: bool,
    pub count: usize,
    pub packages: Vec<String>,
    /// Pending snap refreshes, so "up to date" can't lie on snap-based
    /// desktops: (name, version) from `snap refresh --list`.
    pub snap_count: usize,
    pub snap_packages: Vec<(String, String)>,
    pub reboot_required: bool,
    /// True when the last check couldn't actually read update state (apt or
    /// snap failed/timed out). Counts may then be incomplete — the UI must
    /// show "can't check", never a false "up to date".
    pub check_failed: bool,
    pub error: Option<String>,

    // Live, in-app upgrade run (via pkexec + apt-get).
    pub installing: bool,
    pub install_log: Vec<String>,
    pub install_done: bool,
    pub install_ok: bool,
}

pub struct UpdatesChecker {
    pub state: Arc<Mutex<Updates>>,
}

impl UpdatesChecker {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(Updates::default())),
        }
    }

    pub fn is_checking(&self) -> bool {
        self.state.lock().map(|s| s.checking).unwrap_or(false)
    }

    pub fn is_installing(&self) -> bool {
        self.state.lock().map(|s| s.installing).unwrap_or(false)
    }

    /// Start (or restart) a background update check. Reads APT's cached lists,
    /// so it's fast and needs no root.
    pub fn check(&self, ctx: egui::Context) {
        if self.is_checking() || self.is_installing() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.checking = true;
            s.error = None;
        }
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let reboot_required = reboot_required();
            let apt = list_upgradable();
            let snaps = list_snap_refreshes();

            if let Ok(mut s) = state.lock() {
                s.checking = false;
                s.checked = true;
                s.check_failed = apt.is_none() || snaps.is_none();
                s.error = check_error(apt.is_none(), snaps.is_none());
                let (count, packages) = apt.unwrap_or_default();
                s.count = count;
                s.packages = packages;
                let snaps = snaps.unwrap_or_default();
                s.snap_count = snaps.len();
                s.snap_packages = snaps;
                s.reboot_required = reboot_required;
            }
            ctx.request_repaint();
        });
    }

    /// Refresh every snap from inside the app, mirroring `install`: one
    /// `pkexec snap refresh` with output streamed into the same live log.
    pub fn refresh_snaps(&self, ctx: egui::Context) {
        if self.is_installing() || self.is_checking() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.installing = true;
            s.install_done = false;
            s.install_ok = false;
            s.install_log.clear();
            s.install_log
                .push("Requesting administrator access…".to_string());
            s.error = None;
        }
        ctx.request_repaint();

        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let child = Command::new("pkexec")
                .arg("/bin/sh")
                .arg("-c")
                .arg("snap refresh 2>&1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();

            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    finish(
                        &state,
                        &ctx,
                        false,
                        Some(format!("Couldn't start snap refresh: {e}")),
                    );
                    return;
                }
            };

            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    push_log(&state, line);
                    ctx.request_repaint();
                }
            }
            let mut err_txt = String::new();
            if let Some(mut es) = child.stderr.take() {
                let _ = es.read_to_string(&mut err_txt);
            }
            let status = child.wait();
            let ok = matches!(&status, Ok(st) if st.success());
            let err = if ok {
                None
            } else {
                let detail = err_txt.trim();
                Some(if detail.is_empty() {
                    "Snap refresh did not finish. See the log above.".to_string()
                } else {
                    detail.lines().last().unwrap_or(detail).to_string()
                })
            };
            finish(&state, &ctx, ok, err);
        });
    }

    /// Install every available upgrade from inside the app.
    ///
    /// Runs `apt-get update && apt-get -y dist-upgrade` as root via `pkexec`
    /// (graphical auth), merging apt's stdout/stderr into `install_log` line by
    /// line so the UI shows a live transcript. Refreshes the update count and
    /// reboot flag when finished.
    pub fn install(&self, ctx: egui::Context) {
        if self.is_installing() || self.is_checking() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.installing = true;
            s.install_done = false;
            s.install_ok = false;
            s.install_log.clear();
            s.install_log
                .push("Requesting administrator access…".to_string());
            s.error = None;
        }
        ctx.request_repaint();

        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            // One authentication for the whole run: refresh lists, then upgrade
            // non-interactively, keeping existing config files on conflicts.
            // `2>&1` folds apt's progress (which it prints to stderr) into the
            // stream we read, so nothing is lost from the live log.
            let script = "apt-get update && \
                 DEBIAN_FRONTEND=noninteractive apt-get -y \
                 -o Dpkg::Options::=--force-confdef \
                 -o Dpkg::Options::=--force-confold \
                 dist-upgrade 2>&1";

            let child = Command::new("pkexec")
                .arg("/bin/sh")
                .arg("-c")
                .arg(script)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();

            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    finish(
                        &state,
                        &ctx,
                        false,
                        Some(format!("Couldn't start updater: {e}")),
                    );
                    return;
                }
            };

            // Stream stdout (which now carries apt's progress too) line by line.
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    push_log(&state, line);
                    ctx.request_repaint();
                }
            }

            // pkexec reports auth failures / dismissal on its own stderr.
            let mut err_txt = String::new();
            if let Some(mut es) = child.stderr.take() {
                let _ = es.read_to_string(&mut err_txt);
            }

            let status = child.wait();
            let ok = matches!(&status, Ok(st) if st.success());
            let err = if ok {
                None
            } else {
                let detail = err_txt.trim();
                Some(if detail.is_empty() {
                    match &status {
                        Ok(st) => format!("Update did not finish ({st}). See the log above."),
                        Err(e) => format!("Update failed to run: {e}"),
                    }
                } else {
                    // pkexec dismissal / wrong password lands here.
                    detail.lines().last().unwrap_or(detail).to_string()
                })
            };

            finish(&state, &ctx, ok, err);
        });
    }
}

/// Append a line to the live install log, trimming the oldest if it grows too
/// large.
fn push_log(state: &Arc<Mutex<Updates>>, line: String) {
    if let Ok(mut s) = state.lock() {
        s.install_log.push(line);
        let len = s.install_log.len();
        if len > LOG_CAP {
            s.install_log.drain(0..len - LOG_CAP);
        }
    }
}

/// Mark an install run finished and re-derive the update counts / reboot flag.
fn finish(state: &Arc<Mutex<Updates>>, ctx: &egui::Context, ok: bool, err: Option<String>) {
    let reboot_required = reboot_required();
    let apt = list_upgradable();
    let snaps = list_snap_refreshes();
    let (apt_failed, snap_failed) = (apt.is_none(), snaps.is_none());
    if let Ok(mut s) = state.lock() {
        s.installing = false;
        s.install_done = true;
        s.install_ok = ok;
        s.checked = true;
        s.check_failed = apt_failed || snap_failed;
        let (count, packages) = apt.unwrap_or_default();
        s.count = count;
        s.packages = packages;
        let snaps = snaps.unwrap_or_default();
        s.snap_count = snaps.len();
        s.snap_packages = snaps;
        s.reboot_required = reboot_required;
        // The install's own error outranks a re-check hiccup in the one
        // message slot; check_failed still flags the stale counts above.
        if let Some(e) = err {
            s.error = Some(e);
        } else if s.error.is_none() {
            s.error = check_error(apt_failed, snap_failed);
        }
    }
    ctx.request_repaint();
}

/// The user-facing message for a failed check, or `None` when both probes ran.
fn check_error(apt_failed: bool, snap_failed: bool) -> Option<String> {
    let what = match (apt_failed, snap_failed) {
        (true, true) => "APT or snap updates",
        (true, false) => "APT updates",
        (false, true) => "snap updates",
        (false, false) => return None,
    };
    Some(format!(
        "Couldn't check for {what} — the tools didn't respond. Counts may be incomplete; try “Check again”."
    ))
}

/// Count and name the packages APT considers upgradable (from cached lists).
/// `None` when the command failed or timed out — the caller must surface that
/// as "can't check", never as zero updates.
fn list_upgradable() -> Option<(usize, Vec<String>)> {
    let mut count = 0usize;
    let mut packages = Vec::new();
    let mut cmd = Command::new("apt");
    cmd.args(["list", "--upgradable"]);
    let out = crate::util::output_timeout(cmd, 15).ok()?;
    if !out.status.success() {
        return None;
    }
    // Lines look like: "pkg/suite 1.2 amd64 [upgradable from: 1.1]".
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((name, _)) = line.split_once('/') {
            let name = name.trim();
            if !name.is_empty() {
                count += 1;
                if packages.len() < 200 {
                    packages.push(name.to_string());
                }
            }
        }
    }
    Some((count, packages))
}

/// Pending snap refreshes as (name, new version). `Some(empty)` when snap is
/// absent or everything is current; `None` when the check itself failed (store
/// unreachable, timeout) — that must read as "can't check", not "up to date".
/// The check contacts the snap store, hence the longer timeout.
fn list_snap_refreshes() -> Option<Vec<(String, String)>> {
    if !crate::util::has_bin("snap") {
        return Some(Vec::new());
    }
    let mut cmd = Command::new("snap");
    cmd.args(["refresh", "--list"]);
    cmd.env("LC_ALL", "C");
    match crate::util::output_timeout(cmd, 30) {
        Ok(out) if out.status.success() => {
            Some(parse_snap_refresh(&String::from_utf8_lossy(&out.stdout)))
        }
        _ => None,
    }
}

/// Parse `snap refresh --list` (columns: Name Version Rev Size Publisher
/// Notes) → (name, version) pairs. "All snaps up to date." goes to stderr, so
/// an up-to-date system yields an empty stdout and an empty result here.
pub(crate) fn parse_snap_refresh(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 6 || cols[0] == "Name" {
                return None;
            }
            crate::cleanup::valid_snap_name(cols[0])
                .then(|| (cols[0].to_string(), cols[1].to_string()))
        })
        .collect()
}

/// Ubuntu drops this flag file when an installed upgrade needs a reboot.
fn reboot_required() -> bool {
    Path::new("/run/reboot-required").exists() || Path::new("/var/run/reboot-required").exists()
}

/// Launch the desktop's graphical updater (tries Software Updater, then GNOME Software).
pub fn open_software_updater() {
    let candidates: [(&str, &[&str]); 2] = [
        ("update-manager", &[]),
        ("gnome-software", &["--mode=updates"]),
    ];
    for (cmd, args) in candidates {
        let ok = Command::new(cmd)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok();
        if ok {
            return;
        }
    }
}

pub fn reboot() {
    let _ = Command::new("systemctl").arg("reboot").spawn();
}

pub fn power_off() {
    let _ = Command::new("systemctl").arg("poweroff").spawn();
}

/// Relaunch this application and exit the current instance.
pub fn restart_app() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).spawn();
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from a real `snap refresh --list` on Ubuntu 24.04.
    const REFRESH_FIXTURE: &str = "\
Name           Version                         Rev   Size    Publisher       Notes
code           e4c7e7b1                        254   493MB   vscode**        classic
cups           2.4.19-2                        1238  50.2MB  openprinting**  -
firefox        153.0.1-2                       8702  270MB   mozilla**       -
gnome-46-2404  0+git.b31ceab-sdk0+git.f0723a0  164   644MB   canonical**     -
pixeltaken     0.14.2b1                        57    132MB   jointoit        -
";

    #[test]
    fn snap_refresh_parser_reads_pending_rows() {
        let snaps = parse_snap_refresh(REFRESH_FIXTURE);
        assert_eq!(snaps.len(), 5);
        assert_eq!(snaps[0], ("code".to_string(), "e4c7e7b1".to_string()));
        assert_eq!(snaps[4], ("pixeltaken".to_string(), "0.14.2b1".to_string()));
    }

    #[test]
    fn snap_refresh_parser_empty_when_up_to_date() {
        // "All snaps up to date." goes to stderr; stdout is empty.
        assert!(parse_snap_refresh("").is_empty());
        // And even if a prose line ever landed on stdout, it can't parse as a row.
        assert!(parse_snap_refresh("All snaps up to date.\n").is_empty());
    }

    #[test]
    fn snap_refresh_parser_skips_header_and_junk() {
        let out = "Name Version Rev Size Publisher Notes\nshort row\nBad;name 1.0 1 1MB pub -\n";
        assert!(parse_snap_refresh(out).is_empty());
    }
}
