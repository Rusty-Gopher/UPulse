//! Visual theme, palette, and small formatting/drawing helpers shared by the UI.
//!
//! Colors live in a [`Palette`] (dark is canonical, light is derived) held in a
//! thread-local — the UI is single-threaded, so a `Cell` is enough. The color
//! accessors (`ACCENT()`, `CARD()`, …) read the active palette, so switching
//! `set_palette(Palette::light())` + `install()` recolors the whole app with no
//! per-widget changes.

use std::cell::Cell;

use egui::{Color32, Context, CornerRadius, Stroke, Visuals};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The complete theme. Every field maps to one accessor fn below.
#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub card_hi: Color32,
    pub track: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub cyan: Color32,
    pub violet: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

impl Palette {
    pub const fn dark() -> Self {
        Self {
            bg: rgb(0x0e, 0x11, 0x17),
            panel: rgb(0x12, 0x16, 0x1f),
            card: rgb(0x18, 0x1d, 0x27),
            card_hi: rgb(0x20, 0x27, 0x33),
            track: rgb(0x2a, 0x31, 0x3f),
            text: rgb(0xe6, 0xe9, 0xef),
            muted: rgb(0x8b, 0x93, 0xa7),
            faint: rgb(0x5c, 0x64, 0x78),
            accent: rgb(0x60, 0xa5, 0xfa),
            cyan: rgb(0x34, 0xd3, 0x99),
            violet: rgb(0xa7, 0x8b, 0xfa),
            good: rgb(0x4a, 0xde, 0x80),
            warn: rgb(0xfb, 0xbf, 0x24),
            bad: rgb(0xf8, 0x71, 0x71),
        }
    }

    #[allow(dead_code)]
    pub const fn light() -> Self {
        Self {
            bg: rgb(0xf7, 0xf8, 0xfa),
            panel: rgb(0xff, 0xff, 0xff),
            card: rgb(0xff, 0xff, 0xff),
            card_hi: rgb(0xf0, 0xf2, 0xf6),
            track: rgb(0xe2, 0xe6, 0xee),
            text: rgb(0x10, 0x17, 0x25),
            muted: rgb(0x5b, 0x64, 0x78),
            faint: rgb(0x8a, 0x92, 0xa4),
            accent: rgb(0x25, 0x63, 0xeb),
            cyan: rgb(0x05, 0x96, 0x69),
            violet: rgb(0x7c, 0x3a, 0xed),
            good: rgb(0x16, 0xa3, 0x4a),
            warn: rgb(0xb4, 0x53, 0x09),
            bad: rgb(0xdc, 0x26, 0x26),
        }
    }

    /// True while this is a light palette (drives the egui base visuals).
    #[allow(dead_code)]
    fn is_light(&self) -> bool {
        self.text.r() < 128
    }
}

thread_local! {
    static ACTIVE: Cell<Palette> = const { Cell::new(Palette::dark()) };
}

#[allow(dead_code)]
pub fn set_palette(p: Palette) {
    ACTIVE.with(|a| a.set(p));
}

pub fn palette() -> Palette {
    ACTIVE.with(|a| a.get())
}

/// Whether the active palette is the light one.
#[allow(dead_code)]
pub fn is_light() -> bool {
    palette().is_light()
}

// --- color accessors (read the active palette) ------------------------------
// Uppercase to match the semantic token names used throughout the UI.
#[allow(non_snake_case)]
pub fn BG() -> Color32 {
    palette().bg
}
#[allow(dead_code)]
#[allow(non_snake_case)]
pub fn PANEL() -> Color32 {
    palette().panel
}
#[allow(non_snake_case)]
pub fn CARD() -> Color32 {
    palette().card
}
#[allow(non_snake_case)]
pub fn CARD_HI() -> Color32 {
    palette().card_hi
}
#[allow(non_snake_case)]
pub fn TRACK() -> Color32 {
    palette().track
}
#[allow(non_snake_case)]
pub fn TEXT() -> Color32 {
    palette().text
}
#[allow(non_snake_case)]
pub fn MUTED() -> Color32 {
    palette().muted
}
#[allow(dead_code)]
#[allow(non_snake_case)]
pub fn FAINT() -> Color32 {
    palette().faint
}
#[allow(non_snake_case)]
pub fn ACCENT() -> Color32 {
    palette().accent
}
#[allow(non_snake_case)]
pub fn CYAN() -> Color32 {
    palette().cyan
}
#[allow(non_snake_case)]
pub fn VIOLET() -> Color32 {
    palette().violet
}
#[allow(non_snake_case)]
pub fn GOOD() -> Color32 {
    palette().good
}
#[allow(non_snake_case)]
pub fn WARN() -> Color32 {
    palette().warn
}
#[allow(non_snake_case)]
pub fn BAD() -> Color32 {
    palette().bad
}

/// Install the visuals + spacing for the active palette. Call again after
/// `set_palette` to switch themes at runtime.
pub fn install(ctx: &Context) {
    let p = palette();
    let mut v = if p.is_light() {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.bg;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.card;
    v.widgets.noninteractive.bg_fill = p.card;
    v.widgets.inactive.bg_fill = p.card_hi;
    v.widgets.hovered.bg_fill = p.card_hi;
    v.widgets.active.bg_fill = p.accent;
    v.selection.bg_fill = p.accent.linear_multiply(0.35);
    v.window_corner_radius = CornerRadius::same(10);
    // One style for both of egui's built-in themes — our own palette decides
    // light vs dark, so egui must never switch visuals underneath us.
    ctx.all_styles_mut(|style| {
        style.visuals = v.clone();
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.window_margin = egui::Margin::same(14);
        // No widget animations: they keep requesting repaints, which would pin
        // the window at ~60 fps. We only need ~1 fps to refresh the numbers.
        style.animation_time = 0.0;
    });
}

/// Green → amber → red depending on how full something is (`frac` in `0..=1`).
pub fn usage_color(frac: f32) -> Color32 {
    if frac < 0.60 {
        GOOD()
    } else if frac < 0.85 {
        WARN()
    } else {
        BAD()
    }
}

/// A rounded card container with padding.
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(CARD())
        .corner_radius(CornerRadius::same(12))
        .stroke(Stroke::new(1.0, CARD_HI()))
        .inner_margin(egui::Margin::same(16))
        .show(ui, add);
}

/// A rounded, filled progress bar drawn by hand for a crisp look.
pub fn bar(ui: &mut egui::Ui, frac: f32, color: Color32, height: f32) {
    let w = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, height), egui::Sense::hover());
    let painter = ui.painter();
    let rounding = CornerRadius::same((height / 2.0) as u8);
    painter.rect_filled(rect, rounding, TRACK());
    if frac > 0.0 {
        let mut fill = rect;
        fill.set_width(w * frac.clamp(0.0, 1.0));
        painter.rect_filled(fill, rounding, color);
    }
}

/// Human-readable byte size, e.g. `12.4 GB`.
pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Human-readable transfer rate, e.g. `3.2 MB/s`.
pub fn fmt_rate(bytes_per_sec: f32) -> String {
    format!("{}/s", fmt_bytes(bytes_per_sec.max(0.0) as u64))
}

/// Uptime as `3d 04:12` style text.
pub fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h:02}:{m:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}
