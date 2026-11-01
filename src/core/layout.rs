//! Notch geometry, layout mapping, and DPI scaling subsystem.
//!
//! Pure mathematical calculations for notch dimensions, corner radii, canvas sizes,
//! typography layout constraints, content alpha transitions, and minute-timer scheduling.

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

/// Font size for clock in closed state (logical pixels).
pub const CLOSED_CLOCK_FONT_SIZE: f32 = 13.0;
/// Font size for clock in open state (logical pixels).
#[allow(dead_code)]
pub const OPEN_CLOCK_FONT_SIZE: f32 = 40.0;
/// Font size for date in open state (logical pixels).
#[allow(dead_code)]
pub const OPEN_DATE_FONT_SIZE: f32 = 13.0;

/// Horizontal padding inside the open notch (logical pixels).
#[allow(dead_code)] // Fase 1 T1.4
pub const OPEN_CONTENT_HORIZONTAL_PADDING: f32 = 20.0;

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

/// Calculates the opacity alpha [0.0..1.0] for the closed notch content.
/// alpha = clamp(1.0 - 2.2 * p_height, 0.0, 1.0)
#[must_use]
pub fn closed_content_alpha(p_height: f32) -> f32 {
    let p = p_height.clamp(0.0, 1.0);
    (1.0 - 2.2 * p).clamp(0.0, 1.0)
}

/// Calculates the opacity alpha [0.0..1.0] for the open notch content.
/// alpha = clamp((p_height - 0.45) * 2.2, 0.0, 1.0)
#[must_use]
pub fn open_content_alpha(p_height: f32) -> f32 {
    let p = p_height.clamp(0.0, 1.0);
    ((p - 0.45) * 2.2).clamp(0.0, 1.0)
}

/// Calculates the delay in milliseconds until the next minute boundary + 50ms safety margin.
/// `second`: 0..=59
/// `millisecond`: 0..=999
/// Clamped strictly between 1 ms and 61,000 ms.
#[must_use]
pub fn calculate_next_minute_delay_ms(second: u32, millisecond: u32) -> u32 {
    let safe_sec = second.min(59);
    let safe_ms = millisecond.min(999);
    let remaining_sec = 60 - safe_sec;
    let total_ms = (remaining_sec * 1000).saturating_sub(safe_ms) + 50;
    total_ms.clamp(1, 61_000)
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

    #[test]
    fn test_content_alpha_curves() {
        // Closed state
        assert_eq!(closed_content_alpha(0.0), 1.0);
        assert_eq!(open_content_alpha(0.0), 0.0);

        // Transition midpoint (p ~ 0.45): closed content is 0, open content is 0
        assert_eq!(closed_content_alpha(0.46), 0.0);
        assert_eq!(open_content_alpha(0.45), 0.0);

        // Open state
        assert_eq!(closed_content_alpha(1.0), 0.0);
        assert_eq!(open_content_alpha(1.0), 1.0);

        // Clamping bounds
        assert_eq!(closed_content_alpha(-0.5), 1.0);
        assert_eq!(closed_content_alpha(1.5), 0.0);
        assert_eq!(open_content_alpha(-0.5), 0.0);
        assert_eq!(open_content_alpha(1.5), 1.0);
    }

    #[test]
    fn test_next_minute_delay_calculation() {
        // Exact top of the minute (00.000s) -> (60 - 0)*1000 - 0 + 50 = 60050 ms
        assert_eq!(calculate_next_minute_delay_ms(0, 0), 60_050);

        // Half minute (30.500s) -> (60 - 30)*1000 - 500 + 50 = 29550 ms
        assert_eq!(calculate_next_minute_delay_ms(30, 500), 29_550);

        // End of minute (59.900s) -> (60 - 59)*1000 - 900 + 50 = 150 ms
        assert_eq!(calculate_next_minute_delay_ms(59, 900), 150);

        // 59.999s -> 1000 - 999 + 50 = 51 ms
        assert_eq!(calculate_next_minute_delay_ms(59, 999), 51);

        // Day boundary / out of bounds protection
        let delay_max = calculate_next_minute_delay_ms(0, 0);
        assert!((1..=61_000).contains(&delay_max));
        let delay_min = calculate_next_minute_delay_ms(59, 999);
        assert!((1..=61_000).contains(&delay_min));
    }

    #[test]
    fn test_longest_date_fits_within_open_notch_width() {
        let renderer = crate::core::text::FontRenderer::new();
        if !renderer.has_font() {
            println!(
                "[SKIP] System font not found in test environment; skipping text layout fit test"
            );
            return;
        }

        let longest_id = "Rabu, 30 September 2026";
        let longest_en = "Wednesday, September 30, 2026";
        let scales = [1.0_f32, 1.25, 1.5, 2.0];

        for &scale in &scales {
            let max_allowed_w = (OPEN_WIDTH_LOGICAL - OPEN_CONTENT_HORIZONTAL_PADDING) * scale;
            let font_size = (OPEN_DATE_FONT_SIZE * scale).round() as u32;

            let w_id = renderer.measure_text_width(longest_id, font_size, false);
            let w_en = renderer.measure_text_width(longest_en, font_size, false);

            println!(
                "[B4 DATE_LAYOUT_FIT @ Scale {:.2}x] Max allowed: {:.1} px | ID ('{}'): {:.1} px | EN ('{}'): {:.1} px",
                scale, max_allowed_w, longest_id, w_id, longest_en, w_en
            );

            assert!(
                w_id <= max_allowed_w,
                "Longest ID date exceeded available width at scale {scale}: {w_id} > {max_allowed_w}"
            );
            assert!(
                w_en <= max_allowed_w,
                "Longest EN date exceeded available width at scale {scale}: {w_en} > {max_allowed_w}"
            );
        }
    }
}
