//! The egui application: layout, panels, and per-frame rendering.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{pos2, Align, Color32, CornerRadius, Layout, Pos2, RichText, Sense, Shape, Stroke};

use crate::apps::{Apps, Pkg};
use crate::cleanup::{CleanTarget, Cleanup};
use crate::icons;
use crate::kernels::Kernels;
use crate::metrics::{Metrics, Ring};
use crate::repos::{Repo, Repos};
use crate::scan::Scanner;
use crate::startup::{Service, StartupApp, StartupMgr};
use crate::system::SystemProbe;
use crate::theme::{self, ACCENT, BAD, CYAN, GOOD, MUTED, TEXT, VIOLET, WARN};
use crate::updates::{self, UpdatesChecker};

const REFRESH: Duration = Duration::from_millis(1000); // focused: fast metrics
const IDLE_REFRESH: Duration = Duration::from_millis(3000); // unfocused: back off
const DISK_REFRESH: Duration = Duration::from_millis(8000); // mounts change slowly
const PROC_REFRESH: Duration = Duration::from_millis(2000); // expensive /proc walk
/// Shared scroll height for the Large Files / Processes tables.
const TABLE_H: f32 = 440.0;
/// Scan-result table geometry (Large Files / Build Artifacts).
const SCAN_SIZE_W: f32 = 74.0;
const SCAN_NAME_W: f32 = 300.0;
/// Max width for a tab's content column. Every tab is capped and centered to
/// this so panels don't stretch edge-to-edge (and look ragged) on wide monitors,
/// and so action buttons stay next to their rows.
const CONTENT_W: f32 = 1400.0;
/// Inner content height of an app/file row card (name + summary line).
const ROW_H: f32 = 40.0;
/// Virtualised slot height for a row = card (ROW_H + 16 margin) + gap.
const ROW_SLOT: f32 = 62.0;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Overview,
    Performance,
    Storage,
    Apps,
    Cleanup,
    Startup,
    Sources,
    SystemInfo,
    Updates,
}

/// Startup tab sub-view.
#[derive(PartialEq, Clone, Copy)]
enum StartupMode {
    Apps,
    Services,
}

/// Performance tab sub-view: live graphs, or the process table.
#[derive(PartialEq, Clone, Copy)]
enum PerfMode {
    Graphs,
    Processes,
}

/// Storage tab sub-view: the large-file scanner, or the build artifacts it
/// found — each gets the full table height instead of splitting the screen.
#[derive(PartialEq, Clone, Copy)]
enum StorageMode {
    Files,
    Artifacts,
}

#[derive(PartialEq, Clone, Copy)]
enum Sort {
    Cpu,
    Mem,
    Name,
    Pid,
}

/// Apps tab sub-view: manage what's installed, or find something to install.
#[derive(PartialEq, Clone, Copy)]
enum AppsMode {
    Installed,
    Install,
}

/// A hard-to-reverse action awaiting an explicit second click to confirm.
#[derive(PartialEq, Clone, Copy)]
enum Confirm {
    Reboot,
    PowerOff,
    RestartApp,
}

pub struct App {
    metrics: Metrics,
    last_fast: Instant,
    last_disk: Instant,
    last_proc: Instant,
    tab: Tab,
    perf_mode: PerfMode,
    storage_mode: StorageMode,
    sort: Sort,
    filter: String,
    scanner: Scanner,
    threshold_mb: u64,
    updates: UpdatesChecker,
    updates_kicked: bool,
    system: SystemProbe,
    system_kicked: bool,
    apps: Apps,
    apps_kicked: bool,
    apps_mode: AppsMode,
    apps_filter: String,
    apps_query: String,
    apps_confirm: Option<String>,
    apps_selected: HashSet<String>,
    apps_bulk_confirm: bool,
    cleanup: Cleanup,
    cleanup_kicked: bool,
    cleanup_selected: HashSet<String>,
    cleanup_confirm: bool,
    kernels: Kernels,
    kernel_confirm: Option<String>,
    artifact_confirm: Option<String>,
    artifact_all_confirm: bool,
    startup: StartupMgr,
    startup_kicked: bool,
    startup_mode: StartupMode,
    startup_filter: String,
    svc_confirm: Option<String>,
    repos: Repos,
    repos_kicked: bool,
    repos_input: String,
    repos_confirm: Option<String>,
    scan_confirm: Option<String>,
    proc_selected: Option<u32>,
    proc_confirm: Option<u32>,
    confirm: Option<Confirm>,
    bin_size: u64,
    /// Whether apt + pkexec are present — the Apps/Updates actions need them.
    pkg_ok: bool,
    // Production surfaces.
    theme_light: bool,
    palette_open: bool,
    palette_query: String,
    palette_sel: usize,
    first_run: bool,
    toasts: Vec<Toast>,
}

/// A transient bottom-right notification with a time-to-live.
struct Toast {
    born: Instant,
    accent: Color32,
    glyph: &'static str,
    title: String,
    detail: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        let now = Instant::now();
        let bin_size = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        Self {
            metrics: Metrics::new(),
            last_fast: now,
            last_disk: now,
            last_proc: now,
            tab: Tab::Overview,
            perf_mode: PerfMode::Graphs,
            storage_mode: StorageMode::Files,
            sort: Sort::Cpu,
            filter: String::new(),
            scanner: Scanner::new(),
            threshold_mb: 100,
            updates: UpdatesChecker::new(),
            updates_kicked: false,
            system: SystemProbe::new(),
            system_kicked: false,
            apps: Apps::new(),
            apps_kicked: false,
            apps_mode: AppsMode::Installed,
            apps_filter: String::new(),
            apps_query: String::new(),
            apps_confirm: None,
            apps_selected: HashSet::new(),
            apps_bulk_confirm: false,
            cleanup: Cleanup::new(),
            cleanup_kicked: false,
            cleanup_selected: HashSet::new(),
            cleanup_confirm: false,
            kernels: Kernels::new(),
            kernel_confirm: None,
            artifact_confirm: None,
            artifact_all_confirm: false,
            startup: StartupMgr::new(),
            startup_kicked: false,
            startup_mode: StartupMode::Apps,
            startup_filter: String::new(),
            svc_confirm: None,
            repos: Repos::new(),
            repos_kicked: false,
            repos_input: String::new(),
            repos_confirm: None,
            scan_confirm: None,
            proc_selected: None,
            proc_confirm: None,
            confirm: None,
            bin_size,
            pkg_ok: crate::util::package_tools_available(),
            theme_light: false,
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            first_run: !welcome_seen(),
            toasts: Vec::new(),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        // Throttle everything down when the window isn't focused — a background
        // monitor shouldn't burn CPU.
        let focused = ctx.input(|i| i.focused);
        let interval = if focused { REFRESH } else { IDLE_REFRESH };

        // Cheap metrics (CPU / memory / network / load) — every tick.
        if self.last_fast.elapsed() >= interval {
            self.metrics.refresh_fast();
            self.last_fast = Instant::now();
        }
        // Disks change slowly; rescan occasionally.
        if self.last_disk.elapsed() >= DISK_REFRESH {
            self.metrics.refresh_disks();
            self.last_disk = Instant::now();
        }
        // Processes are expensive — only enumerate them while the list is on
        // screen (Performance tab) and the window is focused.
        if focused && self.tab == Tab::Performance && self.last_proc.elapsed() >= PROC_REFRESH {
            self.metrics.refresh_procs();
            self.last_proc = Instant::now();
        }
        // Kick off a background update check on first launch so the Overview
        // "needs attention" panel is populated without visiting the Updates tab.
        // It only reads APT's cached lists, so it's cheap and needs no root.
        if self.pkg_ok && !self.updates_kicked {
            self.updates.check(ctx.clone());
            self.updates_kicked = true;
        }
        // Gather deep system info the first time its tab is opened.
        if self.tab == Tab::SystemInfo && !self.system_kicked {
            self.system.gather(ctx.clone());
            self.system_kicked = true;
        }
        // Enumerate installed packages the first time the Apps tab is opened.
        if self.pkg_ok && self.tab == Tab::Apps && !self.apps_kicked {
            self.apps.load_installed(ctx.clone());
            self.apps_kicked = true;
        }
        // Measure reclaimable space (and list kernels) the first time the
        // Cleanup tab is opened.
        if self.pkg_ok && self.tab == Tab::Cleanup && !self.cleanup_kicked {
            self.cleanup.scan(ctx.clone());
            self.kernels.load(ctx.clone());
            self.cleanup_kicked = true;
        }
        // Load startup apps + services the first time the Startup tab is opened.
        if self.tab == Tab::Startup && !self.startup_kicked {
            self.startup.load(ctx.clone());
            self.startup_kicked = true;
        }
        // Load apt sources the first time the Sources tab is opened.
        if self.pkg_ok && self.tab == Tab::Sources && !self.repos_kicked {
            self.repos.load(ctx.clone());
            self.repos_kicked = true;
        }

        // Snapshot the update state for the overall health pill in the header.
        // Pending snap refreshes count too — "Healthy" must not lie on a
        // snap-based desktop with refreshes waiting.
        let (upd_count, reboot_required, upd_checked, upd_failed) = {
            let s = self.updates.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.count + s.snap_count,
                s.reboot_required,
                s.checked,
                s.check_failed,
            )
        };
        let status = Health::compute(
            &self.metrics,
            upd_count,
            reboot_required,
            upd_checked,
            upd_failed,
        );

        let (toggle_theme, open_palette) = top_bar(ui, &self.metrics, status, self.theme_light);
        // Ctrl+K opens the command palette; Esc closes overlays.
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K)) {
            self.palette_open = !self.palette_open;
            self.palette_query.clear();
            self.palette_sel = 0;
        }
        if open_palette {
            self.palette_open = true;
            self.palette_query.clear();
            self.palette_sel = 0;
        }
        if toggle_theme {
            self.set_theme(ctx, !self.theme_light);
        }

        // Left sidebar: the navigational backbone the whole app hangs off.
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(198.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::PANEL())
                    .inner_margin(egui::Margin::symmetric(10, 10)),
            )
            .show(ui, |ui| {
                nav(ui, &mut self.tab, upd_count);
            });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    centered_content(ui, |ui| match self.tab {
                        Tab::Overview => {
                            overview_tab(
                                ui,
                                &self.metrics,
                                &self.updates,
                                self.pkg_ok,
                                &mut self.tab,
                            );
                        }
                        Tab::Performance => {
                            ui.horizontal(|ui| {
                                mode_btn(ui, &mut self.perf_mode, PerfMode::Graphs, "Graphs");
                                let procs = format!("Processes ({})", self.metrics.procs.len());
                                mode_btn(ui, &mut self.perf_mode, PerfMode::Processes, &procs);
                            });
                            ui.add_space(10.0);
                            match self.perf_mode {
                                PerfMode::Graphs => {
                                    ui.columns(2, |cols| {
                                        cpu_card(&mut cols[0], &self.metrics);
                                        memory_card(&mut cols[1], &self.metrics);
                                    });
                                    ui.add_space(10.0);
                                    network_card(ui, &self.metrics);
                                    if self.metrics.temp.is_some() {
                                        ui.add_space(10.0);
                                        temperature_card(ui, &self.metrics);
                                    }
                                }
                                PerfMode::Processes => {
                                    process_card(
                                        ui,
                                        &self.metrics,
                                        &mut self.sort,
                                        &mut self.filter,
                                        &mut self.proc_selected,
                                        &mut self.proc_confirm,
                                    );
                                }
                            }
                        }
                        Tab::Storage => {
                            disks_card(ui, &self.metrics);
                            ui.add_space(10.0);
                            let (nfiles, narts) = {
                                let s =
                                    self.scanner.state.lock().unwrap_or_else(|e| e.into_inner());
                                (s.top.len(), s.artifacts.len())
                            };
                            ui.horizontal(|ui| {
                                mode_btn(
                                    ui,
                                    &mut self.storage_mode,
                                    StorageMode::Files,
                                    &format!("Large Files ({nfiles})"),
                                );
                                mode_btn(
                                    ui,
                                    &mut self.storage_mode,
                                    StorageMode::Artifacts,
                                    &format!("Build Artifacts ({narts})"),
                                );
                            });
                            ui.add_space(10.0);
                            match self.storage_mode {
                                StorageMode::Files => bigfiles_card(
                                    ui,
                                    &self.scanner,
                                    &mut self.threshold_mb,
                                    &mut self.scan_confirm,
                                    ctx,
                                ),
                                StorageMode::Artifacts => artifacts_card(
                                    ui,
                                    &self.scanner,
                                    &mut self.artifact_confirm,
                                    &mut self.artifact_all_confirm,
                                    ctx,
                                ),
                            }
                        }
                        Tab::Apps => {
                            if self.pkg_ok {
                                apps_tab(
                                    ui,
                                    &self.apps,
                                    &mut self.apps_mode,
                                    &mut self.apps_filter,
                                    &mut self.apps_query,
                                    &mut self.apps_confirm,
                                    &mut self.apps_selected,
                                    &mut self.apps_bulk_confirm,
                                    ctx,
                                );
                            } else {
                                unavailable_card(ui, "App management");
                            }
                        }
                        Tab::Cleanup => {
                            if self.pkg_ok {
                                cleanup_tab(
                                    ui,
                                    &self.cleanup,
                                    &mut self.cleanup_selected,
                                    &mut self.cleanup_confirm,
                                    ctx,
                                );
                                ui.add_space(10.0);
                                kernels_card(ui, &self.kernels, &mut self.kernel_confirm, ctx);
                            } else {
                                unavailable_card(ui, "Cleanup");
                            }
                        }
                        Tab::Startup => {
                            startup_tab(
                                ui,
                                &self.startup,
                                &mut self.startup_mode,
                                &mut self.startup_filter,
                                &mut self.svc_confirm,
                                ctx,
                            );
                        }
                        Tab::Sources => {
                            if self.pkg_ok {
                                sources_tab(
                                    ui,
                                    &self.repos,
                                    &mut self.repos_input,
                                    &mut self.repos_confirm,
                                    ctx,
                                );
                            } else {
                                unavailable_card(ui, "Repositories");
                            }
                        }
                        Tab::SystemInfo => {
                            system_info_tab(ui, &self.system, &self.metrics, ctx);
                        }
                        Tab::Updates => {
                            if self.pkg_ok {
                                updates_card(ui, &self.updates, ctx);
                            } else {
                                unavailable_card(ui, "System updates");
                            }
                            ui.add_space(10.0);
                            ui.columns(2, |cols| {
                                power_card(&mut cols[0], &mut self.confirm);
                                about_card(&mut cols[1], self.bin_size);
                            });
                        }
                    });
                });
        });

        // Overlays (drawn last, on top): first-run sheet, command palette, toasts.
        if self.first_run {
            self.show_first_run(ctx);
        }
        if self.palette_open {
            self.show_palette(ctx);
        }
        self.show_toasts(ctx);

        // Keep the live graphs moving without spinning the CPU at full tilt.
        ctx.request_repaint_after(interval);
    }
}

/// A command-palette action.
#[derive(Clone, Copy)]
enum PaletteAction {
    GoTo(Tab),
    ToggleTheme,
}

impl App {
    fn set_theme(&mut self, ctx: &egui::Context, light: bool) {
        self.theme_light = light;
        theme::set_palette(if light {
            theme::Palette::light()
        } else {
            theme::Palette::dark()
        });
        theme::install(ctx);
        self.push_toast(
            if light { "☀" } else { "☾" },
            ACCENT(),
            if light { "Light theme" } else { "Dark theme" },
            "",
        );
    }

    fn push_toast(&mut self, glyph: &'static str, accent: Color32, title: &str, detail: &str) {
        self.toasts.push(Toast {
            born: Instant::now(),
            accent,
            glyph,
            title: title.to_string(),
            detail: detail.to_string(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.born.elapsed().as_secs_f32() < 4.0);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new("toasts".into())
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_max_width(330.0);
                for t in self.toasts.iter().rev() {
                    egui::Frame::NONE
                        .fill(theme::CARD())
                        .stroke(Stroke::new(1.0, theme::CARD_HI()))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 6],
                            blur: 18,
                            spread: 0,
                            color: Color32::from_black_alpha(90),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(t.glyph).size(16.0).strong().color(t.accent),
                                );
                                ui.add_space(4.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(&t.title).size(13.0).strong().color(TEXT()),
                                    );
                                    if !t.detail.is_empty() {
                                        ui.label(
                                            RichText::new(&t.detail).size(11.0).color(MUTED()),
                                        );
                                    }
                                });
                            });
                        });
                    ui.add_space(8.0);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn show_first_run(&mut self, ctx: &egui::Context) {
        // Dim backdrop.
        egui::Area::new("firstrun_bg".into())
            .fixed_pos(pos2(0.0, 0.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.painter()
                    .rect_filled(ctx.content_rect(), 0.0, Color32::from_black_alpha(170));
            });

        const POINTS: [(&str, &str); 3] = [
            ("It never runs as root", "UPulse starts unprivileged and asks for your password only for the one action that needs it, through your desktop's own prompt."),
            ("Destructive things take two clicks", "The first click arms; the second fires. OS-critical packages, services and repos are read-only and marked Protected."),
            ("Your documents are off limits", "Cleanup only touches caches, logs, orphaned packages and the trash you chose — never anything in your home documents."),
        ];
        let mut close = false;
        egui::Window::new("welcome")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                egui::Frame::NONE
                    .fill(theme::CARD())
                    .stroke(Stroke::new(1.0, theme::CARD_HI()))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(egui::Margin::same(22)),
            )
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(
                    RichText::new("Welcome to UPulse")
                        .size(18.0)
                        .strong()
                        .color(TEXT()),
                );
                ui.label(
                    RichText::new("Your Ubuntu — at a glance and under control.")
                        .size(12.0)
                        .color(MUTED()),
                );
                ui.add_space(14.0);
                for (i, (title, body)) in POINTS.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(6), theme::CARD_HI());
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{}", i + 1),
                            egui::FontId::proportional(11.0),
                            ACCENT(),
                        );
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(*title).size(13.0).strong().color(TEXT()));
                            ui.label(RichText::new(*body).size(11.5).color(MUTED()));
                        });
                    });
                    ui.add_space(12.0);
                }
                ui.add_space(2.0);
                if primary_button(ui, "Get started", ACCENT()).clicked() {
                    close = true;
                }
            });
        if close {
            self.first_run = false;
            mark_welcome_seen();
        }
    }

    fn show_palette(&mut self, ctx: &egui::Context) {
        // The static command table.
        let cmds: [(&str, &str, PaletteAction); 10] = [
            ("GO TO", "Overview", PaletteAction::GoTo(Tab::Overview)),
            (
                "GO TO",
                "Performance",
                PaletteAction::GoTo(Tab::Performance),
            ),
            ("GO TO", "Storage", PaletteAction::GoTo(Tab::Storage)),
            ("GO TO", "Apps", PaletteAction::GoTo(Tab::Apps)),
            ("GO TO", "Cleanup", PaletteAction::GoTo(Tab::Cleanup)),
            (
                "GO TO",
                "Startup & Services",
                PaletteAction::GoTo(Tab::Startup),
            ),
            ("GO TO", "Sources", PaletteAction::GoTo(Tab::Sources)),
            ("GO TO", "System Info", PaletteAction::GoTo(Tab::SystemInfo)),
            ("GO TO", "Updates", PaletteAction::GoTo(Tab::Updates)),
            ("THEME", "Toggle light / dark", PaletteAction::ToggleTheme),
        ];
        let q = self.palette_query.to_lowercase();
        let filtered: Vec<&(&str, &str, PaletteAction)> = cmds
            .iter()
            .filter(|(_, label, _)| q.is_empty() || label.to_lowercase().contains(&q))
            .collect();

        // Keyboard: Esc closes, Up/Down move, Enter runs.
        let (esc, up, down, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
            )
        });
        if esc {
            self.palette_open = false;
            return;
        }
        if !filtered.is_empty() {
            if down {
                self.palette_sel = (self.palette_sel + 1) % filtered.len();
            }
            if up {
                self.palette_sel = (self.palette_sel + filtered.len() - 1) % filtered.len();
            }
        }
        self.palette_sel = self.palette_sel.min(filtered.len().saturating_sub(1));

        let mut run: Option<PaletteAction> = None;

        egui::Area::new("palette_bg".into())
            .fixed_pos(pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let r = ctx.content_rect();
                ui.painter()
                    .rect_filled(r, 0.0, Color32::from_black_alpha(150));
                if ui
                    .interact(r, "palette_bg_click".into(), Sense::click())
                    .clicked()
                {
                    self.palette_open = false;
                }
            });

        egui::Window::new("palette")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 96.0))
            .frame(
                egui::Frame::NONE
                    .fill(theme::CARD())
                    .stroke(Stroke::new(1.0, theme::CARD_HI()))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ctx, |ui| {
                ui.set_width(520.0);
                let te = ui.add(
                    egui::TextEdit::singleline(&mut self.palette_query)
                        .hint_text("Type a command…  (Esc to close)")
                        .desired_width(f32::INFINITY)
                        .font(egui::FontId::proportional(15.0)),
                );
                te.request_focus();
                ui.add_space(8.0);
                if filtered.is_empty() {
                    ui.label(
                        RichText::new("No matching commands")
                            .size(12.0)
                            .color(MUTED()),
                    );
                }
                for (idx, (group, label, action)) in filtered.iter().enumerate() {
                    let sel = idx == self.palette_sel;
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 30.0),
                        Sense::click(),
                    );
                    if sel {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(7),
                            ACCENT().linear_multiply(0.16),
                        );
                    } else if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(7), theme::CARD_HI());
                    }
                    ui.painter().text(
                        pos2(rect.left() + 12.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(13.5),
                        TEXT(),
                    );
                    ui.painter().text(
                        pos2(rect.right() - 12.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        group,
                        egui::FontId::proportional(10.0),
                        MUTED(),
                    );
                    if resp.clicked() || (enter && sel) {
                        run = Some(*action);
                    }
                }
            });

        if let Some(action) = run {
            match action {
                PaletteAction::GoTo(t) => self.tab = t,
                PaletteAction::ToggleTheme => self.set_theme(ctx, !self.theme_light),
            }
            self.palette_open = false;
        }
    }
}

/// Path of the first-run flag, e.g. `~/.config/upulse/welcomed`.
fn welcome_flag_path() -> Option<PathBuf> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.config")))?;
    Some(PathBuf::from(base).join("upulse").join("welcomed"))
}

fn welcome_seen() -> bool {
    // If we can't tell, don't nag.
    welcome_flag_path().map(|p| p.exists()).unwrap_or(true)
}

fn mark_welcome_seen() {
    if let Some(p) = welcome_flag_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, b"1");
    }
}

// ---------------------------------------------------------------------------
// Header + tabs
// ---------------------------------------------------------------------------

/// Single overall system-health state (per the "one at-a-glance status" rule).
struct Health {
    color: Color32,
    label: String,
}

impl Health {
    /// Tri-state honesty rule: a check that failed (or hasn't run where it
    /// should have) must never fall through to "Healthy" — missing data reads
    /// as "can't check", not as good news.
    fn compute(
        m: &Metrics,
        updates: usize,
        reboot_required: bool,
        upd_checked: bool,
        upd_failed: bool,
    ) -> Self {
        let disk = worst_disk_frac(m);
        let (color, label) = if disk >= 0.90 || m.mem_frac() >= 0.90 {
            (BAD(), "Attention needed".to_string())
        } else if reboot_required {
            (WARN(), "Restart required".to_string())
        } else if upd_checked && updates > 0 {
            (WARN(), format!("{updates} updates"))
        } else if upd_checked && upd_failed {
            (MUTED(), "Can't check updates".to_string())
        } else {
            (GOOD(), "Healthy".to_string())
        };
        Self { color, label }
    }
}

/// The single global strip above the panels: brand + overall-health pill + a
/// live uptime, and — right-aligned — the always-on system vitals (CPU / Mem /
/// Disk / Net). This is the *only* chrome above the sidebar+panel, so panels
/// never repeat these numbers.
fn top_bar(ui: &mut egui::Ui, m: &Metrics, status: Health, light: bool) -> (bool, bool) {
    let mut toggle_theme = false;
    let mut open_palette = false;
    egui::Panel::top("topbar")
        .frame(
            egui::Frame::NONE
                .fill(theme::PANEL())
                .inner_margin(egui::Margin::symmetric(16, 10)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (brand_rect, _) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::hover());
                icons::brand(ui.painter(), brand_rect, ACCENT());
                ui.label(RichText::new("UPulse").size(20.0).strong().color(ACCENT()));
                ui.add_space(10.0);
                health_pill(ui, &status);
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("up {}", theme::fmt_uptime(m.uptime)))
                        .size(11.0)
                        .color(MUTED()),
                );

                // Vitals, right-aligned. `right_to_left` reverses insertion order,
                // so add Net → Disk → Mem → CPU to read CPU → Net on screen.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Theme toggle + command-palette affordance (rightmost).
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(if light { "☾" } else { "☀" })
                                    .size(14.0)
                                    .color(MUTED()),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Toggle light / dark")
                        .clicked()
                    {
                        toggle_theme = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("⌘K").size(11.0).strong().color(MUTED()),
                            )
                            .fill(theme::CARD())
                            .stroke(Stroke::new(1.0, theme::CARD_HI()))
                            .corner_radius(CornerRadius::same(7)),
                        )
                        .on_hover_text("Command palette (Ctrl+K)")
                        .clicked()
                    {
                        open_palette = true;
                    }
                    ui.add_space(4.0);
                    net_chip(ui, m);
                    let disk = worst_disk_frac(m);
                    vitals_chip(ui, "DISK", &format!("{:.0}%", disk * 100.0), disk);
                    vitals_chip(
                        ui,
                        "MEM",
                        &format!("{:.0}%", m.mem_frac() * 100.0),
                        m.mem_frac(),
                    );
                    vitals_chip(
                        ui,
                        "CPU",
                        &format!("{:.0}%", m.cpu_global),
                        m.cpu_global / 100.0,
                    );
                });
            });
        });
    (toggle_theme, open_palette)
}

/// A compact live-metric pill for the top bar: small label + value coloured by
/// load. Matches the app's rounded-card language at a smaller scale.
fn vitals_chip(ui: &mut egui::Ui, label: &str, value: &str, frac: f32) {
    egui::Frame::NONE
        .fill(theme::CARD())
        .stroke(Stroke::new(1.0, theme::CARD_HI()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(RichText::new(label).size(9.5).strong().color(MUTED()));
                ui.label(
                    RichText::new(value)
                        .size(12.5)
                        .strong()
                        .monospace()
                        .color(theme::usage_color(frac)),
                );
                // 38×4 mini bar showing the same fraction.
                let (rect, _) = ui.allocate_exact_size(egui::vec2(38.0, 4.0), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(2), theme::TRACK());
                let mut fill = rect;
                fill.set_width(38.0 * frac.clamp(0.0, 1.0));
                painter.rect_filled(fill, CornerRadius::same(2), theme::usage_color(frac));
            });
        });
    ui.add_space(6.0);
}

/// The network vitals pill: down/up rates side by side.
fn net_chip(ui: &mut egui::Ui, m: &Metrics) {
    egui::Frame::NONE
        .fill(theme::CARD())
        .stroke(Stroke::new(1.0, theme::CARD_HI()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("↓").size(12.0).color(CYAN()));
                ui.label(
                    RichText::new(theme::fmt_rate(m.net_rx))
                        .size(11.5)
                        .strong()
                        .monospace()
                        .color(CYAN()),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("↑").size(12.0).color(VIOLET()));
                ui.label(
                    RichText::new(theme::fmt_rate(m.net_tx))
                        .size(11.5)
                        .strong()
                        .monospace()
                        .color(VIOLET()),
                );
            });
        });
    ui.add_space(6.0);
}

/// Coloured dot + word summarising overall health.
fn health_pill(ui: &mut egui::Ui, status: &Health) {
    egui::Frame::NONE
        .fill(status.color.linear_multiply(0.16))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, status.color);
            ui.label(
                RichText::new(&status.label)
                    .size(12.0)
                    .strong()
                    .color(status.color),
            );
        });
}

/// The left-sidebar navigation: one full-width button per section, styled like
/// the old segmented control (ACCENT() fill when selected, muted otherwise).
fn nav(ui: &mut egui::Ui, tab: &mut Tab, upd_count: usize) {
    ui.add_space(2.0);
    nav_group(ui, "MONITOR");
    nav_btn(ui, tab, Tab::Overview, "Overview", icons::overview, 0);
    nav_btn(
        ui,
        tab,
        Tab::Performance,
        "Performance",
        icons::performance,
        0,
    );
    nav_btn(ui, tab, Tab::Storage, "Storage", icons::storage, 0);
    nav_group(ui, "MANAGE");
    nav_btn(ui, tab, Tab::Apps, "Apps", icons::apps, 0);
    nav_btn(ui, tab, Tab::Cleanup, "Cleanup", icons::cleanup, 0);
    nav_btn(ui, tab, Tab::Startup, "Startup", icons::startup, 0);
    nav_btn(ui, tab, Tab::Sources, "Sources", icons::sources, 0);
    nav_group(ui, "SYSTEM");
    nav_btn(
        ui,
        tab,
        Tab::SystemInfo,
        "System Info",
        icons::system_info,
        0,
    );
    nav_btn(ui, tab, Tab::Updates, "Updates", icons::updates, upd_count);
}

/// A small letter-spaced section caption in the sidebar.
fn nav_group(ui: &mut egui::Ui, label: &str) {
    ui.add_space(11.0);
    // Fake letter-spacing by joining chars with thin spaces — egui has no
    // letter-spacing, and this reads as the spec's 9.5px spaced caption.
    let spaced: String = label.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(
            RichText::new(spaced)
                .size(9.5)
                .strong()
                .color(theme::FAINT()),
        );
    });
    ui.add_space(4.0);
}

/// A sidebar row: hand-drawn icon + label, selected = ACCENT fill with
/// BG-colored text, hover = CARD_HI. `badge` (>0) draws a count pill (Updates).
fn nav_btn(
    ui: &mut egui::Ui,
    tab: &mut Tab,
    value: Tab,
    label: &str,
    icon: icons::IconFn,
    badge: usize,
) {
    let selected = *tab == value;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), Sense::click());
    let hovered = resp.hovered();
    let painter = ui.painter();

    let bg = if selected {
        ACCENT()
    } else if hovered {
        theme::CARD_HI()
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        painter.rect_filled(rect, CornerRadius::same(8), bg);
    }
    let fg = if selected { theme::BG() } else { MUTED() };

    // Icon (17px logical box), vertically centered.
    let isz = 17.0;
    let icon_rect = egui::Rect::from_min_size(
        pos2(rect.min.x + 10.0, rect.center().y - isz / 2.0),
        egui::vec2(isz, isz),
    );
    icon(painter, icon_rect, fg);

    // Label.
    painter.text(
        pos2(rect.min.x + 10.0 + isz + 9.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        fg,
    );

    // Updates badge pill on the right.
    if badge > 0 {
        let txt = badge.to_string();
        let pill_w = 16.0 + txt.len() as f32 * 6.0;
        let pill = egui::Rect::from_min_size(
            pos2(rect.max.x - pill_w - 8.0, rect.center().y - 8.0),
            egui::vec2(pill_w, 16.0),
        );
        let (fill, tcol) = if selected {
            (theme::BG(), ACCENT())
        } else {
            (ACCENT(), theme::BG())
        };
        painter.rect_filled(pill, CornerRadius::same(8), fill);
        painter.text(
            pill.center(),
            egui::Align2::CENTER_CENTER,
            txt,
            egui::FontId::proportional(10.5),
            tcol,
        );
    }

    if resp.clicked() {
        *tab = value;
    }
    ui.add_space(3.0);
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(15.0).strong().color(TEXT()));
    ui.add_space(6.0);
}

/// Cap a tab's content to `CONTENT_W` and center it, so every panel lines up to
/// the same width instead of stretching edge-to-edge on wide monitors.
/// (Spacer-based: `Margin` became `i8` in egui 0.36, far too small to center
/// content on a wide monitor.)
fn centered_content(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width().min(CONTENT_W);
    let side = ((ui.available_width() - w) * 0.5).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space(side);
        ui.vertical(|ui| {
            ui.set_max_width(w);
            add(ui);
        });
    });
}

/// Shown in place of a feature that needs APT + pkexec on a system that lacks
/// them (non-Ubuntu/Debian, or a minimal install without PolicyKit).
fn unavailable_card(ui: &mut egui::Ui, feature: &str) {
    theme::card(ui, |ui| {
        section_title(ui, feature);
        ui.label(
            RichText::new(format!(
                "{feature} needs APT and pkexec, which aren't available on this system."
            ))
            .size(13.0)
            .color(TEXT()),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("This feature is built for Ubuntu and other Debian-based systems.")
                .size(12.0)
                .color(MUTED()),
        );
    });
}

// ---------------------------------------------------------------------------
// Overview tab — the landing page: identity, what needs attention, and live
// mini-graphs, all composed from the shared card/graph helpers.
// ---------------------------------------------------------------------------

fn overview_tab(
    ui: &mut egui::Ui,
    m: &Metrics,
    updates: &UpdatesChecker,
    pkg_ok: bool,
    tab: &mut Tab,
) {
    // Snapshot the update state once so we don't hold the lock while drawing.
    // apt + snap combined: both are "updates available" to the user.
    let (upd_count, reboot_required, upd_checked, upd_failed, checking) = {
        let s = updates.state.lock().unwrap_or_else(|e| e.into_inner());
        (
            s.count + s.snap_count,
            s.reboot_required,
            s.checked,
            s.check_failed,
            s.checking,
        )
    };

    attention_card(
        ui,
        m,
        upd_count,
        reboot_required,
        upd_checked,
        upd_failed,
        checking,
        pkg_ok,
        tab,
    );
    ui.add_space(10.0);

    ui.columns(2, |cols| {
        mini_graph_card(
            &mut cols[0],
            "CPU",
            &format!("{:.0}% overall", m.cpu_global),
            &m.cpu_hist,
            ACCENT(),
        );
        mini_graph_card(
            &mut cols[1],
            "Memory",
            &format!(
                "{} of {} used",
                theme::fmt_bytes(m.mem_used),
                theme::fmt_bytes(m.mem_total)
            ),
            &m.mem_hist,
            CYAN(),
        );
    });
    ui.add_space(10.0);

    // Reuse the Storage tab's disk renderer for an at-a-glance fill summary.
    disks_card(ui, m);
}

/// The actionable to-do list. Each real problem gets a coloured dot, a plain
/// description, and a button that jumps to the tab where it can be dealt with.
/// When nothing's wrong it collapses to a single reassuring line.
#[allow(clippy::too_many_arguments)]
fn attention_card(
    ui: &mut egui::Ui,
    m: &Metrics,
    upd_count: usize,
    reboot_required: bool,
    upd_checked: bool,
    upd_failed: bool,
    checking: bool,
    pkg_ok: bool,
    tab: &mut Tab,
) {
    theme::card(ui, |ui| {
        section_title(ui, "Needs attention");
        let mut any = false;

        if upd_checked && upd_count > 0 {
            any = true;
            let msg = format!(
                "{} update{} available",
                upd_count,
                if upd_count == 1 { "" } else { "s" }
            );
            attention_row(ui, WARN(), &msg, "Review", tab, Tab::Updates);
        }
        if upd_checked && upd_failed {
            any = true;
            attention_row(
                ui,
                MUTED(),
                "Couldn't check for updates",
                "Details",
                tab,
                Tab::Updates,
            );
        }
        if reboot_required {
            any = true;
            attention_row(
                ui,
                WARN(),
                "Restart required to finish updates",
                "Power",
                tab,
                Tab::Updates,
            );
        }
        for d in m.disks.iter().filter(|d| d.total > 0) {
            let frac = d.used as f32 / d.total as f32;
            if frac >= 0.85 {
                any = true;
                let msg = format!("{} is {:.0}% full", d.mount, frac * 100.0);
                attention_row(
                    ui,
                    theme::usage_color(frac),
                    &msg,
                    "Storage",
                    tab,
                    Tab::Storage,
                );
            }
        }
        if m.mem_frac() >= 0.85 {
            any = true;
            let msg = format!("Memory is {:.0}% used", m.mem_frac() * 100.0);
            attention_row(
                ui,
                theme::usage_color(m.mem_frac()),
                &msg,
                "Details",
                tab,
                Tab::Performance,
            );
        }

        if !any {
            ui.add_space(2.0);
            // Only show the checking spinner while an update check is actually in
            // flight; if the tools aren't present it would spin forever.
            if pkg_ok && (checking || !upd_checked) {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(RichText::new("Checking system…").size(13.0).color(MUTED()));
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✓").size(16.0).strong().color(GOOD()));
                    ui.label(
                        RichText::new("Everything looks healthy")
                            .size(13.0)
                            .strong()
                            .color(GOOD()),
                    );
                });
            }
        }
    });
}

fn attention_row(
    ui: &mut egui::Ui,
    color: Color32,
    msg: &str,
    action: &str,
    tab: &mut Tab,
    target: Tab,
) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.add_space(2.0);
        ui.label(RichText::new(msg).size(13.0).color(TEXT()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new(action).size(12.0).strong().color(ACCENT()))
                        .frame(false),
                )
                .clicked()
            {
                *tab = target;
            }
        });
    });
    ui.add_space(6.0);
}

fn mini_graph_card(ui: &mut egui::Ui, title: &str, subtitle: &str, ring: &Ring, color: Color32) {
    theme::card(ui, |ui| {
        section_title(ui, title);
        ui.label(RichText::new(subtitle).size(12.0).color(MUTED()));
        ui.add_space(4.0);
        sparkline(ui, title, ring, color, 100.0, 56.0);
    });
}

// ---------------------------------------------------------------------------
// System Info tab — deep, mostly-static hardware & OS facts, gathered once on a
// background thread (see `system.rs`) and shown as label/value cards.
// ---------------------------------------------------------------------------

fn system_info_tab(ui: &mut egui::Ui, probe: &SystemProbe, m: &Metrics, ctx: &egui::Context) {
    let snapshot = probe
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(info) = snapshot else {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(18.0));
            ui.label(
                RichText::new("Gathering system information…")
                    .size(13.0)
                    .color(MUTED()),
            );
        });
        return;
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("System Information")
                .size(15.0)
                .strong()
                .color(TEXT()),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(RichText::new("Refresh").size(13.0)).clicked() {
                probe.gather(ctx.clone());
            }
        });
    });
    ui.add_space(8.0);

    ui.columns(2, |cols| {
        info_card(&mut cols[0], "Operating System", &info.os);
        info_card(&mut cols[1], "Machine", &info.machine);
    });
    ui.add_space(10.0);
    ui.columns(2, |cols| {
        processor_card(&mut cols[0], &info.cpu, m);
        info_card(&mut cols[1], "Memory", &info.memory);
    });
    ui.add_space(10.0);
    ui.columns(2, |cols| {
        graphics_card(&mut cols[0], &info.gpus);
        sensors_card(&mut cols[1], &info.temps, &info.battery);
    });
}

fn info_card(ui: &mut egui::Ui, title: &str, rows: &[(String, String)]) {
    theme::card(ui, |ui| {
        section_title(ui, title);
        for (label, value) in rows {
            about_row(ui, label, value);
        }
    });
}

/// Like `info_card` for the CPU, but appends a live load-average row (the one
/// live value that used to live in the old header).
fn processor_card(ui: &mut egui::Ui, rows: &[(String, String)], m: &Metrics) {
    theme::card(ui, |ui| {
        section_title(ui, "Processor");
        for (label, value) in rows {
            about_row(ui, label, value);
        }
        about_row(
            ui,
            "Load avg (1/5/15)",
            &format!("{:.2}  ·  {:.2}  ·  {:.2}", m.load[0], m.load[1], m.load[2]),
        );
    });
}

fn graphics_card(ui: &mut egui::Ui, gpus: &[String]) {
    theme::card(ui, |ui| {
        section_title(ui, "Graphics");
        if gpus.is_empty() {
            ui.label(
                RichText::new("No display adapter reported")
                    .size(12.0)
                    .color(MUTED()),
            );
            return;
        }
        for g in gpus {
            ui.label(RichText::new(g).size(12.0).color(TEXT()));
            ui.add_space(4.0);
        }
    });
}

fn sensors_card(ui: &mut egui::Ui, temps: &[(String, f32)], battery: &Option<(String, u8)>) {
    theme::card(ui, |ui| {
        section_title(ui, "Sensors");
        if let Some((status, pct)) = battery {
            let col = if *pct >= 50 {
                GOOD()
            } else if *pct >= 20 {
                WARN()
            } else {
                BAD()
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("Battery").size(13.0).strong().color(TEXT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{pct}%  ·  {status}"))
                            .size(12.0)
                            .color(MUTED()),
                    );
                });
            });
            ui.add_space(4.0);
            theme::bar(ui, *pct as f32 / 100.0, col, 10.0);
            ui.add_space(10.0);
        }
        if temps.is_empty() {
            if battery.is_none() {
                ui.label(
                    RichText::new("No sensors reported")
                        .size(12.0)
                        .color(MUTED()),
                );
            }
            return;
        }
        for (label, c) in temps {
            let color = if *c < 60.0 {
                GOOD()
            } else if *c < 80.0 {
                WARN()
            } else {
                BAD()
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(12.0).color(MUTED()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{c:.0} °C"))
                            .size(12.0)
                            .strong()
                            .color(color),
                    );
                });
            });
            ui.add_space(4.0);
        }
    });
}

// ---------------------------------------------------------------------------
// Performance tab
// ---------------------------------------------------------------------------

fn cpu_card(ui: &mut egui::Ui, m: &Metrics) {
    theme::card(ui, |ui| {
        section_title(ui, "CPU");
        ui.label(
            RichText::new(format!("{:.0}% overall", m.cpu_global))
                .size(12.0)
                .color(MUTED()),
        );
        ui.add_space(4.0);
        sparkline(ui, "cpu_spark", &m.cpu_hist, ACCENT(), 100.0, 56.0);
        ui.add_space(12.0);
        ui.label(RichText::new("per core").size(11.0).color(MUTED()));
        ui.add_space(6.0);
        core_grid(ui, &m.per_core);
    });
}

/// Per-core heat grid: one bottom-anchored bar per core, coloured by usage,
/// with a c0..cN mono label under each. Wraps past 16 columns so it scales to
/// many-core machines.
fn core_grid(ui: &mut egui::Ui, cores: &[f32]) {
    if cores.is_empty() {
        return;
    }
    let per_row = cores.len().min(16);
    let col_h = 44.0;
    for (row, chunk) in cores.chunks(per_row).enumerate() {
        let w = ui.available_width();
        let gap = 6.0;
        let cw = ((w - gap * (per_row as f32 - 1.0)) / per_row as f32).max(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (j, &pct) in chunk.iter().enumerate() {
                let i = row * per_row + j;
                let frac = (pct / 100.0).clamp(0.0, 1.0);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(cw, col_h), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(3), theme::CARD_HI());
                let fh = (col_h - 14.0) * frac; // leave room for the label
                let bar = egui::Rect::from_min_max(
                    pos2(rect.left(), rect.bottom() - 14.0 - fh),
                    pos2(rect.right(), rect.bottom() - 14.0),
                );
                painter.rect_filled(bar, CornerRadius::same(3), theme::usage_color(frac));
                painter.text(
                    pos2(rect.center().x, rect.bottom() - 6.0),
                    egui::Align2::CENTER_CENTER,
                    format!("c{i}"),
                    egui::FontId::monospace(9.0),
                    MUTED(),
                );
                resp.on_hover_text(format!("core {i}: {pct:.0}%"));
            }
        });
    }
}

fn memory_card(ui: &mut egui::Ui, m: &Metrics) {
    theme::card(ui, |ui| {
        section_title(ui, "Memory");
        ui.label(
            RichText::new(format!(
                "{} of {} used",
                theme::fmt_bytes(m.mem_used),
                theme::fmt_bytes(m.mem_total)
            ))
            .size(12.0)
            .color(MUTED()),
        );
        ui.add_space(4.0);
        sparkline(ui, "mem_spark", &m.mem_hist, CYAN(), 100.0, 56.0);
        ui.add_space(10.0);

        labelled_bar(
            ui,
            "RAM",
            &format!(
                "{} / {}",
                theme::fmt_bytes(m.mem_used),
                theme::fmt_bytes(m.mem_total)
            ),
            m.mem_frac(),
        );
        ui.add_space(10.0);
        if m.swap_total > 0 {
            labelled_bar(
                ui,
                "Swap",
                &format!(
                    "{} / {}",
                    theme::fmt_bytes(m.swap_used),
                    theme::fmt_bytes(m.swap_total)
                ),
                m.swap_frac(),
            );
        } else {
            ui.label(
                RichText::new("Swap  —  none configured")
                    .size(12.0)
                    .color(MUTED()),
            );
        }
    });
}

fn network_card(ui: &mut egui::Ui, m: &Metrics) {
    theme::card(ui, |ui| {
        section_title(ui, "Network");
        ui.horizontal(|ui| {
            ui.label(RichText::new("↓ down").size(12.0).color(GOOD()));
            ui.label(
                RichText::new(theme::fmt_rate(m.net_rx))
                    .size(12.0)
                    .strong()
                    .color(TEXT()),
            );
            ui.add_space(16.0);
            ui.label(RichText::new("↑ up").size(12.0).color(ACCENT()));
            ui.label(
                RichText::new(theme::fmt_rate(m.net_tx))
                    .size(12.0)
                    .strong()
                    .color(TEXT()),
            );
        });
        ui.add_space(6.0);
        // Auto-scale so quiet periods still show detail; floor at 64 KB/s.
        let y_max = m.rx_hist.max().max(m.tx_hist.max()).max(64.0 * 1024.0);
        plot_area(
            ui,
            90.0,
            y_max,
            &[
                (m.rx_hist.values(), GOOD(), true),
                (m.tx_hist.values(), ACCENT(), false),
            ],
        );
    });
}

/// Live CPU temperature graph. Only rendered when a usable thermal zone
/// exists (`m.temp` is `Some`); the dispatch skips it otherwise.
fn temperature_card(ui: &mut egui::Ui, m: &Metrics) {
    let Some(t) = m.temp else {
        return;
    };
    theme::card(ui, |ui| {
        section_title(ui, "CPU Temperature");
        let color = theme::usage_color((t / 100.0).clamp(0.0, 1.0));
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{t:.0}°C"))
                    .size(12.0)
                    .strong()
                    .color(color),
            );
            ui.label(
                RichText::new("package sensor · last ~2 minutes")
                    .size(11.0)
                    .color(MUTED()),
            );
        });
        ui.add_space(4.0);
        sparkline(ui, "temp_spark", &m.temp_hist, color, 100.0, 56.0);
    });
}

fn labelled_bar(ui: &mut egui::Ui, label: &str, value: &str, frac: f32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).strong().color(TEXT()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(MUTED()));
        });
    });
    ui.add_space(4.0);
    theme::bar(ui, frac, theme::usage_color(frac), 12.0);
}

// ---------------------------------------------------------------------------
// Storage tab
// ---------------------------------------------------------------------------

fn disks_card(ui: &mut egui::Ui, m: &Metrics) {
    theme::card(ui, |ui| {
        section_title(ui, "Disks");
        if m.disks.is_empty() {
            ui.label(RichText::new("No disks reported").color(MUTED()));
            return;
        }
        for (i, d) in m.disks.iter().enumerate() {
            let frac = if d.total == 0 {
                0.0
            } else {
                d.used as f32 / d.total as f32
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(&d.mount).size(13.0).strong().color(TEXT()));
                let removable = if d.removable { " · removable" } else { "" };
                ui.label(
                    RichText::new(format!("{}  ·  {} · {}{}", d.name, d.fs, d.kind, removable))
                        .size(11.0)
                        .color(MUTED()),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} free of {}",
                            theme::fmt_bytes(d.total.saturating_sub(d.used)),
                            theme::fmt_bytes(d.total)
                        ))
                        .size(12.0)
                        .color(MUTED()),
                    );
                });
            });
            ui.add_space(4.0);
            theme::bar(ui, frac, theme::usage_color(frac), 12.0);
            if i + 1 < m.disks.len() {
                ui.add_space(12.0);
            }
        }
    });
}

/// What the user did to a compact scan-result row this frame.
enum ScanRowAct {
    Nothing,
    Arm,
    Fire,
    Disarm,
}

/// Column header for the scan-result tables — same geometry as `scan_row`, so
/// every label sits exactly above its column.
fn scan_header(ui: &mut egui::Ui) {
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mk = |s: &str| RichText::new(s).size(11.0).strong().color(MUTED());
                proc_cell(ui, SCAN_SIZE_W, mk("SIZE"), true);
                ui.add_space(4.0);
                proc_cell(ui, SCAN_NAME_W, mk("NAME"), false);
                proc_cell(ui, 200.0, mk("LOCATION"), false);
            });
        });
}

/// Compact striped row for scan results (Large Files / Build Artifacts):
/// fixed columns — right-aligned size, name (· kind), location — so every row
/// shares the same shape, with the full path on hover and a trailing
/// two-click Delete. Dense on purpose — these lists run to dozens of rows.
#[allow(clippy::too_many_arguments)]
fn scan_row(
    ui: &mut egui::Ui,
    stripe: bool,
    size: u64,
    name: &str,
    kind: Option<&str>,
    dir: &str,
    full_path: &str,
    armed: bool,
    busy: bool,
) -> ScanRowAct {
    let mut act = ScanRowAct::Nothing;
    let fill = if stripe {
        theme::CARD_HI()
    } else {
        Color32::TRANSPARENT
    };
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Fixed-width size column (right-aligned, like the process
                // table): every value's right edge sits on the same line.
                proc_cell(
                    ui,
                    SCAN_SIZE_W,
                    RichText::new(theme::fmt_bytes(size))
                        .size(12.0)
                        .monospace()
                        .color(VIOLET()),
                    true,
                );
                ui.add_space(4.0);
                // Fixed-width name column so every location starts on the
                // same edge; long names truncate, the hover has the full path.
                ui.allocate_ui_with_layout(
                    egui::vec2(SCAN_NAME_W, 18.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_size(egui::vec2(SCAN_NAME_W, 18.0));
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            name,
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::proportional(13.0),
                                color: TEXT(),
                                ..Default::default()
                            },
                        );
                        if let Some(k) = kind {
                            job.append(
                                &format!("  ·  {k}"),
                                0.0,
                                egui::TextFormat {
                                    font_id: egui::FontId::proportional(11.0),
                                    color: MUTED(),
                                    ..Default::default()
                                },
                            );
                        }
                        ui.add(egui::Label::new(job).truncate())
                            .on_hover_text(full_path);
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if busy {
                        // A delete worker owns these rows right now; no buttons.
                    } else if armed {
                        if chip_button(ui, "Delete", Color32::WHITE, BAD()).clicked() {
                            act = ScanRowAct::Fire;
                        }
                        if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                            act = ScanRowAct::Disarm;
                        }
                    } else if chip_button(ui, "Delete", BAD(), BAD().linear_multiply(0.16))
                        .clicked()
                    {
                        act = ScanRowAct::Arm;
                    }
                    // Location fills the space between name and Delete.
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(dir).size(11.0).color(MUTED()))
                                .truncate(),
                        )
                        .on_hover_text(full_path);
                    });
                });
            });
        });
    act
}

fn bigfiles_card(
    ui: &mut egui::Ui,
    scanner: &Scanner,
    threshold_mb: &mut u64,
    confirm: &mut Option<String>,
    ctx: &egui::Context,
) {
    // Deletion locks the scan state internally, so collect the intent here and
    // act once the read lock below is released.
    let mut want_delete: Option<String> = None;

    theme::card(ui, |ui| {
        let running = scanner.is_running();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Large Files")
                    .size(15.0)
                    .strong()
                    .color(TEXT()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if running {
                    if ui.button(RichText::new("Cancel").size(13.0)).clicked() {
                        scanner.cancel();
                    }
                    ui.add(egui::Spinner::new().size(16.0));
                } else {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Scan home")
                                    .size(13.0)
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(ACCENT())
                            .corner_radius(egui::CornerRadius::same(8)),
                        )
                        .clicked()
                    {
                        let home = std::env::var("HOME")
                            .map(PathBuf::from)
                            .unwrap_or_else(|_| PathBuf::from("/"));
                        scanner.start(home, *threshold_mb * 1024 * 1024, ctx.clone());
                    }
                }
                ui.label(RichText::new("MB").size(11.0).color(MUTED()));
                egui::ComboBox::from_id_salt("threshold")
                    .selected_text(threshold_mb.to_string())
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for opt in [10u64, 50, 100, 500, 1024] {
                            ui.selectable_value(threshold_mb, opt, opt.to_string());
                        }
                    });
                ui.label(RichText::new("≥").size(12.0).color(MUTED()));
            });
        });
        ui.add_space(6.0);

        let s = scanner.state.lock().unwrap_or_else(|e| e.into_inner());
        let status = if s.running {
            format!(
                "Scanning {} … {} files checked, {} large so far",
                s.root, s.scanned, s.matches
            )
        } else if s.done {
            format!(
                "Found {} files ≥ {} under {} ({} files scanned)",
                s.matches,
                theme::fmt_bytes(s.threshold),
                s.root,
                s.scanned
            )
        } else {
            "Scan your home folder to find what's eating disk space.".to_string()
        };
        ui.label(RichText::new(status).size(12.0).color(MUTED()));

        if let Some(err) = &s.last_error {
            ui.add_space(6.0);
            ui.label(RichText::new(err).size(12.0).color(BAD()));
        }

        if !s.top.is_empty() {
            ui.add_space(8.0);
            scan_header(ui);
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .max_height(TABLE_H)
                .auto_shrink([false, false])
                .id_salt("bigfiles_scroll")
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (i, f) in s.top.iter().enumerate() {
                        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                        // Show the containing folder, not the whole path again.
                        let dir = f.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                        let armed = confirm.as_deref() == Some(f.path.as_str());
                        match scan_row(
                            ui,
                            i % 2 == 1,
                            f.size,
                            name,
                            None,
                            dir,
                            &f.path,
                            armed,
                            false,
                        ) {
                            ScanRowAct::Arm => *confirm = Some(f.path.clone()),
                            ScanRowAct::Fire => {
                                want_delete = Some(f.path.clone());
                                *confirm = None;
                            }
                            ScanRowAct::Disarm => *confirm = None,
                            ScanRowAct::Nothing => {}
                        }
                    }
                });
        }
    });

    if let Some(path) = want_delete {
        scanner.delete(&path);
        ctx.request_repaint();
    }
}

/// Build/dependency directories found by the home scan (`node_modules`, Cargo
/// `target/`, `__pycache__`, `.venv`), deletable per-project or all at once —
/// they regenerate on the next build or install.
fn artifacts_card(
    ui: &mut egui::Ui,
    scanner: &Scanner,
    confirm: &mut Option<String>,
    all_confirm: &mut bool,
    ctx: &egui::Context,
) {
    // Deletion locks the scan state internally; collect the intent here and
    // act after the read lock below is released.
    let mut want_delete: Vec<String> = Vec::new();

    theme::card(ui, |ui| {
        section_title(ui, "Build Artifacts");
        ui.label(
            RichText::new(
                "Dependency and build caches found by the home scan — safe to \
                 delete, they come back on the next build or install.",
            )
            .size(12.0)
            .color(MUTED()),
        );

        let s = scanner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = &s.last_error {
            ui.add_space(6.0);
            ui.label(RichText::new(err).size(12.0).color(BAD()));
        }
        if s.artifacts.is_empty() {
            ui.add_space(6.0);
            let msg = if s.deleting {
                "Deleting…"
            } else if s.done {
                "None found in the last scan."
            } else {
                "Run a scan from Large Files to look for them."
            };
            ui.label(RichText::new(msg).size(12.0).color(MUTED()));
            return;
        }

        let total: u64 = s.artifacts.iter().map(|a| a.size).sum();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} found  ·  {} total",
                    s.artifacts.len(),
                    theme::fmt_bytes(total)
                ))
                .size(11.0)
                .color(MUTED()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if s.deleting {
                    ui.label(RichText::new("Deleting…").size(12.0).color(MUTED()));
                    ui.add(egui::Spinner::new().size(14.0));
                } else if *all_confirm {
                    if chip_button(ui, "Delete all now", Color32::WHITE, BAD()).clicked() {
                        want_delete = s.artifacts.iter().map(|a| a.path.clone()).collect();
                        *all_confirm = false;
                    }
                    if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                        *all_confirm = false;
                    }
                } else if chip_button(
                    ui,
                    &format!("Delete all · {}", theme::fmt_bytes(total)),
                    BAD(),
                    BAD().linear_multiply(0.16),
                )
                .clicked()
                {
                    *all_confirm = true;
                }
            });
        });
        ui.add_space(8.0);
        scan_header(ui);
        ui.add_space(2.0);
        egui::ScrollArea::vertical()
            .max_height(TABLE_H)
            .auto_shrink([false, false])
            .id_salt("artifacts_scroll")
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, a) in s.artifacts.iter().enumerate() {
                    // "target" or "node_modules" alone says nothing — the
                    // project folder above it is the informative name; the
                    // kind label already says what gets deleted.
                    let dir = a.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                    let project = dir
                        .rsplit('/')
                        .next()
                        .filter(|p| !p.is_empty())
                        .unwrap_or(a.path.as_str());
                    let armed = confirm.as_deref() == Some(a.path.as_str());
                    match scan_row(
                        ui,
                        i % 2 == 1,
                        a.size,
                        project,
                        Some(a.kind.label()),
                        dir,
                        &a.path,
                        armed,
                        s.deleting,
                    ) {
                        ScanRowAct::Arm => *confirm = Some(a.path.clone()),
                        ScanRowAct::Fire => {
                            want_delete = vec![a.path.clone()];
                            *confirm = None;
                        }
                        ScanRowAct::Disarm => *confirm = None,
                        ScanRowAct::Nothing => {}
                    }
                }
            });
    });

    if !want_delete.is_empty() {
        scanner.delete_artifacts(want_delete, ctx.clone());
    }
}

fn process_card(
    ui: &mut egui::Ui,
    m: &Metrics,
    sort: &mut Sort,
    filter: &mut String,
    selected: &mut Option<u32>,
    confirm: &mut Option<u32>,
) {
    let mut want_kill: Option<u32> = None;

    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Processes").size(15.0).strong().color(TEXT()));
            ui.label(
                RichText::new(format!("({})", m.procs.len()))
                    .size(12.0)
                    .color(MUTED()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(filter)
                        .hint_text("filter…")
                        .desired_width(140.0),
                );
                ui.label(RichText::new("sort").size(11.0).color(MUTED()));
                sort_btn(ui, sort, Sort::Pid, "PID");
                sort_btn(ui, sort, Sort::Name, "Name");
                sort_btn(ui, sort, Sort::Mem, "Mem");
                sort_btn(ui, sort, Sort::Cpu, "CPU");
            });
        });

        // Action bar for the selected process (End = SIGTERM, two-click confirm).
        if let Some(pid) = *selected {
            let name = m
                .procs
                .iter()
                .find(|p| p.pid == pid)
                .map(|p| p.name.as_str())
                .unwrap_or("(gone)");
            ui.add_space(6.0);
            egui::Frame::NONE
                .fill(theme::CARD_HI())
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("Selected: {name}  ·  PID {pid}"))
                                .size(12.0)
                                .color(TEXT()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if *confirm == Some(pid) {
                                if chip_button(ui, "End process", Color32::WHITE, BAD()).clicked() {
                                    want_kill = Some(pid);
                                    *confirm = None;
                                }
                                if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                                    *confirm = None;
                                }
                            } else {
                                if chip_button(
                                    ui,
                                    "End process",
                                    BAD(),
                                    BAD().linear_multiply(0.16),
                                )
                                .clicked()
                                {
                                    *confirm = Some(pid);
                                }
                                if ui.button(RichText::new("Clear").size(12.0)).clicked() {
                                    *selected = None;
                                    *confirm = None;
                                }
                            }
                        });
                    });
                });
        }
        ui.add_space(8.0);

        let needle = filter.to_lowercase();
        let mut rows: Vec<_> = m
            .procs
            .iter()
            .filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle))
            .collect();
        match *sort {
            Sort::Cpu => rows.sort_by(|a, b| {
                b.cpu
                    .partial_cmp(&a.cpu)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Mem => rows.sort_by_key(|p| std::cmp::Reverse(p.mem)),
            Sort::Name => rows.sort_by_key(|a| a.name.to_lowercase()),
            Sort::Pid => rows.sort_by_key(|a| a.pid),
        }

        // Header and rows share one fixed-column layout so every header sits
        // exactly above its values.
        proc_row(
            ui, "PID", "NAME", "USER", "CPU%", "MEM", true, false, false, false,
        );
        ui.add_space(2.0);

        egui::ScrollArea::vertical()
            .max_height(TABLE_H)
            .auto_shrink([false, false])
            .id_salt("proc_scroll")
            .show(ui, |ui| {
                for (i, p) in rows.iter().take(400).enumerate() {
                    let is_sel = *selected == Some(p.pid);
                    let resp = proc_row(
                        ui,
                        &p.pid.to_string(),
                        &p.name,
                        &p.user,
                        &format!("{:.1}", p.cpu),
                        &theme::fmt_bytes(p.mem),
                        false,
                        p.cpu > 50.0,
                        i % 2 == 1,
                        is_sel,
                    );
                    if resp.map(|r| r.clicked()).unwrap_or(false) {
                        *selected = if is_sel { None } else { Some(p.pid) };
                        *confirm = None;
                    }
                }
                if rows.len() > 400 {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("Showing the top 400 of {} processes", rows.len()))
                            .size(11.0)
                            .color(MUTED()),
                    );
                }
            });
    });

    if let Some(pid) = want_kill {
        // Clear the selection only if the signal was actually delivered; on
        // failure (e.g. a root-owned process) keep it selected so the user can
        // see it didn't work rather than it silently staying put.
        if kill_process(pid) {
            *selected = None;
            *confirm = None;
        }
    }
}

/// Politely ask a process to exit (SIGTERM), returning whether the signal was
/// delivered. `.status()` reaps the `kill` child (no zombie). Root-owned
/// processes fail without privilege — a pkexec fallback is a later nicety.
fn kill_process(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One process-table row (header or data) laid out with fixed-width columns so
/// the two always align. Numeric columns (CPU%, MEM) are right-aligned; NAME
/// flexes to fill the remaining width.
#[allow(clippy::too_many_arguments)]
fn proc_row(
    ui: &mut egui::Ui,
    pid: &str,
    name: &str,
    user: &str,
    cpu: &str,
    mem: &str,
    header: bool,
    cpu_hot: bool,
    stripe: bool,
    selected: bool,
) -> Option<egui::Response> {
    let size = if header { 11.0 } else { 12.0 };
    let mk = |s: &str, color: Color32, mono: bool| {
        let mut r = RichText::new(s).size(size).color(color);
        if mono {
            r = r.monospace();
        }
        if header {
            r = r.strong();
        }
        r
    };
    let name_color = if header { MUTED() } else { TEXT() };
    let cpu_color = if header {
        MUTED()
    } else if cpu_hot {
        BAD()
    } else {
        TEXT()
    };
    let mem_color = if header { MUTED() } else { TEXT() };

    let fill = if selected {
        ACCENT().linear_multiply(0.25)
    } else if stripe {
        theme::CARD_HI()
    } else {
        Color32::TRANSPARENT
    };
    let resp = egui::Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                proc_cell(ui, 56.0, mk(pid, MUTED(), true), false);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    proc_cell(ui, 92.0, mk(mem, mem_color, true), true);
                    proc_cell(ui, 60.0, mk(cpu, cpu_color, true), true);
                    proc_cell(ui, 120.0, mk(user, MUTED(), false), false);
                    // NAME fills the remaining width.
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(mk(name, name_color, false)).truncate());
                    });
                });
            });
        })
        .response;

    // Header isn't interactive; data rows are click-to-select.
    if header {
        None
    } else {
        Some(
            resp.interact(egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand),
        )
    }
}

/// A fixed-width table cell; `right` right-aligns its text (for numbers).
fn proc_cell(ui: &mut egui::Ui, w: f32, text: RichText, right: bool) {
    let layout = if right {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    ui.allocate_ui_with_layout(egui::vec2(w, 16.0), layout, |ui| {
        // Pin the cell to its declared width: allocate_ui_with_layout treats
        // the size as a hint and advances the row cursor by the text's actual
        // width, which drifts the columns out from under the header.
        ui.set_min_size(egui::vec2(w, 16.0));
        ui.add(egui::Label::new(text).truncate());
    });
}

fn sort_btn(ui: &mut egui::Ui, sort: &mut Sort, value: Sort, label: &str) {
    let selected = *sort == value;
    let color = if selected { ACCENT() } else { MUTED() };
    if ui
        .add(egui::Button::new(RichText::new(label).size(12.0).color(color)).frame(false))
        .clicked()
    {
        *sort = value;
    }
}

// ---------------------------------------------------------------------------
// Apps tab — manage installed APT packages and install new ones. Heavy work
// (dpkg-query / apt-cache / apt-get) runs on background threads in `apps.rs`;
// the long installed list is virtualised via `show_rows`, so only the handful
// of on-screen rows are ever built.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn apps_tab(
    ui: &mut egui::Ui,
    apps: &Apps,
    mode: &mut AppsMode,
    filter: &mut String,
    query: &mut String,
    confirm: &mut Option<String>,
    selected: &mut HashSet<String>,
    bulk_confirm: &mut bool,
    ctx: &egui::Context,
) {
    // Collect button intents while the lock is held, act after releasing it
    // (calling back into `apps` would otherwise re-lock and deadlock).
    let mut want_refresh = false;
    let mut want_search = false;
    let mut want_dismiss = false;
    let mut want_install: Option<String> = None;
    let mut want_remove: Option<String> = None;
    let mut want_remove_multi: Option<Vec<String>> = None;

    {
        let s = apps.state.lock().unwrap_or_else(|e| e.into_inner());

        // A running (or just-finished) install/remove takes over the view.
        if s.busy || (s.action_done && !s.log.is_empty()) {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    if s.busy {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(
                            RichText::new(&s.action_title)
                                .size(14.0)
                                .strong()
                                .color(ACCENT()),
                        );
                        ui.label(
                            RichText::new("authenticate in the pop-up if asked")
                                .size(11.0)
                                .color(MUTED()),
                        );
                    } else if s.action_ok {
                        ui.label(RichText::new("✓").size(18.0).strong().color(GOOD()));
                        ui.label(RichText::new("Done").size(14.0).strong().color(GOOD()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                want_dismiss = true;
                            }
                        });
                    } else {
                        ui.label(RichText::new("✕").size(18.0).strong().color(BAD()));
                        ui.label(
                            RichText::new("Didn't finish")
                                .size(14.0)
                                .strong()
                                .color(BAD()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                want_dismiss = true;
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                install_log_view(ui, &s.log);
            });
            if s.busy {
                return; // nothing else is actionable mid-run
            }
            ui.add_space(10.0);
        }

        theme::card(ui, |ui| {
            // Header: title + mode toggle + (Installed) refresh.
            ui.horizontal(|ui| {
                ui.label(RichText::new("Apps").size(15.0).strong().color(TEXT()));
                ui.add_space(8.0);
                mode_btn(ui, mode, AppsMode::Installed, "Installed");
                mode_btn(ui, mode, AppsMode::Install, "Install new");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if *mode == AppsMode::Installed
                        && ui
                            .add_enabled(
                                !s.loading,
                                egui::Button::new(RichText::new("Refresh").size(13.0)),
                            )
                            .clicked()
                    {
                        want_refresh = true;
                    }
                });
            });
            ui.add_space(10.0);

            if let Some(err) = &s.error {
                ui.label(RichText::new(err).size(12.0).color(BAD()));
                ui.add_space(6.0);
            }

            match *mode {
                AppsMode::Installed => {
                    if s.loading && s.installed.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(16.0));
                            ui.label(
                                RichText::new("Reading installed packages…")
                                    .size(13.0)
                                    .color(MUTED()),
                            );
                        });
                        return;
                    }
                    // Drop selections for packages that are no longer installed
                    // (e.g. removed by a prior run) so the count/size stay honest.
                    selected.retain(|n| s.installed.iter().any(|p| &p.name == n));

                    ui.add(
                        egui::TextEdit::singleline(filter)
                            .hint_text("filter installed…")
                            .desired_width(240.0),
                    );
                    ui.add_space(8.0);

                    let needle = filter.to_lowercase();
                    let filtered: Vec<&Pkg> = s
                        .installed
                        .iter()
                        .filter(|p| {
                            needle.is_empty()
                                || p.name.to_lowercase().contains(&needle)
                                || p.summary.to_lowercase().contains(&needle)
                        })
                        .collect();

                    let protected = s.installed.iter().filter(|p| p.protected).count();
                    ui.label(
                        RichText::new(format!(
                            "{} of {} packages · {} total · {} protected · sorted by size",
                            filtered.len(),
                            s.installed.len(),
                            theme::fmt_bytes(s.total_size_kb.saturating_mul(1024)),
                            protected
                        ))
                        .size(11.0)
                        .color(MUTED()),
                    );
                    ui.add_space(8.0);

                    // Bulk-remove bar for checkbox selections (two-click confirm).
                    if !selected.is_empty() {
                        egui::Frame::NONE
                            .fill(theme::CARD_HI())
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("{} selected", selected.len()))
                                            .size(12.0)
                                            .strong()
                                            .color(TEXT()),
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if *bulk_confirm {
                                            let label = format!("Remove {} now", selected.len());
                                            if chip_button(ui, &label, Color32::WHITE, BAD())
                                                .clicked()
                                            {
                                                want_remove_multi =
                                                    Some(selected.iter().cloned().collect());
                                                *bulk_confirm = false;
                                            }
                                            if ui
                                                .button(RichText::new("Cancel").size(12.0))
                                                .clicked()
                                            {
                                                *bulk_confirm = false;
                                            }
                                        } else {
                                            if chip_button(
                                                ui,
                                                "Remove selected",
                                                BAD(),
                                                BAD().linear_multiply(0.16),
                                            )
                                            .clicked()
                                            {
                                                *bulk_confirm = true;
                                            }
                                            if ui
                                                .button(RichText::new("Clear").size(12.0))
                                                .clicked()
                                            {
                                                selected.clear();
                                                *bulk_confirm = false;
                                            }
                                        }
                                    });
                                });
                            });
                        ui.add_space(8.0);
                    }

                    // One card per app, row-virtualised so only on-screen cards
                    // build. Pin the inter-card gap so each row is exactly
                    // ROW_SLOT tall (card 56 + gap 6) and scrolling stays aligned.
                    egui::ScrollArea::vertical()
                        .max_height(TABLE_H)
                        .auto_shrink([false, false])
                        .id_salt("apps_installed")
                        .show_rows(ui, ROW_SLOT, filtered.len(), |ui, range| {
                            ui.spacing_mut().item_spacing.y = ROW_SLOT - ROW_H - 16.0;
                            for i in range {
                                installed_card(
                                    ui,
                                    filtered[i],
                                    confirm,
                                    &mut want_remove,
                                    selected,
                                );
                            }
                        });
                }
                AppsMode::Install => {
                    let mut go = false;
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(query)
                                .hint_text("search Ubuntu's app catalog… e.g. gimp, vlc, obs")
                                .desired_width(ui.available_width() - 130.0),
                        );
                        let entered =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui
                            .add_enabled(
                                !s.searching,
                                egui::Button::new(
                                    RichText::new("Search")
                                        .size(13.0)
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(ACCENT())
                                .corner_radius(CornerRadius::same(8))
                                .min_size(egui::vec2(0.0, 28.0)),
                            )
                            .clicked()
                            || entered
                        {
                            go = true;
                        }
                    });
                    if go {
                        want_search = true;
                    }
                    ui.add_space(10.0);

                    if s.searching {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(16.0));
                            ui.label(RichText::new("Searching…").size(13.0).color(MUTED()));
                        });
                    } else if s.searched {
                        if s.results.is_empty() {
                            ui.label(
                                RichText::new("No matching packages found.")
                                    .size(13.0)
                                    .color(MUTED()),
                            );
                        } else {
                            let shown = s.results.len().min(10);
                            ui.label(
                                RichText::new(format!(
                                    "Showing the {shown} best matches of {}",
                                    s.results.len()
                                ))
                                .size(11.0)
                                .color(MUTED()),
                            );
                            ui.add_space(8.0);
                            for p in s.results.iter().take(10) {
                                let is_installed = s.installed.iter().any(|q| q.name == p.name);
                                search_card(ui, p, is_installed, &mut want_install);
                                ui.add_space(8.0);
                            }
                        }
                    } else {
                        ui.label(
                            RichText::new("Type an app name and search to install it from Ubuntu's repositories.")
                                .size(12.0)
                                .color(MUTED()),
                        );
                    }
                }
            }
        });
    }

    // --- act on collected intents (lock released) --------------------------
    if want_dismiss {
        if let Ok(mut s) = apps.state.lock() {
            s.log.clear();
            s.action_done = false;
        }
    }
    if want_refresh {
        apps.load_installed(ctx.clone());
    }
    if want_search {
        apps.search(query.clone(), ctx.clone());
    }
    if let Some(pkg) = want_install {
        apps.run("install", pkg, ctx.clone());
    }
    if let Some(pkg) = want_remove {
        apps.run("remove", pkg, ctx.clone());
    }
    if let Some(pkgs) = want_remove_multi {
        // Don't clear the selection here — it's pruned against the reloaded
        // installed list next render, so successfully-removed packages drop off
        // while any that failed to remove stay selected for a retry.
        apps.run_multi("remove", pkgs, ctx.clone());
    }
}

/// A raised card for one installed package: a select checkbox (non-critical
/// only), size badge + name/summary, and a clearly-clickable Uninstall (two-click)
/// — or a read-only "Protected" tag for OS-critical packages.
fn installed_card(
    ui: &mut egui::Ui,
    p: &Pkg,
    confirm: &mut Option<String>,
    want_remove: &mut Option<String>,
    selected: &mut HashSet<String>,
) {
    let is_sel = selected.contains(&p.name);
    app_card(ui, ROW_H, is_sel, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Right: the action — or a read-only lock for critical OS packages.
            if p.protected {
                protected_tag(ui);
            } else if confirm.as_deref() == Some(p.name.as_str()) {
                if chip_button(ui, "Remove now", Color32::WHITE, BAD()).clicked() {
                    *want_remove = Some(p.name.clone());
                    *confirm = None;
                }
                if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                    *confirm = None;
                }
            } else if chip_button(ui, "Uninstall", BAD(), BAD().linear_multiply(0.16)).clicked() {
                *confirm = Some(p.name.clone());
            }

            // Left: [select box] size badge + name + summary, filling the rest.
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                if p.protected {
                    // Keep the badge column aligned with the select-box rows.
                    ui.add_space(24.0);
                } else {
                    let checked = selected.contains(&p.name);
                    if select_box(ui, checked).clicked() {
                        if checked {
                            selected.remove(&p.name);
                        } else {
                            selected.insert(p.name.clone());
                        }
                    }
                    ui.add_space(6.0);
                }
                size_badge(ui, p.size_kb);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&p.name).size(14.0).strong().color(TEXT()));
                    if !p.summary.is_empty() {
                        ui.add(
                            egui::Label::new(RichText::new(&p.summary).size(11.0).color(MUTED()))
                                .truncate(),
                        );
                    }
                });
            });
        });
    });
}

/// A card for a search result: name + full description, with Install on the
/// right — or a muted "Installed" marker when it's already present.
fn search_card(ui: &mut egui::Ui, p: &Pkg, is_installed: bool, want_install: &mut Option<String>) {
    app_card(ui, ROW_H, false, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if is_installed {
                ui.label(
                    RichText::new("✓ Installed")
                        .size(12.0)
                        .strong()
                        .color(GOOD()),
                );
            } else if chip_button(ui, "Install", Color32::WHITE, ACCENT()).clicked() {
                *want_install = Some(p.name.clone());
            }

            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&p.name).size(14.0).strong().color(TEXT()));
                    if !p.summary.is_empty() {
                        ui.add(
                            egui::Label::new(RichText::new(&p.summary).size(11.0).color(MUTED()))
                                .truncate(),
                        );
                    }
                });
            });
        });
    });
}

/// The shared raised-card frame used by the app / file rows. The inner content
/// is allocated at a *fixed* height so nested left/right layouts can't balloon
/// to fill the whole virtualised scroll area (which they otherwise do).
fn app_card(ui: &mut egui::Ui, height: f32, selected: bool, add: impl FnOnce(&mut egui::Ui)) {
    let (fill, stroke) = if selected {
        (
            ACCENT().linear_multiply(0.10),
            Stroke::new(1.0, ACCENT().linear_multiply(0.35)),
        )
    } else {
        (theme::CARD_HI(), Stroke::NONE)
    };
    egui::Frame::NONE
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let w = ui.available_width();
            ui.allocate_ui_with_layout(
                egui::vec2(w, height),
                Layout::left_to_right(Align::Center),
                |ui| {
                    add(ui);
                },
            );
        });
}

/// A non-clickable "Protected" tag shown in place of Uninstall for OS-critical
/// packages, with an explanation on hover.
fn protected_tag(ui: &mut egui::Ui) {
    egui::Frame::NONE
        .fill(theme::TRACK())
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(9, 6))
        .show(ui, |ui| {
            ui.label(RichText::new("Protected").size(11.0).strong().color(MUTED()));
        })
        .response
        .on_hover_text("Critical system package — removing it could break your system, so it's read-only here.");
}

/// A compact, obviously-clickable pill button (fill + coloured label).
fn chip_button(ui: &mut egui::Ui, label: &str, fg: Color32, fill: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).strong().color(fg))
            .fill(fill)
            .corner_radius(CornerRadius::same(6))
            .min_size(egui::vec2(0.0, 28.0)),
    )
}

/// A clearly-visible custom checkbox: an outlined square (empty) that fills with
/// ACCENT() and a white tick when selected. egui's default checkbox is nearly
/// invisible on our dark rows, so we hand-draw one.
fn select_box(ui: &mut egui::Ui, checked: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::click());
    let painter = ui.painter();
    let rounding = CornerRadius::same(4);
    if checked {
        painter.rect_filled(rect, rounding, ACCENT());
        let stroke = Stroke::new(2.0, Color32::WHITE);
        let p1 = pos2(rect.left() + 4.0, rect.center().y + 1.0);
        let p2 = pos2(rect.center().x - 1.0, rect.bottom() - 4.5);
        let p3 = pos2(rect.right() - 3.5, rect.top() + 5.0);
        painter.line_segment([p1, p2], stroke);
        painter.line_segment([p2, p3], stroke);
    } else {
        let border = if resp.hovered() { ACCENT() } else { MUTED() };
        painter.rect_stroke(
            rect,
            rounding,
            Stroke::new(1.5, border),
            egui::StrokeKind::Middle,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn size_badge(ui: &mut egui::Ui, size_kb: u64) {
    ui.add_sized(
        [76.0, 18.0],
        egui::Label::new(
            RichText::new(theme::fmt_bytes(size_kb.saturating_mul(1024)))
                .monospace()
                .size(12.0)
                .strong()
                .color(VIOLET()),
        ),
    );
}

fn mode_btn<M: PartialEq + Copy>(ui: &mut egui::Ui, mode: &mut M, value: M, label: &str) {
    let selected = *mode == value;
    let (fg, bg) = if selected {
        (Color32::WHITE, ACCENT())
    } else {
        (MUTED(), Color32::TRANSPARENT)
    };
    let btn = egui::Button::new(RichText::new(label).size(12.0).strong().color(fg))
        .fill(bg)
        .corner_radius(CornerRadius::same(7))
        .min_size(egui::vec2(0.0, 26.0));
    if ui.add(btn).clicked() {
        *mode = value;
    }
}

// ---------------------------------------------------------------------------
// Cleanup tab — measure reclaimable space in safe caches/logs and clear the
// selected ones (see `cleanup.rs`). User documents are never touched.
// ---------------------------------------------------------------------------

fn cleanup_tab(
    ui: &mut egui::Ui,
    cleanup: &Cleanup,
    selected: &mut HashSet<String>,
    confirm: &mut bool,
    ctx: &egui::Context,
) {
    let mut want_scan = false;
    let mut want_clean = false;
    let mut want_dismiss = false;

    {
        let s = cleanup.state.lock().unwrap_or_else(|e| e.into_inner());

        // A running (or just-finished) clean takes over with its live log.
        if s.busy || (s.done && !s.log.is_empty()) {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    if s.busy {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(
                            RichText::new("Cleaning…")
                                .size(14.0)
                                .strong()
                                .color(ACCENT()),
                        );
                        ui.label(
                            RichText::new("authenticate in the pop-up if asked")
                                .size(11.0)
                                .color(MUTED()),
                        );
                    } else {
                        let (icon, color, text) = if s.ok {
                            ("✓", GOOD(), "Cleaned")
                        } else {
                            ("✕", BAD(), "Some items couldn't be cleaned")
                        };
                        ui.label(RichText::new(icon).size(18.0).strong().color(color));
                        ui.label(RichText::new(text).size(14.0).strong().color(color));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                want_dismiss = true;
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                install_log_view(ui, &s.log);
            });
            if s.busy {
                return;
            }
            ui.add_space(10.0);
        }

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Cleanup").size(15.0).strong().color(TEXT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !s.scanning,
                            egui::Button::new(RichText::new("Rescan").size(13.0)),
                        )
                        .clicked()
                    {
                        want_scan = true;
                    }
                });
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Reclaim disk space from caches and logs. Your documents are never touched.",
                )
                .size(12.0)
                .color(MUTED()),
            );
            ui.add_space(10.0);

            if let Some(err) = &s.error {
                ui.label(RichText::new(err).size(12.0).color(BAD()));
                ui.add_space(6.0);
            }

            if s.scanning && s.targets.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(
                        RichText::new("Calculating reclaimable space…")
                            .size(13.0)
                            .color(MUTED()),
                    );
                });
                return;
            }

            ui.label(
                RichText::new(format!(
                    "{} reclaimable in total",
                    theme::fmt_bytes(s.total())
                ))
                .size(11.0)
                .color(MUTED()),
            );
            ui.add_space(8.0);

            // Deselect targets that now have nothing to free (e.g. after a
            // rescan), so the "frees ~X" total can't read as a no-op.
            selected.retain(|k| {
                s.targets
                    .iter()
                    .any(|t| t.key == k && (t.size > 0 || t.items > 0))
            });

            for t in &s.targets {
                cleanup_row(ui, t, selected);
                ui.add_space(6.0);
            }

            let sel_size: u64 = s
                .targets
                .iter()
                .filter(|t| selected.contains(t.key))
                .map(|t| t.size)
                .sum();

            ui.add_space(4.0);
            egui::Frame::NONE
                .fill(theme::CARD_HI())
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if selected.is_empty() {
                            ui.label(
                                RichText::new("Select what to clean above")
                                    .size(12.0)
                                    .color(MUTED()),
                            );
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "{} selected  ·  frees ~{}",
                                    selected.len(),
                                    theme::fmt_bytes(sel_size)
                                ))
                                .size(12.0)
                                .strong()
                                .color(TEXT()),
                            );
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if selected.is_empty() {
                                return;
                            }
                            if *confirm {
                                if chip_button(ui, "Clean now", Color32::WHITE, ACCENT()).clicked()
                                {
                                    want_clean = true;
                                    *confirm = false;
                                }
                                if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                                    *confirm = false;
                                }
                            } else {
                                if chip_button(
                                    ui,
                                    "Clean selected",
                                    ACCENT(),
                                    ACCENT().linear_multiply(0.16),
                                )
                                .clicked()
                                {
                                    *confirm = true;
                                }
                                if ui.button(RichText::new("Clear").size(12.0)).clicked() {
                                    selected.clear();
                                    *confirm = false;
                                }
                            }
                        });
                    });
                });
        });
    }

    if want_scan {
        cleanup.scan(ctx.clone());
    }
    if want_clean {
        let keys: Vec<String> = selected.iter().cloned().collect();
        cleanup.clean(keys, ctx.clone());
        selected.clear();
        *confirm = false;
    }
    if want_dismiss {
        let mut s = cleanup.state.lock().unwrap_or_else(|e| e.into_inner());
        s.log.clear();
        s.done = false;
    }
}

/// One cleanup target row: select box + name/detail on the left, reclaimable
/// size on the right.
fn cleanup_row(ui: &mut egui::Ui, t: &CleanTarget, selected: &mut HashSet<String>) {
    let is_sel = selected.contains(t.key);
    // Snap/flatpak targets can have countable items even when sizing failed;
    // they stay selectable so the count keeps the row honest.
    let selectable = t.size > 0 || t.items > 0;
    app_card(ui, ROW_H, is_sel, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let size_color = if selectable { VIOLET() } else { MUTED() };
            let size_text = if t.size == 0 && t.items > 0 {
                format!("{} items", t.items)
            } else {
                theme::fmt_bytes(t.size)
            };
            ui.label(
                RichText::new(size_text)
                    .monospace()
                    .size(13.0)
                    .strong()
                    .color(size_color),
            );
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                if selectable {
                    let checked = selected.contains(t.key);
                    if select_box(ui, checked).clicked() {
                        if checked {
                            selected.remove(t.key);
                        } else {
                            selected.insert(t.key.to_string());
                        }
                    }
                } else {
                    // Nothing to reclaim — not selectable, keep the column aligned.
                    ui.add_space(18.0);
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(t.label).size(14.0).strong().color(TEXT()));
                    ui.add(
                        egui::Label::new(
                            RichText::new(t.detail.as_str()).size(11.0).color(MUTED()),
                        )
                        .truncate(),
                    );
                });
            });
        });
    });
}

/// Installed-kernel list with per-kernel removal (see `kernels.rs`). The
/// running and newest kernels are protected; removal is two-click and the
/// backend re-derives protection before acting.
fn kernels_card(
    ui: &mut egui::Ui,
    kernels: &Kernels,
    confirm: &mut Option<String>,
    ctx: &egui::Context,
) {
    let mut want_remove: Option<String> = None;
    let mut want_dismiss = false;

    {
        let s = kernels.state.lock().unwrap_or_else(|e| e.into_inner());

        // A running (or just-finished) removal takes over with its live log.
        if s.busy || (s.done && !s.log.is_empty()) {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    if s.busy {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(
                            RichText::new("Removing old kernel…")
                                .size(14.0)
                                .strong()
                                .color(ACCENT()),
                        );
                        ui.label(
                            RichText::new("authenticate in the pop-up if asked")
                                .size(11.0)
                                .color(MUTED()),
                        );
                    } else {
                        let (icon, color, text) = if s.ok {
                            ("✓", GOOD(), "Kernel removed")
                        } else {
                            ("✕", BAD(), "The kernel couldn't be removed")
                        };
                        ui.label(RichText::new(icon).size(18.0).strong().color(color));
                        ui.label(RichText::new(text).size(14.0).strong().color(color));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                want_dismiss = true;
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                install_log_view(ui, &s.log);
            });
            if s.busy {
                return;
            }
            ui.add_space(10.0);
        }

        theme::card(ui, |ui| {
            section_title(ui, "Kernels");
            ui.add_space(2.0);
            ui.label(
                RichText::new(
                    "Installed Linux kernels. The running and newest kernels are \
                     protected; \"Unused packages\" above already removes the ones \
                     apt considers expendable.",
                )
                .size(12.0)
                .color(MUTED()),
            );
            ui.add_space(8.0);

            if let Some(err) = &s.error {
                ui.label(RichText::new(err).size(12.0).color(BAD()));
                ui.add_space(6.0);
            }

            if s.loading && s.kernels.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(
                        RichText::new("Listing installed kernels…")
                            .size(13.0)
                            .color(MUTED()),
                    );
                });
                return;
            }

            for k in &s.kernels {
                app_card(ui, ROW_H, false, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if k.protected {
                            egui::Frame::NONE
                                .fill(theme::TRACK())
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(9, 6))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("Protected")
                                            .size(11.0)
                                            .strong()
                                            .color(MUTED()),
                                    );
                                })
                                .response
                                .on_hover_text(
                                    "The running kernel and the newest installed kernel \
                                     are never removable — you need one to boot and one \
                                     to fall back on.",
                                );
                        } else if confirm.as_deref() == Some(k.version.as_str()) {
                            if chip_button(ui, "Remove now", Color32::WHITE, BAD()).clicked() {
                                want_remove = Some(k.version.clone());
                                *confirm = None;
                            }
                            if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                                *confirm = None;
                            }
                        } else if chip_button(ui, "Remove", BAD(), BAD().linear_multiply(0.16))
                            .clicked()
                        {
                            *confirm = Some(k.version.clone());
                        }

                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(&k.version)
                                        .monospace()
                                        .size(14.0)
                                        .strong()
                                        .color(TEXT()),
                                );
                                let mut detail = format!(
                                    "{} package{}  ·  {}",
                                    k.packages.len(),
                                    if k.packages.len() == 1 { "" } else { "s" },
                                    theme::fmt_bytes(k.size_kb * 1024)
                                );
                                if k.running {
                                    detail.push_str("  ·  running");
                                }
                                if k.newest {
                                    detail.push_str("  ·  newest");
                                }
                                ui.label(RichText::new(detail).size(11.0).color(MUTED()));
                            });
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
    }

    if let Some(v) = want_remove {
        kernels.remove(v, ctx.clone());
    }
    if want_dismiss {
        let mut s = kernels.state.lock().unwrap_or_else(|e| e.into_inner());
        s.log.clear();
        s.done = false;
    }
}

// ---------------------------------------------------------------------------
// Startup & Services tab — autostart entries (user-level toggle) and systemd
// services (Start/Stop via pkexec, critical units guarded). See `startup.rs`.
// ---------------------------------------------------------------------------

fn startup_tab(
    ui: &mut egui::Ui,
    mgr: &StartupMgr,
    mode: &mut StartupMode,
    filter: &mut String,
    svc_confirm: &mut Option<String>,
    ctx: &egui::Context,
) {
    let mut want_reload = false;
    let mut want_toggle: Option<(String, bool)> = None; // (file, enable)
    let mut want_service: Option<(String, &'static str)> = None; // (unit, action)

    {
        let s = mgr.state.lock().unwrap_or_else(|e| e.into_inner());

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Startup & Services")
                        .size(15.0)
                        .strong()
                        .color(TEXT()),
                );
                ui.add_space(8.0);
                mode_btn(ui, mode, StartupMode::Apps, "Startup Apps");
                mode_btn(ui, mode, StartupMode::Services, "Services");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !s.loading && !s.busy,
                            egui::Button::new(RichText::new("Reload").size(13.0)),
                        )
                        .clicked()
                    {
                        want_reload = true;
                    }
                    if s.busy {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                });
            });
            ui.add_space(8.0);

            if let Some(err) = &s.error {
                ui.label(RichText::new(err).size(12.0).color(BAD()));
                ui.add_space(6.0);
            }

            if s.loading && s.apps.is_empty() && s.services.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(RichText::new("Loading…").size(13.0).color(MUTED()));
                });
                return;
            }

            match *mode {
                StartupMode::Apps => {
                    ui.label(
                        RichText::new(
                            "Apps that launch when you log in — toggle to enable or disable.",
                        )
                        .size(12.0)
                        .color(MUTED()),
                    );
                    ui.add_space(8.0);
                    if s.apps.is_empty() {
                        ui.label(
                            RichText::new("No startup apps found.")
                                .size(13.0)
                                .color(MUTED()),
                        );
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .max_height(TABLE_H)
                        .auto_shrink([false, false])
                        .id_salt("startup_apps")
                        .show_rows(ui, ROW_SLOT, s.apps.len(), |ui, range| {
                            ui.spacing_mut().item_spacing.y = ROW_SLOT - ROW_H - 16.0;
                            for i in range {
                                startup_app_row(ui, &s.apps[i], &mut want_toggle);
                            }
                        });
                }
                StartupMode::Services => {
                    if s.services.is_empty() {
                        ui.label(
                            RichText::new("No systemd services found on this system.")
                                .size(13.0)
                                .color(MUTED()),
                        );
                        return;
                    }
                    ui.add(
                        egui::TextEdit::singleline(filter)
                            .hint_text("filter services…")
                            .desired_width(240.0),
                    );
                    ui.add_space(8.0);
                    let needle = filter.to_lowercase();
                    let filtered: Vec<&Service> = s
                        .services
                        .iter()
                        .filter(|sv| {
                            needle.is_empty()
                                || sv.unit.to_lowercase().contains(&needle)
                                || sv.description.to_lowercase().contains(&needle)
                        })
                        .collect();
                    ui.label(
                        RichText::new(format!(
                            "{} of {} services · green = running",
                            filtered.len(),
                            s.services.len()
                        ))
                        .size(11.0)
                        .color(MUTED()),
                    );
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .max_height(TABLE_H)
                        .auto_shrink([false, false])
                        .id_salt("startup_services")
                        .show_rows(ui, ROW_SLOT, filtered.len(), |ui, range| {
                            ui.spacing_mut().item_spacing.y = ROW_SLOT - ROW_H - 16.0;
                            for i in range {
                                service_row(ui, filtered[i], svc_confirm, &mut want_service);
                            }
                        });
                }
            }
        });
    }

    if want_reload {
        mgr.load(ctx.clone());
    }
    if let Some((file, enable)) = want_toggle {
        mgr.toggle_startup(file, enable, ctx.clone());
    }
    if let Some((unit, action)) = want_service {
        mgr.service_action(unit, action, ctx.clone());
    }
}

fn startup_app_row(ui: &mut egui::Ui, app: &StartupApp, want_toggle: &mut Option<(String, bool)>) {
    app_card(ui, ROW_H, false, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (word, color) = if app.enabled {
                ("On", GOOD())
            } else {
                ("Off", MUTED())
            };
            ui.label(RichText::new(word).size(12.0).strong().color(color));
            ui.add_space(4.0);
            if select_box(ui, app.enabled).clicked() {
                *want_toggle = Some((app.file.clone(), !app.enabled));
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&app.name).size(14.0).strong().color(TEXT()));
                    if !app.comment.is_empty() {
                        ui.add(
                            egui::Label::new(RichText::new(&app.comment).size(11.0).color(MUTED()))
                                .truncate(),
                        );
                    }
                });
            });
        });
    });
}

fn service_row(
    ui: &mut egui::Ui,
    sv: &Service,
    confirm: &mut Option<String>,
    want_service: &mut Option<(String, &'static str)>,
) {
    app_card(ui, ROW_H, false, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Right: action.
            if sv.protected {
                protected_tag(ui);
            } else if sv.active {
                if confirm.as_deref() == Some(sv.unit.as_str()) {
                    if chip_button(ui, "Stop now", Color32::WHITE, BAD()).clicked() {
                        *want_service = Some((sv.unit.clone(), "stop"));
                        *confirm = None;
                    }
                    if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                        *confirm = None;
                    }
                } else if chip_button(ui, "Stop", BAD(), BAD().linear_multiply(0.16)).clicked() {
                    *confirm = Some(sv.unit.clone());
                }
            } else if chip_button(ui, "Start", Color32::WHITE, GOOD()).clicked() {
                *want_service = Some((sv.unit.clone(), "start"));
            }

            // Left: status dot + unit + description (+ boot state).
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                ui.painter().circle_filled(
                    rect.center(),
                    4.0,
                    if sv.active { GOOD() } else { MUTED() },
                );
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&sv.unit).size(13.0).strong().color(TEXT()));
                    let boot = if sv.enabled {
                        "  ·  starts at boot"
                    } else {
                        ""
                    };
                    let detail = if sv.description.is_empty() {
                        boot.trim_start_matches("  ·  ").to_string()
                    } else {
                        format!("{}{}", sv.description, boot)
                    };
                    ui.add(
                        egui::Label::new(RichText::new(detail).size(11.0).color(MUTED()))
                            .truncate(),
                    );
                });
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Sources tab — apt repository / PPA manager (see `repos.rs`). Base Ubuntu
// repos are protected; PPAs and third-party sources can be added/removed.
// ---------------------------------------------------------------------------

fn sources_tab(
    ui: &mut egui::Ui,
    repos: &Repos,
    input: &mut String,
    confirm: &mut Option<String>,
    ctx: &egui::Context,
) {
    let mut want_reload = false;
    let mut want_add = false;
    let mut want_remove: Option<Repo> = None;
    let mut want_dismiss = false;

    {
        let s = repos.state.lock().unwrap_or_else(|e| e.into_inner());

        // A running (or just-finished) add/remove takes over with its live log.
        if s.busy || (s.done && !s.log.is_empty()) {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    if s.busy {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(
                            RichText::new("Working…")
                                .size(14.0)
                                .strong()
                                .color(ACCENT()),
                        );
                        ui.label(
                            RichText::new("authenticate in the pop-up if asked")
                                .size(11.0)
                                .color(MUTED()),
                        );
                    } else {
                        let (icon, color, text) = if s.ok {
                            ("✓", GOOD(), "Done")
                        } else {
                            ("✕", BAD(), "Didn't finish")
                        };
                        ui.label(RichText::new(icon).size(18.0).strong().color(color));
                        ui.label(RichText::new(text).size(14.0).strong().color(color));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                want_dismiss = true;
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                install_log_view(ui, &s.log);
            });
            if s.busy {
                return;
            }
            ui.add_space(10.0);
        }

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Repositories")
                        .size(15.0)
                        .strong()
                        .color(TEXT()),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !s.loading,
                            egui::Button::new(RichText::new("Reload").size(13.0)),
                        )
                        .clicked()
                    {
                        want_reload = true;
                    }
                });
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new("Add a PPA or remove third-party sources. Base Ubuntu repositories are protected.")
                    .size(12.0)
                    .color(MUTED()),
            );
            ui.add_space(8.0);

            if let Some(err) = &s.error {
                ui.label(RichText::new(err).size(12.0).color(BAD()));
                ui.add_space(6.0);
            }

            // Add-a-PPA row. `add-apt-repository` ships in
            // software-properties-common and is often absent on minimal systems.
            let can_add = crate::util::has_bin("add-apt-repository");
            ui.horizontal(|ui| {
                let resp = ui.add_enabled(
                    can_add,
                    egui::TextEdit::singleline(input)
                        .hint_text("ppa:user/name")
                        .desired_width(ui.available_width() - 150.0),
                );
                let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui
                    .add_enabled(
                        can_add,
                        egui::Button::new(
                            RichText::new("Add PPA")
                                .size(13.0)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(ACCENT())
                        .corner_radius(CornerRadius::same(8))
                        .min_size(egui::vec2(0.0, 28.0)),
                    )
                    .clicked()
                    || (entered && can_add)
                {
                    want_add = true;
                }
            });
            if !can_add {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Adding a PPA needs `software-properties-common` — install it to enable this.")
                        .size(11.0)
                        .color(WARN()),
                );
            }
            ui.add_space(10.0);

            if s.loading && s.repos.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(RichText::new("Loading sources…").size(13.0).color(MUTED()));
                });
                return;
            }

            ui.label(
                RichText::new(format!("{} sources", s.repos.len()))
                    .size(11.0)
                    .color(MUTED()),
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(TABLE_H)
                .auto_shrink([false, false])
                .id_salt("sources_list")
                .show_rows(ui, ROW_SLOT, s.repos.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = ROW_SLOT - ROW_H - 16.0;
                    for i in range {
                        repo_row(ui, &s.repos[i], confirm, &mut want_remove);
                    }
                });
        });
    }

    if want_reload {
        repos.load(ctx.clone());
    }
    if want_add {
        repos.add_ppa(input.clone(), ctx.clone());
        input.clear();
    }
    if let Some(r) = want_remove {
        repos.remove(r, ctx.clone());
    }
    if want_dismiss {
        let mut s = repos.state.lock().unwrap_or_else(|e| e.into_inner());
        s.log.clear();
        s.done = false;
    }
}

fn repo_row(
    ui: &mut egui::Ui,
    repo: &Repo,
    confirm: &mut Option<String>,
    want_remove: &mut Option<Repo>,
) {
    app_card(ui, ROW_H, false, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if repo.protected {
                protected_tag(ui);
            } else if confirm.as_deref() == Some(repo.key().as_str()) {
                if chip_button(ui, "Remove now", Color32::WHITE, BAD()).clicked() {
                    *want_remove = Some(repo.clone());
                    *confirm = None;
                }
                if ui.button(RichText::new("Cancel").size(12.0)).clicked() {
                    *confirm = None;
                }
            } else if chip_button(ui, "Remove", BAD(), BAD().linear_multiply(0.16)).clicked() {
                *confirm = Some(repo.key());
            }

            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let primary = repo.ppa_spec.as_deref().unwrap_or(&repo.label);
                    ui.label(RichText::new(primary).size(14.0).strong().color(TEXT()));
                    let detail = if repo.ppa_spec.is_some() {
                        format!("{}  ·  {}", repo.label, repo.file)
                    } else {
                        repo.file.clone()
                    };
                    ui.add(
                        egui::Label::new(RichText::new(detail).size(11.0).color(MUTED()))
                            .truncate(),
                    );
                });
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Updates tab
// ---------------------------------------------------------------------------

fn primary_button(ui: &mut egui::Ui, label: &str, fill: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(13.0)
                .strong()
                .color(Color32::WHITE),
        )
        .fill(fill)
        .corner_radius(CornerRadius::same(8))
        .min_size(egui::vec2(0.0, 30.0)),
    )
}

fn updates_card(ui: &mut egui::Ui, checker: &UpdatesChecker, ctx: &egui::Context) {
    // Collect button intents while the lock is held, then act after releasing it.
    let mut want_check = false;
    let mut want_install = false;
    let mut want_refresh_snaps = false;
    let mut want_updater = false;
    let mut want_dismiss = false;

    {
        let s = checker.state.lock().unwrap_or_else(|e| e.into_inner());
        let busy = s.checking || s.installing;

        theme::card(ui, |ui| {
            // ---- Header: title + actions -----------------------------------
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("System Updates")
                        .size(15.0)
                        .strong()
                        .color(TEXT()),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Primary install action, offered only when there's something
                    // to install and we're idle.
                    if s.checked && s.count > 0 && !busy {
                        let label = format!(
                            "Install {} update{}",
                            s.count,
                            if s.count == 1 { "" } else { "s" }
                        );
                        if primary_button(ui, &label, ACCENT()).clicked() {
                            want_install = true;
                        }
                    }
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(RichText::new("Check again").size(13.0)),
                        )
                        .clicked()
                    {
                        want_check = true;
                    }
                    // Advanced escape hatch to the full graphical updater.
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(
                                RichText::new("Software Updater").size(13.0).color(MUTED()),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Open Ubuntu's graphical Software Updater")
                        .clicked()
                    {
                        want_updater = true;
                    }
                });
            });
            ui.add_space(10.0);

            // ---- Live install run ------------------------------------------
            if s.installing || (s.install_done && !s.install_log.is_empty()) {
                if s.installing {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(
                            RichText::new("Installing updates…")
                                .size(14.0)
                                .strong()
                                .color(ACCENT()),
                        );
                        ui.label(
                            RichText::new("authenticate in the pop-up if asked")
                                .size(11.0)
                                .color(MUTED()),
                        );
                    });
                } else if s.install_ok {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✓").size(18.0).strong().color(GOOD()));
                        ui.label(
                            RichText::new("Updates installed")
                                .size(14.0)
                                .strong()
                                .color(GOOD()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                want_dismiss = true;
                            }
                        });
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✕").size(18.0).strong().color(BAD()));
                        ui.label(
                            RichText::new("Update didn't finish")
                                .size(14.0)
                                .strong()
                                .color(BAD()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                want_dismiss = true;
                            }
                        });
                    });
                }
                ui.add_space(8.0);
                install_log_view(ui, &s.install_log);

                if s.reboot_required {
                    ui.add_space(8.0);
                    reboot_banner(ui);
                }
                return;
            }

            // ---- Checking / error / not-yet-checked ------------------------
            if s.checking {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(18.0));
                    ui.label(
                        RichText::new("Checking for updates…")
                            .size(13.0)
                            .color(MUTED()),
                    );
                });
                return;
            }
            if let Some(err) = &s.error {
                ui.label(RichText::new(err).size(13.0).color(BAD()));
                ui.add_space(6.0);
            }
            if !s.checked {
                ui.label(
                    RichText::new("Press “Check again” to look for updates.")
                        .size(13.0)
                        .color(MUTED()),
                );
                return;
            }

            // ---- Idle status -----------------------------------------------
            // "Up to date" only when BOTH apt and snap agree — snap refreshes
            // are updates too (the Snap Store would show them even if we didn't)
            // — and only when the check actually completed: a failed probe must
            // read as unknown, never as good news.
            if s.count == 0 && s.snap_count == 0 {
                if s.check_failed {
                    ui.label(
                        RichText::new("Update status unknown — the last check didn't complete.")
                            .size(14.0)
                            .color(MUTED()),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✓").size(22.0).strong().color(GOOD()));
                        ui.label(
                            RichText::new("Up to date")
                                .size(20.0)
                                .strong()
                                .color(GOOD()),
                        );
                    });
                }
            } else if s.count > 0 {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(s.count.to_string())
                            .size(28.0)
                            .strong()
                            .color(WARN()),
                    );
                    ui.label(
                        RichText::new("packages can be upgraded")
                            .size(14.0)
                            .color(TEXT()),
                    );
                });
            }

            if s.reboot_required {
                ui.add_space(8.0);
                reboot_banner(ui);
            }

            if s.count > 0 {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Install applies every available upgrade — you'll be asked for your password once.",
                    )
                    .size(11.0)
                    .color(MUTED()),
                );
            }

            if !s.packages.is_empty() {
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .id_salt("pkgs")
                    .show(ui, |ui| {
                        for name in &s.packages {
                            ui.label(RichText::new(name).size(12.0).monospace().color(TEXT()));
                        }
                    });
            }

            // ---- Snap refreshes --------------------------------------------
            if s.snap_count > 0 {
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(s.snap_count.to_string())
                            .size(28.0)
                            .strong()
                            .color(WARN()),
                    );
                    ui.label(
                        RichText::new(format!(
                            "snap{} can be refreshed",
                            if s.snap_count == 1 { "" } else { "s" }
                        ))
                        .size(14.0)
                        .color(TEXT()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = format!(
                            "Refresh {} snap{}",
                            s.snap_count,
                            if s.snap_count == 1 { "" } else { "s" }
                        );
                        if primary_button(ui, &label, ACCENT()).clicked() {
                            want_refresh_snaps = true;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Apps that are open may need to be closed before they can refresh.",
                    )
                    .size(11.0)
                    .color(MUTED()),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .id_salt("snap_pkgs")
                    .show(ui, |ui| {
                        for (name, version) in &s.snap_packages {
                            ui.label(
                                RichText::new(format!("{name}  →  {version}"))
                                    .size(12.0)
                                    .monospace()
                                    .color(TEXT()),
                            );
                        }
                    });
            }
        });
    }

    if want_updater {
        updates::open_software_updater();
    }
    if want_check {
        checker.check(ctx.clone());
    }
    if want_install {
        checker.install(ctx.clone());
    }
    if want_refresh_snaps {
        checker.refresh_snaps(ctx.clone());
    }
    if want_dismiss {
        let mut s = checker.state.lock().unwrap_or_else(|e| e.into_inner());
        s.install_log.clear();
        s.install_done = false;
    }
}

/// A dark, monospace transcript of apt's live output, pinned to the newest line.
fn install_log_view(ui: &mut egui::Ui, lines: &[String]) {
    egui::Frame::NONE
        .fill(theme::BG())
        .corner_radius(CornerRadius::same(8))
        .stroke(Stroke::new(1.0, theme::CARD_HI()))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .id_salt("install_log")
                .show(ui, |ui| {
                    for line in lines {
                        ui.label(RichText::new(line).size(11.0).monospace().color(MUTED()));
                    }
                });
        });
}

fn reboot_banner(ui: &mut egui::Ui) {
    egui::Frame::NONE
        .fill(WARN().linear_multiply(0.16))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(
                RichText::new("⚠  Restart required to finish installing updates")
                    .size(13.0)
                    .strong()
                    .color(WARN()),
            );
        });
}

fn power_card(ui: &mut egui::Ui, confirm: &mut Option<Confirm>) {
    theme::card(ui, |ui| {
        section_title(ui, "Power");
        ui.label(
            RichText::new("Restart this app, or reboot / shut down the system.")
                .size(12.0)
                .color(MUTED()),
        );
        ui.add_space(10.0);

        power_action(
            ui,
            confirm,
            Confirm::RestartApp,
            "Restart App",
            ACCENT(),
            "Restart App?",
        );
        ui.add_space(8.0);
        power_action(
            ui,
            confirm,
            Confirm::Reboot,
            "Reboot System…",
            theme::VIOLET(),
            "Reboot now?",
        );
        ui.add_space(8.0);
        power_action(
            ui,
            confirm,
            Confirm::PowerOff,
            "Power Off…",
            BAD(),
            "Power off now?",
        );
    });
}

/// A two-click power button: first click arms it, second click fires. Any
/// other armed action is cancelled so only one is pending at a time.
fn power_action(
    ui: &mut egui::Ui,
    confirm: &mut Option<Confirm>,
    action: Confirm,
    label: &str,
    color: Color32,
    prompt: &str,
) {
    if *confirm == Some(action) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(prompt).size(13.0).strong().color(color));
            if primary_button(ui, "Confirm", color).clicked() {
                *confirm = None;
                match action {
                    Confirm::Reboot => updates::reboot(),
                    Confirm::PowerOff => updates::power_off(),
                    Confirm::RestartApp => updates::restart_app(),
                }
            }
            if ui.button(RichText::new("Cancel").size(13.0)).clicked() {
                *confirm = None;
            }
        });
    } else if primary_button(ui, label, color).clicked() {
        *confirm = Some(action);
    }
}

fn about_card(ui: &mut egui::Ui, bin_size: u64) {
    theme::card(ui, |ui| {
        section_title(ui, "About");
        ui.label(
            RichText::new(format!("UPulse  v{}", env!("CARGO_PKG_VERSION")))
                .size(14.0)
                .strong()
                .color(TEXT()),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("A clean, GPU-rendered overview of your Ubuntu system.")
                .size(12.0)
                .color(MUTED()),
        );
        ui.add_space(10.0);
        about_row(ui, "App size", &theme::fmt_bytes(bin_size));
        about_row(ui, "Built with", "Rust · egui · sysinfo");
        about_row(ui, "System updates", "installed in-app · Updates tab");
        about_row(ui, "App update", "re-run install.sh in the project");
    });
}

fn about_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.0).color(MUTED()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(TEXT()));
        });
    });
    ui.add_space(2.0);
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn sparkline(ui: &mut egui::Ui, _id: &str, ring: &Ring, color: Color32, y_max: f32, height: f32) {
    plot_area(ui, height, y_max, &[(ring.values(), color, true)]);
}

/// Hand-drawn line graph (no `egui_plot`): a rounded track with one or more
/// series. Each series is `(samples, color, fill_under)`. Cheap to paint and,
/// crucially, it requests no animation repaints — so a focused window stays at
/// ~1 fps instead of spinning at 60 fps.
fn plot_area(ui: &mut egui::Ui, height: f32, y_max: f32, series: &[(Vec<f32>, Color32, bool)]) {
    let w = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), theme::CARD_HI());

    let maxv = y_max.max(1.0);
    let y_of = |v: f32| rect.bottom() - (v / maxv).clamp(0.0, 1.0) * rect.height();

    for (samples, color, fill) in series {
        let n = samples.len();
        if n < 2 {
            continue;
        }
        let x_step = rect.width() / (n as f32 - 1.0);
        let x_of = |i: usize| rect.left() + i as f32 * x_step;

        let pts: Vec<Pos2> = samples
            .iter()
            .enumerate()
            .map(|(i, v)| pos2(x_of(i), y_of(*v)))
            .collect();

        if *fill {
            // Fill under the curve as a triangle-strip mesh with a vertical
            // gradient — top vertices at α .42, bottom fully transparent.
            let top = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 107);
            let bot = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 0);
            let mut mesh = egui::Mesh::default();
            for (i, p) in pts.iter().enumerate() {
                mesh.colored_vertex(*p, top);
                mesh.colored_vertex(pos2(p.x, rect.bottom()), bot);
                if i > 0 {
                    let t0 = (2 * (i - 1)) as u32;
                    let b0 = t0 + 1;
                    let t1 = (2 * i) as u32;
                    let b1 = t1 + 1;
                    mesh.add_triangle(t0, b0, t1);
                    mesh.add_triangle(b0, b1, t1);
                }
            }
            painter.add(Shape::mesh(mesh));
        }
        painter.add(Shape::line(pts, Stroke::new(2.0, *color)));
    }
}

fn worst_disk_frac(m: &Metrics) -> f32 {
    m.disks
        .iter()
        .filter(|d| d.total > 0)
        .map(|d| d.used as f32 / d.total as f32)
        .fold(0.0, f32::max)
}
