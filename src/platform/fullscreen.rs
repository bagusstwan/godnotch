//! Platform integration for fullscreen window and application detection.
//!
//! According to SAD.md and AGENTS.md, this module encapsulates all Win32
//! hook and window inspection calls. It adheres strictly to the privacy
//! guarantee: only window rect, style (caption bit), and class name (for shell
//! exclusion) are inspected; window titles, document contents, and process paths
//! are NEVER accessed or logged.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Shell::{
    QUNS_APP, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
    SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, GWL_STYLE, GetClassNameW, GetForegroundWindow, GetWindowLongW,
    GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, OBJID_WINDOW, PostMessageW,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
};

use crate::core::fullscreen::{
    Rect, UserNotificationState, WindowClassKind, classify_window_class, is_fullscreen,
    is_window_on_monitor, should_hide_notch, style_has_caption,
};

/// Custom window message sent to `NotchWindow` to trigger fullscreen evaluation.
pub const WM_APP_FULLSCREEN_CHECK: u32 = WM_APP + 3;

/// Timer identifier for debouncing rapid window transition events (120 ms one-shot).
pub const TIMER_FULLSCREEN_DEBOUNCE: usize = 105;

/// Target notch window handle for WinEvent callback delivery.
static TARGET_NOTCH_HWND: AtomicIsize = AtomicIsize::new(0);

/// Current foreground window handle tracked by the location hook.
static FOREGROUND_HWND: AtomicIsize = AtomicIsize::new(0);

/// Flag indicating that a fullscreen evaluation message is currently pending in the message queue.
static EVALUATION_PENDING: AtomicBool = AtomicBool::new(false);

/// Debug counter for WinEvent callback invocations.
static CALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns the number of WinEvent callback invocations since the last reset.
#[allow(dead_code)]
#[must_use]
pub fn get_callback_count() -> u64 {
    CALLBACK_COUNTER.load(Ordering::Relaxed)
}

/// Resets the callback counter to zero.
#[allow(dead_code)]
pub fn reset_callback_count() {
    CALLBACK_COUNTER.store(0, Ordering::Relaxed);
}

/// WinEvent callback function invoked by Windows for registered events.
///
/// # Safety
/// This is an `extern "system"` callback called by Windows. It must NOT access
/// `RefCell<NotchWindowState>`, must NOT allocate memory or block, and only
/// coalesces events into a single `WM_APP_FULLSCREEN_CHECK` message.
unsafe extern "system" fn winevent_callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    CALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Event filtering:
    // 1. For EVENT_OBJECT_LOCATIONCHANGE: filter STRICTLY to the foreground window itself.
    if event == EVENT_OBJECT_LOCATIONCHANGE {
        if id_object != OBJID_WINDOW.0 || hwnd.0.is_null() {
            return;
        }
        let current_fg = FOREGROUND_HWND.load(Ordering::Relaxed);
        if hwnd.0 as isize != current_fg {
            return;
        }
    } else if event != EVENT_SYSTEM_FOREGROUND
        && event != EVENT_SYSTEM_MINIMIZESTART
        && event != EVENT_SYSTEM_MINIMIZEEND
    {
        return;
    }

    let target = TARGET_NOTCH_HWND.load(Ordering::Relaxed);
    if target == 0 {
        return;
    }

    // Coalesce: only post if no check message is currently pending.
    if !EVALUATION_PENDING.swap(true, Ordering::AcqRel) {
        // SAFETY: Posting custom coalesced message to the notch window message loop.
        unsafe {
            let _ = PostMessageW(
                HWND(target as *mut _),
                WM_APP_FULLSCREEN_CHECK,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

/// Manages Windows event hooks (`SetWinEventHook`) for non-polling fullscreen detection.
pub struct FullscreenWatcher {
    target_hwnd: HWND,
    hook_global: HWINEVENTHOOK,
    hook_location: HWINEVENTHOOK,
    current_fg_thread_id: u32,
    current_fg_hwnd: HWND,
}

impl FullscreenWatcher {
    /// Starts watching foreground and window transition events for the target notch window.
    ///
    /// # Safety
    /// Must be called from the UI thread that owns `target_hwnd`.
    #[must_use]
    pub fn start(target_hwnd: HWND) -> Self {
        TARGET_NOTCH_HWND.store(target_hwnd.0 as isize, Ordering::Release);
        EVALUATION_PENDING.store(false, Ordering::Release);

        // 1. Register global hook for FOREGROUND and MINIMIZE events:
        // Range from EVENT_SYSTEM_FOREGROUND (0x0003) to EVENT_SYSTEM_MINIMIZEEND (0x0017).
        // SAFETY: Registering an out-of-context hook skipping our own process.
        let hook_global = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_MINIMIZEEND,
                None,
                Some(winevent_callback),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };

        let mut watcher = Self {
            target_hwnd,
            hook_global,
            hook_location: HWINEVENTHOOK::default(),
            current_fg_thread_id: 0,
            current_fg_hwnd: HWND::default(),
        };

        // 2. Set up initial location hook on the current foreground window thread.
        watcher.update_foreground_hook();

        watcher
    }

    /// Re-scopes the `EVENT_OBJECT_LOCATIONCHANGE` hook to the thread of the current foreground window.
    ///
    /// This prevents event flooding because the location hook only listens to events
    /// generated by the active foreground thread, and the callback further filters
    /// for `idObject == OBJID_WINDOW` and `hwnd == foreground_hwnd`.
    pub fn update_foreground_hook(&mut self) {
        // SAFETY: Querying foreground window handle.
        let fg = unsafe { GetForegroundWindow() };

        if fg == self.current_fg_hwnd {
            return;
        }

        self.current_fg_hwnd = fg;
        FOREGROUND_HWND.store(fg.0 as isize, Ordering::Release);

        let mut thread_id = 0u32;
        if !fg.0.is_null() {
            // SAFETY: Querying thread ID of foreground window.
            unsafe {
                thread_id = GetWindowThreadProcessId(fg, None);
            }
        }

        if thread_id == self.current_fg_thread_id && !self.hook_location.is_invalid() {
            return;
        }

        // Unhook existing location hook
        if !self.hook_location.is_invalid() {
            // SAFETY: Unregistering previous location hook.
            unsafe {
                let _ = UnhookWinEvent(self.hook_location);
            }
            self.hook_location = HWINEVENTHOOK::default();
        }

        self.current_fg_thread_id = thread_id;

        // Register new location hook strictly scoped to the foreground thread
        if thread_id != 0 {
            // SAFETY: Scoping location hook to the specific foreground thread ID.
            let hook = unsafe {
                SetWinEventHook(
                    EVENT_OBJECT_LOCATIONCHANGE,
                    EVENT_OBJECT_LOCATIONCHANGE,
                    None,
                    Some(winevent_callback),
                    0,
                    thread_id,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            self.hook_location = hook;
        }
    }

    /// Evaluates whether a fullscreen window is currently covering the notch's monitor.
    #[must_use]
    pub fn check_fullscreen(&mut self, notch_monitor: Rect) -> bool {
        self.update_foreground_hook();
        check_is_fullscreen_covering_monitor(notch_monitor, self.target_hwnd)
    }

    /// Stops all hooks and cleans up atomic references.
    pub fn stop(&mut self) {
        if !self.hook_location.is_invalid() {
            // SAFETY: Unregistering location hook.
            unsafe {
                let _ = UnhookWinEvent(self.hook_location);
            }
            self.hook_location = HWINEVENTHOOK::default();
        }

        if !self.hook_global.is_invalid() {
            // SAFETY: Unregistering global hook.
            unsafe {
                let _ = UnhookWinEvent(self.hook_global);
            }
            self.hook_global = HWINEVENTHOOK::default();
        }

        TARGET_NOTCH_HWND.store(0, Ordering::Release);
        FOREGROUND_HWND.store(0, Ordering::Release);
        EVALUATION_PENDING.store(false, Ordering::Release);
        self.current_fg_thread_id = 0;
        self.current_fg_hwnd = HWND::default();
    }
}

impl Drop for FullscreenWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Evaluates whether a foreground window is currently running in fullscreen covering `notch_monitor`.
///
/// # Privacy Guarantee
/// Only window rect geometry, window style (caption bit), and window class name
/// are read. Window titles, process executable names, command lines, and document
/// contents are never accessed or logged.
#[must_use]
pub fn check_is_fullscreen_covering_monitor(notch_monitor: Rect, notch_hwnd: HWND) -> bool {
    // 1. Get foreground window handle
    // SAFETY: Querying current foreground window.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || fg == notch_hwnd {
        return false;
    }

    // Validate that the window is still valid and visible (fail-safe: if closed, don't hide)
    // SAFETY: Checking validity and visibility of window handle.
    unsafe {
        if !IsWindow(fg).as_bool() || !IsWindowVisible(fg).as_bool() {
            return false;
        }
    }

    // 2. Classify class name (filter Desktop/Shell and Godnotch)
    let class_name = get_window_class_name(fg);
    let class_kind = classify_window_class(&class_name);
    if class_kind != WindowClassKind::Normal {
        return false;
    }

    // 3. Style caption check
    let style = get_window_style(fg);
    let has_caption = style_has_caption(style);

    // 4. Window rect check
    let win_rect = match get_window_rect(fg) {
        Some(r) => r,
        None => return false,
    };

    // 5. Evaluate pure geometry fullscreen
    let is_rect_fs = is_fullscreen(win_rect, notch_monitor, has_caption, class_kind);

    // 6. Query Windows user notification state
    let notif_state = query_user_notification_state();

    // 7. Check if window is on notch's monitor
    let is_on_monitor = is_window_on_monitor(win_rect, notch_monitor);

    // 8. Combine signals
    should_hide_notch(is_rect_fs, notif_state, is_on_monitor)
}

/// Queries the bounding rectangle of a window in screen coordinates.
#[must_use]
pub fn get_window_rect(hwnd: HWND) -> Option<Rect> {
    let mut rc = RECT::default();
    // SAFETY: Querying bounding rectangle of a Win32 window.
    let res = unsafe { GetWindowRect(hwnd, &mut rc) };
    if res.is_ok() {
        Some(Rect::new(rc.left, rc.top, rc.right, rc.bottom))
    } else {
        None
    }
}

/// Reads the `GWL_STYLE` window style flags.
#[must_use]
pub fn get_window_style(hwnd: HWND) -> u32 {
    // SAFETY: Reading window style bits via GetWindowLongW.
    unsafe { GetWindowLongW(hwnd, GWL_STYLE) as u32 }
}

/// Reads the registered class name of a window.
///
/// Limits read to 256 characters. Does not read window text/titles.
#[must_use]
pub fn get_window_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: Reading window class name into stack buffer.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        String::new()
    }
}

/// Queries the monitor rectangle for the monitor nearest to `hwnd`.
#[must_use]
pub fn get_monitor_rect_for_window(hwnd: HWND) -> Option<Rect> {
    // SAFETY: Locating nearest monitor and querying its geometry.
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if hmon.0.is_null() {
            return None;
        }
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            Some(Rect::new(
                mi.rcMonitor.left,
                mi.rcMonitor.top,
                mi.rcMonitor.right,
                mi.rcMonitor.bottom,
            ))
        } else {
            None
        }
    }
}

/// Queries Windows User Notification State via `SHQueryUserNotificationState`.
#[must_use]
pub fn query_user_notification_state() -> UserNotificationState {
    // SAFETY: Calling shell notification query API.
    let res = unsafe { SHQueryUserNotificationState() };
    let state = match res {
        Ok(s) => s,
        Err(_) => return UserNotificationState::Normal,
    };

    match state {
        QUNS_RUNNING_D3D_FULL_SCREEN => UserNotificationState::RunningD3dFullScreen,
        QUNS_PRESENTATION_MODE => UserNotificationState::PresentationMode,
        QUNS_BUSY => UserNotificationState::Busy,
        QUNS_QUIET_TIME => UserNotificationState::QuietTime,
        QUNS_APP => UserNotificationState::App,
        _ => UserNotificationState::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThreadId, GetProcessHandleCount,
    };

    #[test]
    fn test_hook_recreation_does_not_leak_handles() {
        unsafe {
            let mut handle_count_before = 0u32;
            let ok = GetProcessHandleCount(GetCurrentProcess(), &mut handle_count_before);
            assert!(ok.is_ok(), "GetProcessHandleCount should succeed");

            let current_thread = GetCurrentThreadId();
            let mut current_hook = HWINEVENTHOOK::default();

            // Perform 1000 re-registrations of the thread-scoped location hook
            for _ in 0..1000 {
                if !current_hook.is_invalid() {
                    let _ = UnhookWinEvent(current_hook);
                }
                current_hook = SetWinEventHook(
                    EVENT_OBJECT_LOCATIONCHANGE,
                    EVENT_OBJECT_LOCATIONCHANGE,
                    None,
                    Some(winevent_callback),
                    0,
                    current_thread,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                assert!(!current_hook.is_invalid(), "SetWinEventHook should succeed");
            }

            if !current_hook.is_invalid() {
                let _ = UnhookWinEvent(current_hook);
            }

            let mut handle_count_after = 0u32;
            let ok = GetProcessHandleCount(GetCurrentProcess(), &mut handle_count_after);
            assert!(ok.is_ok(), "GetProcessHandleCount should succeed");

            println!(
                "[HOOK_LEAK_TEST] Handles before: {}, after 1000 re-registrations: {}, delta: {}",
                handle_count_before,
                handle_count_after,
                handle_count_after as i32 - handle_count_before as i32
            );

            // Handle count must not leak. 1000 failed unhooks would leak ~1000 handles.
            // Allow headroom (< 100) for concurrent thread pools, DCs, and window objects created by parallel tests.
            let delta = handle_count_after as i32 - handle_count_before as i32;
            assert!(
                delta < 100,
                "Hook re-registration leaked handles! Before: {}, After: {}, Delta: {}",
                handle_count_before,
                handle_count_after,
                delta
            );
        }
    }

    #[test]
    fn test_notification_state_query_does_not_crash() {
        // Must succeed or return a valid state without crashing on Windows
        let state = query_user_notification_state();
        println!("[NOTIF_STATE] Current user notification state: {:?}", state);
    }

    #[test]
    fn test_fullscreen_watcher_lifecycle() {
        // Create watcher with a dummy HWND
        let dummy_hwnd = HWND(0x1234 as *mut _);
        let mut watcher = FullscreenWatcher::start(dummy_hwnd);

        // Update foreground hook several times
        watcher.update_foreground_hook();
        watcher.update_foreground_hook();

        // Stop cleanly
        watcher.stop();
    }
}
