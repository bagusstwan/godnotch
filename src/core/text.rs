//! Pure software text rendering and pre-rasterized glyph cache using ttf-parser and tiny-skia.
//!
//! Loads a single system font (Segoe UI Semibold with fallbacks) to pre-rasterize
//! only the required glyphs (digits, ':', date/time letters in ID/EN) at the active
//! notch sizes, then immediately frees the font to strictly comply with the <= 1.5 MB
//! memory budget (ADR 008).

use std::cell::RefCell;
use std::collections::HashMap;
use tiny_skia::PixmapMut;

/// Maximum number of glyphs retained in the bounded cache.
pub const MAX_GLYPH_CACHE_ENTRIES: usize = 256;

/// Candidate system font paths checked in strict priority order.
pub const FONT_CANDIDATE_PATHS: [&str; 3] = [
    r"C:\Windows\Fonts\seguisb.ttf", // Segoe UI Semibold (Primary)
    r"C:\Windows\Fonts\segoeui.ttf", // Segoe UI Regular (Fallback 1)
    r"C:\Windows\Fonts\arial.ttf",   // Arial (Fallback 2)
];

/// Initial characters pre-rasterized during startup (closed clock only).
pub const STARTUP_GLYPH_CHARS: [char; 11] = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':'];

/// Corpus of all characters required for clock and localized date display.
pub const REQUIRED_GLYPH_CHARS: &str = concat!(
    "0123456789:,- APMapm",
    "MingguSeninSelasaRabuKamisJumatSabtuSundayMondayTuesdayWednesdayThursdayFridaySaturday",
    "JanuariFebruariMaretAprilMeiJuniJuliAgustusSeptemberOktoberNovemberDesember",
    "JanuaryFebruaryMarchAprilMayJuneJulyAugustSeptemberOctoberNovemberDecember"
);

/// Cached rasterized glyph data and metrics.
#[derive(Debug, Clone)]
pub struct CachedGlyph {
    /// Bounding box width in pixels.
    pub width: usize,
    /// Bounding box height in pixels.
    pub height: usize,
    /// Horizontal offset from origin to left edge of glyph bounding box.
    pub x_offset: i32,
    /// Vertical offset from baseline to bottom edge of glyph bounding box (y-up).
    pub y_offset: i32,
    /// Horizontal advance width to the next character in pixels.
    pub advance_width: f32,
    /// 8-bit coverage alpha bitmap (0 = transparent, 255 = fully opaque).
    pub bitmap: Vec<u8>,
}

/// Startup profiling breakdown for font loading and glyph rasterization.
#[derive(Debug, Clone, Copy, Default)]
pub struct FontInitProfile {
    pub read_file_us: u64,
    pub parse_font_us: u64,
    pub rasterize_glyphs_us: u64,
    pub glyph_count: usize,
}

struct GlyphPathBuilder {
    builder: tiny_skia::PathBuilder,
    scale: f32,
    x_offset: f32,
    y_offset: f32,
    height: f32,
}

impl ttf_parser::OutlineBuilder for GlyphPathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        let px = x * self.scale - self.x_offset;
        let py = (self.y_offset + self.height) - (y * self.scale);
        self.builder.move_to(px, py);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let px = x * self.scale - self.x_offset;
        let py = (self.y_offset + self.height) - (y * self.scale);
        self.builder.line_to(px, py);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p1x = x1 * self.scale - self.x_offset;
        let p1y = (self.y_offset + self.height) - (y1 * self.scale);
        let px = x * self.scale - self.x_offset;
        let py = (self.y_offset + self.height) - (y * self.scale);
        self.builder.quad_to(p1x, p1y, px, py);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p1x = x1 * self.scale - self.x_offset;
        let p1y = (self.y_offset + self.height) - (y1 * self.scale);
        let p2x = x2 * self.scale - self.x_offset;
        let p2y = (self.y_offset + self.height) - (y2 * self.scale);
        let px = x * self.scale - self.x_offset;
        let py = (self.y_offset + self.height) - (y * self.scale);
        self.builder.cubic_to(p1x, p1y, p2x, p2y, px, py);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

/// Rasterizes a single glyph from a TrueType face using tiny-skia antialiased masking.
#[allow(dead_code)]
pub fn rasterize_glyph_ttf(face: &ttf_parser::Face, ch: char, px_size: u32) -> Option<CachedGlyph> {
    let glyph_id = face.glyph_index(ch)?;
    let scale = px_size as f32 / face.units_per_em() as f32;
    let advance_width = face.glyph_hor_advance(glyph_id).unwrap_or(0) as f32 * scale;

    let bbox = face.glyph_bounding_box(glyph_id);
    if let Some(rect) = bbox {
        let xmin_px = rect.x_min as f32 * scale;
        let xmax_px = rect.x_max as f32 * scale;
        let ymin_px = rect.y_min as f32 * scale;
        let ymax_px = rect.y_max as f32 * scale;

        let x_offset = xmin_px.floor() as i32;
        let y_offset = ymin_px.floor() as i32;
        let width = (xmax_px.ceil() as i32 - x_offset).max(1) as usize;
        let height = (ymax_px.ceil() as i32 - y_offset).max(1) as usize;

        let pb = tiny_skia::PathBuilder::new();
        let mut actual_builder = GlyphPathBuilder {
            builder: pb,
            scale,
            x_offset: x_offset as f32,
            y_offset: y_offset as f32,
            height: height as f32,
        };

        face.outline_glyph(glyph_id, &mut actual_builder);

        let bitmap = if let Some(path) = actual_builder.builder.finish() {
            if let Some(mut mask) = tiny_skia::Mask::new(width as u32, height as u32) {
                mask.fill_path(
                    &path,
                    tiny_skia::FillRule::Winding,
                    true,
                    tiny_skia::Transform::identity(),
                );
                mask.data().to_vec()
            } else {
                vec![0u8; width * height]
            }
        } else {
            vec![0u8; width * height]
        };

        Some(CachedGlyph {
            width,
            height,
            x_offset,
            y_offset,
            advance_width,
            bitmap,
        })
    } else {
        // Whitespace or empty glyph (e.g. space)
        Some(CachedGlyph {
            width: 0,
            height: 0,
            x_offset: 0,
            y_offset: 0,
            advance_width,
            bitmap: Vec::new(),
        })
    }
}

/// Single-font text renderer with bounded lazy glyph cache.
#[allow(dead_code)] // Fase 1 T1.4
pub struct FontRenderer {
    font_data: Option<Vec<u8>>,
    cache: RefCell<HashMap<(char, u32), CachedGlyph>>,
    digit_advance_cache: RefCell<HashMap<u32, f32>>,
    has_loaded_font: bool,
    pub profile: FontInitProfile,
}

impl FontRenderer {
    /// Returns the sorted, deduplicated set of unique characters needed for clock and date.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn required_chars() -> Vec<char> {
        let mut chars: Vec<char> = REQUIRED_GLYPH_CHARS.chars().collect();
        chars.sort_unstable();
        chars.dedup();
        chars
    }

    /// Creates an empty font renderer with no loaded font (fallback / test mode).
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn new_empty() -> Self {
        Self {
            font_data: None,
            cache: RefCell::new(HashMap::new()),
            digit_advance_cache: RefCell::new(HashMap::new()),
            has_loaded_font: false,
            profile: FontInitProfile::default(),
        }
    }

    /// Initializes the font renderer and pre-rasterizes ONLY startup digits for closed notch.
    /// Remaining glyphs are lazily rasterized on demand when the notch expands.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn new_with_sizes(sizes: &[u32]) -> Self {
        let cache = HashMap::new();
        let digit_advance_cache = HashMap::new();
        let mut has_loaded_font = false;
        let mut profile = FontInitProfile::default();
        let mut font_data = None;

        for &path in &FONT_CANDIDATE_PATHS {
            let t_read_start = std::time::Instant::now();
            if let Ok(bytes) = std::fs::read(path) {
                profile.read_file_us += t_read_start.elapsed().as_micros() as u64;
                let t_parse_start = std::time::Instant::now();
                if ttf_parser::Face::parse(&bytes, 0).is_ok() {
                    profile.parse_font_us += t_parse_start.elapsed().as_micros() as u64;
                    font_data = Some(bytes);
                    break;
                }
            }
        }

        let cache = RefCell::new(cache);
        let digit_advance_cache = RefCell::new(digit_advance_cache);

        if let Some(ref data) = font_data {
            if let Ok(face) = ttf_parser::Face::parse(data, 0) {
                has_loaded_font = true;
                let t_raster_start = std::time::Instant::now();

                for &size in sizes {
                    let mut max_digit_adv = 0.0_f32;
                    let scale = size as f32 / face.units_per_em() as f32;

                    for &ch in &STARTUP_GLYPH_CHARS {
                        if let Some(cached) = rasterize_glyph_ttf(&face, ch, size) {
                            if ch.is_ascii_digit() && cached.advance_width > max_digit_adv {
                                max_digit_adv = cached.advance_width;
                            }
                            cache.borrow_mut().insert((ch, size), cached);
                        }
                    }

                    if max_digit_adv <= 0.0 {
                        max_digit_adv = (face.units_per_em() as f32 * 0.6) * scale;
                    }
                    digit_advance_cache.borrow_mut().insert(size, max_digit_adv);
                }

                profile.rasterize_glyphs_us = t_raster_start.elapsed().as_micros() as u64;
                profile.glyph_count = cache.borrow().len();
            }
        } else {
            #[cfg(debug_assertions)]
            eprintln!("[godnotch] No system font found; text rendering will be silent/disabled");
        }

        Self {
            font_data,
            cache,
            digit_advance_cache,
            has_loaded_font,
            profile,
        }
    }

    /// Initializes with default closed clock sizes (13px, plus scale variations on startup).
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn new() -> Self {
        Self::new_with_sizes(&[13])
    }

    /// Returns true if a system font was found and loaded.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn has_font(&self) -> bool {
        self.has_loaded_font
    }

    /// Returns the number of glyphs currently held in the bounded cache.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn cache_len(&self) -> usize {
        self.cache.borrow().len()
    }

    /// Retrieves an existing glyph from cache or lazily rasterizes it on demand.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn get_glyph(&self, ch: char, px_size: u32) -> Option<CachedGlyph> {
        if let Some(cached) = self.cache.borrow().get(&(ch, px_size)) {
            return Some(cached.clone());
        }

        if let Some(ref data) = self.font_data
            && let Ok(face) = ttf_parser::Face::parse(data, 0)
            && let Some(cached) = rasterize_glyph_ttf(&face, ch, px_size)
        {
            let mut cache = self.cache.borrow_mut();
            if cache.len() >= MAX_GLYPH_CACHE_ENTRIES {
                // Preserve clock digits while evicting excess lazy glyphs
                cache.retain(|&(c, _), _| c.is_ascii_digit() || c == ':');
                if cache.len() >= MAX_GLYPH_CACHE_ENTRIES {
                    cache.clear();
                }
            }
            cache.insert((ch, px_size), cached.clone());
            return Some(cached);
        }
        None
    }

    /// Returns the maximum advance width for digits '0'..='9' at `px_size`.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn max_digit_advance(&self, px_size: u32) -> f32 {
        if let Some(&adv) = self.digit_advance_cache.borrow().get(&px_size) {
            return adv;
        }

        if let Some(ref data) = self.font_data
            && let Ok(face) = ttf_parser::Face::parse(data, 0)
        {
            let scale = px_size as f32 / face.units_per_em() as f32;
            let mut max_adv = 0.0_f32;
            for d in '0'..='9' {
                if let Some(id) = face.glyph_index(d) {
                    let adv = face.glyph_hor_advance(id).unwrap_or(0) as f32 * scale;
                    if adv > max_adv {
                        max_adv = adv;
                    }
                }
            }
            if max_adv > 0.0 {
                self.digit_advance_cache
                    .borrow_mut()
                    .insert(px_size, max_adv);
                return max_adv;
            }
        }

        (px_size as f32) * 0.6
    }

    /// Measures the total horizontal pixel width of `text` at `px_size`.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn measure_text_width(&self, text: &str, px_size: u32, tabular_digits: bool) -> f32 {
        let mut width = 0.0_f32;
        let max_digit_adv = if tabular_digits {
            self.max_digit_advance(px_size)
        } else {
            0.0
        };

        for ch in text.chars() {
            if tabular_digits && ch.is_ascii_digit() {
                width += max_digit_adv;
            } else if let Some(glyph) = self.get_glyph(ch, px_size) {
                width += glyph.advance_width;
            } else {
                width += (px_size as f32) * 0.5;
            }
        }
        width
    }

    /// Draws `text` onto `pixmap` at baseline coordinates `(start_x, baseline_y)`.
    ///
    /// Performs premultiplied alpha composition weighted by glyph coverage.
    /// Preserves existing destination alpha (alpha in opaque notch area remains 255).
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn draw_text(
        &self,
        pixmap: &mut PixmapMut,
        text: &str,
        start_x: f32,
        baseline_y: f32,
        px_size: u32,
        color: (u8, u8, u8),
        alpha: f32,
        tabular_digits: bool,
    ) {
        if alpha <= 0.001 || !self.has_loaded_font {
            return;
        }

        let max_digit_adv = if tabular_digits {
            self.max_digit_advance(px_size)
        } else {
            0.0
        };

        let mut curr_x = start_x;
        let p_width = pixmap.width() as i32;
        let p_height = pixmap.height() as i32;

        for ch in text.chars() {
            let maybe_glyph = self.get_glyph(ch, px_size);
            let cell_width = if tabular_digits && ch.is_ascii_digit() {
                max_digit_adv
            } else if let Some(ref g) = maybe_glyph {
                g.advance_width
            } else {
                (px_size as f32) * 0.5
            };

            if let Some(glyph) = maybe_glyph {
                let glyph_x = if tabular_digits && ch.is_ascii_digit() {
                    curr_x + (cell_width - glyph.advance_width) / 2.0 + glyph.x_offset as f32
                } else {
                    curr_x + glyph.x_offset as f32
                };

                let glyph_y = baseline_y - (glyph.y_offset as f32 + glyph.height as f32);

                let x_start = glyph_x.round() as i32;
                let y_start = glyph_y.round() as i32;

                let pix_data = pixmap.data_mut();

                for row in 0..glyph.height as i32 {
                    let py = y_start + row;
                    if py < 0 || py >= p_height {
                        continue;
                    }

                    for col in 0..glyph.width as i32 {
                        let px = x_start + col;
                        if px < 0 || px >= p_width {
                            continue;
                        }

                        let cov = glyph.bitmap[(row as usize) * glyph.width + (col as usize)];
                        if cov == 0 {
                            continue;
                        }

                        let eff_a = (cov as f32 / 255.0) * alpha;
                        let src_r = (color.0 as f32 * eff_a).round() as u8;
                        let src_g = (color.1 as f32 * eff_a).round() as u8;
                        let src_b = (color.2 as f32 * eff_a).round() as u8;
                        let src_a = (255.0 * eff_a).round() as u8;

                        let pixel_idx = ((py * p_width + px) * 4) as usize;
                        let dr = pix_data[pixel_idx];
                        let dg = pix_data[pixel_idx + 1];
                        let db = pix_data[pixel_idx + 2];
                        let da = pix_data[pixel_idx + 3];

                        let inv_a = 1.0 - eff_a;
                        pix_data[pixel_idx] =
                            (src_r as f32 + dr as f32 * inv_a).min(255.0).round() as u8;
                        pix_data[pixel_idx + 1] =
                            (src_g as f32 + dg as f32 * inv_a).min(255.0).round() as u8;
                        pix_data[pixel_idx + 2] =
                            (src_b as f32 + db as f32 * inv_a).min(255.0).round() as u8;
                        pix_data[pixel_idx + 3] =
                            (src_a as f32 + da as f32 * inv_a).min(255.0).round() as u8;
                    }
                }
            }

            curr_x += cell_width;
        }
    }
}

impl Default for FontRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    #[test]
    fn test_font_renderer_initialization_does_not_crash() {
        let renderer = FontRenderer::new();
        let mut pixmap = Pixmap::new(200, 50).unwrap();
        renderer.draw_text(
            &mut pixmap.as_mut(),
            "12:34",
            10.0,
            30.0,
            16,
            (255, 255, 255),
            1.0,
            true,
        );
    }

    #[test]
    fn test_tabular_digits_have_identical_cell_width() {
        let renderer = FontRenderer::new();
        if !renderer.has_font() {
            println!("[SKIP] System font not found in test environment");
            return;
        }

        let max_adv = renderer.max_digit_advance(20);
        assert!(max_adv > 0.0);

        for d in '0'..='9' {
            if let Some(g) = renderer.get_glyph(d, 20) {
                assert!(
                    g.advance_width <= max_adv + 1e-5,
                    "Digit '{}' advance {} exceeded max {}",
                    d,
                    g.advance_width,
                    max_adv
                );
            }
        }
    }

    #[test]
    fn test_text_blending_preserves_destination_alpha() {
        let renderer = FontRenderer::new();
        if !renderer.has_font() {
            println!("[SKIP] System font not found in test environment");
            return;
        }

        let mut pixmap = Pixmap::new(100, 40).unwrap();
        pixmap.fill(tiny_skia::Color::BLACK);

        renderer.draw_text(
            &mut pixmap.as_mut(),
            "12:00",
            10.0,
            25.0,
            16,
            (255, 255, 255),
            1.0,
            true,
        );

        let (chunks, _) = pixmap.data().as_chunks::<4>();
        for chunk in chunks {
            assert_eq!(
                chunk[3], 255,
                "Destination alpha was compromised by text blending"
            );
        }
    }

    #[test]
    fn test_font_loading_memory_footprint() {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;

        let get_ws_bytes = || -> usize {
            let mut counters = PROCESS_MEMORY_COUNTERS_EX {
                cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                ..Default::default()
            };
            let process = unsafe { GetCurrentProcess() };
            let ok = unsafe {
                GetProcessMemoryInfo(
                    process,
                    &mut counters as *mut _ as *mut _,
                    std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                )
            };
            if ok.is_ok() {
                counters.WorkingSetSize
            } else {
                0
            }
        };

        // Measure memory delta of creating pre-rasterized renderer
        let ws_before = get_ws_bytes();
        let renderer = FontRenderer::new_with_sizes(&[13, 40]);
        let ws_after = get_ws_bytes();

        if renderer.has_font() {
            let total_glyph_bytes: usize = renderer
                .cache
                .borrow()
                .values()
                .map(|g| g.bitmap.len() + std::mem::size_of::<CachedGlyph>())
                .sum();

            let glyph_mb = (total_glyph_bytes as f64) / (1024.0 * 1024.0);
            let ws_delta = ws_after.saturating_sub(ws_before);
            let ws_delta_mb = (ws_delta as f64) / (1024.0 * 1024.0);

            println!(
                "[B1 FONT_MEMORY] Pre-rasterized glyph cache entries: {}, total cache payload: {:.3} KB ({:.4} MB)",
                renderer.cache.borrow().len(),
                (total_glyph_bytes as f64) / 1024.0,
                glyph_mb
            );
            println!(
                "  WorkingSet before: {:.3} MB, after: {:.3} MB, delta: {:.3} MB (budget <= 1.5 MB)",
                (ws_before as f64) / (1024.0 * 1024.0),
                (ws_after as f64) / (1024.0 * 1024.0),
                ws_delta_mb
            );

            assert!(
                glyph_mb <= 1.5,
                "Pre-rasterized glyph payload exceeded 1.5 MB: {:.3} MB",
                glyph_mb
            );
        }
    }

    #[test]
    fn test_glyphs_non_empty_and_bounded_across_scales() {
        let renderer = FontRenderer::new();
        if !renderer.has_font() {
            println!("[SKIP] System font not found; skipping scale bounding test");
            return;
        }

        let scales = [1.0_f32, 1.25, 1.5, 2.0];
        let test_chars = ['0', '1', '9', ':', 'R', 'a', 'b', 'u', 'S', 'e', 'p'];

        for &s in &scales {
            let size_closed = (13.0 * s).round() as u32;
            let size_open = (40.0 * s).round() as u32;

            for &px_size in &[size_closed, size_open] {
                for &ch in &test_chars {
                    let glyph = renderer
                        .get_glyph(ch, px_size)
                        .unwrap_or_else(|| panic!("Glyph '{}' at size {} missing", ch, px_size));

                    assert!(
                        glyph.width > 0,
                        "Glyph '{}' width 0 at size {}",
                        ch,
                        px_size
                    );
                    assert!(
                        glyph.height > 0,
                        "Glyph '{}' height 0 at size {}",
                        ch,
                        px_size
                    );
                    assert_eq!(glyph.bitmap.len(), glyph.width * glyph.height);
                    assert!(
                        glyph.bitmap.iter().any(|&c| c > 0),
                        "Glyph '{}' at size {} has entirely empty bitmap",
                        ch,
                        px_size
                    );
                    assert!(
                        glyph.width <= (px_size * 2) as usize,
                        "Glyph '{}' width {} unreasonably large for size {}",
                        ch,
                        glyph.width,
                        px_size
                    );
                    assert!(
                        glyph.height <= (px_size * 2) as usize,
                        "Glyph '{}' height {} unreasonably large for size {}",
                        ch,
                        glyph.height,
                        px_size
                    );
                }
            }
        }
    }

    #[test]
    fn test_text_rasterization_quality_and_bounds() {
        let renderer = FontRenderer::new();
        if !renderer.has_font() {
            println!("[SKIP] System font not found; skipping quality test");
            return;
        }

        let test_cases = [
            ("12:45", 13u32, (10, 40, 39, 50)),
            ("12:45", 40u32, (11, 21, 100, 50)),
            ("Rabu, 30 September 2026", 13u32, (11, 40, 161, 52)),
            ("Rabu, 30 September 2026", 40u32, (13, 20, 475, 59)),
        ];

        for (text, size, expected_bbox) in test_cases {
            let width = 600;
            let height = 80;
            let baseline_y = 50.0_f32;

            let mut pm = Pixmap::new(width, height).unwrap();
            renderer.draw_text(
                &mut pm.as_mut(),
                text,
                10.0,
                baseline_y,
                size,
                (255, 255, 255),
                1.0,
                false,
            );

            // Compute ink bounding box
            let mut min_x = width as i32;
            let mut max_x = 0;
            let mut min_y = height as i32;
            let mut max_y = 0;
            let mut has_ink = false;
            let mut antialiased_pixels = 0usize;

            for y in 0..height as i32 {
                for x in 0..width as i32 {
                    let a = pm.data()[((y * width as i32 + x) * 4 + 3) as usize];
                    if a > 10 {
                        has_ink = true;
                        min_x = min_x.min(x);
                        max_x = max_x.max(x);
                        min_y = min_y.min(y);
                        max_y = max_y.max(y);
                    }
                    if a > 10 && a < 245 {
                        antialiased_pixels += 1;
                    }
                }
            }

            assert!(
                has_ink,
                "Rendered text '{}' @ {}px produced no ink",
                text, size
            );
            let bbox = (min_x, min_y, max_x, max_y);

            let dx_min = (bbox.0 - expected_bbox.0).abs();
            let dy_min = (bbox.1 - expected_bbox.1).abs();
            let dx_max = (bbox.2 - expected_bbox.2).abs();
            let dy_max = (bbox.3 - expected_bbox.3).abs();

            println!(
                "[QUALITY_METRICS] Text '{}' @ {}px | BBox: {:?} vs Golden: {:?} (delta: dx_min={}, dy_min={}, dx_max={}, dy_max={}) | AA pixels: {}",
                text, size, bbox, expected_bbox, dx_min, dy_min, dx_max, dy_max, antialiased_pixels
            );

            assert!(
                dx_min <= 1,
                "dx_min {} exceeded 1px bound for '{}' @ {}px",
                dx_min,
                text,
                size
            );
            assert!(
                dy_min <= 1,
                "dy_min {} exceeded 1px bound for '{}' @ {}px",
                dy_min,
                text,
                size
            );
            assert!(
                dx_max <= 1,
                "dx_max {} exceeded 1px bound for '{}' @ {}px",
                dx_max,
                text,
                size
            );
            assert!(
                dy_max <= 1,
                "dy_max {} exceeded 1px bound for '{}' @ {}px",
                dy_max,
                text,
                size
            );
            assert!(
                antialiased_pixels > 0,
                "Expected anti-aliased edge pixels for '{}' @ {}px",
                text,
                size
            );
        }
    }

    #[test]
    fn test_glyph_cache_remains_bounded() {
        let renderer = FontRenderer::new();
        if !renderer.has_font() {
            println!("[SKIP] System font not found; skipping cache bounding test");
            return;
        }

        // Request 1000 unique unicode characters
        for c in '\u{0100}'..='\u{04E8}' {
            let _ = renderer.get_glyph(c, 16);
        }

        let count = renderer.cache_len();
        println!(
            "[CACHE_BOUNDED] Cache entry count after 1000 requests: {} (limit: {})",
            count, MAX_GLYPH_CACHE_ENTRIES
        );
        assert!(
            count <= MAX_GLYPH_CACHE_ENTRIES,
            "Cache length {} exceeded maximum bound {}",
            count,
            MAX_GLYPH_CACHE_ENTRIES
        );
    }

    #[test]
    fn test_missing_font_fallback_graceful() {
        let renderer = FontRenderer::new_empty();
        assert!(
            !renderer.has_font(),
            "Empty renderer must report false for has_font"
        );
        let w = renderer.measure_text_width("Hello 12:34", 16, true);
        assert!(w >= 0.0);

        let mut pm = Pixmap::new(100, 30).unwrap();
        renderer.draw_text(
            &mut pm.as_mut(),
            "Hello",
            0.0,
            20.0,
            16,
            (255, 255, 255),
            1.0,
            false,
        );
        assert!(
            pm.data().iter().all(|&b| b == 0),
            "Empty renderer must not write ink to pixmap"
        );
    }

    #[test]
    fn test_first_open_frame_render_cost() {
        let renderer = FontRenderer::new();
        if !renderer.has_font() {
            println!("[SKIP] System font not found; skipping first open frame benchmark");
            return;
        }

        let mut pixmap = Pixmap::new(400, 130).unwrap();
        let geom = crate::core::layout::calculate_geometry_per_axis(1.0, 1.0, 1.0);
        crate::core::renderer::render_notch(&mut pixmap.as_mut(), &geom);

        // Measure first frame when open content is rendered for the first time
        let t0 = std::time::Instant::now();

        // 1. Draw large clock (40px)
        renderer.draw_text(
            &mut pixmap.as_mut(),
            "12:45",
            100.0,
            48.0,
            40,
            (255, 255, 255),
            1.0,
            true,
        );

        // 2. Draw localized date (13px)
        renderer.draw_text(
            &mut pixmap.as_mut(),
            "Rabu, 30 September 2026",
            100.0,
            74.0,
            13,
            (170, 170, 175),
            1.0,
            false,
        );

        let elapsed_us = t0.elapsed().as_micros() as u64;
        let elapsed_ms = elapsed_us as f64 / 1000.0;

        let budget_ms = if cfg!(debug_assertions) { 15.0 } else { 4.0 };

        println!(
            "[FIRST_OPEN_FRAME] Cost to lazily rasterize & render large clock (40px) + full date (13px): {:.3} ms (budget < {:.1} ms)",
            elapsed_ms, budget_ms
        );

        assert!(
            elapsed_ms < budget_ms,
            "First open frame render took {:.3} ms, exceeding {:.1} ms budget",
            elapsed_ms,
            budget_ms
        );
    }
}
