//! Notch geometry, layout mapping, and DPI scaling subsystem.
//!
//! Pure mathematical calculations for notch dimensions, corner radii, and canvas sizes.

pub const CANVAS_WIDTH_LOGICAL: f32 = 460.0;
pub const CANVAS_HEIGHT_LOGICAL: f32 = 150.0;

pub const CLOSED_WIDTH_LOGICAL: f32 = 190.0;
pub const CLOSED_HEIGHT_LOGICAL: f32 = 34.0;
pub const CLOSED_RADIUS_LOGICAL: f32 = 10.0;

pub const OPEN_WIDTH_LOGICAL: f32 = 420.0;
pub const OPEN_HEIGHT_LOGICAL: f32 = 130.0;
pub const OPEN_RADIUS_LOGICAL: f32 = 28.0;

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
            assert_eq!(cs.width, (460.0 * s).round() as u32);
            assert_eq!(cs.height, (150.0 * s).round() as u32);
        }
    }

    #[test]
    fn test_closed_geometry_across_dpi_scales() {
        let scales = [1.0, 1.25, 1.5, 2.0];
        for s in scales {
            let geom = calculate_geometry(0.0, s);
            let canvas = canvas_size(s);

            let expected_w = (190.0 * s).round();
            let expected_h = (34.0 * s).round();
            let expected_r = (10.0 * s).round();

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

            let expected_w = (420.0 * s).round();
            let expected_h = (130.0 * s).round();
            let expected_r = (28.0 * s).round();

            assert!((geom.width - expected_w).abs() <= 1.0);
            assert!((geom.height - expected_h).abs() <= 1.0);
            assert!((geom.corner_radius - expected_r).abs() <= 1.0);
            assert_eq!(geom.y, 0.0);

            let expected_x = ((canvas.width as f32 - geom.width) / 2.0).round();
            assert_eq!(geom.x, expected_x);
        }
    }

    #[test]
    fn test_overshoot_clamping() {
        // Test with 6% spring overshoot (progress = 1.06)
        let s = 1.0;
        let geom = calculate_geometry(1.06, s);
        let canvas = canvas_size(s);

        assert!(geom.width > 420.0, "Overshoot should expand width");
        assert!(
            geom.width <= canvas.width as f32,
            "Width must not exceed canvas"
        );
        assert!(geom.height > 130.0, "Overshoot should expand height");
        assert!(
            geom.height <= canvas.height as f32,
            "Height must not exceed canvas"
        );
        assert_eq!(geom.y, 0.0);
    }
}
