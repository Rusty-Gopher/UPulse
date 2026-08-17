//! APT repository / PPA manager for the Sources tab.
//!
//! Lists apt sources from `/etc/apt/sources.list`, `sources.list.d/*.list`
//! (one-line format) and `sources.list.d/*.sources` (deb822). Adds a PPA via
//! `pkexec add-apt-repository` and removes third-party sources; the base Ubuntu
//! repositories are marked protected and can't be removed here.

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const LOG_CAP: usize = 600;

#[derive(Clone)]
pub struct Repo {
    pub label: String, // "ppa:user/name" or "URI suite"
    pub file: String,  // basename of the file it lives in
    pub ppa_spec: Option<String>,
    pub protected: bool, // base Ubuntu repos — read-only
}

impl Repo {
    pub fn key(&self) -> String {
        format!("{}|{}", self.file, self.label)
    }
}

#[derive(Default)]
pub struct RepoState {
    pub loading: bool,
    pub loaded: bool,
    pub repos: Vec<Repo>,
    pub error: Option<String>,

    // A running add/remove.
    pub busy: bool,
    pub log: Vec<String>,
    pub done: bool,
    pub ok: bool,
}

pub struct Repos {
    pub state: Arc<Mutex<RepoState>>,
}

impl Repos {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RepoState::default())),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.lock().map(|s| s.busy).unwrap_or(false)
    }

    fn is_loading(&self) -> bool {
        self.state.lock().map(|s| s.loading).unwrap_or(false)
    }

    /// List apt sources on a background thread.
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
            let repos = list_repos();
            if let Ok(mut s) = state.lock() {
                s.repos = repos;
                s.loading = false;
                s.loaded = true;
            }
            ctx.request_repaint();
        });
    }

    /// Add a PPA (e.g. `ppa:user/name`) via `pkexec add-apt-repository`.
    pub fn add_ppa(&self, spec: String, ctx: egui::Context) {
        let spec = spec.trim().to_string();
        if self.is_busy() || self.is_loading() {
            return;
        }
        if !valid_ppa(&spec) {
            self.fail("Enter a PPA like ppa:user/name.");
            ctx.request_repaint();
            return;
        }
        self.run(
            format!("Adding {spec}…"),
            vec!["add-apt-repository".into(), "-y".into(), spec],
            ctx,
        );
    }

    /// Remove a source: PPAs via `add-apt-repository --remove`, other
    /// third-party files by deleting them. Base Ubuntu repos are refused.
    pub fn remove(&self, repo: Repo, ctx: egui::Context) {
        if self.is_busy() || self.is_loading() {
            return;
        }
        if repo.protected {
            self.fail("That's a base Ubuntu repository and can't be removed here.");
            ctx.request_repaint();
            return;
        }
        let args = if let Some(spec) = &repo.ppa_spec {
            // PPAs are removed by spec — never fall through to deleting a file.
            if !valid_ppa(spec) {
                self.fail("Couldn't parse that PPA well enough to remove it safely.");
                ctx.request_repaint();
                return;
            }
            vec![
                "add-apt-repository".into(),
                "-y".into(),
                "--remove".into(),
                spec.clone(),
            ]
        } else {
            // Non-PPA: we delete the whole file, so only do it when the file
            // holds this single, non-protected entry — otherwise we'd silently
            // take sibling (or protected base) repos with it.
            if !valid_source_file(&repo.file) {
                self.fail("Couldn't determine how to remove that source safely.");
                ctx.request_repaint();
                return;
            }
            let siblings: Vec<Repo> = list_repos()
                .into_iter()
                .filter(|r| r.file == repo.file)
                .collect();
            if siblings.len() > 1 || siblings.iter().any(|r| r.protected) {
                self.fail(&format!(
                    "{} defines other repositories too — edit it manually to remove just this one.",
                    repo.file
                ));
                ctx.request_repaint();
                return;
            }
            vec![
                "rm".into(),
                "-f".into(),
                format!("/etc/apt/sources.list.d/{}", repo.file),
            ]
        };
        self.run(format!("Removing {}…", repo.label), args, ctx);
    }

    fn fail(&self, msg: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.error = Some(msg.to_string());
        }
    }

    /// Run a privileged apt-sources command under pkexec, streaming output.
    fn run(&self, title: String, args: Vec<String>, ctx: egui::Context) {
        if let Ok(mut s) = self.state.lock() {
            s.busy = true;
            s.done = false;
            s.ok = false;
            s.log.clear();
            s.log.push(title);
            s.log.push("Requesting administrator access…".to_string());
            s.error = None;
        }
        ctx.request_repaint();

        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            // args are validated (ppa/file) before we get here; pass as argv.
            let child = Command::new("pkexec")
                .arg("--")
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    finish(&state, &ctx, false, Some(format!("Couldn't start: {e}")));
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
                use std::io::Read;
                let _ = es.read_to_string(&mut err_txt);
            }
            let ok = matches!(child.wait(), Ok(st) if st.success());
            let err = if ok {
                None
            } else {
                let d = err_txt.trim();
                Some(if d.is_empty() {
                    "The operation didn't finish. See the log above.".to_string()
                } else {
                    d.lines().last().unwrap_or(d).to_string()
                })
            };
            finish(&state, &ctx, ok, err);
        });
    }
}

fn push_log(state: &Arc<Mutex<RepoState>>, line: String) {
    if let Ok(mut s) = state.lock() {
        s.log.push(line);
        let len = s.log.len();
        if len > LOG_CAP {
            s.log.drain(0..len - LOG_CAP);
        }
    }
}

fn finish(state: &Arc<Mutex<RepoState>>, ctx: &egui::Context, ok: bool, err: Option<String>) {
    let repos = list_repos();
    if let Ok(mut s) = state.lock() {
        s.repos = repos;
        s.busy = false;
        s.done = true;
        s.ok = ok;
        if let Some(e) = err {
            s.error = Some(e);
        }
    }
    ctx.request_repaint();
}

// --- listing ----------------------------------------------------------------

fn list_repos() -> Vec<Repo> {
    let mut repos: Vec<Repo> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push = |label: String, file: String| {
        let uri_lc = label.to_lowercase();
        let protected = file == "sources.list"
            || file == "ubuntu.sources"
            || uri_lc.contains("archive.ubuntu.com")
            || uri_lc.contains("security.ubuntu.com")
            || uri_lc.contains("ports.ubuntu.com")
            || uri_lc.contains("archive.canonical.com");
        let ppa_spec = ppa_from(&label);
        let key = format!("{file}|{label}");
        if seen.insert(key) && !label.is_empty() {
            repos.push(Repo {
                label,
                file,
                ppa_spec,
                protected,
            });
        }
    };

    // One-line format: sources.list + *.list
    for path in one_line_files() {
        let file = basename(&path);
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some(label) = parse_one_line(line) {
                    push(label, file.clone());
                }
            }
        }
    }

    // deb822 format: *.sources
    for path in glob_dir("/etc/apt/sources.list.d", ".sources") {
        let file = basename(&path);
        if let Ok(text) = fs::read_to_string(&path) {
            for label in parse_deb822(&text) {
                push(label, file.clone());
            }
        }
    }

    repos.sort_by(|a, b| {
        a.protected
            .cmp(&b.protected)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    repos
}

fn one_line_files() -> Vec<String> {
    let mut v = vec!["/etc/apt/sources.list".to_string()];
    v.extend(glob_dir("/etc/apt/sources.list.d", ".list"));
    v
}

fn glob_dir(dir: &str, ext: &str) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.to_string_lossy().ends_with(ext) {
                v.push(p.to_string_lossy().to_string());
            }
        }
    }
    v.sort();
    v
}

/// "deb [opts] URI suite comps" → "URI suite" (skips deb-src to de-dup).
fn parse_one_line(line: &str) -> Option<String> {
    let mut it = line.split_whitespace();
    match it.next()? {
        "deb" => {}
        _ => return None, // skip deb-src and anything else
    }
    let mut uri = None;
    let mut suite = None;
    for tok in it {
        if tok.starts_with('[') || tok.ends_with(']') {
            continue; // apt options like [arch=amd64 signed-by=...]
        }
        if uri.is_none() {
            uri = Some(tok);
        } else {
            suite = Some(tok);
            break;
        }
    }
    Some(
        format!("{} {}", uri?, suite.unwrap_or(""))
            .trim()
            .to_string(),
    )
}

/// Parse deb822 blocks → one "URI suite" label per enabled source.
fn parse_deb822(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut uris = None;
        let mut suites = None;
        let mut enabled = true;
        for line in block.lines() {
            if let Some((k, v)) = line.split_once(':') {
                match k.trim().to_lowercase().as_str() {
                    "uris" => uris = v.split_whitespace().next().map(str::to_string),
                    "suites" => suites = v.split_whitespace().next().map(str::to_string),
                    "enabled" => enabled = v.trim().to_lowercase() != "no",
                    _ => {}
                }
            }
        }
        if enabled {
            if let Some(u) = uris {
                out.push(
                    format!("{} {}", u, suites.unwrap_or_default())
                        .trim()
                        .to_string(),
                );
            }
        }
    }
    out
}

/// Derive `ppa:user/name` from a Launchpad PPA URI, if it is one.
fn ppa_from(label: &str) -> Option<String> {
    let lc = label.to_lowercase();
    if !lc.contains("ppa.launchpad") {
        return None;
    }
    // e.g. "https://ppa.launchpadcontent.net/user/name/ubuntu noble"
    let uri = label.split_whitespace().next()?;
    let after = uri
        .split("launchpadcontent.net/")
        .nth(1)
        .or_else(|| uri.split("launchpad.net/").nth(1))?;
    let mut parts = after.split('/');
    let user = parts.next()?;
    let name = parts.next()?;
    if user.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("ppa:{user}/{name}"))
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

// --- validation -------------------------------------------------------------

fn valid_ppa(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("ppa:") {
        if let Some((user, name)) = rest.split_once('/') {
            let ok = |p: &str| {
                !p.is_empty()
                    && p.len() < 100
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            };
            return ok(user) && ok(name);
        }
    }
    false
}

fn valid_source_file(file: &str) -> bool {
    !file.is_empty()
        && file.len() < 200
        && !file.contains('/')
        && !file.contains("..")
        && (file.ends_with(".list") || file.ends_with(".sources"))
        && file
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | ':'))
}
