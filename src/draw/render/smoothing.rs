//! Spline smoothing for hand-drawn point sequences.
//!
//! Pointer sampling gives integer coordinates at irregular intervals, so a raw
//! polyline through them shows visible corners — most obviously on wide,
//! translucent marker strokes where every vertex is several pixels across.
//! Fitting a curve through the samples removes the faceting without changing
//! the stored data.

/// Tension of the Catmull-Rom fit. 0.5 is the centripetal-ish standard: high
/// enough to round the corners, low enough not to overshoot on sharp direction
/// changes the way a looser spline does.
const TENSION: f64 = 0.5;

/// Appends a smooth curve through `points` to the current path.
///
/// Falls back to straight segments for one- and two-point inputs, where there
/// is no curve to fit.
pub(super) fn append_smooth_path(ctx: &cairo::Context, points: &[(i32, i32)]) {
    let Some(&(first_x, first_y)) = points.first() else {
        return;
    };
    ctx.move_to(first_x as f64, first_y as f64);

    if points.len() < 3 {
        for &(x, y) in &points[1..] {
            ctx.line_to(x as f64, y as f64);
        }
        return;
    }

    for index in 0..points.len() - 1 {
        // Catmull-Rom needs a neighbor on each side; duplicate the endpoints so
        // the first and last segments keep the curve anchored exactly on them.
        let previous = point_at(points, index as isize - 1);
        let start = point_at(points, index as isize);
        let end = point_at(points, index as isize + 1);
        let next = point_at(points, index as isize + 2);

        // Convert the Catmull-Rom segment to the equivalent cubic Bezier.
        let control_1 = (
            start.0 + (end.0 - previous.0) * TENSION / 3.0,
            start.1 + (end.1 - previous.1) * TENSION / 3.0,
        );
        let control_2 = (
            end.0 - (next.0 - start.0) * TENSION / 3.0,
            end.1 - (next.1 - start.1) * TENSION / 3.0,
        );

        ctx.curve_to(
            control_1.0,
            control_1.1,
            control_2.0,
            control_2.1,
            end.0,
            end.1,
        );
    }
}

/// Clamped sample, so indices just past either end reuse the endpoint.
fn point_at(points: &[(i32, i32)], index: isize) -> (f64, f64) {
    let clamped = index.clamp(0, points.len() as isize - 1) as usize;
    let (x, y) = points[clamped];
    (x as f64, y as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairo::{Format, ImageSurface};

    fn context() -> cairo::Context {
        let surface = ImageSurface::create(Format::ARgb32, 64, 64).unwrap();
        cairo::Context::new(&surface).unwrap()
    }

    #[test]
    fn an_empty_input_leaves_the_path_empty() {
        let ctx = context();

        append_smooth_path(&ctx, &[]);

        assert!(!ctx.has_current_point().unwrap());
    }

    #[test]
    fn the_curve_starts_and_ends_on_the_sampled_points() {
        let ctx = context();
        let points = [(4, 4), (20, 30), (40, 10), (60, 50)];

        append_smooth_path(&ctx, &points);

        let (x, y) = ctx.current_point().unwrap();
        assert!((x - 60.0).abs() < 1e-9, "curve ended at x={x}");
        assert!((y - 50.0).abs() < 1e-9, "curve ended at y={y}");
        let extents = ctx.path_extents().unwrap();
        assert!(extents.0 <= 4.0 && extents.1 <= 4.0);
    }

    #[test]
    fn a_two_point_input_stays_a_straight_segment() {
        let ctx = context();

        append_smooth_path(&ctx, &[(0, 0), (10, 10)]);

        // A straight line's extents are exactly its endpoints; a curve that
        // overshot would report a larger box.
        let extents = ctx.path_extents().unwrap();
        assert!((extents.0 - 0.0).abs() < 1e-9);
        assert!((extents.2 - 10.0).abs() < 1e-9);
    }

    #[test]
    fn smoothing_does_not_overshoot_a_straight_run() {
        let ctx = context();
        // Collinear samples must stay on the line: a spline with too much
        // tension would bow away from it.
        let points = [(0, 20), (10, 20), (20, 20), (30, 20)];

        append_smooth_path(&ctx, &points);

        let extents = ctx.path_extents().unwrap();
        assert!(
            (extents.1 - 20.0).abs() < 1e-6 && (extents.3 - 20.0).abs() < 1e-6,
            "collinear points bowed to {:?}",
            extents
        );
    }
}
