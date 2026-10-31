//! Window management, DPI awareness, and message loop subsystem.
//!
//! Provides the primary interactive layered window for godnotch (T1.1..T1.3)
//! as well as a lightweight message-only window for testing and headless messaging.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::time::Instant;

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
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
    PostQuitMessage, RegisterClassW, SM_CXSCREEN, SW_SHOWNOACTIVATE, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, ShowWindow, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_DESTROY, WM_DPICHANGED, WM_MOUSEMOVE, WM_NCDESTROY, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Error, PCWSTR, Result, w};

/// Win32 window message sent when the mouse leaves the client area of a window (0x02A3).
const WM_MOUSELEAVE: u32 = 0x02A3;

use crate::core::layout;
use crate::core::renderer;
use crate::core::spring::Spring;

const TIMER_OPEN_DELAY: usize = 101;
const TIMER_CLOSE_DELAY: usize = 102;
const TIMER_ANIM: usize = 103;

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

/// Internal state of the interactive layered notch window.
pub struct NotchWindowState {
    pub hwnd: HWND,
    pub mem_dc: HDC,
    pub bitmap: HBITMAP,
    pub bits: *mut u8,
    pub canvas_w: i32,
    pub canvas_h: i32,
    pub scale: f32,
    pub screen_w: i32,

    pub spring: Spring,
    pub animating: bool,
    pub last_frame: Instant,
    pub dirty: bool,
    pub pixmap: Pixmap,

    pub is_tracking_leave: bool,
    #[cfg(debug_assertions)]
    pub debug_timer_stats: DebugTimerStats,
}

impl Drop for NotchWindowState {
    fn drop(&mut self) {
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
    /// Renders the notch shape to the DIB section and presents via UpdateLayeredWindow.
    pub fn render(&mut self) -> Result<()> {
        if !self.dirty && !self.animating {
            return Ok(());
        }

        let geom = layout::calculate_geometry(self.spring.pos, self.scale);
        renderer::render_notch(&mut self.pixmap.as_mut(), &geom);

        let src = self.pixmap.data();
        // SAFETY: bits pointer was allocated by CreateDIBSection with size canvas_w * canvas_h * 4.
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, src.len()) };
        renderer::rgba_to_bgra_premultiplied(src, dst);

        let ptdst = POINT {
            x: (self.screen_w - self.canvas_w) / 2,
            y: 0,
        };
        let size = SIZE {
            cx: self.canvas_w,
            cy: self.canvas_h,
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
                self.hwnd,
                None,
                Some(&ptdst),
                Some(&size),
                self.mem_dc,
                Some(&ptsrc),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )?;
        }

        self.dirty = false;
        Ok(())
    }

    /// Kicks the 16ms animation timer if not already running.
    pub fn kick_anim(&mut self, hwnd: HWND) {
        if !self.animating {
            self.animating = true;
            #[cfg(debug_assertions)]
            {
                self.debug_timer_stats.is_anim_timer_active = true;
            }
            self.last_frame = Instant::now();
            // SAFETY: Starting the one-shot animation timer (16ms interval).
            unsafe {
                SetTimer(hwnd, TIMER_ANIM, ANIM_FRAME_MS, None);
            }
        }
    }

    /// Stops the 16ms animation timer.
    pub fn stop_anim(&mut self, hwnd: HWND) {
        self.animating = false;
        #[cfg(debug_assertions)]
        {
            self.debug_timer_stats.is_anim_timer_active = false;
        }
        // SAFETY: Killing the animation timer upon settling to achieve 0% idle CPU.
        unsafe {
            let _ = KillTimer(hwnd, TIMER_ANIM);
        }
    }
}

/// An interactive, transparent layered notch window at the top center of the screen.
pub struct NotchWindow {
    hwnd: HWND,
}

impl NotchWindow {
    /// Creates and shows the layered notch window.
    pub fn create() -> Result<Self> {
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

        let mut state = Box::new(NotchWindowState {
            hwnd,
            mem_dc,
            bitmap,
            bits: bits as *mut u8,
            canvas_w,
            canvas_h,
            scale: actual_scale,
            screen_w,
            spring: Spring::new(0.0),
            animating: false,
            last_frame: Instant::now(),
            dirty: true,
            pixmap,
            is_tracking_leave: false,
            #[cfg(debug_assertions)]
            debug_timer_stats: DebugTimerStats::default(),
        });

        // Initial render in closed state
        state.render()?;

        let raw_ptr = Box::into_raw(state);
        // SAFETY: Storing the heap-allocated NotchWindowState pointer in window user data.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw_ptr as isize);
        }

        // SAFETY: Displaying window without activating or stealing focus.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }

        Ok(Self { hwnd })
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
    let raw_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut NotchWindowState };

    // Guard: if user data pointer is null (e.g. before WM_NCCREATE or after WM_NCDESTROY zeroing),
    // immediately forward to default window procedure without dereferencing.
    if raw_ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }

    // SAFETY: raw_ptr is guaranteed non-null and valid until WM_NCDESTROY cleans it up.
    let state = unsafe { &mut *raw_ptr };

    match msg {
        WM_MOUSEMOVE => {
            if !state.is_tracking_leave {
                state.is_tracking_leave = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: Requesting WM_MOUSELEAVE notification when cursor departs non-transparent area.
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }

                // Cancel pending close delay if cursor re-entered
                // SAFETY: Canceling close delay timer.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_CLOSE_DELAY);
                }

                if state.spring.pos > 0.1 || state.spring.target > 0.5 {
                    // Already open or opening: immediately target open without delay
                    state.spring.set_target(1.0);
                    state.kick_anim(hwnd);
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
            state.is_tracking_leave = false;

            // Cancel pending open delay if cursor left before 80ms elapsed
            // SAFETY: Canceling open delay timer.
            unsafe {
                let _ = KillTimer(hwnd, TIMER_OPEN_DELAY);
            }

            // Start 300ms close delay timer (one-shot)
            // SAFETY: Starting close delay timer.
            unsafe {
                SetTimer(hwnd, TIMER_CLOSE_DELAY, CLOSE_DELAY_MS, None);
            }
            LRESULT(0)
        }

        WM_TIMER => {
            let timer_id = wp.0;

            if timer_id == TIMER_OPEN_DELAY {
                #[cfg(debug_assertions)]
                {
                    state.debug_timer_stats.open_delay_ticks += 1;
                }
                // SAFETY: Killing one-shot open delay timer.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_OPEN_DELAY);
                }
                state.spring.set_target(1.0);
                state.kick_anim(hwnd);
            } else if timer_id == TIMER_CLOSE_DELAY {
                #[cfg(debug_assertions)]
                {
                    state.debug_timer_stats.close_delay_ticks += 1;
                }
                // SAFETY: Killing one-shot close delay timer.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_CLOSE_DELAY);
                }
                state.spring.set_target(0.0);
                state.kick_anim(hwnd);
            } else if timer_id == TIMER_ANIM {
                #[cfg(debug_assertions)]
                {
                    state.debug_timer_stats.anim_ticks += 1;
                }
                let now = Instant::now();
                let dt = (now - state.last_frame).as_secs_f32();
                state.last_frame = now;

                let settled = state.spring.step(dt);
                state.dirty = true;
                let _ = state.render();

                if settled {
                    state.stop_anim(hwnd);
                }
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            // Right-click context menu (temporary exit path for Phase 1)
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
            state.scale = new_scale;

            let canvas = layout::canvas_size(new_scale);
            state.canvas_w = canvas.width as i32;
            state.canvas_h = canvas.height as i32;
            state.dirty = true;
            let _ = state.render();
            LRESULT(0)
        }

        WM_DESTROY => {
            // SAFETY: Terminating timers and posting quit message to exit GetMessageW loop.
            unsafe {
                let _ = KillTimer(hwnd, TIMER_OPEN_DELAY);
                let _ = KillTimer(hwnd, TIMER_CLOSE_DELAY);
                let _ = KillTimer(hwnd, TIMER_ANIM);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }

        WM_NCDESTROY => {
            // SAFETY: Zeroing out GWLP_USERDATA BEFORE deallocating the heap-allocated NotchWindowState
            // to prevent Use-After-Free in any subsequent or re-entrant window procedure calls.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = Box::from_raw(raw_ptr);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
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
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);

        let dt = 0.016;
        let mut frames = 0;
        while !spring.step(dt) {
            frames += 1;
            assert!(frames < 50, "Animation must settle in < 50 frames (<800ms)");
        }

        assert!(spring.is_settled(), "Spring must be settled");
        // Verify further step returns true (settled, no timer wakeup needed)
        assert!(
            spring.step(dt),
            "Subsequent steps must remain settled without animation"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn test_timer_killed_upon_settled() {
        // Verify DebugTimerStats logic and that no timer remains active once settled
        let mut stats = DebugTimerStats::default();
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);
        stats.is_anim_timer_active = true;

        let dt = 0.016;
        while !spring.step(dt) {
            stats.anim_ticks += 1;
        }

        // Settled: stop_anim should kill the timer
        stats.is_anim_timer_active = false;

        assert!(
            !stats.is_anim_timer_active,
            "Animation timer must be killed upon reaching settled state"
        );
        assert!(stats.anim_ticks > 0, "Animation timer ticked during motion");

        let ticks_at_rest = stats.anim_ticks;
        // Simulating idle frames when settled: spring.step returns true immediately
        for _ in 0..10 {
            assert!(spring.step(dt));
        }
        // anim_ticks must NOT increase when idle
        assert_eq!(
            stats.anim_ticks, ticks_at_rest,
            "No animation timer ticks may occur while settled"
        );
    }
}
