//! Installed-app management and package install/remove for the Apps tab (APT).
//!
//! Listing runs `dpkg-query` (no root) on a background thread; searching runs
//! `apt-cache search`. Installing/removing runs `apt-get` under `pkexec`
//! (one graphical auth) with output streamed live into a bounded log — the same
//! pattern as `updates.rs`. Everything heavy happens off the UI thread.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Keep the live action log bounded so a chatty apt run can't grow forever.
const LOG_CAP: usize = 600;
/// Cap search results so a broad query can't flood the list.
const SEARCH_CAP: usize = 300;

#[derive(Clone)]
pub struct Pkg {
    pub name: String,
    /// Installed size in KiB (0 when unknown, e.g. for search results).
    pub size_kb: u64,
    pub summary: String,
    /// Critical to the OS — shown read-only, never removable from the app.
    pub protected: bool,
}

#[derive(Default)]
pub struct AppsState {
    // Installed list.
    pub loading: bool,
    pub loaded: bool,
    pub installed: Vec<Pkg>,
    pub total_size_kb: u64,
    pub error: Option<String>,

    // Repository search.
    pub searching: bool,
    pub searched: bool,
    pub results: Vec<Pkg>,

    // A running install/remove (via pkexec).
    pub busy: bool,
    pub action_title: String,
    pub log: Vec<String>,
    pub action_done: bool,
    pub action_ok: bool,
}

pub struct Apps {
    pub state: Arc<Mutex<AppsState>>,
}

impl Apps {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(AppsState::default())),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.lock().map(|s| s.busy).unwrap_or(false)
    }

    fn is_loading(&self) -> bool {
        self.state.lock().map(|s| s.loading).unwrap_or(false)
    }

    fn is_searching(&self) -> bool {
        self.state.lock().map(|s| s.searching).unwrap_or(false)
    }

    /// Enumerate installed packages via `dpkg-query` (no root), sorted biggest
    /// first so "what's taking space" is answered immediately.
    pub fn load_installed(&self, ctx: egui::Context) {
        if self.is_loading() || self.is_busy() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.loading = true;
            s.error = None;
        }
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let (installed, total) = list_installed();
            if let Ok(mut s) = state.lock() {
                s.total_size_kb = total;
                if installed.is_empty() {
                    s.error = Some("Couldn't read installed packages (is dpkg available?)".into());
                }
                s.installed = installed;
                s.loading = false;
                s.loaded = true;
            }
            ctx.request_repaint();
        });
    }

    /// Search APT's package index for `query` via `apt-cache search`.
    pub fn search(&self, query: String, ctx: egui::Context) {
        let query = query.trim().to_string();
        if query.is_empty() || self.is_searching() || self.is_busy() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.searching = true;
        }
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let results = search_repo(&query);
            if let Ok(mut s) = state.lock() {
                s.results = results;
                s.searching = false;
                s.searched = true;
            }
            ctx.request_repaint();
        });
    }

    /// Install or remove one package under `pkexec`, streaming apt's output.
    pub fn run(&self, verb: &'static str, pkg: String, ctx: egui::Context) {
        self.run_multi(verb, vec![pkg], ctx);
    }

    /// Install or remove several packages in a single `pkexec apt-get` run,
    /// streaming output. Every name is protection-checked (for removes) and
    /// shell-validated before it's interpolated. Refreshes the installed list
    /// when finished.
    pub fn run_multi(&self, verb: &'static str, pkgs: Vec<String>, ctx: egui::Context) {
        if self.is_busy() || self.is_loading() || pkgs.is_empty() {
            return;
        }
        for pkg in &pkgs {
            // Backstop: never remove a protected package, even if some future UI
            // path tries to. Re-derives protection authoritatively so it matches
            // exactly what the UI marks read-only.
            if verb == "remove" && pkg_protected(pkg) {
                if let Ok(mut s) = self.state.lock() {
                    s.error = Some(format!(
                        "{pkg} is a protected system package and can't be removed here."
                    ));
                }
                ctx.request_repaint();
                return;
            }
            // Defend the shell interpolation below: reject anything that isn't a
            // plausible package name (letters, digits, and `.+-:~`).
            if !valid_pkg(pkg) {
                if let Ok(mut s) = self.state.lock() {
                    s.error = Some(format!(
                        "Refusing to act on suspicious package name: {pkg:?}"
                    ));
                }
                ctx.request_repaint();
                return;
            }
        }

        let verb_ing = if verb == "remove" {
            "Removing"
        } else {
            "Installing"
        };
        let noun = if pkgs.len() == 1 {
            pkgs[0].clone()
        } else {
            format!("{} packages", pkgs.len())
        };
        let title = format!("{verb_ing} {noun}…");
        let joined = pkgs.join(" ");
        if let Ok(mut s) = self.state.lock() {
            s.busy = true;
            s.action_done = false;
            s.action_ok = false;
            s.action_title = title;
            s.log.clear();
            s.log.push("Requesting administrator access…".to_string());
            s.error = None;
        }
        ctx.request_repaint();

        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            // All names are validated above, so this interpolation is safe.
            // `2>&1` folds apt's progress (stderr) into the stream we read live.
            let script = format!(
                "DEBIAN_FRONTEND=noninteractive apt-get -y \
                 -o Dpkg::Options::=--force-confdef \
                 -o Dpkg::Options::=--force-confold \
                 {verb} {joined} 2>&1"
            );

            let child = Command::new("pkexec")
                .arg("/bin/sh")
                .arg("-c")
                .arg(&script)
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
                        Some(format!("Couldn't start apt: {e}")),
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

            // pkexec reports auth failure / dismissal on its own stderr.
            let mut err_txt = String::new();
            if let Some(mut es) = child.stderr.take() {
                use std::io::Read;
                let _ = es.read_to_string(&mut err_txt);
            }

            let status = child.wait();
            let ok = matches!(&status, Ok(st) if st.success());
            let err = if ok {
                None
            } else {
                let detail = err_txt.trim();
                Some(if detail.is_empty() {
                    "The operation didn't finish. See the log above.".to_string()
                } else {
                    detail.lines().last().unwrap_or(detail).to_string()
                })
            };
            finish(&state, &ctx, ok, err);
        });
    }
}

/// Append a line to the live log, trimming the oldest if it grows too large.
fn push_log(state: &Arc<Mutex<AppsState>>, line: String) {
    if let Ok(mut s) = state.lock() {
        s.log.push(line);
        let len = s.log.len();
        if len > LOG_CAP {
            s.log.drain(0..len - LOG_CAP);
        }
    }
}

/// Mark an action finished and refresh the installed list from disk.
fn finish(state: &Arc<Mutex<AppsState>>, ctx: &egui::Context, ok: bool, err: Option<String>) {
    let (installed, total) = list_installed();
    if let Ok(mut s) = state.lock() {
        s.busy = false;
        s.action_done = true;
        s.action_ok = ok;
        if !installed.is_empty() {
            s.installed = installed;
            s.total_size_kb = total;
        }
        if let Some(e) = err {
            s.error = Some(e);
        }
    }
    ctx.request_repaint();
}

/// Parse `dpkg-query` for installed packages: (list, total installed KiB).
fn list_installed() -> (Vec<Pkg>, u64) {
    let mut cmd = Command::new("dpkg-query");
    cmd.args([
        "-W",
        "-f=${db:Status-Abbrev}\t${Package}\t${Installed-Size}\t${Essential}\t${Priority}\t${binary:Summary}\n",
    ]);
    let out = crate::util::output_timeout(cmd, 15);
    let mut pkgs = Vec::new();
    let mut total = 0u64;
    if let Ok(out) = out {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut parts = line.splitn(6, '\t');
            let status = parts.next().unwrap_or("");
            // "ii " = installed and configured. Skip half-installed / config-only.
            if !status.starts_with("ii") {
                continue;
            }
            let name = parts.next().unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let size_kb = parts
                .next()
                .unwrap_or("")
                .trim()
                .parse::<u64>()
                .unwrap_or(0);
            let essential = parts.next().unwrap_or("").trim();
            let priority = parts.next().unwrap_or("").trim();
            let summary = parts.next().unwrap_or("").to_string();
            let protected = essential == "yes" || priority == "required" || is_critical(&name);
            total += size_kb;
            pkgs.push(Pkg {
                name,
                size_kb,
                summary,
                protected,
            });
        }
    }
    pkgs.sort_by(|a, b| b.size_kb.cmp(&a.size_kb).then_with(|| a.name.cmp(&b.name)));
    (pkgs, total)
}

/// OS-critical packages that Ubuntu often marks merely "optional" but whose
/// removal would break boot, hardware, login, or the ability to fix the system.
/// Over-inclusion is deliberate: the cost of protecting one extra package is far
/// lower than the cost of letting someone delete their bootloader.
pub fn is_critical(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "libc-bin",
        "udev",
        "init",
        "init-system-helpers",
        "sudo",
        "pkexec",
        "gdm3",
        "lightdm",
        "sddm",
        "gnome-shell",
        "gnome-session",
        "mutter",
        "ubuntu-session",
        "ubuntu-desktop",
        "ubuntu-desktop-minimal",
        "network-manager",
        "netplan.io",
        "efibootmgr",
        "intel-microcode",
        "amd64-microcode",
    ];
    const PREFIX: &[&str] = &[
        "linux-image",
        "linux-headers",
        "linux-modules",
        "linux-generic",
        "linux-firmware",
        "linux-oem",
        "linux-lowlatency",
        "grub",
        "shim",
        "libc6",
        "libgcc",
        "libstdc++",
        "systemd",
        "libpam",
        "libapt",
        "apt",
        "dpkg",
        "xserver-xorg",
        "polkit",
        "policykit",
    ];
    EXACT.contains(&name) || PREFIX.iter().any(|p| name.starts_with(p))
}

/// Authoritative protection check for the removal backstop: the name-pattern
/// criticals plus dpkg's own `Essential: yes` / `Priority: required` flags.
/// Matches the `protected` field computed in `list_installed`.
fn pkg_protected(name: &str) -> bool {
    if is_critical(name) {
        return true;
    }
    let mut cmd = Command::new("dpkg-query");
    cmd.args(["-W", "-f=${Essential}\t${Priority}", name]);
    match crate::util::output_timeout(cmd, 10) {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut fields = text.split('\t');
            let essential = fields.next().unwrap_or("").trim();
            let priority = fields.next().unwrap_or("").trim();
            essential == "yes" || priority == "required"
        }
        // Fail closed: if we can't verify, treat as protected rather than
        // risk removing an Essential/required package.
        Err(_) => true,
    }
}

/// Parse `apt-cache search` output ("name - summary") and rank by relevance to
/// `query` so the most on-point packages (exact / prefix / substring name
/// matches) float to the top — `apt-cache` itself returns them unordered and
/// name-matches are buried among description matches.
fn search_repo(query: &str) -> Vec<Pkg> {
    let q = query.to_lowercase();
    let mut cmd = Command::new("apt-cache");
    cmd.args(["search", query]);
    let out = crate::util::output_timeout(cmd, 15);
    let mut pkgs = Vec::new();
    if let Ok(out) = out {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((name, summary)) = line.split_once(" - ") {
                pkgs.push(Pkg {
                    name: name.trim().to_string(),
                    size_kb: 0,
                    summary: summary.trim().to_string(),
                    protected: false,
                });
                if pkgs.len() >= SEARCH_CAP {
                    break;
                }
            }
        }
    }
    pkgs.sort_by(|a, b| {
        rank(&a.name, &q)
            .cmp(&rank(&b.name, &q))
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.name.cmp(&b.name))
    });
    pkgs
}

/// Lower is better: exact name (0) < prefix (1) < substring (2) < desc-only (3).
fn rank(name: &str, q: &str) -> u8 {
    let n = name.to_lowercase();
    if n == q {
        0
    } else if n.starts_with(q) {
        1
    } else if n.contains(q) {
        2
    } else {
        3
    }
}

/// Package names are `[a-z0-9][a-z0-9.+-]*` optionally with `:arch`; allow that
/// set so a name can never carry shell metacharacters into the script above.
/// Shared with `kernels`, which interpolates package names the same way.
pub(crate) fn valid_pkg(p: &str) -> bool {
    !p.is_empty()
        && p.len() < 200
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | ':' | '~'))
}
