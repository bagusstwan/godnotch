//! Pure software rendering subsystem using tiny-skia.
//!
//! Renders anti-aliased notch shapes into a CPU Pixmap and provides color conversion
//! to Win32 32-bit DIB Section BGRA premultiplied format.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, PixmapMut, Transform};

use crate::core::layout::NotchGeometry;

/// Kappa constant for cubic bezier circular arc approximation (4 * (sqrt(2) - 1) / 3).
pub const KAPPA: f32 = 0.552_284_8;

/// Renders the notch outline into a pixmap using tiny-skia with anti-aliasing.
///
/// Draws a solid black (#000000) notch flush with the top edge (y=0) and
/// rounded bottom corners.
pub fn render_notch(pixmap: &mut PixmapMut, geom: &NotchGeometry) {
    pixmap.fill(Color::TRANSPARENT);

    let x0 = geom.x;
    let x1 = geom.x + geom.width;
    let y0 = geom.y;
    let y1 = geom.y + geom.height;
    let r = geom.corner_radius;

    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y0);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + r * KAPPA, x1 - r + r * KAPPA, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - r * KAPPA, y1, x0, y1 - r + r * KAPPA, x0, y1 - r);
    pb.close();

    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color_rgba8(0, 0, 0, 255);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Converts premultiplied RGBA pixels from tiny-skia to premultiplied BGRA pixels for Win32 DIB Sections.
///
/// Both buffers must have identical lengths, and the length must be a multiple of 4 bytes (32-bit ARGB).
pub fn rgba_to_bgra_premultiplied(src: &[u8], dst: &mut [u8]) {
    assert_eq!(
        src.len(),
        dst.len(),
        "Source and destination pixel buffers must have matching lengths"
    );
    let (src_chunks, _) = src.as_chunks::<4>();
    let (dst_chunks, _) = dst.as_chunks_mut::<4>();
    for (s, d) in src_chunks.iter().zip(dst_chunks.iter_mut()) {
        d[0] = s[2]; // Blue  = Red from tiny-skia
        d[1] = s[1]; // Green = Green from tiny-skia
        d[2] = s[0]; // Red   = Blue from tiny-skia
        d[3] = s[3]; // Alpha = Alpha
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    #[test]
    fn test_rgba_to_bgra_conversion_all_cases() {
        // Test case 1: Fully transparent pixel (alpha 0)
        let src_trans = [0u8, 0, 0, 0];
        let mut dst_trans = [255u8; 4];
        rgba_to_bgra_premultiplied(&src_trans, &mut dst_trans);
        assert_eq!(dst_trans, [0, 0, 0, 0]);

        // Test case 2: Solid black (alpha 255)
        let src_black = [0u8, 0, 0, 255];
        let mut dst_black = [0u8; 4];
        rgba_to_bgra_premultiplied(&src_black, &mut dst_black);
        assert_eq!(dst_black, [0, 0, 0, 255]);

        // Test case 3: Solid red (alpha 255) -> In BGRA, Blue=0, Green=0, Red=255, Alpha=255
        let src_red = [255u8, 0, 0, 255];
        let mut dst_red = [0u8; 4];
        rgba_to_bgra_premultiplied(&src_red, &mut dst_red);
        assert_eq!(dst_red, [0, 0, 255, 255]);

        // Test case 4: Semi-transparent premultiplied pixel
        let src_semi = [128u8, 64, 32, 200];
        let mut dst_semi = [0u8; 4];
        rgba_to_bgra_premultiplied(&src_semi, &mut dst_semi);
        assert_eq!(dst_semi, [32, 64, 128, 200]);
    }

    #[test]
    fn test_render_notch_output() {
        let mut pixmap = Pixmap::new(460, 150).unwrap();
        let geom = crate::core::layout::calculate_geometry(0.0, 1.0);

        render_notch(&mut pixmap.as_mut(), &geom);

        // Pixmap should contain both transparent pixels outside the notch and black pixels inside
        let data = pixmap.data();

        // Top-left corner outside the notch (e.g. x=0, y=0) must be transparent
        assert_eq!(data[0..4], [0, 0, 0, 0]);

        // Center of the notch (x=230, y=10) must be solid black
        let center_idx = (10 * 460 + 230) * 4;
        assert_eq!(
            &data[center_idx..center_idx + 4],
            &[0, 0, 0, 255],
            "Center of the notch must be solid black"
        );
    }

    #[test]
    fn test_render_time_benchmark() {
        for scale in [1.0, 2.0] {
            let canvas = crate::core::layout::canvas_size(scale);
            let mut pixmap = Pixmap::new(canvas.width, canvas.height).unwrap();
            let geom = crate::core::layout::calculate_geometry(0.5, scale);

            // Warm up
            render_notch(&mut pixmap.as_mut(), &geom);

            let iterations = 100;
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                render_notch(&mut pixmap.as_mut(), &geom);
            }
            let elapsed = start.elapsed();
            let per_frame_ms = elapsed.as_secs_f64() * 1000.0 / iterations as f64;
            println!(
                "Render time at scale {:.1}x ({}x{}): {:.3} ms/frame",
                scale, canvas.width, canvas.height, per_frame_ms
            );
            assert!(
                per_frame_ms < 4.0,
                "Render time must be < 4.0 ms per frame, got {:.3} ms",
                per_frame_ms
            );
        }
    }
}
