//! Pure logic for fullscreen window detection.
//!
//! Fully decoupled from Win32 APIs so that detection rules, geometry checks,
//! and signal combination can be comprehensively tested on any host platform.

/// Tolerance in pixels for window rect bounds covering the monitor.
///
/// Certain games, video players, and borderless fullscreen windows align
/// within 1-2 pixels of the monitor boundaries due to DPI scaling rounding
/// or multi-monitor edge clipping.
#[allow(dead_code)]
pub const FULLSCREEN_TOLERANCE_PX: i32 = 2;

/// Standard Win32 `WS_CAPTION` style mask (`WS_BORDER | WS_DLGFRAME`).
#[allow(dead_code)]
pub const WS_CAPTION_MASK: u32 = 0x00C0_0000;

/// Pure representation of an integer 2D rectangle (compatible with Win32 `RECT`).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[allow(dead_code)]
impl Rect {
    /// Constructs a new rectangle.
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Returns the width in pixels.
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    /// Returns the height in pixels.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// Returns true if the rectangle has non-positive area.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// Returns true if the rectangle contains the specified point (inclusive of left/top, exclusive of right/bottom).
    #[must_use]
    pub const fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// Calculates the intersection area with another rectangle.
    #[must_use]
    pub fn intersection_area(&self, other: &Self) -> i64 {
        let inter_left = self.left.max(other.left);
        let inter_top = self.top.max(other.top);
        let inter_right = self.right.min(other.right);
        let inter_bottom = self.bottom.min(other.bottom);

        let w = (inter_right - inter_left).max(0) as i64;
        let h = (inter_bottom - inter_top).max(0) as i64;
        w * h
    }
}

/// Classification of window class names for filtering shell and internal windows.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowClassKind {
    /// A normal application window (candidate for fullscreen detection).
    Normal,
    /// Windows desktop or taskbar shell window (e.g. `Progman`, `WorkerW`, `Shell_TrayWnd`).
    Shell,
    /// Godnotch's own window.
    Godnotch,
}

/// Classifies a window class name.
///
/// Shell windows and Godnotch windows must NEVER trigger notch hiding.
#[allow(dead_code)]
#[must_use]
pub fn classify_window_class(class_name: &str) -> WindowClassKind {
    let lower = class_name.trim();

    // Fast case-insensitive prefix / equality check
    if lower.eq_ignore_ascii_case("progman")
        || lower.eq_ignore_ascii_case("workerw")
        || lower.eq_ignore_ascii_case("shell_traywnd")
        || lower.eq_ignore_ascii_case("shell_secondarytraywnd")
    {
        WindowClassKind::Shell
    } else if lower.to_ascii_lowercase().starts_with("godnotch") {
        WindowClassKind::Godnotch
    } else {
        WindowClassKind::Normal
    }
}

/// Determines whether a Win32 window style has the caption / titlebar bit set.
#[allow(dead_code)]
#[must_use]
pub const fn style_has_caption(style: u32) -> bool {
    (style & WS_CAPTION_MASK) == WS_CAPTION_MASK
}

/// Determines whether a window rectangle is on a specific monitor.
///
/// Evaluates whether the window's midpoint lies within the monitor's rectangle,
/// or if the intersection covers the majority of the window.
#[allow(dead_code)]
#[must_use]
pub fn is_window_on_monitor(window_rect: Rect, monitor_rect: Rect) -> bool {
    if window_rect.is_empty() || monitor_rect.is_empty() {
        return false;
    }

    let mid_x = window_rect.left + window_rect.width() / 2;
    let mid_y = window_rect.top + window_rect.height() / 2;

    if monitor_rect.contains_point(mid_x, mid_y) {
        return true;
    }

    // Fallback: check if intersection area is > 0
    window_rect.intersection_area(&monitor_rect) > 0
}

/// Checks if a window is running in fullscreen covering the monitor where the notch lives.
///
/// # Rules
/// 1. Window must be a `Normal` application window (not desktop, not taskbar, not Godnotch).
/// 2. Window must NOT have a titlebar/caption (`has_caption == false`). Normal maximized
///    windows have a caption bar and only fill the work area, so they return `false`.
/// 3. Window rect must cover the entire monitor rect within `FULLSCREEN_TOLERANCE_PX`.
/// 4. Fullscreen windows on another monitor return `false`.
#[allow(dead_code)]
#[must_use]
pub fn is_fullscreen(
    window_rect: Rect,
    monitor_rect: Rect,
    has_caption: bool,
    class_kind: WindowClassKind,
) -> bool {
    // Rule 1: Exclude shell and godnotch windows
    if class_kind != WindowClassKind::Normal {
        return false;
    }

    // Rule 2: Normal maximized windows with a title bar are not fullscreen
    if has_caption {
        return false;
    }

    // Degenerate geometry checks
    if window_rect.is_empty() || monitor_rect.is_empty() {
        return false;
    }

    // Rule 3: Check if the window covers the entire monitor rect within tolerance.
    // Notice that if the window is on another monitor, its coordinates will not cover monitor_rect.
    let covers_left = window_rect.left <= monitor_rect.left + FULLSCREEN_TOLERANCE_PX;
    let covers_top = window_rect.top <= monitor_rect.top + FULLSCREEN_TOLERANCE_PX;
    let covers_right = window_rect.right >= monitor_rect.right - FULLSCREEN_TOLERANCE_PX;
    let covers_bottom = window_rect.bottom >= monitor_rect.bottom - FULLSCREEN_TOLERANCE_PX;

    covers_left && covers_top && covers_right && covers_bottom
}

/// Abstract representation of `SHQueryUserNotificationState` query results.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserNotificationState {
    /// Normal desktop state, notifications accepted.
    #[default]
    Normal,
    /// Direct3D fullscreen application or game is active (`QUNS_RUNNING_D3D_FULL_SCREEN`).
    RunningD3dFullScreen,
    /// Presentation mode is active, e.g. PowerPoint slideshow (`QUNS_PRESENTATION_MODE`).
    PresentationMode,
    /// User is busy (e.g. `QUNS_BUSY`). Not sufficient on its own to hide notch.
    Busy,
    /// Windows Quiet Hours / Focus Assist (`QUNS_QUIET_TIME`).
    QuietTime,
    /// Fullscreen Windows Store / UWP app (`QUNS_APP`).
    App,
}

/// Combines window geometry detection with Windows User Notification State.
///
/// # Rules
/// - If `is_rect_fullscreen` is true, the window geometry conclusively proves
///   a fullscreen window is covering the notch's monitor: hide notch (`true`).
/// - If `notification_state` is `RunningD3dFullScreen` or `PresentationMode`:
///   - If the active window is on the notch's monitor (`is_on_notch_monitor == true`), hide notch (`true`).
///   - If the active window is on a different monitor, do NOT hide notch (`false`).
/// - `Busy` (or `QuietTime`/`Normal`) alone is NEVER sufficient to hide the notch: return `false`.
#[allow(dead_code)]
#[must_use]
pub fn should_hide_notch(
    is_rect_fullscreen: bool,
    notification_state: UserNotificationState,
    is_on_notch_monitor: bool,
) -> bool {
    if is_rect_fullscreen {
        return true;
    }

    match notification_state {
        UserNotificationState::RunningD3dFullScreen | UserNotificationState::PresentationMode => {
            is_on_notch_monitor
        }
        UserNotificationState::Normal
        | UserNotificationState::Busy
        | UserNotificationState::QuietTime
        | UserNotificationState::App => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY_MONITOR: Rect = Rect::new(0, 0, 1920, 1080);
    const SECONDARY_MONITOR_RIGHT: Rect = Rect::new(1920, 0, 3840, 1080);
    const SECONDARY_MONITOR_LEFT: Rect = Rect::new(-1920, 0, 0, 1080);
    const MONITOR_4K: Rect = Rect::new(0, 0, 3840, 2160);

    #[test]
    fn test_exact_fullscreen_primary() {
        let win = Rect::new(0, 0, 1920, 1080);
        assert!(is_fullscreen(
            win,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));
    }

    #[test]
    fn test_fullscreen_tolerance_bounds() {
        // 1 pixel slightly undersized (e.g. 1px off on bottom/right due to rounding)
        let win_under_1px = Rect::new(1, 1, 1919, 1079);
        assert!(is_fullscreen(
            win_under_1px,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));

        // 2 pixels undersized (maximum allowed tolerance)
        let win_under_2px = Rect::new(2, 2, 1918, 1078);
        assert!(is_fullscreen(
            win_under_2px,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));

        // 3 pixels undersized: fails tolerance check
        let win_under_3px = Rect::new(3, 2, 1918, 1078);
        assert!(!is_fullscreen(
            win_under_3px,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));

        // 2 pixels oversized (borderless extending slightly beyond monitor bounds)
        let win_over_2px = Rect::new(-2, -2, 1922, 1082);
        assert!(is_fullscreen(
            win_over_2px,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));
    }

    #[test]
    fn test_normal_maximized_window_has_caption() {
        // Maximized window covering work area (taskbar at bottom 40px)
        let win_work_area = Rect::new(0, 0, 1920, 1040);
        assert!(!is_fullscreen(
            win_work_area,
            PRIMARY_MONITOR,
            true, // has caption
            WindowClassKind::Normal
        ));

        // Maximized window covering entire monitor but STILL having caption
        let win_full_with_caption = Rect::new(0, 0, 1920, 1080);
        assert!(!is_fullscreen(
            win_full_with_caption,
            PRIMARY_MONITOR,
            true, // has caption -> not fullscreen
            WindowClassKind::Normal
        ));
    }

    #[test]
    fn test_borderless_smaller_than_monitor() {
        // Windowed borderless game running at 1280x720 centered
        let win_windowed = Rect::new(320, 180, 1600, 900);
        assert!(!is_fullscreen(
            win_windowed,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));
    }

    #[test]
    fn test_fullscreen_on_different_monitor() {
        // Fullscreen game on secondary right monitor (1920..3840)
        let win_sec = Rect::new(1920, 0, 3840, 1080);

        // When notch is on PRIMARY_MONITOR, it must NOT hide
        assert!(!is_fullscreen(
            win_sec,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));

        // When evaluated against the secondary monitor itself, it is fullscreen
        assert!(is_fullscreen(
            win_sec,
            SECONDARY_MONITOR_RIGHT,
            false,
            WindowClassKind::Normal
        ));
    }

    #[test]
    fn test_negative_coordinates_left_monitor() {
        // Secondary monitor positioned to the left (-1920..0)
        let win_left = Rect::new(-1920, 0, 0, 1080);

        // Does not hide notch on primary monitor
        assert!(!is_fullscreen(
            win_left,
            PRIMARY_MONITOR,
            false,
            WindowClassKind::Normal
        ));

        // Hides notch if notch is positioned on the left monitor
        assert!(is_fullscreen(
            win_left,
            SECONDARY_MONITOR_LEFT,
            false,
            WindowClassKind::Normal
        ));
    }

    #[test]
    fn test_high_dpi_4k_monitor() {
        let win_4k = Rect::new(0, 0, 3840, 2160);
        assert!(is_fullscreen(
            win_4k,
            MONITOR_4K,
            false,
            WindowClassKind::Normal
        ));
    }

    #[test]
    fn test_shell_and_desktop_classes_excluded() {
        let win = Rect::new(0, 0, 1920, 1080);

        // Desktop Progman/WorkerW
        assert!(!is_fullscreen(
            win,
            PRIMARY_MONITOR,
            false,
            classify_window_class("Progman")
        ));
        assert!(!is_fullscreen(
            win,
            PRIMARY_MONITOR,
            false,
            classify_window_class("WorkerW")
        ));

        // Taskbars
        assert!(!is_fullscreen(
            win,
            PRIMARY_MONITOR,
            false,
            classify_window_class("Shell_TrayWnd")
        ));
        assert!(!is_fullscreen(
            win,
            PRIMARY_MONITOR,
            false,
            classify_window_class("Shell_SecondaryTrayWnd")
        ));

        // Godnotch own window
        assert!(!is_fullscreen(
            win,
            PRIMARY_MONITOR,
            false,
            classify_window_class("GodNotchLayeredWindow")
        ));
        assert!(!is_fullscreen(
            win,
            PRIMARY_MONITOR,
            false,
            classify_window_class("GodNotchMessageWindow")
        ));
    }

    #[test]
    fn test_style_has_caption() {
        const WS_POPUP: u32 = 0x8000_0000;
        const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000; // includes WS_CAPTION

        assert!(!style_has_caption(WS_POPUP));
        assert!(style_has_caption(WS_OVERLAPPEDWINDOW));
        assert!(style_has_caption(WS_CAPTION_MASK));
    }

    #[test]
    fn test_is_window_on_monitor() {
        let win_on_primary = Rect::new(100, 100, 500, 400);
        assert!(is_window_on_monitor(win_on_primary, PRIMARY_MONITOR));
        assert!(!is_window_on_monitor(
            win_on_primary,
            SECONDARY_MONITOR_RIGHT
        ));

        let win_on_secondary = Rect::new(2000, 100, 2500, 400);
        assert!(!is_window_on_monitor(win_on_secondary, PRIMARY_MONITOR));
        assert!(is_window_on_monitor(
            win_on_secondary,
            SECONDARY_MONITOR_RIGHT
        ));
    }

    #[test]
    fn test_should_hide_notch_signal_combinations() {
        // 1. Rect fullscreen overrides everything
        assert!(should_hide_notch(true, UserNotificationState::Normal, true));
        assert!(should_hide_notch(true, UserNotificationState::Busy, false));

        // 2. D3D fullscreen on notch monitor -> hides
        assert!(should_hide_notch(
            false,
            UserNotificationState::RunningD3dFullScreen,
            true
        ));
        // D3D fullscreen on DIFFERENT monitor -> DOES NOT hide
        assert!(!should_hide_notch(
            false,
            UserNotificationState::RunningD3dFullScreen,
            false
        ));

        // 3. Presentation mode on notch monitor -> hides
        assert!(should_hide_notch(
            false,
            UserNotificationState::PresentationMode,
            true
        ));
        // Presentation mode on different monitor -> DOES NOT hide
        assert!(!should_hide_notch(
            false,
            UserNotificationState::PresentationMode,
            false
        ));

        // 4. Busy alone -> NEVER hides
        assert!(!should_hide_notch(false, UserNotificationState::Busy, true));
        assert!(!should_hide_notch(
            false,
            UserNotificationState::Busy,
            false
        ));

        // 5. Normal desktop -> DOES NOT hide
        assert!(!should_hide_notch(
            false,
            UserNotificationState::Normal,
            true
        ));
    }
}
