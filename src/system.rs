//! Deep, mostly-static system information for the System Info tab.
//!
//! Everything here is read from world-readable sources — `/etc/os-release`,
//! `/proc/{cpuinfo,meminfo}`, `/sys/class/{dmi,thermal,power_supply}`, and
//! `lspci` — so no root is needed. Gathering runs on a background thread (a few
//! small file reads plus one `lspci`), reporting through an `Arc<Mutex<…>>` and
//! asking egui to repaint when the snapshot is ready, exactly like `scan`/`updates`.

use std::fs;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// A single label/value pair shown as a row.
pub type Row = (String, String);

#[derive(Default, Clone)]
pub struct SystemInfo {
    pub os: Vec<Row>,
    pub machine: Vec<Row>,
    pub cpu: Vec<Row>,
    pub memory: Vec<Row>,
    pub gpus: Vec<String>,
    /// Thermal readings: (label, °C).
    pub temps: Vec<(String, f32)>,
    /// Battery, if present: (status, percent).
    pub battery: Option<(String, u8)>,
}

pub struct SystemProbe {
    pub state: Arc<Mutex<Option<SystemInfo>>>,
    running: Arc<Mutex<bool>>,
}

impl SystemProbe {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Gather (or re-gather) the snapshot on a background thread.
    pub fn gather(&self, ctx: egui::Context) {
        {
            let mut r = self.running.lock().unwrap_or_else(|e| e.into_inner());
            if *r {
                return;
            }
            *r = true;
        }
        let state = Arc::clone(&self.state);
        let running = Arc::clone(&self.running);
        std::thread::spawn(move || {
            let info = collect();
            *state.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
            *running.lock().unwrap_or_else(|e| e.into_inner()) = false;
            ctx.request_repaint();
        });
    }
}

fn collect() -> SystemInfo {
    let os_rel = parse_kv("/etc/os-release");

    let os = vec![
        row(
            "Distribution",
            os_rel.get("PRETTY_NAME").cloned().unwrap_or_default(),
        ),
        row(
            "Version",
            os_rel.get("VERSION").cloned().unwrap_or_default(),
        ),
        row("Kernel", read_trim("/proc/sys/kernel/osrelease")),
        row("Architecture", std::env::consts::ARCH.to_string()),
        row("Hostname", read_trim("/proc/sys/kernel/hostname")),
    ];

    let machine = vec![
        row("Vendor", dmi("sys_vendor")),
        row("Model", dmi("product_name")),
        row("Board", dmi("board_name")),
        row("BIOS", bios_line()),
    ];

    let cpu = cpu_rows();
    let memory = mem_rows();
    let gpus = gpus();
    let temps = temps();
    let battery = battery();

    SystemInfo {
        os,
        machine,
        cpu,
        memory,
        gpus,
        temps,
        battery,
    }
}

// --- individual probes ------------------------------------------------------

fn cpu_rows() -> Vec<Row> {
    let text = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut model = String::new();
    let mut mhz = String::new();
    let mut cores = String::new();
    let mut threads = 0usize;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "processor" => threads += 1,
            "model name" if model.is_empty() => model = v.to_string(),
            "cpu MHz" if mhz.is_empty() => mhz = v.to_string(),
            "cpu cores" if cores.is_empty() => cores = v.to_string(),
            _ => {}
        }
    }
    let mut rows = vec![row("Model", model)];
    if !cores.is_empty() {
        rows.push(row("Cores", cores));
    }
    rows.push(row("Threads", threads.to_string()));
    if let Ok(f) = mhz.parse::<f32>() {
        rows.push(row("Clock", format!("{:.0} MHz", f)));
    }
    rows
}

fn mem_rows() -> Vec<Row> {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |key: &str| -> Option<u64> {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix(key) {
                // e.g. "MemTotal:       16283176 kB"
                return rest
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
                    .map(|kb| kb * 1024);
            }
        }
        None
    };
    let mut rows = Vec::new();
    if let Some(b) = get("MemTotal:") {
        rows.push(row("RAM total", crate::theme::fmt_bytes(b)));
    }
    match get("SwapTotal:") {
        Some(b) if b > 0 => rows.push(row("Swap total", crate::theme::fmt_bytes(b))),
        _ => rows.push(row("Swap total", "none".to_string())),
    }
    rows
}

/// Parse `lspci` for display adapters (VGA / 3D / Display controllers).
fn gpus() -> Vec<String> {
    let out = match crate::util::output_timeout(Command::new("lspci"), 8) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| {
            l.contains("VGA compatible controller")
                || l.contains("3D controller")
                || l.contains("Display controller")
        })
        // Line: "01:00.0 VGA compatible controller: NVIDIA ...". Keep the vendor/model tail.
        .filter_map(|l| l.splitn(3, ':').nth(2).map(|s| s.trim().to_string()))
        .collect()
}

/// Thermal zones from `/sys/class/thermal`. Labels use the zone `type`.
fn temps() -> Vec<(String, f32)> {
    let mut out = thermal_zones();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(8); // laptops expose a dozen zones; the hottest few are enough.
    out
}

/// Every thermal zone under `/sys/class/thermal` as (type, °C), unsorted.
/// Cheap enough that `metrics` also samples it once per second for the live
/// temperature graph.
pub(crate) fn thermal_zones() -> Vec<(String, f32)> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir("/sys/class/thermal") else {
        return out;
    };
    for entry in dir.flatten() {
        let p = entry.path();
        if !p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("thermal_zone"))
            .unwrap_or(false)
        {
            continue;
        }
        let label = read_trim(p.join("type").to_str().unwrap_or(""));
        if let Ok(milli) = read_trim(p.join("temp").to_str().unwrap_or("")).parse::<f32>() {
            out.push((label, milli / 1000.0));
        }
    }
    out
}

/// The CPU temperature out of a zone set: the dedicated `x86_pkg_temp` zone
/// when present, otherwise the hottest zone. Readings outside 0–150 °C come
/// from broken ACPI sensors and are ignored; `None` means no usable zone.
pub(crate) fn pick_cpu_temp(zones: &[(String, f32)]) -> Option<f32> {
    let plausible = |t: f32| t > 0.0 && t <= 150.0;
    if let Some(&(_, t)) = zones
        .iter()
        .find(|(l, t)| l == "x86_pkg_temp" && plausible(*t))
    {
        return Some(t);
    }
    zones
        .iter()
        .map(|&(_, t)| t)
        .filter(|&t| plausible(t))
        .fold(None, |acc: Option<f32>, t| {
            Some(acc.map_or(t, |a| a.max(t)))
        })
}

/// The system battery under `/sys/class/power_supply`, if any. Prefers a real
/// `BAT*` supply so peripheral batteries (e.g. a `hidpp` wireless mouse, which
/// also reports `type == Battery`) don't get mistaken for the laptop's.
fn battery() -> Option<(String, u8)> {
    let mut names: Vec<String> = fs::read_dir("/sys/class/power_supply")
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    // BAT0/BAT1 first, then anything else.
    names.sort_by_key(|n| !n.starts_with("BAT"));

    for name in names {
        let dir = format!("/sys/class/power_supply/{name}");
        if read_trim(&format!("{dir}/type")) != "Battery" {
            continue;
        }
        let Ok(pct) = read_trim(&format!("{dir}/capacity")).parse::<u8>() else {
            continue;
        };
        let status = read_trim(&format!("{dir}/status"));
        let status = if status.is_empty() {
            "Unknown".to_string()
        } else {
            status
        };
        return Some((status, pct));
    }
    None
}

// --- small readers ----------------------------------------------------------

fn dmi(field: &str) -> String {
    read_trim(&format!("/sys/class/dmi/id/{field}"))
}

fn bios_line() -> String {
    let ver = dmi("bios_version");
    let date = dmi("bios_date");
    match (ver.is_empty(), date.is_empty()) {
        (true, _) => String::new(),
        (false, true) => ver,
        (false, false) => format!("{ver}  ·  {date}"),
    }
}

fn read_trim(path: &str) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Parse a simple `KEY=value` file (like `/etc/os-release`), stripping quotes.
fn parse_kv(path: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(text) = fs::read_to_string(path) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
            }
        }
    }
    map
}

fn row(label: &str, value: String) -> Row {
    let value = if value.is_empty() {
        "—".to_string()
    } else {
        value
    };
    (label.to_string(), value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(label: &str, t: f32) -> (String, f32) {
        (label.to_string(), t)
    }

    #[test]
    fn cpu_temp_prefers_pkg_zone_even_when_cooler() {
        let zones = [z("acpitz", 70.0), z("x86_pkg_temp", 48.0), z("SEN1", 60.0)];
        assert_eq!(pick_cpu_temp(&zones), Some(48.0));
    }

    #[test]
    fn cpu_temp_falls_back_to_hottest() {
        let zones = [z("acpitz", 45.0), z("TCPU", 52.5), z("SEN1", 40.0)];
        assert_eq!(pick_cpu_temp(&zones), Some(52.5));
    }

    #[test]
    fn cpu_temp_none_without_zones() {
        assert_eq!(pick_cpu_temp(&[]), None);
    }

    #[test]
    fn cpu_temp_ignores_broken_sensors() {
        // -273.1 (unset ACPI) is filtered out; a near-zero sensor loses to max.
        let zones = [z("acpitz", -273.1), z("SEN5", 1.05), z("TCPU", 47.0)];
        assert_eq!(pick_cpu_temp(&zones), Some(47.0));
        let all_junk = [z("acpitz", -273.1), z("SEN5", 200.0)];
        assert_eq!(pick_cpu_temp(&all_junk), None);
        // A junk pkg zone falls through to the hottest plausible zone.
        let junk_pkg = [z("x86_pkg_temp", 0.0), z("TCPU", 41.0)];
        assert_eq!(pick_cpu_temp(&junk_pkg), Some(41.0));
    }
}
