//! Installed-kernel listing and safe removal of old kernels, for the card on
//! the Cleanup tab.
//!
//! `apt-get autoremove` (the Cleanup "Unused packages" target) only removes
//! kernels apt itself considers expendable; this module shows *every*
//! installed kernel and lets the user purge a specific old one. The running
//! kernel and the newest installed kernel are always protected, and the
//! removal worker re-derives that protection from scratch right before acting
//! (fail closed on any read failure) — same backstop idea as `apps::run_multi`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const LOG_CAP: usize = 600;

#[derive(Clone)]
pub struct Kernel {
    /// Kernel release, e.g. "6.17.0-1030-oem" — matches `uname -r`.
    pub version: String,
    /// Installed packages that go with it (image + modules + headers).
    pub packages: Vec<String>,
    /// Summed dpkg Installed-Size in KiB.
    pub size_kb: u64,
    pub running: bool,
    pub newest: bool,
    pub protected: bool,
}

#[derive(Default)]
pub struct KernelsState {
    pub loading: bool,
    pub loaded: bool,
    /// Newest first.
    pub kernels: Vec<Kernel>,
    pub error: Option<String>,

    // A running removal.
    pub busy: bool,
    pub log: Vec<String>,
    pub done: bool,
    pub ok: bool,
}

pub struct Kernels {
    pub state: Arc<Mutex<KernelsState>>,
}

impl Kernels {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(KernelsState::default())),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.lock().map(|s| s.busy).unwrap_or(false)
    }

    fn is_loading(&self) -> bool {
        self.state.lock().map(|s| s.loading).unwrap_or(false)
    }

    /// Enumerate installed kernels on a background thread.
    pub fn load(&self, ctx: egui::Context) {
        if self.is_loading() || self.is_busy() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.loading = true;
            s.error = None;
        }
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let result = enumerate();
            if let Ok(mut s) = state.lock() {
                match result {
                    Ok(kernels) => s.kernels = kernels,
                    Err(e) => {
                        s.kernels = Vec::new(); // fail closed: nothing removable
                        s.error = Some(e);
                    }
                }
                s.loading = false;
                s.loaded = true;
            }
            ctx.request_repaint();
        });
    }

    /// Purge one old kernel's packages via a single `pkexec apt-get purge`,
    /// streaming output. The UI has already two-click confirmed; protection is
    /// still re-derived fresh here as a backstop.
    pub fn remove(&self, version: String, ctx: egui::Context) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.busy = true;
            s.done = false;
            s.ok = false;
            s.log.clear();
            s.log.push(format!("Checking {version} is safe to remove…"));
            s.error = None;
        }
        ctx.request_repaint();

        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            // Backstop: everything re-read from the system, never from the UI.
            let kernels = match enumerate() {
                Ok(k) => k,
                Err(e) => return refuse(&state, &ctx, e),
            };
            let Some(k) = kernels.iter().find(|k| k.version == version) else {
                return refuse(&state, &ctx, format!("{version} is not installed."));
            };
            if k.protected || k.packages.is_empty() {
                return refuse(
                    &state,
                    &ctx,
                    format!("{version} is protected (running or newest) and can't be removed."),
                );
            }
            for p in &k.packages {
                let prefixed = p.starts_with("linux-image-")
                    || p.starts_with("linux-modules")
                    || p.starts_with("linux-headers-");
                if !crate::apps::valid_pkg(p) || !prefixed {
                    return refuse(&state, &ctx, format!("Refusing suspicious package: {p:?}"));
                }
            }

            let joined = k.packages.join(" ");
            push_log(&state, "Requesting administrator access…".to_string());
            ctx.request_repaint();

            // Names validated above, so this interpolation is safe.
            let script = format!(
                "DEBIAN_FRONTEND=noninteractive apt-get -y \
                 -o Dpkg::Options::=--force-confdef \
                 -o Dpkg::Options::=--force-confold \
                 purge {joined} 2>&1"
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
                Err(e) => return refuse(&state, &ctx, format!("Couldn't start apt: {e}")),
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
            let ok = matches!(child.wait(), Ok(st) if st.success());
            let err = if ok {
                None
            } else {
                let detail = err_txt.trim();
                Some(if detail.is_empty() {
                    "The removal didn't finish. See the log above.".to_string()
                } else {
                    detail.lines().last().unwrap_or(detail).to_string()
                })
            };

            // Re-enumerate so the removed kernel disappears from the list.
            let kernels = enumerate().unwrap_or_default();
            if let Ok(mut s) = state.lock() {
                s.kernels = kernels;
                s.busy = false;
                s.done = true;
                s.ok = ok;
                if let Some(e) = err {
                    s.error = Some(e);
                }
            }
            ctx.request_repaint();
        });
    }
}

/// Refuse a removal before anything ran: clear busy, surface the reason.
fn refuse(state: &Arc<Mutex<KernelsState>>, ctx: &egui::Context, msg: String) {
    if let Ok(mut s) = state.lock() {
        s.busy = false;
        s.error = Some(msg);
    }
    ctx.request_repaint();
}

fn push_log(state: &Arc<Mutex<KernelsState>>, line: String) {
    if let Ok(mut s) = state.lock() {
        s.log.push(line);
        let len = s.log.len();
        if len > LOG_CAP {
            s.log.drain(0..len - LOG_CAP);
        }
    }
}

/// Read the running release and installed kernel packages. Any failure is an
/// `Err` — the caller treats that as "nothing removable" (fail closed).
fn enumerate() -> Result<Vec<Kernel>, String> {
    let running = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("Couldn't read the running kernel version: {e}"))?;
    if running.is_empty() {
        return Err("Couldn't determine the running kernel.".into());
    }

    let mut cmd = Command::new("dpkg-query");
    cmd.args([
        "-W",
        "-f=${Package}\t${db:Status-Abbrev}\t${Installed-Size}\n",
        "linux-image-*",
        "linux-modules-*",
        "linux-headers-*",
    ]);
    // dpkg-query exits nonzero when a pattern matches nothing but still prints
    // the rest, so parse stdout regardless of the exit status.
    let out = crate::util::output_timeout(cmd, 15)
        .map_err(|e| format!("Couldn't query installed kernels: {e}"))?;
    let rows = parse_dpkg_rows(&String::from_utf8_lossy(&out.stdout));
    let kernels = build_kernels(&rows, &running);
    if kernels.is_empty() {
        return Err("No installed kernel packages found.".into());
    }
    Ok(kernels)
}

/// dpkg-query lines `package\tstatus-abbrev\tinstalled-size` → tuples.
fn parse_dpkg_rows(out: &str) -> Vec<(String, String, u64)> {
    out.lines()
        .filter_map(|line| {
            let mut it = line.split('\t');
            let pkg = it.next()?.trim();
            let status = it.next()?.trim();
            let size = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            (!pkg.is_empty()).then(|| (pkg.to_string(), status.to_string(), size))
        })
        .collect()
}

/// True for meta/flavor packages (`linux-image-generic`,
/// `linux-headers-oem-24.04d`, …): the part after the prefix doesn't start
/// with a version digit. Metas are never listed as removable kernels.
pub(crate) fn is_meta(pkg: &str) -> bool {
    for prefix in ["linux-image-", "linux-modules-", "linux-headers-"] {
        if let Some(rest) = pkg.strip_prefix(prefix) {
            return !rest.starts_with(|c: char| c.is_ascii_digit());
        }
    }
    false
}

/// Sortable key for kernel releases: every digit run as a number, so
/// `6.8.0-45-generic` → [6, 8, 0, 45] and 45 beats 9 (string compare doesn't).
pub(crate) fn kver_key(v: &str) -> Vec<u64> {
    let mut key = Vec::new();
    let mut cur: Option<u64> = None;
    for c in v.chars() {
        if let Some(d) = c.to_digit(10) {
            cur = Some(cur.unwrap_or(0).saturating_mul(10).saturating_add(d as u64));
        } else if let Some(n) = cur.take() {
            key.push(n);
        }
    }
    if let Some(n) = cur {
        key.push(n);
    }
    key
}

/// `6.8.0-45-generic` → `6.8.0-45`: strip trailing non-numeric flavor
/// segments, for matching `linux-headers-6.8.0-45` style packages.
fn base_version(v: &str) -> &str {
    let mut b = v;
    while let Some(i) = b.rfind('-') {
        if b[i + 1..].starts_with(|c: char| c.is_ascii_digit()) {
            break;
        }
        b = &b[..i];
    }
    b
}

/// Group installed dpkg rows into kernels and mark protection. Only rows in
/// state `ii` count; versions with characters outside `[A-Za-z0-9.-]` are
/// dropped. `protected = running || newest`.
pub(crate) fn build_kernels(rows: &[(String, String, u64)], running: &str) -> Vec<Kernel> {
    let installed: Vec<(&str, u64)> = rows
        .iter()
        .filter(|(_, st, _)| st.trim() == "ii")
        .map(|(p, _, sz)| (p.as_str(), *sz))
        .collect();

    let valid_kver = |v: &str| {
        !v.is_empty()
            && v.len() < 100
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
    };

    let mut versions: Vec<String> = installed
        .iter()
        .filter(|(p, _)| !is_meta(p))
        .filter_map(|(p, _)| p.strip_prefix("linux-image-"))
        .filter(|v| valid_kver(v))
        .map(|v| v.to_string())
        .collect();
    versions.sort();
    versions.dedup();
    if versions.is_empty() {
        return Vec::new();
    }

    let newest = versions
        .iter()
        .max_by(|a, b| kver_key(a).cmp(&kver_key(b)).then(a.cmp(b)))
        .cloned();

    let mut out: Vec<Kernel> = versions
        .iter()
        .map(|v| {
            let base = base_version(v);
            let mut companions = vec![
                format!("linux-image-{v}"),
                format!("linux-modules-{v}"),
                format!("linux-modules-extra-{v}"),
                format!("linux-headers-{v}"),
            ];
            if base != v {
                companions.push(format!("linux-headers-{base}"));
            }
            let mut packages = Vec::new();
            let mut size_kb = 0u64;
            for (p, sz) in &installed {
                if companions.iter().any(|c| c == p) {
                    packages.push(p.to_string());
                    size_kb += sz;
                }
            }
            let is_running = v == running;
            let is_newest = Some(v) == newest.as_ref();
            Kernel {
                version: v.clone(),
                packages,
                size_kb,
                running: is_running,
                newest: is_newest,
                protected: is_running || is_newest,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        kver_key(&b.version)
            .cmp(&kver_key(&a.version))
            .then(b.version.cmp(&a.version))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(p: &str, st: &str, sz: u64) -> (String, String, u64) {
        (p.to_string(), st.to_string(), sz)
    }

    fn fixture() -> Vec<(String, String, u64)> {
        vec![
            row("linux-image-6.8.0-45-generic", "ii", 16000),
            row("linux-modules-6.8.0-45-generic", "ii", 158000),
            row("linux-modules-extra-6.8.0-45-generic", "ii", 120000),
            row("linux-headers-6.8.0-45-generic", "ii", 29000),
            row("linux-headers-6.8.0-45", "ii", 27000),
            row("linux-image-6.8.0-9-generic", "ii", 16000),
            row("linux-modules-6.8.0-9-generic", "ii", 158000),
            row("linux-image-6.5.0-21-generic", "rc", 16000), // config-files only
            row("linux-image-generic-hwe-24.04", "ii", 10),   // meta
            row("linux-headers-generic", "ii", 10),           // meta
        ]
    }

    #[test]
    fn groups_versions_and_flags_protection() {
        let ks = build_kernels(&fixture(), "6.8.0-45-generic");
        assert_eq!(ks.len(), 2); // rc row and metas excluded
                                 // Newest first.
        assert_eq!(ks[0].version, "6.8.0-45-generic");
        assert!(ks[0].running && ks[0].newest && ks[0].protected);
        assert_eq!(ks[0].packages.len(), 5); // image+modules+extra+headers+headers-base
        assert_eq!(ks[0].size_kb, 16000 + 158000 + 120000 + 29000 + 27000);
        assert_eq!(ks[1].version, "6.8.0-9-generic");
        assert!(!ks[1].running && !ks[1].newest && !ks[1].protected);
        assert_eq!(ks[1].packages.len(), 2);
    }

    #[test]
    fn running_old_kernel_is_still_protected() {
        let ks = build_kernels(&fixture(), "6.8.0-9-generic");
        assert!(ks.iter().all(|k| k.protected)); // running old + newest
    }

    #[test]
    fn single_kernel_machines_have_nothing_removable() {
        let rows = vec![
            row("linux-image-6.8.0-45-generic", "ii", 16000),
            row("linux-modules-6.8.0-45-generic", "ii", 158000),
        ];
        let ks = build_kernels(&rows, "6.8.0-45-generic");
        assert_eq!(ks.len(), 1);
        assert!(ks[0].protected);
    }

    #[test]
    fn numeric_ordering_beats_string_ordering() {
        assert!(kver_key("6.8.0-45-generic") > kver_key("6.8.0-9-generic"));
        assert!(kver_key("6.17.0-1030-oem") > kver_key("6.17.0-1028-oem"));
        assert!(kver_key("6.10.0") > kver_key("6.9.9"));
    }

    #[test]
    fn meta_packages_are_recognised() {
        assert!(is_meta("linux-image-generic"));
        assert!(is_meta("linux-image-generic-hwe-24.04"));
        assert!(is_meta("linux-image-oem-24.04d"));
        assert!(is_meta("linux-headers-generic"));
        assert!(!is_meta("linux-image-6.8.0-45-generic"));
        assert!(!is_meta("linux-headers-6.8.0-45"));
        assert!(!is_meta("bash"));
    }

    #[test]
    fn malformed_and_empty_input_fail_closed() {
        assert!(build_kernels(&[], "6.8.0-45-generic").is_empty());
        // A version carrying shell junk is dropped entirely.
        let rows = vec![row("linux-image-6.8.0-45;rm -rf /", "ii", 1)];
        assert!(build_kernels(&rows, "x").is_empty());
    }
}
