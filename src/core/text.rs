//! Pure software text rendering and pre-rasterized glyph cache using fontdue.
//!
//! Loads a single system font (Segoe UI Semibold with fallbacks) to pre-rasterize
//! only the required glyphs (digits, ':', date/time letters in ID/EN) at the active
//! notch sizes, then immediately frees the font to strictly comply with the <= 1.5 MB
//! memory budget (ADR 008).

use fontdue::{Font, FontSettings};
use std::collections::HashMap;
use tiny_skia::PixmapMut;

/// Candidate system font paths checked in strict priority order.
pub const FONT_CANDIDATE_PATHS: [&str; 3] = [
    r"C:\Windows\Fonts\seguisb.ttf", // Segoe UI Semibold (Primary)
    r"C:\Windows\Fonts\segoeui.ttf", // Segoe UI Regular (Fallback 1)
    r"C:\Windows\Fonts\arial.ttf",   // Arial (Fallback 2)
];

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

/// Single-font text renderer with pre-rasterized glyph cache for minimal memory footprint.
#[allow(dead_code)] // Fase 1 T1.4
pub struct FontRenderer {
    cache: HashMap<(char, u32), CachedGlyph>,
    digit_advance_cache: HashMap<u32, f32>,
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

    /// Initializes the font renderer and pre-rasterizes glyphs for the specified pixel sizes.
    /// Immediately frees the Font structure to enforce the strict <= 1.5 MB memory budget.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn new_with_sizes(sizes: &[u32]) -> Self {
        let mut cache = HashMap::new();
        let mut digit_advance_cache = HashMap::new();
        let mut has_loaded_font = false;
        let mut profile = FontInitProfile::default();

        let mut font = None;
        for &path in &FONT_CANDIDATE_PATHS {
            let t_read_start = std::time::Instant::now();
            if let Ok(bytes) = std::fs::read(path) {
                profile.read_file_us += t_read_start.elapsed().as_micros() as u64;
                let t_parse_start = std::time::Instant::now();
                if let Ok(f) = Font::from_bytes(bytes, FontSettings::default()) {
                    profile.parse_font_us += t_parse_start.elapsed().as_micros() as u64;
                    font = Some(f);
                    break;
                }
            }
        }

        if let Some(font) = font {
            has_loaded_font = true;
            let chars = Self::required_chars();

            let t_raster_start = std::time::Instant::now();
            for &size in sizes {
                let mut max_digit_adv = 0.0_f32;
                for &ch in &chars {
                    let (metrics, bitmap) = font.rasterize(ch, size as f32);
                    let cached = CachedGlyph {
                        width: metrics.width,
                        height: metrics.height,
                        x_offset: metrics.xmin,
                        y_offset: metrics.ymin,
                        advance_width: metrics.advance_width,
                        bitmap,
                    };
                    if ch.is_ascii_digit() && cached.advance_width > max_digit_adv {
                        max_digit_adv = cached.advance_width;
                    }
                    cache.insert((ch, size), cached);
                }
                if max_digit_adv <= 0.0 {
                    max_digit_adv = (size as f32) * 0.6;
                }
                digit_advance_cache.insert(size, max_digit_adv);
            }
            profile.rasterize_glyphs_us = t_raster_start.elapsed().as_micros() as u64;
            profile.glyph_count = cache.len();
            // `font` is dropped here, reclaiming all parsed font tables and raw buffer memory!
        } else {
            #[cfg(debug_assertions)]
            eprintln!("[godnotch] No system font found; text rendering will be silent/disabled");
        }

        Self {
            cache,
            digit_advance_cache,
            has_loaded_font,
            profile,
        }
    }

    /// Initializes with default sizes across common DPI scales (13px..80px).
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn new() -> Self {
        Self::new_with_sizes(&[13, 16, 20, 26, 40, 50, 60, 80])
    }

    /// Returns true if a system font was found and loaded.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn has_font(&self) -> bool {
        self.has_loaded_font
    }

    /// Retrieves a pre-rasterized glyph from cache.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn get_glyph(&self, ch: char, px_size: u32) -> Option<&CachedGlyph> {
        self.cache.get(&(ch, px_size))
    }

    /// Returns the maximum advance width for digits '0'..='9' at `px_size`.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn max_digit_advance(&self, px_size: u32) -> f32 {
        if let Some(&adv) = self.digit_advance_cache.get(&px_size) {
            adv
        } else {
            (px_size as f32) * 0.6
        }
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
            let cell_width = if tabular_digits && ch.is_ascii_digit() {
                max_digit_adv
            } else if let Some(g) = self.get_glyph(ch, px_size) {
                g.advance_width
            } else {
                (px_size as f32) * 0.5
            };

            if let Some(glyph) = self.get_glyph(ch, px_size) {
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
                .values()
                .map(|g| g.bitmap.len() + std::mem::size_of::<CachedGlyph>())
                .sum();

            let glyph_mb = (total_glyph_bytes as f64) / (1024.0 * 1024.0);
            let ws_delta = ws_after.saturating_sub(ws_before);
            let ws_delta_mb = (ws_delta as f64) / (1024.0 * 1024.0);

            println!(
                "[B1 FONT_MEMORY] Pre-rasterized glyph cache entries: {}, total cache payload: {:.3} KB ({:.4} MB)",
                renderer.cache.len(),
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
}
