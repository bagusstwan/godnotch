//! System tray (notification area) integration for godnotch.
//!
//! Responsible for runtime icon generation, `Shell_NotifyIconW` registration (NIM_ADD,
//! NIM_MODIFY, NIM_DELETE), taskbar restart recovery (`TaskbarCreated`), and context menu
//! ("Mulai bersama Windows", separator, "Keluar").
//!
//! Follows SAD.md section 3 & 5 and AGENTS.md: all Win32 API calls are contained here
//! with explicit `// SAFETY:` justifications.

use std::ffi::c_void;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HDC,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    GetSystemMetrics, HICON, ICONINFO, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    PostMessageW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
};
use windows::core::{PCWSTR, w};

use crate::core::i18n::{self, Language};

/// Application-defined callback message for tray notification events.
pub const WM_APP_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// Unique identifier for godnotch's notification icon in the system tray.
pub const TRAY_ICON_UID: u32 = 101;

/// Menu item identifier for toggling "Mulai bersama Windows" (Start with Windows).
pub const IDM_AUTOSTART: usize = 1001;

/// Menu item identifier for toggling "Sembunyikan saat layar penuh" (Hide on fullscreen).
pub const IDM_HIDE_ON_FULLSCREEN: usize = 1002;

/// Menu item identifier for "Buka berkas pengaturan" (Open settings file).
pub const IDM_OPEN_SETTINGS_FILE: usize = 1003;

/// Menu item identifier for "Muat ulang pengaturan" (Reload settings).
pub const IDM_RELOAD_SETTINGS: usize = 1004;

/// Menu item identifier for "Keluar" (Exit).
pub const IDM_EXIT: usize = 1005;

/// Action triggered by the user from the tray context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayMenuCommand {
    /// Toggle whether the application runs automatically on user login.
    ToggleAutostart,
    /// Toggle whether the notch hides when a fullscreen window is active.
    ToggleHideOnFullscreen,
    /// Open settings.json in the default text editor.
    OpenSettingsFile,
    /// Reload settings.json from disk and apply changes live.
    ReloadSettings,
    /// Terminate the application cleanly.
    Exit,
}

/// Generates a software-rasterized notch icon Pixmap at the specified pixel `size`.
///
/// Design: A clean, sleek capsule-shaped notch positioned against the top edge,
/// with a solid dark charcoal body, a subtle light contrast stroke (guaranteeing
/// readability across both dark and light Windows taskbars), and a subtle center accent dot.
#[must_use]
pub fn generate_tray_icon_pixmap(size: u32) -> Pixmap {
    let size = size.max(16);
    let mut pixmap = Pixmap::new(size, size).expect("valid tray icon pixmap dimensions");

    let s = size as f32;
    let w = (s * 0.75).round();
    let h = (s * 0.45).round().max(4.0);
    let x = ((s - w) / 2.0).round();
    let y = 1.0_f32;
    let r = (h * 0.45).min(w * 0.45);

    // Build the notch capsule path: flat top edge, rounded bottom corners
    let mut pb = PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.close();

    if let Some(path) = pb.finish() {
        // 1. Fill capsule with solid dark charcoal / black
        let mut fill_paint = Paint::default();
        fill_paint.set_color(Color::from_rgba8(20, 20, 22, 255));
        fill_paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &fill_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );

        // 2. Stroke outline with subtle light border for high contrast on dark taskbars
        let mut stroke_paint = Paint::default();
        stroke_paint.set_color(Color::from_rgba8(220, 220, 225, 200));
        stroke_paint.anti_alias = true;
        let stroke = Stroke {
            width: 1.0,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &stroke_paint, &stroke, Transform::identity(), None);

        // 3. Center accent dot (camera/sensor indicator)
        let dot_r = if size >= 24 { 1.2_f32 } else { 0.8_f32 };
        let dot_cx = (s / 2.0).round();
        let dot_cy = y + (h * 0.45).round();
        let mut dot_pb = PathBuilder::new();
        dot_pb.push_circle(dot_cx, dot_cy, dot_r);
        if let Some(dot_path) = dot_pb.finish() {
            let mut dot_paint = Paint::default();
            dot_paint.set_color(Color::from_rgba8(240, 240, 245, 230));
            dot_paint.anti_alias = true;
            pixmap.fill_path(
                &dot_path,
                &dot_paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    pixmap
}

/// Converts a 32-bpp premultiplied RGBA `tiny_skia::Pixmap` into a Win32 `HICON` handle.
///
/// Cleans up intermediate GDI bitmaps (`hbmColor` and `hbmMask`) immediately after
/// icon creation, guaranteeing zero GDI handle leaks. The returned `HICON` must be
/// freed with `DestroyIcon` upon termination.
pub fn create_hicon_from_pixmap(pixmap: &Pixmap) -> Result<HICON, windows::core::Error> {
    let width = pixmap.width();
    let height = pixmap.height();

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // Top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: Creating a 32-bpp DIB section for the icon's color bitmap.
    let hbm_color =
        unsafe { CreateDIBSection(HDC::default(), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)? };

    if bits.is_null() {
        // SAFETY: Deleting color bitmap handle on allocation failure.
        unsafe {
            let _ = DeleteObject(hbm_color);
        }
        return Err(windows::core::Error::from_win32());
    }

    // Convert RGBA premultiplied (tiny-skia) to BGRA premultiplied (Win32 DIB)
    // SAFETY: DIB section memory buffer is guaranteed to have width * height * 4 bytes.
    let dest_slice =
        unsafe { std::slice::from_raw_parts_mut(bits as *mut u8, (width * height * 4) as usize) };
    let (src_chunks, _) = pixmap.data().as_chunks::<4>();
    let (dest_chunks, _) = dest_slice.as_chunks_mut::<4>();
    for (src_chunk, dest_chunk) in src_chunks.iter().zip(dest_chunks.iter_mut()) {
        dest_chunk[0] = src_chunk[2]; // B
        dest_chunk[1] = src_chunk[1]; // G
        dest_chunk[2] = src_chunk[0]; // R
        dest_chunk[3] = src_chunk[3]; // A
    }

    // Create 1-bpp monochrome mask bitmap with all 0s (alpha channel in hbmColor handles transparency)
    let mask_stride = width.div_ceil(16) * 2;
    let mask_bytes = vec![0u8; (mask_stride * height) as usize];
    // SAFETY: Creating 1-bpp monochrome mask bitmap filled with zeros.
    let hbm_mask = unsafe {
        CreateBitmap(
            width as i32,
            height as i32,
            1,
            1,
            Some(mask_bytes.as_ptr() as *const c_void),
        )
    };

    let icon_info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: hbm_mask,
        hbmColor: hbm_color,
    };

    // SAFETY: Creating HICON indirect from the color and mask bitmaps.
    let hicon_res = unsafe { CreateIconIndirect(&icon_info) };

    // SAFETY: Temporary color and mask bitmaps are copied by CreateIconIndirect and must be deleted.
    unsafe {
        let _ = DeleteObject(hbm_color);
        let _ = DeleteObject(hbm_mask);
    }

    hicon_res
}

/// Manages system notification area (tray) registration and interaction.
pub struct TrayIcon {
    hwnd: HWND,
    hicon: HICON,
    taskbar_created_msg: u32,
    is_added: bool,
}

impl TrayIcon {
    /// Creates and initializes the system tray icon for the given `hwnd`.
    pub fn create(hwnd: HWND) -> Result<Self, windows::core::Error> {
        // SAFETY: Registering the system "TaskbarCreated" message for explorer restart recovery.
        let taskbar_created_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

        // Determine appropriate icon size matching the current system DPI
        // SAFETY: Querying SM_CXSMICON metric.
        let sys_metric_w = unsafe { GetSystemMetrics(SM_CXSMICON) };
        let icon_size = if sys_metric_w > 0 {
            sys_metric_w as u32
        } else {
            16
        };

        let pixmap = generate_tray_icon_pixmap(icon_size);
        let hicon = create_hicon_from_pixmap(&pixmap)?;

        let mut tray = Self {
            hwnd,
            hicon,
            taskbar_created_msg,
            is_added: false,
        };

        let _ = tray.add();
        Ok(tray)
    }

    /// Returns the registered `TaskbarCreated` window message identifier.
    #[must_use]
    pub fn taskbar_created_msg(&self) -> u32 {
        self.taskbar_created_msg
    }

    /// Adds or updates the icon in the system notification area.
    pub fn add(&mut self) -> Result<(), windows::core::Error> {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ICON_UID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.hicon,
            ..Default::default()
        };

        let tip: Vec<u16> = "godnotch".encode_utf16().collect();
        let copy_len = tip.len().min(nid.szTip.len() - 1);
        nid.szTip[..copy_len].copy_from_slice(&tip[..copy_len]);

        // SAFETY: Adding tray icon to notification area.
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() };
        if ok {
            self.is_added = true;
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            // SAFETY: Upgrading notify icon to NOTIFYICON_VERSION_4.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
            }
            Ok(())
        } else {
            Err(windows::core::Error::from_win32())
        }
    }

    /// Removes the icon from the system notification area.
    pub fn remove(&mut self) {
        if self.is_added {
            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ICON_UID,
                ..Default::default()
            };
            // SAFETY: Removing notification icon from the taskbar.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            self.is_added = false;
        }
    }

    /// Re-adds the icon when Windows Explorer restarts.
    pub fn handle_taskbar_created(&mut self) {
        let _ = self.add();
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
        if !self.hicon.is_invalid() {
            // SAFETY: Freeing the runtime-generated icon handle.
            unsafe {
                let _ = DestroyIcon(self.hicon);
            }
        }
    }
}

/// Displays the tray context popup menu and returns the selected command, if any.
///
/// Follows standard Win32 tray menu guidelines (KB139291):
/// calls `SetForegroundWindow` prior to `TrackPopupMenu` and `PostMessageW(hwnd, WM_NULL, ...)`
/// immediately afterwards so clicking outside properly dismisses the menu without leaving it hanging.
///
/// Does NOT hold any borrow on internal application state during the modal menu loop,
/// ensuring 100% re-entrancy safety.
pub fn show_context_menu(
    hwnd: HWND,
    pos: Option<POINT>,
    is_autostart_checked: bool,
    hide_on_fullscreen_checked: bool,
    lang: Language,
) -> Option<TrayMenuCommand> {
    let pt = match pos {
        Some(p) => p,
        None => {
            let mut cursor_pt = POINT::default();
            // SAFETY: Reading current mouse cursor position.
            unsafe {
                let _ = GetCursorPos(&mut cursor_pt);
            }
            cursor_pt
        }
    };

    // SAFETY: Creating popup menu.
    let menu = unsafe { CreatePopupMenu().ok()? };

    let autostart_label = i18n::tray_autostart_label(lang);
    let mut autostart_wide: Vec<u16> = autostart_label.encode_utf16().chain(Some(0)).collect();

    let hide_label = i18n::tray_hide_on_fullscreen_label(lang);
    let mut hide_wide: Vec<u16> = hide_label.encode_utf16().chain(Some(0)).collect();

    let open_settings_label = i18n::tray_open_settings_file_label(lang);
    let mut open_settings_wide: Vec<u16> =
        open_settings_label.encode_utf16().chain(Some(0)).collect();

    let reload_settings_label = i18n::tray_reload_settings_label(lang);
    let mut reload_settings_wide: Vec<u16> = reload_settings_label
        .encode_utf16()
        .chain(Some(0))
        .collect();

    let exit_label = i18n::tray_exit_label(lang);
    let mut exit_wide: Vec<u16> = exit_label.encode_utf16().chain(Some(0)).collect();

    let autostart_flags = MF_STRING
        | if is_autostart_checked {
            MF_CHECKED
        } else {
            MF_UNCHECKED
        };

    let hide_flags = MF_STRING
        | if hide_on_fullscreen_checked {
            MF_CHECKED
        } else {
            MF_UNCHECKED
        };

    // SAFETY: Appending menu items:
    // 1. [x] Mulai bersama Windows
    // 2. [x] Sembunyikan saat layar penuh
    // 3. Buka berkas pengaturan
    // 4. Muat ulang pengaturan
    // 5. Separator
    // 6. Keluar
    unsafe {
        let _ = AppendMenuW(
            menu,
            autostart_flags,
            IDM_AUTOSTART,
            PCWSTR::from_raw(autostart_wide.as_mut_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            hide_flags,
            IDM_HIDE_ON_FULLSCREEN,
            PCWSTR::from_raw(hide_wide.as_mut_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_OPEN_SETTINGS_FILE,
            PCWSTR::from_raw(open_settings_wide.as_mut_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_RELOAD_SETTINGS,
            PCWSTR::from_raw(reload_settings_wide.as_mut_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_EXIT,
            PCWSTR::from_raw(exit_wide.as_mut_ptr()),
        );
    }

    // SAFETY: Setting foreground window before TrackPopupMenu as required by KB139291.
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }

    // SAFETY: Running modal TrackPopupMenu tracking right button or return command.
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        )
    };

    // SAFETY: PostMessage WM_NULL to force task switch as required by KB139291.
    unsafe {
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }

    match cmd.0 as usize {
        IDM_AUTOSTART => Some(TrayMenuCommand::ToggleAutostart),
        IDM_HIDE_ON_FULLSCREEN => Some(TrayMenuCommand::ToggleHideOnFullscreen),
        IDM_OPEN_SETTINGS_FILE => Some(TrayMenuCommand::OpenSettingsFile),
        IDM_RELOAD_SETTINGS => Some(TrayMenuCommand::ReloadSettings),
        IDM_EXIT => Some(TrayMenuCommand::Exit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tray_icon_pixmap_dimensions_and_alpha() {
        for size in [16u32, 20, 24, 32] {
            let pixmap = generate_tray_icon_pixmap(size);
            assert_eq!(pixmap.width(), size);
            assert_eq!(pixmap.height(), size);

            let data = pixmap.data();
            assert_eq!(data.len(), (size * size * 4) as usize);

            let mut has_ink = false;
            let mut has_transparent = false;

            for chunk in data.as_chunks::<4>().0 {
                let r = chunk[0];
                let g = chunk[1];
                let b = chunk[2];
                let a = chunk[3];

                // Premultiplied alpha property: R, G, B <= A
                assert!(
                    r <= a,
                    "Premultiplied alpha violated at size {}: r ({}) > a ({})",
                    size,
                    r,
                    a
                );
                assert!(
                    g <= a,
                    "Premultiplied alpha violated at size {}: g ({}) > a ({})",
                    size,
                    g,
                    a
                );
                assert!(
                    b <= a,
                    "Premultiplied alpha violated at size {}: b ({}) > a ({})",
                    size,
                    b,
                    a
                );

                if a > 0 {
                    has_ink = true;
                } else {
                    has_transparent = true;
                }
            }

            assert!(
                has_ink,
                "Tray icon pixmap at size {} must have ink pixels",
                size
            );
            assert!(
                has_transparent,
                "Tray icon pixmap at size {} must have transparent background",
                size
            );
        }
    }

    #[test]
    fn test_tray_hicon_creation_and_destruction() {
        let pixmap = generate_tray_icon_pixmap(16);
        let hicon = create_hicon_from_pixmap(&pixmap).expect("hicon creation succeeds");
        assert!(!hicon.is_invalid(), "hicon must be valid handle");

        // SAFETY: Destroying test icon.
        let ok = unsafe { DestroyIcon(hicon).is_ok() };
        assert!(ok, "DestroyIcon must succeed");
    }

    #[test]
    fn test_tray_menu_command_resolution() {
        assert_eq!(
            match IDM_AUTOSTART {
                IDM_AUTOSTART => Some(TrayMenuCommand::ToggleAutostart),
                _ => None,
            },
            Some(TrayMenuCommand::ToggleAutostart)
        );

        assert_eq!(
            match IDM_EXIT {
                IDM_EXIT => Some(TrayMenuCommand::Exit),
                _ => None,
            },
            Some(TrayMenuCommand::Exit)
        );

        assert_eq!(
            match 9999 {
                IDM_AUTOSTART => Some(TrayMenuCommand::ToggleAutostart),
                IDM_EXIT => Some(TrayMenuCommand::Exit),
                _ => None,
            },
            None
        );
    }
}
