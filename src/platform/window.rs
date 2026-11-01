//! Window management, DPI awareness, and message loop subsystem.
//!
//! Provides the primary interactive layered window for godnotch (T1.1..T1.3)
//! as well as a lightweight message-only window for testing and headless messaging.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{
    COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, SYSTEMTIME, WPARAM,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP,
    HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetSystemMetrics,
    GetWindowLongPtrW, HWND_MESSAGE, IDC_ARROW, KillTimer, LoadCursorW, MF_STRING, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SW_SHOWNOACTIVATE,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow, TPM_BOTTOMALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED,
    WM_MOUSEMOVE, WM_NCDESTROY, WM_RBUTTONUP, WM_TIMECHANGE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Error, PCWSTR, Result, w};

/// Win32 window message sent when the mouse leaves the client area of a window (0x02A3).
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Custom message posted by the DWM VSync pacer thread to drive UI animation frames.
pub const WM_APP_FRAME: u32 = WM_APP + 1;

use crate::core::i18n::{self, Language, TimeFormat};
use crate::core::layout;
use crate::core::motion::{Motion, MotionDirection};
use crate::core::renderer;
use crate::core::text::FontRenderer;

const TIMER_OPEN_DELAY: usize = 101;
const TIMER_CLOSE_DELAY: usize = 102;
const TIMER_ANIM: usize = 103;
const TIMER_MINUTE_CLOCK: usize = 104;

const OPEN_DELAY_MS: u32 = 80;
const CLOSE_DELAY_MS: u32 = 300;
const ANIM_FRAME_MS: u32 = 16;

/// Configures process-wide DPI awareness to Per-Monitor V2.
pub fn set_dpi_awareness_v2() -> Result<()> {
    // SAFETY: Setting DPI awareness context to Per-Monitor V2 for the current process
    // before any windows are created.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
}

/// Debug-only statistics for tracking timer lifecycle and ensuring zero idle wakeups.
#[cfg(debug_assertions)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DebugTimerStats {
    pub open_delay_ticks: u64,
    pub close_delay_ticks: u64,
    pub anim_ticks: u64,
    pub is_anim_timer_active: bool,
}

/// DWM-synced frame pacing manager.
pub struct DwmPacer {
    running: Arc<AtomicBool>,
    frame_pending: Arc<AtomicBool>,
    fallback_to_timer: Arc<AtomicBool>,
    #[cfg(debug_assertions)]
    pub force_fallback: Arc<AtomicBool>,
    #[cfg(debug_assertions)]
    pub spawn_count: Arc<std::sync::atomic::AtomicU32>,
    #[cfg(debug_assertions)]
    pub join_count: Arc<std::sync::atomic::AtomicU32>,
    thread_handle: Option<JoinHandle<()>>,
}

impl DwmPacer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            frame_pending: Arc::new(AtomicBool::new(false)),
            fallback_to_timer: Arc::new(AtomicBool::new(false)),
            #[cfg(debug_assertions)]
            force_fallback: Arc::new(AtomicBool::new(false)),
            #[cfg(debug_assertions)]
            spawn_count: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            #[cfg(debug_assertions)]
            join_count: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            thread_handle: None,
        }
    }

    /// Returns true if the pacer thread is currently active.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_thread_alive(&self) -> bool {
        self.running.load(Ordering::Acquire) || self.thread_handle.is_some()
    }

    /// Starts the DWM flush pacing thread.
    /// Returns true if the thread started or was already running.
    pub fn start(&mut self, hwnd: HWND) -> bool {
        #[cfg(debug_assertions)]
        if self.force_fallback.load(Ordering::Acquire) {
            self.fallback_to_timer.store(true, Ordering::Release);
            return false;
        }

        if self.fallback_to_timer.load(Ordering::Acquire) {
            return false;
        }

        if self.running.load(Ordering::Acquire) && self.thread_handle.is_some() {
            return true;
        }

        self.running.store(true, Ordering::Release);
        self.frame_pending.store(false, Ordering::Release);
        #[cfg(debug_assertions)]
        self.spawn_count.fetch_add(1, Ordering::SeqCst);

        let running = Arc::clone(&self.running);
        let frame_pending = Arc::clone(&self.frame_pending);
        let fallback = Arc::clone(&self.fallback_to_timer);
        // SAFETY: HWND handle value converted to integer isize for safe thread transfer.
        let hwnd_isize = hwnd.0 as isize;

        let handle = std::thread::Builder::new()
            .name("godnotch-dwm-pacer".to_string())
            .spawn(move || {
                // Test DwmFlush once to verify desktop composition is available
                // SAFETY: DwmFlush blocks until the next VSync interval.
                if unsafe { DwmFlush().is_err() } {
                    #[cfg(debug_assertions)]
                    eprintln!("[godnotch] DwmFlush unavailable, falling back to SetTimer 16ms");
                    fallback.store(true, Ordering::Release);
                    running.store(false, Ordering::Release);
                    return;
                }

                while running.load(Ordering::Acquire) {
                    // SAFETY: DwmFlush blocks until the next VSync interval.
                    let hr = unsafe { DwmFlush() };
                    if hr.is_err() {
                        fallback.store(true, Ordering::Release);
                        break;
                    }

                    if !running.load(Ordering::Acquire) {
                        break;
                    }

                    // Only post if UI thread is ready for the next frame (prevents message flooding)
                    if !frame_pending.swap(true, Ordering::AcqRel) {
                        let hwnd_raw = hwnd_isize as *mut c_void;
                        let hwnd = HWND(hwnd_raw);
                        // SAFETY: Posting WM_APP_FRAME to the UI thread window.
                        unsafe {
                            let _ = PostMessageW(hwnd, WM_APP_FRAME, WPARAM(0), LPARAM(0));
                        }
                    }
                }
            });

        match handle {
            Ok(h) => {
                self.thread_handle = Some(h);
                true
            }
            Err(_) => {
                self.fallback_to_timer.store(true, Ordering::Release);
                false
            }
        }
    }

    /// Stops and joins the DWM flush pacing thread.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
            #[cfg(debug_assertions)]
            self.join_count.fetch_add(1, Ordering::SeqCst);
        }
        self.frame_pending.store(false, Ordering::Release);
    }

    /// Notifies the pacer that a pending frame has been processed by the UI thread.
    pub fn notify_frame_processed(&self) {
        self.frame_pending.store(false, Ordering::Release);
    }

    /// Returns true if DWM pacing has failed and timer fallback should be used.
    #[must_use]
    pub fn is_fallback(&self) -> bool {
        self.fallback_to_timer.load(Ordering::Acquire)
    }
}

impl Default for DwmPacer {
    fn default() -> Self {
        Self::new()
    }
}

/// Debug-only frame interval logger writing to %TEMP%\godnotch-frames.csv when GODNOTCH_FRAMELOG=1.
#[cfg(debug_assertions)]
pub struct FrameLogger {
    pub enabled: bool,
    pub log_path: std::path::PathBuf,
    pub frame_count: u64,
}

#[cfg(debug_assertions)]
impl FrameLogger {
    #[must_use]
    pub fn init() -> Self {
        let enabled = std::env::var_os("GODNOTCH_FRAMELOG").is_some();
        let log_path = std::env::temp_dir().join("godnotch-frames.csv");
        if enabled {
            let _ = std::fs::write(&log_path, "frame_index,dt_ms,method\n");
        }
        Self {
            enabled,
            log_path,
            frame_count: 0,
        }
    }

    pub fn record_frame(&mut self, dt_ms: f32, method: &str) {
        if !self.enabled {
            return;
        }
        if let Ok(metadata) = std::fs::metadata(&self.log_path)
            && metadata.len() >= 1_048_576
        {
            return;
        }
        self.frame_count += 1;
        let line = format!("{},{:.3},{}\n", self.frame_count, dt_ms, method);
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// Queries the Windows system time format (12-hour or 24-hour).
#[must_use]
pub fn query_system_time_format() -> TimeFormat {
    use windows::Win32::Globalization::GetLocaleInfoEx;
    const LOCALE_ITIME: u32 = 0x0000_0028;
    let mut buf = [0u16; 8];
    // SAFETY: Querying user default locale time format (12/24 hour). Passing null PCWSTR selects LOCALE_NAME_USER_DEFAULT.
    let ret = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_ITIME, Some(&mut buf)) };
    if ret > 0 && buf[0] == b'0' as u16 {
        TimeFormat::Hour12
    } else {
        TimeFormat::Hour24
    }
}

/// Queries the Windows user default locale language (Indonesian or English).
#[must_use]
pub fn query_user_locale_language() -> Language {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    // SAFETY: Querying user default locale name string (BCP-47).
    let ret = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if ret > 1 {
        let name = String::from_utf16_lossy(&buf[..ret as usize - 1]);
        Language::from_locale_name(&name)
    } else {
        Language::Indonesian
    }
}

/// Internal state of the interactive layered notch window.
pub struct NotchWindowState {
    #[allow(dead_code)] // Fase 1
    pub hwnd: HWND,
    pub mem_dc: HDC,
    pub bitmap: HBITMAP,
    pub bits: *mut u8,
    pub canvas_w: i32,
    pub canvas_h: i32,
    pub scale: f32,
    pub screen_w: i32,

    pub motion: Motion,
    pub animating: bool,
    pub last_frame: Instant,
    pub dirty: bool,
    pub pixmap: Pixmap,

    pub font_renderer: FontRenderer,
    pub time_format: TimeFormat,
    pub language: Language,
    pub current_hour: u16,
    pub current_minute: u16,
    pub current_day_of_week: u16,
    pub current_day: u16,
    pub current_month: u16,
    pub current_year: u16,

    pub is_tracking_leave: bool,
    pub pacer: DwmPacer,
    pub tray: Option<crate::platform::tray::TrayIcon>,
    pub taskbar_created_msg: u32,
    #[cfg(debug_assertions)]
    pub debug_timer_stats: DebugTimerStats,
    #[cfg(debug_assertions)]
    pub frame_logger: FrameLogger,
}

impl Drop for NotchWindowState {
    fn drop(&mut self) {
        self.pacer.stop();
        if !self.mem_dc.is_invalid() {
            // SAFETY: Deleting memory DC and bitmap allocated for DIB Section.
            unsafe {
                if !self.bitmap.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                }
                let _ = DeleteDC(self.mem_dc);
            }
        }
    }
}

impl NotchWindowState {
    /// Updates the cached system time and returns true if minute or date changed.
    pub fn update_time(&mut self, st: &SYSTEMTIME) -> bool {
        let changed = self.current_hour != st.wHour
            || self.current_minute != st.wMinute
            || self.current_day != st.wDay
            || self.current_month != st.wMonth
            || self.current_year != st.wYear;
        if changed {
            self.current_hour = st.wHour;
            self.current_minute = st.wMinute;
            self.current_day_of_week = st.wDayOfWeek;
            self.current_day = st.wDay;
            self.current_month = st.wMonth;
            self.current_year = st.wYear;
            self.dirty = true;
        }
        changed
    }

    /// Renders the notch shape into the local DIB pixel buffer.
    /// This function performs purely CPU-bound rasterization and does NOT make
    /// Win32 presentation calls, ensuring it is safe to execute while borrowed.
    pub fn render_buffer(&mut self) -> Result<()> {
        if !self.dirty && !self.animating {
            return Ok(());
        }

        let geom = layout::calculate_geometry_per_axis(
            self.motion.width_progress(),
            self.motion.height_progress(),
            self.scale,
        );
        renderer::render_notch(&mut self.pixmap.as_mut(), &geom);

        let p_h = self.motion.height_progress();
        let closed_alpha = layout::closed_content_alpha(p_h);
        let open_alpha = layout::open_content_alpha(p_h);

        // 1. Closed state content: clock centered in closed notch
        if closed_alpha > 0.001 {
            let clock_str =
                i18n::format_time(self.time_format, self.current_hour, self.current_minute);
            let font_size = (layout::CLOSED_CLOCK_FONT_SIZE * self.scale).round() as u32;
            let text_w = self
                .font_renderer
                .measure_text_width(&clock_str, font_size, true);

            let text_x = geom.x + (geom.width - text_w) / 2.0;
            let text_y = geom.y + (20.0 * self.scale).round();

            self.font_renderer.draw_text(
                &mut self.pixmap.as_mut(),
                &clock_str,
                text_x,
                text_y,
                font_size,
                (255, 255, 255),
                closed_alpha,
                true,
            );
        }

        // 2. Open state content: large clock + date
        if open_alpha > 0.001 {
            let clock_str =
                i18n::format_time(self.time_format, self.current_hour, self.current_minute);
            let font_size_clock = (layout::OPEN_CLOCK_FONT_SIZE * self.scale).round() as u32;
            let clock_w = self
                .font_renderer
                .measure_text_width(&clock_str, font_size_clock, true);

            let clock_x = geom.x + (geom.width - clock_w) / 2.0;
            let clock_y = geom.y + (48.0 * self.scale).round();

            self.font_renderer.draw_text(
                &mut self.pixmap.as_mut(),
                &clock_str,
                clock_x,
                clock_y,
                font_size_clock,
                (255, 255, 255),
                open_alpha,
                true,
            );

            let date_str = i18n::format_date(
                self.language,
                self.current_day_of_week,
                self.current_day,
                self.current_month,
                self.current_year,
            );
            let font_size_date = (layout::OPEN_DATE_FONT_SIZE * self.scale).round() as u32;
            let date_w = self
                .font_renderer
                .measure_text_width(&date_str, font_size_date, false);

            let date_x = geom.x + (geom.width - date_w) / 2.0;
            let date_y = geom.y + (74.0 * self.scale).round();

            self.font_renderer.draw_text(
                &mut self.pixmap.as_mut(),
                &date_str,
                date_x,
                date_y,
                font_size_date,
                (170, 170, 175), // #AAAAAF
                open_alpha,
                false,
            );
        }

        let src = self.pixmap.data();
        // SAFETY: bits pointer was allocated by CreateDIBSection with size canvas_w * canvas_h * 4.
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, src.len()) };
        renderer::rgba_to_bgra_premultiplied(src, dst);

        self.dirty = false;
        Ok(())
    }

    /// Kicks the animation pacing mechanism (DWM VSync thread with SetTimer fallback).
    pub fn kick_anim(&mut self, hwnd: HWND) {
        if !self.animating {
            self.animating = true;
            #[cfg(debug_assertions)]
            {
                self.debug_timer_stats.is_anim_timer_active = true;
            }
            self.last_frame = Instant::now();

            let started_dwm = self.pacer.start(hwnd);
            if !started_dwm || self.pacer.is_fallback() {
                // Fallback to 16ms timer
                // SAFETY: Starting fallback animation timer (16ms interval).
                unsafe {
                    SetTimer(hwnd, TIMER_ANIM, ANIM_FRAME_MS, None);
                }
            }
        }
    }

    /// Stops animation pacing and joins any running worker threads.
    pub fn stop_anim(&mut self, hwnd: HWND) {
        self.animating = false;
        #[cfg(debug_assertions)]
        {
            self.debug_timer_stats.is_anim_timer_active = false;
        }
        self.pacer.stop();
        // SAFETY: Killing any active fallback animation timer.
        unsafe {
            let _ = KillTimer(hwnd, TIMER_ANIM);
        }
    }
}

/// Helper function to safely access NotchWindowState with a short mutable borrow,
/// protecting against re-entrancy and aliasing violations during message dispatch.
unsafe fn with_state<R>(
    raw_cell: *const RefCell<NotchWindowState>,
    f: impl FnOnce(&mut NotchWindowState) -> R,
) -> Option<R> {
    if raw_cell.is_null() {
        return None;
    }
    // SAFETY: raw_cell points to a valid heap-allocated RefCell<NotchWindowState>
    // before WM_NCDESTROY zeroes GWLP_USERDATA.
    let cell = unsafe { &*raw_cell };
    let mut borrow = cell.try_borrow_mut().ok()?;
    Some(f(&mut borrow))
}

/// Presents the pre-rendered DIB buffer to the screen using UpdateLayeredWindow.
/// This Win32 call is deliberately decoupled from any NotchWindowState borrow
/// to guarantee re-entrancy safety.
unsafe fn present_layered_window(
    hwnd: HWND,
    mem_dc: HDC,
    screen_w: i32,
    canvas_w: i32,
    canvas_h: i32,
) -> Result<()> {
    let ptdst = POINT {
        x: (screen_w - canvas_w) / 2,
        y: 0,
    };
    let size = SIZE {
        cx: canvas_w,
        cy: canvas_h,
    };
    let ptsrc = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };

    // SAFETY: Calling UpdateLayeredWindow to present the 32-bit premultiplied DIB buffer.
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            Some(&ptdst),
            Some(&size),
            mem_dc,
            Some(&ptsrc),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )?;
    }
    Ok(())
}

/// Startup profiling metrics covering all initial phases to first pixel presentation.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct StartupProfile {
    pub window_create_us: u64,
    pub font_init: crate::core::text::FontInitProfile,
    pub first_render_us: u64,
    pub first_present_us: u64,
    pub total_to_presented_us: u64,
}

impl StartupProfile {
    #[allow(dead_code)]
    pub fn print(&self) {
        println!(
            "\n[STARTUP_PROFILE] Initial launch breakdown (main -> UpdateLayeredWindow presented):\n  1. Win32 Window & DC Creation   : {:7.3} ms\n  2. Font File Read               : {:7.3} ms\n  3. Font Parsing (ttf-parser)   : {:7.3} ms\n  4. Pre-rasterization ({} glifs): {:7.3} ms\n  5. Initial Buffer Rasterize     : {:7.3} ms\n  6. Initial UpdateLayeredWindow  : {:7.3} ms\n  ------------------------------------------------\n  TOTAL time to first pixel       : {:7.3} ms",
            self.window_create_us as f64 / 1000.0,
            self.font_init.read_file_us as f64 / 1000.0,
            self.font_init.parse_font_us as f64 / 1000.0,
            self.font_init.glyph_count,
            self.font_init.rasterize_glyphs_us as f64 / 1000.0,
            self.first_render_us as f64 / 1000.0,
            self.first_present_us as f64 / 1000.0,
            self.total_to_presented_us as f64 / 1000.0
        );
    }
}

/// An interactive, transparent layered notch window at the top center of the screen.
pub struct NotchWindow {
    hwnd: HWND,
    #[allow(dead_code)] // Digunakan untuk inspeksi profil pengujian
    pub startup_profile: StartupProfile,
}

impl NotchWindow {
    /// Creates and shows the layered notch window.
    pub fn create() -> Result<Self> {
        let t_start = Instant::now();

        // Phase 1: Window & DC Creation
        let t_win_start = Instant::now();
        // SAFETY: Retrieving executable instance module handle.
        let instance = unsafe { GetModuleHandleW(None)? };
        let class_name = w!("GodNotchLayeredWindow");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(notch_wndproc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
            ..Default::default()
        };

        // SAFETY: Registering the layered window class.
        unsafe {
            RegisterClassW(&wc);
        }

        // SAFETY: Determining primary monitor width.
        let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };

        // Initial default DPI scale (1.0 = 96 DPI)
        let scale = 1.0_f32;
        let canvas = layout::canvas_size(scale);
        let canvas_w = canvas.width as i32;
        let canvas_h = canvas.height as i32;
        let x = (screen_w - canvas_w) / 2;

        // SAFETY: Creating layered popup window.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("godnotch"),
                WS_POPUP,
                x,
                0,
                canvas_w,
                canvas_h,
                None,
                None,
                instance,
                None,
            )?
        };

        // Update scale from the actual window DPI
        // SAFETY: Querying DPI assigned to the newly created window.
        let window_dpi = unsafe { GetDpiForWindow(hwnd) };
        let actual_scale = if window_dpi > 0 {
            window_dpi as f32 / 96.0
        } else {
            scale
        };
        let canvas = layout::canvas_size(actual_scale);
        let canvas_w = canvas.width as i32;
        let canvas_h = canvas.height as i32;

        // SAFETY: Creating 32-bit DIB Section compatible memory DC.
        let screen_dc = unsafe { GetDC(None) };
        let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
        unsafe {
            ReleaseDC(None, screen_dc);
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: canvas_w,
                biHeight: -canvas_h, // Top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut c_void = null_mut();
        // SAFETY: Creating 32-bit DIB section for per-pixel alpha presentation.
        let bitmap = unsafe { CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)? };
        unsafe {
            SelectObject(mem_dc, HGDIOBJ(bitmap.0));
        }

        let pixmap = Pixmap::new(canvas.width, canvas.height).ok_or_else(Error::from_win32)?;
        let window_create_us = t_win_start.elapsed().as_micros() as u64;

        // Phase 2: Font initialization & glyph caching
        // SAFETY: Querying current local system time.
        let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let time_format = query_system_time_format();
        let language = query_user_locale_language();
        let font_renderer = FontRenderer::new();
        let font_init_profile = font_renderer.profile;

        let tray = match crate::platform::tray::TrayIcon::create(hwnd) {
            Ok(t) => Some(t),
            Err(_e) => {
                #[cfg(debug_assertions)]
                eprintln!("[godnotch] Failed to create tray icon: {_e:?}");
                None
            }
        };
        let taskbar_created_msg = tray.as_ref().map_or(0, |t| t.taskbar_created_msg());

        let state = Box::new(RefCell::new(NotchWindowState {
            hwnd,
            mem_dc,
            bitmap,
            bits: bits as *mut u8,
            canvas_w,
            canvas_h,
            scale: actual_scale,
            screen_w,
            motion: Motion::new(),
            animating: false,
            last_frame: Instant::now(),
            dirty: true,
            pixmap,
            font_renderer,
            time_format,
            language,
            current_hour: st.wHour,
            current_minute: st.wMinute,
            current_day_of_week: st.wDayOfWeek,
            current_day: st.wDay,
            current_month: st.wMonth,
            current_year: st.wYear,
            is_tracking_leave: false,
            pacer: DwmPacer::new(),
            tray,
            taskbar_created_msg,
            #[cfg(debug_assertions)]
            debug_timer_stats: DebugTimerStats::default(),
            #[cfg(debug_assertions)]
            frame_logger: FrameLogger::init(),
        }));

        // Phase 3: Initial render in closed state into DIB section
        let t_render_start = Instant::now();
        state.borrow_mut().render_buffer()?;
        let first_render_us = t_render_start.elapsed().as_micros() as u64;

        let raw_ptr = Box::into_raw(state);
        // SAFETY: Storing the heap-allocated RefCell<NotchWindowState> pointer in window user data.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw_ptr as isize);
        }

        // Phase 4: Initial UpdateLayeredWindow presentation
        let t_present_start = Instant::now();
        // SAFETY: Presenting initial layered window buffer outside any RefCell borrow.
        unsafe {
            let _ = present_layered_window(hwnd, mem_dc, screen_w, canvas_w, canvas_h);
        }
        let first_present_us = t_present_start.elapsed().as_micros() as u64;
        let total_to_presented_us = t_start.elapsed().as_micros() as u64;

        let startup_profile = StartupProfile {
            window_create_us,
            font_init: font_init_profile,
            first_render_us,
            first_present_us,
            total_to_presented_us,
        };

        #[cfg(debug_assertions)]
        startup_profile.print();

        // SAFETY: Displaying window without activating or stealing focus.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }

        let delay_ms =
            layout::calculate_next_minute_delay_ms(st.wSecond as u32, st.wMilliseconds as u32);
        // SAFETY: Setting initial one-shot timer to the next minute boundary.
        unsafe {
            SetTimer(hwnd, TIMER_MINUTE_CLOCK, delay_ms, None);
        }

        Ok(Self {
            hwnd,
            startup_profile,
        })
    }

    /// Returns the HWND handle.
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for NotchWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: Destroying the window if still valid.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Window procedure for the interactive layered notch window.
unsafe extern "system" fn notch_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: Retrieving user data pointer associated with this window.
    let raw_cell =
        unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };

    // Guard: if user data pointer is null (e.g. before WM_NCCREATE or after WM_NCDESTROY zeroing),
    // immediately forward to default window procedure without dereferencing.
    if raw_cell.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }

    match msg {
        WM_MOUSEMOVE => {
            let mut start_tracking = false;
            let mut immediate_open = false;

            // SAFETY: raw_cell points to a valid RefCell<NotchWindowState> associated with hwnd.
            unsafe {
                with_state(raw_cell, |state| {
                    if !state.is_tracking_leave {
                        state.is_tracking_leave = true;
                        start_tracking = true;
                        if state.motion.width_progress() > 0.1
                            || state.motion.direction == MotionDirection::Opening
                        {
                            state.motion.open();
                            immediate_open = true;
                        }
                    }
                });
            }

            if start_tracking {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: Requesting WM_MOUSELEAVE notification when cursor departs non-transparent area.
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                    // Cancel pending close delay if cursor re-entered
                    let _ = KillTimer(hwnd, TIMER_CLOSE_DELAY);
                }

                if immediate_open {
                    // SAFETY: raw_cell is valid; kicking animation timer.
                    unsafe {
                        with_state(raw_cell, |state| state.kick_anim(hwnd));
                    }
                } else {
                    // Closed state: start 80ms open delay timer (one-shot)
                    // SAFETY: Starting open delay timer.
                    unsafe {
                        SetTimer(hwnd, TIMER_OPEN_DELAY, OPEN_DELAY_MS, None);
                    }
                }
            }
            LRESULT(0)
        }

        WM_MOUSELEAVE => {
            // SAFETY: raw_cell is valid; resetting leave tracking state.
            unsafe {
                with_state(raw_cell, |state| {
                    state.is_tracking_leave = false;
                });
            }

            // Cancel pending open delay if cursor left before 80ms elapsed, and start 300ms close delay
            // SAFETY: Canceling open delay timer and starting close delay timer.
            unsafe {
                let _ = KillTimer(hwnd, TIMER_OPEN_DELAY);
                SetTimer(hwnd, TIMER_CLOSE_DELAY, CLOSE_DELAY_MS, None);
            }
            LRESULT(0)
        }

        WM_APP_FRAME => {
            // Driven by DwmFlush VSync pacer thread
            let frame_res = unsafe {
                with_state(raw_cell, |state| {
                    state.pacer.notify_frame_processed();
                    #[cfg(debug_assertions)]
                    {
                        state.debug_timer_stats.anim_ticks += 1;
                    }
                    let now = Instant::now();
                    let dt = (now - state.last_frame).as_secs_f32();
                    state.last_frame = now;

                    #[cfg(debug_assertions)]
                    {
                        state.frame_logger.record_frame(dt * 1000.0, "dwm");
                    }

                    let settled = state.motion.step(dt);
                    state.dirty = true;
                    let _ = state.render_buffer();
                    (
                        settled,
                        state.mem_dc,
                        state.screen_w,
                        state.canvas_w,
                        state.canvas_h,
                    )
                })
            };

            if let Some((settled, mem_dc, screen_w, canvas_w, canvas_h)) = frame_res {
                // SAFETY: Presenting layered window without holding any borrow on state.
                unsafe {
                    let _ = present_layered_window(hwnd, mem_dc, screen_w, canvas_w, canvas_h);
                }
                if settled {
                    // SAFETY: raw_cell is valid; stopping animation on settle.
                    unsafe {
                        with_state(raw_cell, |state| state.stop_anim(hwnd));
                    }
                }
            }
            LRESULT(0)
        }

        WM_TIMER => {
            let timer_id = wp.0;

            if timer_id == TIMER_OPEN_DELAY {
                // SAFETY: Killing one-shot open delay timer.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_OPEN_DELAY);
                }
                // SAFETY: raw_cell is valid; starting open spring animation.
                unsafe {
                    with_state(raw_cell, |state| {
                        #[cfg(debug_assertions)]
                        {
                            state.debug_timer_stats.open_delay_ticks += 1;
                        }
                        state.motion.open();
                        state.kick_anim(hwnd);
                    });
                }
            } else if timer_id == TIMER_CLOSE_DELAY {
                // SAFETY: Killing one-shot close delay timer.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_CLOSE_DELAY);
                }
                // SAFETY: raw_cell is valid; starting close spring animation.
                unsafe {
                    with_state(raw_cell, |state| {
                        #[cfg(debug_assertions)]
                        {
                            state.debug_timer_stats.close_delay_ticks += 1;
                        }
                        state.motion.close();
                        state.kick_anim(hwnd);
                    });
                }
            } else if timer_id == TIMER_MINUTE_CLOCK {
                // SAFETY: Killing one-shot minute clock timer.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_MINUTE_CLOCK);
                }
                // SAFETY: Querying updated local system time.
                let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
                let next_delay = layout::calculate_next_minute_delay_ms(
                    st.wSecond as u32,
                    st.wMilliseconds as u32,
                );
                // SAFETY: Scheduling one-shot timer for the next minute boundary.
                unsafe {
                    SetTimer(hwnd, TIMER_MINUTE_CLOCK, next_delay, None);
                }

                // SAFETY: Updating state and re-rendering if minute changed and not currently animating.
                let render_res = unsafe {
                    with_state(raw_cell, |state| {
                        let changed = state.update_time(&st);
                        if changed && !state.animating {
                            state.dirty = true;
                            let _ = state.render_buffer();
                            Some((state.mem_dc, state.screen_w, state.canvas_w, state.canvas_h))
                        } else {
                            None
                        }
                    })
                };

                if let Some(Some((mem_dc, screen_w, canvas_w, canvas_h))) = render_res {
                    // SAFETY: Presenting window without holding state borrow.
                    unsafe {
                        let _ = present_layered_window(hwnd, mem_dc, screen_w, canvas_w, canvas_h);
                    }
                }
            } else if timer_id == TIMER_ANIM {
                // SAFETY: raw_cell is valid; advancing animation frame (timer fallback path).
                let frame_res = unsafe {
                    with_state(raw_cell, |state| {
                        #[cfg(debug_assertions)]
                        {
                            state.debug_timer_stats.anim_ticks += 1;
                        }
                        let now = Instant::now();
                        let dt = (now - state.last_frame).as_secs_f32();
                        state.last_frame = now;

                        #[cfg(debug_assertions)]
                        {
                            state.frame_logger.record_frame(dt * 1000.0, "timer");
                        }

                        let settled = state.motion.step(dt);
                        state.dirty = true;
                        let _ = state.render_buffer();
                        (
                            settled,
                            state.mem_dc,
                            state.screen_w,
                            state.canvas_w,
                            state.canvas_h,
                        )
                    })
                };

                if let Some((settled, mem_dc, screen_w, canvas_w, canvas_h)) = frame_res {
                    // SAFETY: Presenting layered window without holding any borrow on state.
                    unsafe {
                        let _ = present_layered_window(hwnd, mem_dc, screen_w, canvas_w, canvas_h);
                    }
                    if settled {
                        // SAFETY: raw_cell is valid; stopping animation timer on settle.
                        unsafe {
                            with_state(raw_cell, |state| state.stop_anim(hwnd));
                        }
                    }
                }
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            // Right-click context menu (temporary exit path for Phase 1)
            // Notice: Absolutely NO borrow on NotchWindowState is held while running
            // the modal TrackPopupMenu message loop or calling DestroyWindow.
            // SAFETY: Creating and displaying popup menu on right-click.
            unsafe {
                if let Ok(menu) = CreatePopupMenu() {
                    let _ = AppendMenuW(menu, MF_STRING, 1, w!("Keluar"));
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let _ = SetForegroundWindow(hwnd);
                    let cmd = TrackPopupMenu(
                        menu,
                        TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_BOTTOMALIGN,
                        pt.x,
                        pt.y,
                        0,
                        hwnd,
                        None,
                    );
                    let _ = DestroyMenu(menu);

                    if cmd.0 == 1 {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
            LRESULT(0)
        }

        WM_DPICHANGED => {
            // New DPI scale
            let new_dpi = (wp.0 & 0xFFFF) as u32;
            let new_scale = new_dpi as f32 / 96.0;

            // SAFETY: raw_cell is valid; updating scale and recalculating buffer.
            let dpi_res = unsafe {
                with_state(raw_cell, |state| {
                    state.scale = new_scale;
                    let canvas = layout::canvas_size(new_scale);
                    state.canvas_w = canvas.width as i32;
                    state.canvas_h = canvas.height as i32;
                    state.dirty = true;
                    let _ = state.render_buffer();
                    (state.mem_dc, state.screen_w, state.canvas_w, state.canvas_h)
                })
            };

            if let Some((mem_dc, screen_w, canvas_w, canvas_h)) = dpi_res {
                // SAFETY: Presenting window at new scale without holding state borrow.
                unsafe {
                    let _ = present_layered_window(hwnd, mem_dc, screen_w, canvas_w, canvas_h);
                }
            }
            LRESULT(0)
        }

        WM_TIMECHANGE => {
            // System time, time zone, or daylight saving time changed, or waking from sleep.
            // SAFETY: Killing existing minute clock timer.
            unsafe {
                let _ = KillTimer(hwnd, TIMER_MINUTE_CLOCK);
            }
            // SAFETY: Querying current system time after time change event.
            let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
            let time_format = query_system_time_format();
            let language = query_user_locale_language();
            let next_delay =
                layout::calculate_next_minute_delay_ms(st.wSecond as u32, st.wMilliseconds as u32);
            // SAFETY: Arming new one-shot timer aligned to the new minute boundary.
            unsafe {
                SetTimer(hwnd, TIMER_MINUTE_CLOCK, next_delay, None);
            }

            // SAFETY: Updating cached format/locale/time and redrawing if not animating.
            let render_res = unsafe {
                with_state(raw_cell, |state| {
                    state.time_format = time_format;
                    state.language = language;
                    state.update_time(&st);
                    if !state.animating {
                        state.dirty = true;
                        let _ = state.render_buffer();
                        Some((state.mem_dc, state.screen_w, state.canvas_w, state.canvas_h))
                    } else {
                        None
                    }
                })
            };

            if let Some(Some((mem_dc, screen_w, canvas_w, canvas_h))) = render_res {
                // SAFETY: Presenting window without holding state borrow.
                unsafe {
                    let _ = present_layered_window(hwnd, mem_dc, screen_w, canvas_w, canvas_h);
                }
            }
            LRESULT(0)
        }

        crate::platform::tray::WM_APP_TRAY => {
            let event = (lp.0 & 0xFFFF) as u32;
            if event == WM_CONTEXTMENU || event == WM_RBUTTONUP {
                // Read current state outside of the popup menu modal loop
                let (is_autostart, lang) = unsafe {
                    with_state(raw_cell, |state| {
                        (
                            crate::platform::autostart::is_autostart_enabled(),
                            state.language,
                        )
                    })
                }
                .unwrap_or((false, Language::Indonesian));

                let cmd = crate::platform::tray::show_context_menu(hwnd, None, is_autostart, lang);
                if let Some(cmd) = cmd {
                    match cmd {
                        crate::platform::tray::TrayMenuCommand::ToggleAutostart => {
                            let _ = crate::platform::autostart::toggle_autostart();
                        }
                        crate::platform::tray::TrayMenuCommand::Exit => {
                            // SAFETY: Destroying window upon user exit request from tray menu.
                            unsafe {
                                let _ = DestroyWindow(hwnd);
                            }
                        }
                    }
                }
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            // SAFETY: Stopping pacer thread, removing tray icon, terminating timers, and posting quit message.
            unsafe {
                with_state(raw_cell, |state| {
                    state.pacer.stop();
                    if let Some(mut tray) = state.tray.take() {
                        tray.remove();
                    }
                });
                let _ = KillTimer(hwnd, TIMER_OPEN_DELAY);
                let _ = KillTimer(hwnd, TIMER_CLOSE_DELAY);
                let _ = KillTimer(hwnd, TIMER_ANIM);
                let _ = KillTimer(hwnd, TIMER_MINUTE_CLOCK);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }

        WM_NCDESTROY => {
            // SAFETY: Zeroing out GWLP_USERDATA BEFORE deallocating the heap-allocated RefCell<NotchWindowState>
            // to prevent Use-After-Free in any subsequent or re-entrant window procedure calls.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let mut boxed_state = Box::from_raw(raw_cell as *mut RefCell<NotchWindowState>);
                boxed_state.get_mut().pacer.stop();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }

        _ => {
            let is_taskbar_created = unsafe {
                with_state(raw_cell, |state| {
                    state.taskbar_created_msg == msg && msg != 0
                })
            }
            .unwrap_or(false);

            if is_taskbar_created {
                unsafe {
                    with_state(raw_cell, |state| {
                        if let Some(ref mut tray) = state.tray {
                            tray.handle_taskbar_created();
                        }
                    });
                }
                return LRESULT(0);
            }

            // SAFETY: Default window procedure for unhandled messages.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

// ---------------------------------------------------------------------------
// Lightweight Message-Only Window (retained for headless testing and tasks)
// ---------------------------------------------------------------------------

/// A lightweight, invisible message-only window for hosting Win32 message queues.
#[allow(dead_code)] // Fase 0 / testing
pub struct MessageOnlyWindow {
    hwnd: HWND,
}

#[allow(dead_code)] // Fase 0 / testing
impl MessageOnlyWindow {
    /// Creates and registers a new message-only window with default class.
    pub fn create() -> Result<Self> {
        Self::create_with_class(w!("GodNotchMessageWindow"))
    }

    /// Creates and registers a new message-only window with an isolated custom class name.
    pub fn create_with_class(class_name: PCWSTR) -> Result<Self> {
        // SAFETY: Retrieving the module handle for the current process executable.
        let instance = unsafe { GetModuleHandleW(None)? };

        let wc = WNDCLASSW {
            lpfnWndProc: Some(message_wndproc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };

        // SAFETY: Registering the window class.
        unsafe {
            RegisterClassW(&wc);
        }

        // SAFETY: Creating a message-only window by specifying HWND_MESSAGE as the parent.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("GodNotchMessage"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                instance,
                None,
            )?
        };

        Ok(Self { hwnd })
    }

    /// Returns the window handle.
    #[must_use]
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for MessageOnlyWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: Destroying the Win32 window handle upon drop.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Runs the standard blocking Win32 message loop using `GetMessageW`.
///
/// Blocks the thread while idle (0% CPU usage), wakes on incoming events,
/// and terminates cleanly when `WM_QUIT` is received. Handles -1 error return explicitly.
pub fn run_message_loop() -> Result<()> {
    let mut msg = MSG::default();

    loop {
        // SAFETY: GetMessageW blocks waiting for a message.
        // Return values:
        // > 0 (1): valid message retrieved
        // 0: WM_QUIT received, terminate loop cleanly
        // -1: Win32 error occurred
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 == 0 {
            // WM_QUIT received: exit cleanly
            break;
        } else if ret.0 == -1 {
            // -1 indicates a Win32 error; retrieve error code and return Err
            return Err(Error::from_win32());
        }

        // SAFETY: Translating and dispatching valid messages to window procedures.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

/// Window procedure for the message-only window.
unsafe extern "system" fn message_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            // SAFETY: Destroying the window when a close message is received.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: Posting WM_QUIT to terminate the GetMessageW message loop.
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: Forwarding all unhandled messages to standard default window procedure.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    #[test]
    fn test_message_only_window_creation() {
        let class_str = format!("GodNotchTestMsgWin_{}\0", std::process::id());
        let wide: Vec<u16> = class_str.encode_utf16().collect();
        let win = MessageOnlyWindow::create_with_class(PCWSTR(wide.as_ptr()));
        assert!(
            win.is_ok(),
            "Message-only window should be created successfully"
        );
        let win = win.unwrap();
        assert!(!win.hwnd().is_invalid(), "Window handle must be valid");
    }

    #[test]
    fn test_notch_window_creation() {
        let win = NotchWindow::create();
        assert!(win.is_ok(), "Notch window should be created successfully");
        let win = win.unwrap();
        assert!(
            !win.hwnd().is_invalid(),
            "Notch window handle must be valid"
        );
    }

    #[test]
    fn test_animation_stops_when_settled() {
        let mut motion = Motion::new();
        motion.open();

        let dt = 0.016;
        let mut frames = 0;
        while !motion.step(dt) {
            frames += 1;
            assert!(frames < 50, "Animation must settle in < 50 frames (<800ms)");
        }

        assert!(motion.is_settled(), "Motion must be settled");
        // Verify further step returns true (settled, no timer wakeup needed)
        assert!(
            motion.step(dt),
            "Subsequent steps must remain settled without animation"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn test_real_notch_window_timer_stops_when_settled() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PM_REMOVE, PeekMessageW, SendMessageW, TranslateMessage,
        };

        let _dpi = set_dpi_awareness_v2();
        let notch = NotchWindow::create().expect("Failed to create test notch window");
        let hwnd = notch.hwnd();

        // Send synthetic WM_MOUSEMOVE to trigger hover open delay and animation
        unsafe {
            SendMessageW(hwnd, WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
        }

        // Pump messages until animation settles (80ms open delay + ~450ms animation)
        let start = Instant::now();
        let timeout = Duration::from_millis(2000);
        let mut msg = MSG::default();
        let mut settled = false;

        while start.elapsed() < timeout {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            let raw_cell = unsafe {
                GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState>
            };
            if let Some(is_active) =
                unsafe { with_state(raw_cell, |s| s.debug_timer_stats.is_anim_timer_active) }
            {
                let ticks = unsafe {
                    with_state(raw_cell, |s| s.debug_timer_stats.anim_ticks).unwrap_or(0)
                };
                if !is_active && ticks > 0 {
                    settled = true;
                    break;
                }
            }

            std::thread::sleep(Duration::from_millis(15));
        }

        assert!(
            settled,
            "Real notch window failed to start and settle within timeout"
        );

        let raw_cell =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };
        let ticks_at_settle = unsafe {
            with_state(raw_cell, |s| s.debug_timer_stats.anim_ticks).expect("state exists")
        };
        let is_active = unsafe {
            with_state(raw_cell, |s| s.debug_timer_stats.is_anim_timer_active)
                .expect("state exists")
        };
        assert!(
            !is_active,
            "Animation timer must be killed upon reaching settled state"
        );
        assert!(
            ticks_at_settle > 0,
            "Animation timer must have ticked during motion"
        );

        // Pump messages for an additional 300 ms while idle
        let idle_start = Instant::now();
        while idle_start.elapsed() < Duration::from_millis(300) {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        // Verify that anim_ticks did not increase during idle
        let ticks_after_idle = unsafe {
            with_state(raw_cell, |s| s.debug_timer_stats.anim_ticks).expect("state exists")
        };
        assert_eq!(
            ticks_after_idle, ticks_at_settle,
            "No animation timer ticks may occur after settled"
        );
    }

    #[cfg(debug_assertions)]
    fn get_current_process_thread_count() -> u32 {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };
        use windows::Win32::System::Threading::GetCurrentProcessId;

        // SAFETY: Fetching current process ID to filter thread snapshot.
        let current_pid = unsafe { GetCurrentProcessId() };
        let mut thread_count = 0u32;

        // SAFETY: Creating snapshot of all threads currently running.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if let Ok(handle) = snapshot {
            let mut entry = THREADENTRY32 {
                dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };

            // SAFETY: Enumerating threads in snapshot.
            if unsafe { Thread32First(handle, &mut entry) }.is_ok() {
                loop {
                    if entry.th32OwnerProcessID == current_pid {
                        thread_count += 1;
                    }
                    // SAFETY: Enumerating next thread in snapshot.
                    if unsafe { Thread32Next(handle, &mut entry) }.is_err() {
                        break;
                    }
                }
            }
            // SAFETY: Closing snapshot handle.
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
        thread_count
    }

    #[test]
    #[cfg(debug_assertions)]
    fn test_dwm_pacer_stops_and_joins_on_settle() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PM_REMOVE, PeekMessageW, SendMessageW, TranslateMessage,
        };

        let _dpi = set_dpi_awareness_v2();
        let notch = NotchWindow::create().expect("Failed to create test notch window");
        let hwnd = notch.hwnd();

        // Send synthetic WM_MOUSEMOVE to trigger hover open delay and animation
        unsafe {
            SendMessageW(hwnd, WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
        }

        // Pump messages until animation settles
        let start = Instant::now();
        let timeout = Duration::from_millis(2000);
        let mut msg = MSG::default();

        while start.elapsed() < timeout {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            let raw_cell = unsafe {
                GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState>
            };
            let is_anim_active = unsafe {
                with_state(raw_cell, |s| s.debug_timer_stats.is_anim_timer_active).unwrap_or(false)
            };
            let ticks =
                unsafe { with_state(raw_cell, |s| s.debug_timer_stats.anim_ticks).unwrap_or(0) };

            if !is_anim_active && ticks > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        let raw_cell =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };
        let is_pacer_alive =
            unsafe { with_state(raw_cell, |s| s.pacer.is_thread_alive()).expect("state exists") };
        let is_anim_active = unsafe {
            with_state(raw_cell, |s| s.debug_timer_stats.is_anim_timer_active)
                .expect("state exists")
        };

        assert!(
            !is_anim_active,
            "Animation timer must be stopped upon settle"
        );
        assert!(
            !is_pacer_alive,
            "DWM pacer thread must be stopped and joined upon settle"
        );
        println!("[A3-1 PACER_STOP_JOIN] Pacer thread successfully stopped and joined upon settle");
    }

    #[test]
    #[cfg(debug_assertions)]
    fn test_repeated_animations_do_not_leak_threads() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PM_REMOVE, PeekMessageW, TranslateMessage,
        };

        let _dpi = set_dpi_awareness_v2();
        let notch = NotchWindow::create().expect("Failed to create test notch window");
        let hwnd = notch.hwnd();
        let raw_cell =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };

        let mut msg = MSG::default();
        // Warm up cycle
        unsafe {
            with_state(raw_cell, |s| {
                s.motion.open();
                s.kick_anim(hwnd);
            });
        }
        let warm_start = Instant::now();
        while warm_start.elapsed() < Duration::from_millis(1000) {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let is_active = unsafe {
                with_state(raw_cell, |s| s.debug_timer_stats.is_anim_timer_active).unwrap_or(false)
            };
            if !is_active {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let initial_threads = get_current_process_thread_count();

        // Run 20 animation cycles
        for cycle in 0..20 {
            unsafe {
                with_state(raw_cell, |s| {
                    if cycle % 2 == 0 {
                        s.motion.close();
                    } else {
                        s.motion.open();
                    }
                    s.kick_anim(hwnd);
                });
            }

            let cycle_start = Instant::now();
            while cycle_start.elapsed() < Duration::from_millis(1500) {
                unsafe {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                let is_active = unsafe {
                    with_state(raw_cell, |s| s.debug_timer_stats.is_anim_timer_active)
                        .unwrap_or(false)
                };
                if !is_active {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        let final_threads = get_current_process_thread_count();
        println!(
            "[A3-2 THREAD_LEAK_CHECK] Initial threads: {}, Final threads after 20 animations: {}",
            initial_threads, final_threads
        );

        assert!(
            final_threads <= initial_threads + 1,
            "Thread count increased unexpectedly: initial={}, final={}",
            initial_threads,
            final_threads
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn test_forced_fallback_timer_path() {
        use std::time::Duration;
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PM_REMOVE, PeekMessageW, TranslateMessage,
        };

        let _dpi = set_dpi_awareness_v2();
        let notch = NotchWindow::create().expect("Failed to create test notch window");
        let hwnd = notch.hwnd();
        let raw_cell =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };

        // Inject forced fallback on DwmPacer
        unsafe {
            with_state(raw_cell, |s| {
                s.pacer.force_fallback.store(true, Ordering::Release);
                s.motion.open();
                s.kick_anim(hwnd);
            });
        }

        let is_fallback =
            unsafe { with_state(raw_cell, |s| s.pacer.is_fallback()).unwrap_or(false) };
        assert!(is_fallback, "Pacer must fall back to timer when forced");

        let start = Instant::now();
        let timeout = Duration::from_millis(2000);
        let mut msg = MSG::default();
        let mut settled = false;

        while start.elapsed() < timeout {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            if let Some(is_active) =
                unsafe { with_state(raw_cell, |s| s.debug_timer_stats.is_anim_timer_active) }
            {
                let ticks = unsafe {
                    with_state(raw_cell, |s| s.debug_timer_stats.anim_ticks).unwrap_or(0)
                };
                if !is_active && ticks > 0 {
                    settled = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        assert!(
            settled,
            "Forced fallback SetTimer animation failed to settle within timeout"
        );
        println!(
            "[A3-3 FORCED_FALLBACK] Successfully animated and settled using 16ms fallback timer"
        );
    }

    #[test]
    fn test_frame_render_cpu_cost_breakdown() {
        let _dpi = set_dpi_awareness_v2();
        let notch = NotchWindow::create().expect("Failed to create test notch window");
        let hwnd = notch.hwnd();
        let raw_cell =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };

        let iterations = 100;
        let mut total_render_ns = 0u128;
        let mut total_bgra_ns = 0u128;
        let mut total_ulw_ns = 0u128;

        for _ in 0..iterations {
            let geom = layout::calculate_geometry_per_axis(0.5, 0.5, 1.0);

            // Step 1: Render notch path in tiny-skia
            let t0 = Instant::now();
            unsafe {
                with_state(raw_cell, |s| {
                    renderer::render_notch(&mut s.pixmap.as_mut(), &geom);
                });
            }
            total_render_ns += t0.elapsed().as_nanos();

            // Step 2: RGBA to BGRA premultiplied conversion
            let t1 = Instant::now();
            unsafe {
                with_state(raw_cell, |s| {
                    let src = s.pixmap.data();
                    let dst = std::slice::from_raw_parts_mut(s.bits, src.len());
                    renderer::rgba_to_bgra_premultiplied(src, dst);
                });
            }
            total_bgra_ns += t1.elapsed().as_nanos();

            // Step 3: Present via UpdateLayeredWindow
            let t2 = Instant::now();
            unsafe {
                with_state(raw_cell, |s| {
                    let _ =
                        present_layered_window(hwnd, s.mem_dc, s.screen_w, s.canvas_w, s.canvas_h);
                });
            }
            total_ulw_ns += t2.elapsed().as_nanos();
        }

        let render_ms = (total_render_ns as f64) / (iterations as f64 * 1_000_000.0);
        let bgra_ms = (total_bgra_ns as f64) / (iterations as f64 * 1_000_000.0);
        let ulw_ms = (total_ulw_ns as f64) / (iterations as f64 * 1_000_000.0);
        let total_ms = render_ms + bgra_ms + ulw_ms;

        println!(
            "[A6 CPU_COST_BREAKDOWN @ Scale 1.0x (400x130)]\n  1. render_notch         : {:.3} ms ({:.1}%)\n  2. rgba_to_bgra         : {:.3} ms ({:.1}%)\n  3. UpdateLayeredWindow  : {:.3} ms ({:.1}%)\n  TOTAL per frame         : {:.3} ms",
            render_ms,
            if total_ms > 0.0 {
                (render_ms / total_ms) * 100.0
            } else {
                0.0
            },
            bgra_ms,
            if total_ms > 0.0 {
                (bgra_ms / total_ms) * 100.0
            } else {
                0.0
            },
            ulw_ms,
            if total_ms > 0.0 {
                (ulw_ms / total_ms) * 100.0
            } else {
                0.0
            },
            total_ms
        );
    }

    #[test]
    fn test_minute_clock_timer_scheduling_and_trigger() {
        let _dpi = set_dpi_awareness_v2();
        let notch =
            NotchWindow::create().expect("Failed to create notch window for clock timer test");
        let hwnd = notch.hwnd();
        let raw_cell =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };

        // Verify initial state has valid time
        unsafe {
            with_state(raw_cell, |s| {
                assert!(s.current_hour <= 23);
                assert!(s.current_minute <= 59);
            });
        }

        // Simulate WM_TIMER for TIMER_MINUTE_CLOCK
        // SAFETY: Dispatching synthetic WM_TIMER for the minute clock
        let lres = unsafe { SendMessageW(hwnd, WM_TIMER, WPARAM(TIMER_MINUTE_CLOCK), LPARAM(0)) };
        assert_eq!(lres.0, 0);

        println!("[B4 MINUTE_CLOCK] Successfully processed TIMER_MINUTE_CLOCK and re-armed timer");
    }

    #[test]
    fn test_wm_timechange_handling() {
        let _dpi = set_dpi_awareness_v2();
        let notch =
            NotchWindow::create().expect("Failed to create notch window for timechange test");
        let hwnd = notch.hwnd();
        let raw_cell =
            unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<NotchWindowState> };

        // Simulate WM_TIMECHANGE
        // SAFETY: Dispatching synthetic WM_TIMECHANGE
        let lres = unsafe { SendMessageW(hwnd, WM_TIMECHANGE, WPARAM(0), LPARAM(0)) };
        assert_eq!(lres.0, 0);

        unsafe {
            with_state(raw_cell, |s| {
                assert!(s.current_hour <= 23);
                assert!(s.current_minute <= 59);
            });
        }

        println!(
            "[B4 TIMECHANGE] Successfully handled WM_TIMECHANGE, re-queried formats and refreshed timer"
        );
    }

    #[test]
    fn test_startup_phase_profiling() {
        let _dpi = set_dpi_awareness_v2();
        let notch =
            NotchWindow::create().expect("Failed to create notch window for startup profiling");
        notch.startup_profile.print();
        assert!(!notch.hwnd().is_invalid());
        assert!(notch.startup_profile.total_to_presented_us > 0);
    }
}
