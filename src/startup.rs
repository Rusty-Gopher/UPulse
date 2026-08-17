//! Startup Apps + Services for the "Startup & Services" tab.
//!
//! - **Startup apps:** autostart `.desktop` entries from `~/.config/autostart`
//!   and `/etc/xdg/autostart`. Enable/disable is user-level (writes a `Hidden`
//!   flag into `~/.config/autostart`), so no root is needed.
//! - **Services:** systemd services via `systemctl`. Start/Stop runs under
//!   `pkexec`; critical units (display manager, NetworkManager, dbus, systemd-*)
//!   are marked protected and can't be stopped here.
//!
//! Listing runs on a background thread; actions run on their own thread and
//! reload the lists when finished.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct StartupApp {
    pub name: String,
    pub file: String, // basename, e.g. "foo.desktop"
    pub comment: String,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct Service {
    pub unit: String,
    pub description: String,
    pub active: bool,    // running now
    pub enabled: bool,   // starts at boot
    pub protected: bool, // critical — can't be stopped here
}

#[derive(Default)]
pub struct StartupState {
    pub loading: bool,
    pub loaded: bool,
    pub apps: Vec<StartupApp>,
    pub services: Vec<Service>,
    pub busy: bool, // an action is running
    pub error: Option<String>,
}

pub struct StartupMgr {
    pub state: Arc<Mutex<StartupState>>,
}

impl StartupMgr {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(StartupState::default())),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.lock().map(|s| s.busy).unwrap_or(false)
    }

    fn is_loading(&self) -> bool {
        self.state.lock().map(|s| s.loading).unwrap_or(false)
    }

    /// Load startup apps + services on a background thread.
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
            let apps = list_startup();
            let services = list_services();
            if let Ok(mut s) = state.lock() {
                s.apps = apps;
                s.services = services;
                s.loading = false;
                s.loaded = true;
            }
            ctx.request_repaint();
        });
    }

    /// Enable or disable an autostart entry (user-level, no root). Reloads after.
    pub fn toggle_startup(&self, file: String, enable: bool, ctx: egui::Context) {
        if self.is_busy() || self.is_loading() || !valid_desktop_file(&file) {
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.busy = true;
            s.error = None;
        }
        ctx.request_repaint();
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let err = set_startup_hidden(&file, !enable)
                .err()
                .map(|e| format!("Couldn't update {file}: {e}"));
            let apps = list_startup();
            if let Ok(mut s) = state.lock() {
                s.apps = apps;
                s.busy = false;
                s.error = err;
            }
            ctx.request_repaint();
        });
    }

    /// Start or stop a service via `pkexec systemctl`. `action` is "start" or
    /// "stop". Refuses to stop a protected unit. Reloads when finished.
    pub fn service_action(&self, unit: String, action: &'static str, ctx: egui::Context) {
        if self.is_busy() || !valid_unit(&unit) {
            return;
        }
        if action == "stop" && is_critical_unit(&unit) {
            if let Ok(mut s) = self.state.lock() {
                s.error = Some(format!(
                    "{unit} is a critical system service and can't be stopped here."
                ));
            }
            ctx.request_repaint();
            return;
        }
        if let Ok(mut s) = self.state.lock() {
            s.busy = true;
            s.error = None;
        }
        ctx.request_repaint();

        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let out = Command::new("pkexec")
                .args(["systemctl", action, &unit])
                .output();
            let err = match out {
                Ok(o) if o.status.success() => None,
                Ok(o) => {
                    let e = String::from_utf8_lossy(&o.stderr);
                    Some(
                        e.trim()
                            .lines()
                            .last()
                            .unwrap_or("The action didn't complete.")
                            .to_string(),
                    )
                }
                Err(e) => Some(format!("Couldn't run systemctl: {e}")),
            };
            let services = list_services();
            if let Ok(mut s) = state.lock() {
                s.services = services;
                s.busy = false;
                s.error = err;
            }
            ctx.request_repaint();
        });
    }
}

// --- startup apps -----------------------------------------------------------

fn list_startup() -> Vec<StartupApp> {
    let home = std::env::var("HOME").unwrap_or_default();
    let user_dir = format!("{home}/.config/autostart");
    let mut map: BTreeMap<String, StartupApp> = BTreeMap::new();
    // System entries first, then user entries override them by filename.
    for dir in ["/etc/xdg/autostart".to_string(), user_dir] {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                continue;
            }
            let file = match path.file_name().and_then(|n| n.to_str()) {
                Some(f) => f.to_string(),
                None => continue,
            };
            let kv = parse_desktop(&path);
            let name = kv.get("Name").cloned().unwrap_or_else(|| file.clone());
            let comment = kv.get("Comment").cloned().unwrap_or_default();
            let hidden = kv.get("Hidden").map(|v| v == "true").unwrap_or(false);
            let auto_enabled = kv
                .get("X-GNOME-Autostart-enabled")
                .map(|v| v != "false")
                .unwrap_or(true);
            map.insert(
                file.clone(),
                StartupApp {
                    name,
                    file,
                    comment,
                    enabled: !hidden && auto_enabled,
                },
            );
        }
    }
    let mut v: Vec<StartupApp> = map.into_values().collect();
    v.sort_by_key(|a| a.name.to_lowercase());
    v
}

/// Write `Hidden`/`X-GNOME-Autostart-enabled` into a user autostart override,
/// copying the system entry first if there's no user copy yet.
fn set_startup_hidden(file: &str, hidden: bool) -> std::io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let user_dir = format!("{home}/.config/autostart");
    fs::create_dir_all(&user_dir)?;
    let user_path = format!("{user_dir}/{file}");

    let mut content = if Path::new(&user_path).exists() {
        fs::read_to_string(&user_path)?
    } else {
        fs::read_to_string(format!("/etc/xdg/autostart/{file}"))
            .unwrap_or_else(|_| "[Desktop Entry]\nType=Application\n".to_string())
    };
    content = set_desktop_key(&content, "Hidden", if hidden { "true" } else { "false" });
    content = set_desktop_key(
        &content,
        "X-GNOME-Autostart-enabled",
        if hidden { "false" } else { "true" },
    );
    fs::write(&user_path, content)
}

/// Replace (or insert) `key=value` strictly within the `[Desktop Entry]` group
/// of a `.desktop` file, never touching keys in other groups (e.g. Desktop
/// Actions). If there's no `[Desktop Entry]` group, one is prepended.
fn set_desktop_key(content: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}=");
    let kv = format!("{key}={value}");
    let is_header = |l: &str| {
        let t = l.trim();
        t.starts_with('[') && t.ends_with(']')
    };
    let lines: Vec<&str> = content.lines().collect();

    let Some(start) = lines.iter().position(|l| l.trim() == "[Desktop Entry]") else {
        // No main group — create one at the top.
        return format!("[Desktop Entry]\n{kv}\n{content}");
    };
    // The group runs until the next header (or end of file).
    let end = lines[start + 1..]
        .iter()
        .position(|l| is_header(l))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());

    let mut out: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();
    out.push(lines[start].to_string()); // the "[Desktop Entry]" header

    let mut replaced = false;
    for &l in &lines[start + 1..end] {
        if l.starts_with(&prefix) {
            if !replaced {
                out.push(kv.clone());
                replaced = true;
            }
            // drop any duplicate key lines
        } else {
            out.push(l.to_string());
        }
    }
    if !replaced {
        // Insert right after the header (position start+1 in `out`).
        out.insert(start + 1, kv);
    }
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    out.join("\n")
}

fn parse_desktop(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Ok(text) = fs::read_to_string(path) {
        for line in text.lines() {
            // Only the main group's simple keys; good enough for our fields.
            if let Some((k, v)) = line.split_once('=') {
                map.entry(k.trim().to_string())
                    .or_insert_with(|| v.trim().to_string());
            }
        }
    }
    map
}

// --- services ---------------------------------------------------------------

fn list_services() -> Vec<Service> {
    // Boot-enabled state.
    let mut enabled: BTreeMap<String, bool> = BTreeMap::new();
    let mut files = Command::new("systemctl");
    files.args([
        "list-unit-files",
        "--type=service",
        "--no-legend",
        "--no-pager",
        "--plain",
    ]);
    if let Ok(out) = crate::util::output_timeout(files, 15) {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut it = line.split_whitespace();
            if let (Some(unit), Some(state)) = (it.next(), it.next()) {
                enabled.insert(unit.to_string(), state == "enabled");
            }
        }
    }

    // Loaded units + running state + description.
    let mut units = Command::new("systemctl");
    units.args([
        "list-units",
        "--type=service",
        "--all",
        "--no-legend",
        "--no-pager",
        "--plain",
    ]);
    let mut services = Vec::new();
    if let Ok(out) = crate::util::output_timeout(units, 15) {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut it = line.split_whitespace();
            let unit = match it.next() {
                Some(u) if u.ends_with(".service") && !u.contains('@') => u.to_string(),
                _ => continue,
            };
            let _load = it.next();
            let active = it.next() == Some("active");
            let _sub = it.next();
            let description = it.collect::<Vec<_>>().join(" ");
            let en = enabled.get(&unit).copied().unwrap_or(false);
            services.push(Service {
                protected: is_critical_unit(&unit),
                unit,
                description,
                active,
                enabled: en,
            });
        }
    }
    // Running first, then alphabetical.
    services.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| a.unit.to_lowercase().cmp(&b.unit.to_lowercase()))
    });
    services
}

/// Units that would break the session/network/login if stopped here.
fn is_critical_unit(u: &str) -> bool {
    const CRIT: &[&str] = &[
        "gdm.service",
        "gdm3.service",
        "lightdm.service",
        "sddm.service",
        "NetworkManager.service",
        "dbus.service",
        "polkit.service",
        "wpa_supplicant.service",
        "accounts-daemon.service",
    ];
    CRIT.contains(&u)
        || u.starts_with("systemd-")
        || u.starts_with("user@")
        || u.starts_with("user-runtime-dir@")
        || u.starts_with("getty@")
        || u.starts_with("dbus")
}

// --- validation -------------------------------------------------------------

/// A safe autostart basename: `<name>.desktop`, no path separators.
fn valid_desktop_file(file: &str) -> bool {
    file.ends_with(".desktop")
        && !file.is_empty()
        && file.len() < 200
        && !file.contains('/')
        && !file.contains("..")
        && file
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// A plausible systemd unit name (passed as argv, but validate anyway).
fn valid_unit(u: &str) -> bool {
    !u.is_empty()
        && u.len() < 200
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | ':' | '\\'))
}
