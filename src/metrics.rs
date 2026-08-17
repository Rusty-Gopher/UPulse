//! System metrics collection built on top of the `sysinfo` crate.
//!
//! A single [`Metrics`] value owns the persistent `sysinfo` handles and is
//! refreshed once per second by the UI layer. It exposes plain, already-cooked
//! numbers so the rendering code never has to touch `sysinfo` directly.

use std::collections::VecDeque;
use std::time::Instant;

use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

/// Number of samples kept for the live graphs (~2 minutes at 1 Hz).
pub const HISTORY: usize = 120;

/// A fixed-length ring buffer of `f32` samples used to drive the sparklines.
pub struct Ring {
    buf: VecDeque<f32>,
}

impl Ring {
    fn new() -> Self {
        Self {
            buf: VecDeque::from(vec![0.0; HISTORY]),
        }
    }

    fn push(&mut self, v: f32) {
        if self.buf.len() >= HISTORY {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
    }

    /// Samples oldest → newest, for drawing.
    pub fn values(&self) -> Vec<f32> {
        self.buf.iter().copied().collect()
    }

    /// Largest sample currently held (used to auto-scale the network graph).
    pub fn max(&self) -> f32 {
        self.buf.iter().cloned().fold(0.0, f32::max)
    }
}

#[derive(Clone)]
pub struct DiskInfo {
    pub name: String,
    pub mount: String,
    pub fs: String,
    pub kind: String,
    pub total: u64,
    pub used: u64,
    pub removable: bool,
}

#[derive(Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
    pub user: String,
}

pub struct Metrics {
    sys: System,
    networks: Networks,
    users: Users,
    last_update: Instant,
    net_primed: bool, // suppress the bogus first network delta

    // Live scalars.
    pub cpu_global: f32,
    pub per_core: Vec<f32>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub uptime: u64,
    pub load: [f64; 3],    // 1 / 5 / 15 minute load average
    pub net_rx: f32,       // bytes/second
    pub net_tx: f32,       // bytes/second
    pub temp: Option<f32>, // CPU temperature in °C; None = no usable sensor
    temp_supported: bool,  // latches false after an empty read so we stop probing

    // Live collections.
    pub disks: Vec<DiskInfo>,
    pub procs: Vec<ProcInfo>,

    // Rolling history for the graphs.
    pub cpu_hist: Ring,
    pub mem_hist: Ring,
    pub rx_hist: Ring,
    pub tx_hist: Ring,
    pub temp_hist: Ring,
}

impl Metrics {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        let mut m = Self {
            sys,
            networks: Networks::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            last_update: Instant::now(),
            net_primed: false,
            cpu_global: 0.0,
            per_core: Vec::new(),
            mem_total: 0,
            mem_used: 0,
            swap_total: 0,
            swap_used: 0,
            uptime: 0,
            load: [0.0; 3],
            net_rx: 0.0,
            net_tx: 0.0,
            temp: None,
            temp_supported: true,
            disks: Vec::new(),
            procs: Vec::new(),
            cpu_hist: Ring::new(),
            mem_hist: Ring::new(),
            rx_hist: Ring::new(),
            tx_hist: Ring::new(),
            temp_hist: Ring::new(),
        };
        m.refresh_fast();
        m.refresh_disks();
        m.refresh_procs();
        m
    }

    /// Fraction of RAM in use, `0.0..=1.0`.
    pub fn mem_frac(&self) -> f32 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f32 / self.mem_total as f32
        }
    }

    /// Fraction of swap in use, `0.0..=1.0`.
    pub fn swap_frac(&self) -> f32 {
        if self.swap_total == 0 {
            0.0
        } else {
            self.swap_used as f32 / self.swap_total as f32
        }
    }

    /// Refresh the cheap, always-visible metrics (CPU, memory, network, load)
    /// and push the history samples. Safe to call every second.
    pub fn refresh_fast(&mut self) {
        let dt = self.last_update.elapsed().as_secs_f32().max(0.001);
        self.last_update = Instant::now();

        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();

        self.cpu_global = self.sys.global_cpu_usage();
        self.per_core = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
        self.mem_total = self.sys.total_memory();
        self.mem_used = self.sys.used_memory();
        self.swap_total = self.sys.total_swap();
        self.swap_used = self.sys.used_swap();
        self.uptime = System::uptime();
        let la = System::load_average();
        self.load = [la.one, la.five, la.fifteen];

        // Network throughput = bytes seen since the previous refresh / elapsed.
        self.networks.refresh();
        let (mut rx, mut tx) = (0u64, 0u64);
        for data in self.networks.values() {
            rx += data.received();
            tx += data.transmitted();
        }
        self.net_rx = rx as f32 / dt;
        self.net_tx = tx as f32 / dt;
        // The first sample's delta spans from list creation and produces a bogus
        // spike that pins the graph's autoscale; drop it.
        if !self.net_primed {
            self.net_rx = 0.0;
            self.net_tx = 0.0;
            self.net_primed = true;
        }

        // CPU temperature from sysfs — a handful of tiny reads. On machines
        // without thermal zones the first read comes back empty and the latch
        // stops us re-probing every second.
        if self.temp_supported {
            let zones = crate::system::thermal_zones();
            self.temp = crate::system::pick_cpu_temp(&zones);
            match self.temp {
                Some(t) => self.temp_hist.push(t),
                None => self.temp_supported = false,
            }
        }

        self.cpu_hist.push(self.cpu_global);
        self.mem_hist.push(self.mem_frac() * 100.0);
        self.rx_hist.push(self.net_rx);
        self.tx_hist.push(self.net_tx);
    }

    /// Re-scan the mounted filesystems. Cheap-ish, but only needs to run
    /// occasionally since mounts and free space change slowly.
    pub fn refresh_disks(&mut self) {
        let disks = Disks::new_with_refreshed_list();
        self.disks = disks
            .iter()
            .map(|d| {
                let total = d.total_space();
                let avail = d.available_space();
                DiskInfo {
                    name: d.name().to_string_lossy().to_string(),
                    mount: d.mount_point().to_string_lossy().to_string(),
                    fs: d.file_system().to_string_lossy().to_string(),
                    kind: format!("{:?}", d.kind()),
                    total,
                    used: total.saturating_sub(avail),
                    removable: d.is_removable(),
                }
            })
            .collect();
    }

    /// Enumerate every process. This is the expensive one (reads `/proc` for
    /// thousands of PIDs), so only call it while the process list is on screen.
    pub fn refresh_procs(&mut self) {
        // Only refresh what the table shows (CPU / memory / user) instead of a
        // full per-process /proc walk — this is the expensive periodic burst,
        // and the extra fields (disk I/O, cmdline, environ, tasks) went unused.
        let kind = ProcessRefreshKind::new()
            .with_cpu()
            .with_memory()
            .with_user(UpdateKind::OnlyIfNotSet);
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        self.procs.clear();
        self.procs.reserve(self.sys.processes().len());
        for p in self.sys.processes().values() {
            let user = p
                .user_id()
                .and_then(|uid| self.users.get_user_by_id(uid))
                .map(|u| u.name().to_string())
                .unwrap_or_else(|| "—".into());
            self.procs.push(ProcInfo {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().to_string(),
                cpu: p.cpu_usage(),
                mem: p.memory(),
                user,
            });
        }
    }
}
