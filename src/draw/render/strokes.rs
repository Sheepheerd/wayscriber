use super::smoothing::append_smooth_path;
use super::types::EraserReplayContext;
use crate::draw::Color;
use crate::draw::shape::{EraserBrush, EraserKind};
use std::cell::Cell;

/// Number of layered halo passes. Enough for a smooth falloff, few enough to
/// stay cheap on strokes that can hold thousands of points.
const GLOW_STEPS: u32 = 3;

/// Widest halo, as a multiple of stroke thickness at full glow strength.
const GLOW_MAX_SPREAD: f64 = 1.6;

/// Halo alpha relative to the stroke's own alpha at full glow strength.
const GLOW_ALPHA: f64 = 0.55;

thread_local! {
    /// Glow strength (0 = off) shared by every marker stroke on this thread.
    ///
    /// Held as a render setting rather than per-stroke data: it is a uniform
    /// style choice, and keeping it out of `Shape` means changing it restyles
    /// existing strokes instead of only newly drawn ones.
    static MARKER_GLOW: Cell<f64> = const { Cell::new(0.0) };
}

/// Sets marker glow strength, clamped to 0.0..=1.0.
pub fn set_marker_glow(strength: f64) {
    MARKER_GLOW.with(|cell| cell.set(strength.clamp(0.0, 1.0)));
}

/// Current marker glow strength.
pub fn marker_glow() -> f64 {
    MARKER_GLOW.with(Cell::get)
}

/// Render freehand stroke (polyline through points)
///
/// This function accepts a borrowed slice, avoiding clones for better performance.
/// Use this for rendering provisional shapes during drawing to prevent quadratic behavior.
pub fn render_freehand_borrowed(
    ctx: &cairo::Context,
    points: &[(i32, i32)],
    color: Color,
    thick: f64,
) {
    if points.is_empty() {
        return;
    }

    ctx.set_source_rgba(color.r, color.g, color.b, color.a);
    ctx.set_line_width(thick);
    ctx.set_line_cap(cairo::LineCap::Round);
    ctx.set_line_join(cairo::LineJoin::Round);

    // Start at first point
    let (x0, y0) = points[0];
    ctx.move_to(x0 as f64, y0 as f64);

    // Draw lines through all points
    for &(x, y) in &points[1..] {
        ctx.line_to(x as f64, y as f64);
    }

    let _ = ctx.stroke();
}

pub(crate) fn render_eraser_stroke(
    ctx: &cairo::Context,
    points: &[(i32, i32)],
    brush: &EraserBrush,
    eraser_ctx: &EraserReplayContext,
) {
    if points.is_empty() {
        return;
    }

    let stroke_width = brush.size.max(1.0);
    let (cap, join) = match brush.kind {
        EraserKind::Circle => (cairo::LineCap::Round, cairo::LineJoin::Round),
        EraserKind::Rect => (cairo::LineCap::Square, cairo::LineJoin::Miter),
    };

    let build_path = |ctx: &cairo::Context| {
        if points.len() == 1 {
            let (x, y) = (points[0].0 as f64, points[0].1 as f64);
            let half = stroke_width / 2.0;
            match brush.kind {
                EraserKind::Circle => ctx.arc(x, y, half, 0.0, std::f64::consts::PI * 2.0),
                EraserKind::Rect => ctx.rectangle(x - half, y - half, stroke_width, stroke_width),
            }
            return;
        }

        let (x0, y0) = points[0];
        ctx.move_to(x0 as f64, y0 as f64);
        for &(x, y) in &points[1..] {
            ctx.line_to(x as f64, y as f64);
        }
    };

    let _ = ctx.save();
    ctx.set_line_width(stroke_width);
    ctx.set_line_cap(cap);
    ctx.set_line_join(join);

    // Clear pass
    build_path(ctx);
    ctx.set_operator(cairo::Operator::Clear);
    let _ = ctx.stroke();

    // Paint background back into the cleared region, if available
    if let Some(pattern) = eraser_ctx.pattern {
        build_path(ctx);
        ctx.set_operator(cairo::Operator::Over);
        let _ = ctx.set_source(pattern);
        let _ = ctx.stroke();
    } else if let Some(color) = eraser_ctx.bg_color {
        build_path(ctx);
        ctx.set_operator(cairo::Operator::Over);
        ctx.set_source_rgba(color.r, color.g, color.b, color.a);
        let _ = ctx.stroke();
    }

    let _ = ctx.restore();
}

/// Render a marker stroke with soft edges and screen blending to mimic a physical highlighter.
pub fn render_marker_stroke_borrowed(
    ctx: &cairo::Context,
    points: &[(i32, i32)],
    color: Color,
    thick: f64,
) {
    if points.is_empty() {
        return;
    }

    // Reduce opacity to keep underlying text visible; clamp to avoid invisible strokes.
    let base_alpha = (color.a * 0.32).clamp(0.05, 0.85);
    let soft_width = (thick * 1.25).max(thick + 1.0);
    let glow = marker_glow();

    let draw_pass = |ctx: &cairo::Context, width: f64, alpha: f64| {
        ctx.set_source_rgba(color.r, color.g, color.b, alpha);
        ctx.set_line_width(width);
        ctx.set_line_cap(cairo::LineCap::Round);
        ctx.set_line_join(cairo::LineJoin::Round);
        // A curve through the samples instead of a polyline: pointer positions
        // are integers, so straight segments between them facet visibly at
        // marker widths.
        append_smooth_path(ctx, points);
        let _ = ctx.stroke();
    };

    ctx.save().ok();
    // Wide translucent edges are where stair-stepping shows most, so ask for
    // the best coverage cairo will compute rather than the default.
    ctx.set_antialias(cairo::Antialias::Best);
    ctx.set_operator(cairo::Operator::Screen);
    if glow > 0.0 {
        // Halo pass: widest and faintest, layered under the stroke so the ink
        // bleeds outward the way wet highlighter does on paper. Stepped so the
        // falloff reads as a gradient rather than one hard band.
        for step in (1..=GLOW_STEPS).rev() {
            let spread = step as f64 / GLOW_STEPS as f64;
            let width = soft_width + thick * GLOW_MAX_SPREAD * glow * spread;
            let alpha = base_alpha * GLOW_ALPHA * glow / (step as f64);
            draw_pass(ctx, width, alpha);
        }
    }
    // Soft outer pass for feathered edges
    draw_pass(ctx, soft_width, base_alpha * 0.7);
    // Core pass for body of the marker
    draw_pass(ctx, thick, base_alpha);
    ctx.restore().ok();
}
