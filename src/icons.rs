//! Nine hand-drawn navigation icons, one per tab. Pure geometry on a 16×16
//! logical box (lines, arcs, rounded rects) drawn with the `Painter` — no image
//! loading, no glyphs. Each fn scales into whatever `rect` the sidebar gives it
//! and strokes in the passed color (the nav foreground).
//!
//! Paths mirror the design's spec-sheet SVGs (1.6px stroke, round caps/joins).

use egui::{pos2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke};

pub type IconFn = fn(&Painter, Rect, Color32);

/// Map a logical 0..16 coordinate into the target rect.
fn p(rect: Rect, x: f32, y: f32) -> Pos2 {
    pos2(
        rect.min.x + x / 16.0 * rect.width(),
        rect.min.y + y / 16.0 * rect.height(),
    )
}

fn stroke(rect: Rect, c: Color32, w: f32) -> Stroke {
    Stroke::new(w * rect.width() / 16.0, c)
}

/// Draw a polyline through logical points.
fn line(painter: &Painter, rect: Rect, pts: &[(f32, f32)], c: Color32, w: f32) {
    let points: Vec<Pos2> = pts.iter().map(|&(x, y)| p(rect, x, y)).collect();
    painter.add(Shape::line(points, stroke(rect, c, w)));
}

/// Arc as a polyline: center (cx,cy), radius r (all logical), angles in degrees
/// (screen space, y-down; 0°=right, 90°=down, 270°=up).
#[allow(clippy::too_many_arguments)]
fn arc(
    painter: &Painter,
    rect: Rect,
    cx: f32,
    cy: f32,
    r: f32,
    a0: f32,
    a1: f32,
    c: Color32,
    w: f32,
) {
    let n = 28;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
            p(rect, cx + r * t.cos(), cy + r * t.sin())
        })
        .collect();
    painter.add(Shape::line(pts, stroke(rect, c, w)));
}

#[allow(clippy::too_many_arguments)]
fn rrect(
    painter: &Painter,
    rect: Rect,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rad: f32,
    c: Color32,
    sw: f32,
) {
    let r = Rect::from_min_max(p(rect, x, y), p(rect, x + w, y + h));
    painter.rect_stroke(
        r,
        CornerRadius::same((rad / 16.0 * rect.width()) as u8),
        stroke(rect, c, sw),
        egui::StrokeKind::Middle,
    );
}

// --- the nine icons ---------------------------------------------------------

/// Brand mark: shield + pulse (the painter twin of `assets/upulse.svg`).
pub fn brand(painter: &Painter, rect: Rect, c: Color32) {
    // Shield outline (closed polyline; curves sampled from the SVG path).
    line(
        painter,
        rect,
        &[
            (8.0, 2.6),
            (11.6, 4.0),
            (11.6, 8.0),
            (11.2, 10.0),
            (9.9, 11.5),
            (8.0, 12.6),
            (6.1, 11.5),
            (4.8, 10.0),
            (4.4, 8.0),
            (4.4, 4.0),
            (8.0, 2.6),
        ],
        c,
        1.5,
    );
    // Pulse.
    line(
        painter,
        rect,
        &[
            (5.9, 8.2),
            (6.9, 8.2),
            (7.6, 6.6),
            (8.5, 9.7),
            (9.1, 8.2),
            (10.1, 8.2),
        ],
        c,
        1.4,
    );
}

/// Gauge — arc + needle.
pub fn overview(painter: &Painter, rect: Rect, c: Color32) {
    arc(painter, rect, 8.0, 12.0, 6.0, 180.0, 360.0, c, 1.6);
    line(painter, rect, &[(8.0, 12.0), (11.2, 8.0)], c, 1.6);
}

/// Line chart.
pub fn performance(painter: &Painter, rect: Rect, c: Color32) {
    line(
        painter,
        rect,
        &[
            (1.5, 12.5),
            (4.7, 7.9),
            (7.5, 10.5),
            (10.5, 5.1),
            (14.0, 9.1),
        ],
        c,
        1.6,
    );
}

/// Stacked disks.
pub fn storage(painter: &Painter, rect: Rect, c: Color32) {
    rrect(painter, rect, 1.8, 2.2, 12.4, 3.4, 1.2, c, 1.4);
    rrect(painter, rect, 1.8, 6.6, 12.4, 3.4, 1.2, c, 1.4);
    rrect(painter, rect, 1.8, 11.0, 12.4, 3.4, 1.2, c, 1.4);
}

/// 2×2 grid.
pub fn apps(painter: &Painter, rect: Rect, c: Color32) {
    rrect(painter, rect, 1.8, 1.8, 5.2, 5.2, 1.4, c, 1.4);
    rrect(painter, rect, 9.0, 1.8, 5.2, 5.2, 1.4, c, 1.4);
    rrect(painter, rect, 1.8, 9.0, 5.2, 5.2, 1.4, c, 1.4);
    rrect(painter, rect, 9.0, 9.0, 5.2, 5.2, 1.4, c, 1.4);
}

/// Trash can.
pub fn cleanup(painter: &Painter, rect: Rect, c: Color32) {
    line(painter, rect, &[(2.0, 4.4), (14.0, 4.4)], c, 1.4); // lid
    line(
        painter,
        rect,
        &[(5.6, 4.4), (5.6, 2.8), (10.4, 2.8), (10.4, 4.4)],
        c,
        1.4,
    ); // handle
    line(
        painter,
        rect,
        &[(3.6, 4.4), (4.4, 13.6), (11.6, 13.6), (12.4, 4.4)],
        c,
        1.4,
    ); // body
}

/// Power symbol — near-full ring open at top + stem.
pub fn startup(painter: &Painter, rect: Rect, c: Color32) {
    arc(painter, rect, 8.0, 8.3, 5.6, 310.0, 590.0, c, 1.6);
    line(painter, rect, &[(8.0, 1.6), (8.0, 6.8)], c, 1.6);
}

/// Cube / layers.
pub fn sources(painter: &Painter, rect: Rect, c: Color32) {
    line(
        painter,
        rect,
        &[(8.0, 1.8), (14.0, 5.2), (8.0, 8.6), (2.0, 5.2), (8.0, 1.8)],
        c,
        1.4,
    );
    line(
        painter,
        rect,
        &[(2.0, 8.6), (8.0, 12.0), (14.0, 8.6)],
        c,
        1.4,
    );
}

/// Info circle.
pub fn system_info(painter: &Painter, rect: Rect, c: Color32) {
    painter.circle_stroke(
        p(rect, 8.0, 8.0),
        6.2 / 16.0 * rect.width(),
        stroke(rect, c, 1.4),
    );
    line(painter, rect, &[(8.0, 7.2), (8.0, 11.0)], c, 1.4);
    painter.circle_filled(p(rect, 8.0, 4.8), 0.9 / 16.0 * rect.width(), c);
}

/// Download to a baseline.
pub fn updates(painter: &Painter, rect: Rect, c: Color32) {
    line(painter, rect, &[(8.0, 1.8), (8.0, 9.2)], c, 1.6);
    line(
        painter,
        rect,
        &[(5.2, 6.6), (8.0, 9.4), (10.8, 6.6)],
        c,
        1.6,
    );
    line(painter, rect, &[(2.4, 13.0), (13.6, 13.0)], c, 1.6);
}
