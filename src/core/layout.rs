//! Notch geometry, layout mapping, and DPI scaling subsystem.
//!
//! Pure mathematical calculations for notch dimensions, corner radii, and canvas sizes.

/// Logical width of the backing window canvas.
pub const CANVAS_WIDTH_LOGICAL: f32 = 400.0;
/// Logical height of the backing window canvas.
pub const CANVAS_HEIGHT_LOGICAL: f32 = 130.0;

/// Logical width of the notch in closed state.
pub const CLOSED_WIDTH_LOGICAL: f32 = 164.0;
/// Logical height of the notch in closed state.
pub const CLOSED_HEIGHT_LOGICAL: f32 = 30.0;
/// Logical corner radius of the notch in closed state.
pub const CLOSED_RADIUS_LOGICAL: f32 = 9.0;

/// Logical width of the notch in open state.
pub const OPEN_WIDTH_LOGICAL: f32 = 360.0;
/// Logical height of the notch in open state.
pub const OPEN_HEIGHT_LOGICAL: f32 = 112.0;
/// Logical corner radius of the notch in open state.
pub const OPEN_RADIUS_LOGICAL: f32 = 24.0;

/// Physical dimensions of the backing window canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanvasSize {
    pub width: u32,
    pub height: u32,
}

/// Physical geometry of the rendered notch shape inside the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NotchGeometry {
    /// Width of the notch in physical pixels.
    pub width: f32,
    /// Height of the notch in physical pixels.
    pub height: f32,
    /// Radius of the bottom corners in physical pixels.
    pub corner_radius: f32,
    /// X coordinate of the top-left corner relative to the canvas origin.
    pub x: f32,
    /// Y coordinate of the top-left corner (always 0.0, flush with screen top).
    pub y: f32,
}

/// Returns the canvas dimensions in physical pixels for a given DPI scale.
#[must_use]
pub fn canvas_size(scale: f32) -> CanvasSize {
    let w = (CANVAS_WIDTH_LOGICAL * scale).round() as u32;
    let h = (CANVAS_HEIGHT_LOGICAL * scale).round() as u32;
    CanvasSize {
        width: w,
        height: h,
    }
}

/// Computes the notch geometry for independent width and height animation progress
/// and DPI scale factor.
///
/// Corner radius is determined exclusively by height progress clamped to [0.0, 1.0].
#[must_use]
pub fn calculate_geometry_per_axis(progress_w: f32, progress_h: f32, scale: f32) -> NotchGeometry {
    let canvas = canvas_size(scale);
    let max_w = canvas.width as f32;
    let max_h = canvas.height as f32;

    let log_w = CLOSED_WIDTH_LOGICAL + (OPEN_WIDTH_LOGICAL - CLOSED_WIDTH_LOGICAL) * progress_w;
    let log_h = CLOSED_HEIGHT_LOGICAL + (OPEN_HEIGHT_LOGICAL - CLOSED_HEIGHT_LOGICAL) * progress_h;

    // Corner radius strictly follows height progress clamped to [0.0, 1.0]
    let p_h = progress_h.clamp(0.0, 1.0);
    let log_r = CLOSED_RADIUS_LOGICAL + (OPEN_RADIUS_LOGICAL - CLOSED_RADIUS_LOGICAL) * p_h;

    let phys_w = (log_w * scale).clamp(CLOSED_WIDTH_LOGICAL * scale * 0.5, max_w);
    let phys_h = (log_h * scale).clamp(CLOSED_HEIGHT_LOGICAL * scale * 0.5, max_h);

    let max_r = (phys_h / 2.0).min(phys_w / 2.0);
    let phys_r = (log_r * scale).clamp(0.0, max_r);

    let x = ((max_w - phys_w) / 2.0).round();
    let y = 0.0_f32;

    NotchGeometry {
        width: phys_w,
        height: phys_h,
        corner_radius: phys_r,
        x,
        y,
    }
}

/// Computes the notch geometry for a given animation progress (0.0 = closed, 1.0 = open)
/// and DPI scale factor.
///
/// Top edge is always flat ($y = 0.0$), and the shape is centered horizontally within the canvas.
/// Overshoot (progress > 1.0) is clamped to the maximum canvas bounds.
#[must_use]
#[allow(dead_code)] // Fase 1 legacy single-axis wrapper
pub fn calculate_geometry(progress: f32, scale: f32) -> NotchGeometry {
    calculate_geometry_per_axis(progress, progress, scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canvas_size_scaling() {
        let scales = [1.0, 1.25, 1.5, 2.0];
        for s in scales {
            let cs = canvas_size(s);
            assert_eq!(cs.width, (CANVAS_WIDTH_LOGICAL * s).round() as u32);
            assert_eq!(cs.height, (CANVAS_HEIGHT_LOGICAL * s).round() as u32);
        }
    }

    #[test]
    fn test_closed_geometry_across_dpi_scales() {
        let scales = [1.0, 1.25, 1.5, 2.0];
        for s in scales {
            let geom = calculate_geometry(0.0, s);
            let canvas = canvas_size(s);

            let expected_w = (CLOSED_WIDTH_LOGICAL * s).round();
            let expected_h = (CLOSED_HEIGHT_LOGICAL * s).round();
            let expected_r = (CLOSED_RADIUS_LOGICAL * s).round();

            assert!((geom.width - expected_w).abs() <= 1.0);
            assert!((geom.height - expected_h).abs() <= 1.0);
            assert!((geom.corner_radius - expected_r).abs() <= 1.0);
            assert_eq!(geom.y, 0.0, "Top edge must be flush with y=0");

            // Horizontal centering
            let expected_x = ((canvas.width as f32 - geom.width) / 2.0).round();
            assert_eq!(geom.x, expected_x);
        }
    }

    #[test]
    fn test_open_geometry_across_dpi_scales() {
        let scales = [1.0, 1.25, 1.5, 2.0];
        for s in scales {
            let geom = calculate_geometry(1.0, s);
            let canvas = canvas_size(s);

            let expected_w = (OPEN_WIDTH_LOGICAL * s).round();
            let expected_h = (OPEN_HEIGHT_LOGICAL * s).round();
            let expected_r = (OPEN_RADIUS_LOGICAL * s).round();

            assert!((geom.width - expected_w).abs() <= 1.0);
            assert!((geom.height - expected_h).abs() <= 1.0);
            assert!((geom.corner_radius - expected_r).abs() <= 1.0);
            assert_eq!(geom.y, 0.0);

            let expected_x = ((canvas.width as f32 - geom.width) / 2.0).round();
            assert_eq!(geom.x, expected_x);
        }
    }

    #[test]
    fn test_overshoot_and_undershoot_bounds() {
        // Maximum opening overshoot: width ~5.43% (progress_w=1.0543), height ~2.84% (progress_h=1.0284)
        let scales = [1.0, 1.25, 1.5, 2.0];
        for s in scales {
            let canvas = canvas_size(s);
            let geom_overshoot = calculate_geometry_per_axis(1.0543, 1.0284, s);

            assert!(
                geom_overshoot.width <= canvas.width as f32,
                "Width with overshoot must not exceed canvas width at scale {s}"
            );
            assert!(
                geom_overshoot.height <= canvas.height as f32,
                "Height with overshoot must not exceed canvas height at scale {s}"
            );

            // Undershoot during close: progress_w = -0.001, progress_h = -0.001
            let geom_undershoot = calculate_geometry_per_axis(-0.001, -0.001, s);
            let min_w = CLOSED_WIDTH_LOGICAL * s * 0.5;
            let min_h = CLOSED_HEIGHT_LOGICAL * s * 0.5;

            assert!(
                geom_undershoot.width >= min_w,
                "Width must remain above minimum threshold during undershoot"
            );
            assert!(
                geom_undershoot.height >= min_h,
                "Height must remain above minimum threshold during undershoot"
            );
        }
    }
}
