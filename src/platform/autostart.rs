//! Windows autostart configuration via HKCU Run registry key.
//!
//! Manages `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`
//! to toggle startup on user login without requiring administrative privileges,
//! scheduled tasks, or modifying system-wide HKLM keys.
//!
//! Follows SAD.md section 3 & 4 and AGENTS.md: all Win32 API calls are contained
//! here with explicit `// SAFETY:` comments. Pure logic (command formatting, path
//! comparison) is decoupled from registry access to enable isolated testing.

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};
use windows::core::PCWSTR;

/// Registry subkey path where per-user autostart entries are registered.
const RUN_SUBKEY_STR: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// Default value name used for godnotch in the Run registry key.
pub const RUN_KEY_VALUE_NAME: &str = "godnotch";

// ---------------------------------------------------------------------------
// Pure Path & Command Logic (Decoupled, Unit-Testable Without Registry)
// ---------------------------------------------------------------------------

/// Wraps an executable path in double quotes suitable for the Windows Run key.
///
/// If `exe_path` is already properly wrapped in double quotes, it is returned
/// unchanged (after trimming). Handles spaces, non-ASCII characters, and long paths.
#[must_use]
pub fn build_run_command(exe_path: &str) -> String {
    let trimmed = exe_path.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed.to_string()
    } else {
        format!("\"{trimmed}\"")
    }
}

/// Extracts the raw executable path from a Run command string by stripping quotes.
#[must_use]
#[allow(dead_code)] // Fase 1 T1.5
pub fn parse_run_command(cmd: &str) -> String {
    let trimmed = cmd.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].trim().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Compares two filesystem paths for equality under Windows semantics.
///
/// Normalizes path separators ('/' vs '\'), strips surrounding quotation marks,
/// and performs case-insensitive comparison (as Windows file systems are case-insensitive).
#[must_use]
#[allow(dead_code)] // Fase 1 T1.5
pub fn is_same_path(path_a: &str, path_b: &str) -> bool {
    let a = parse_run_command(path_a).replace('/', "\\").to_lowercase();
    let b = parse_run_command(path_b).replace('/', "\\").to_lowercase();
    a == b
}

/// Resolves the absolute path of the currently executing process executable.
pub fn get_current_exe_path() -> Result<String, String> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("Failed to determine current exe path: {e}"))
}

// ---------------------------------------------------------------------------
// Win32 Registry Access (Per-User HKCU Only, No Admin Required)
// ---------------------------------------------------------------------------

/// Reads the string value associated with `value_name` from HKCU Run key.
///
/// Returns `None` if the value does not exist, the subkey cannot be opened,
/// or reading fails. Never panics or crashes.
pub fn get_autostart_value(value_name: &str) -> Option<String> {
    let subkey_wide: Vec<u16> = RUN_SUBKEY_STR.encode_utf16().chain(Some(0)).collect();
    let val_name_wide: Vec<u16> = value_name.encode_utf16().chain(Some(0)).collect();

    let mut hkey = HKEY::default();
    // SAFETY: Opening HKCU Run subkey for read-only access.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(subkey_wide.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
    };

    if status.is_err() {
        return None;
    }

    // First query size of the string value in bytes
    let mut byte_len = 0u32;
    // SAFETY: Querying value length from open HKEY.
    let query_size_status = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(val_name_wide.as_ptr()),
            None,
            None,
            None,
            Some(&mut byte_len),
        )
    };

    if query_size_status.is_err() || byte_len == 0 {
        // SAFETY: Closing open HKEY.
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return None;
    }

    // Allocate buffer for UTF-16 wide characters
    let u16_len = (byte_len as usize).div_ceil(std::mem::size_of::<u16>());
    let mut buffer = vec![0u16; u16_len];

    // SAFETY: Fetching the string value into buffer.
    let query_data_status = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(val_name_wide.as_ptr()),
            None,
            None,
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut byte_len),
        )
    };

    // SAFETY: Closing open HKEY.
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    if query_data_status.is_err() {
        return None;
    }

    // Strip trailing null characters and decode string
    let actual_chars = if let Some(last_pos) = buffer.iter().position(|&c| c == 0) {
        &buffer[..last_pos]
    } else {
        &buffer[..]
    };

    Some(String::from_utf16_lossy(actual_chars))
}

/// Sets the string value for `value_name` in the HKCU Run key to the quoted `exe_path`.
pub fn set_autostart_value(value_name: &str, exe_path: &str) -> Result<(), String> {
    let subkey_wide: Vec<u16> = RUN_SUBKEY_STR.encode_utf16().chain(Some(0)).collect();
    let val_name_wide: Vec<u16> = value_name.encode_utf16().chain(Some(0)).collect();

    let cmd = build_run_command(exe_path);
    let cmd_wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();

    let mut hkey = HKEY::default();
    // SAFETY: Opening HKCU Run subkey with KEY_SET_VALUE permissions.
    let open_status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(subkey_wide.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };

    if open_status.is_err() {
        // Fallback to RegCreateKeyExW with KEY_SET_VALUE if key does not exist yet
        // SAFETY: Creating HKCU Run subkey if missing.
        let create_status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR::from_raw(subkey_wide.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut hkey,
                None,
            )
        };
        if let Err(e) = create_status.ok() {
            return Err(format!("RegOpenKeyExW/RegCreateKeyExW failed: {e:?}"));
        }
    }

    // Wide string bytes slice including null terminator
    // SAFETY: Casting wide u16 slice to byte slice for RegSetValueExW.
    let byte_slice = unsafe {
        std::slice::from_raw_parts(
            cmd_wide.as_ptr() as *const u8,
            cmd_wide.len() * std::mem::size_of::<u16>(),
        )
    };

    // SAFETY: Writing REG_SZ value to open HKEY.
    let set_status = unsafe {
        RegSetValueExW(
            hkey,
            PCWSTR::from_raw(val_name_wide.as_ptr()),
            0,
            REG_SZ,
            Some(byte_slice),
        )
    };

    // SAFETY: Closing open HKEY.
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    if let Err(e) = set_status.ok() {
        Err(format!("RegSetValueExW failed: {e:?}"))
    } else {
        Ok(())
    }
}

/// Deletes the value named `value_name` from the HKCU Run key.
pub fn delete_autostart_value(value_name: &str) -> Result<(), String> {
    let subkey_wide: Vec<u16> = RUN_SUBKEY_STR.encode_utf16().chain(Some(0)).collect();
    let val_name_wide: Vec<u16> = value_name.encode_utf16().chain(Some(0)).collect();

    let mut hkey = HKEY::default();
    // SAFETY: Opening HKCU Run subkey with KEY_SET_VALUE permissions to delete value.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(subkey_wide.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };

    if let Err(e) = status.ok() {
        return Err(format!("RegOpenKeyExW failed: {e:?}"));
    }

    // SAFETY: Deleting value from open HKEY.
    let del_status = unsafe { RegDeleteValueW(hkey, PCWSTR::from_raw(val_name_wide.as_ptr())) };

    // SAFETY: Closing open HKEY.
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    if let Err(e) = del_status.ok() {
        Err(format!("RegDeleteValueW failed: {e:?}"))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// High-Level Godnotch Autostart Interface
// ---------------------------------------------------------------------------

/// Checks if godnotch is currently registered to start with Windows.
///
/// Registry is the sole source of truth and is queried on demand when the menu opens.
/// If the entry exists (even if pointing to an older path), returns `true`.
#[must_use]
pub fn is_autostart_enabled() -> bool {
    get_autostart_value(RUN_KEY_VALUE_NAME).is_some()
}

/// Enables autostart by writing the current executable path to the HKCU Run key.
pub fn enable_autostart() -> Result<(), String> {
    let current_exe = get_current_exe_path()?;
    set_autostart_value(RUN_KEY_VALUE_NAME, &current_exe)
}

/// Disables autostart by removing the godnotch entry from the HKCU Run key.
pub fn disable_autostart() -> Result<(), String> {
    delete_autostart_value(RUN_KEY_VALUE_NAME)
}

/// Toggles the autostart registration state.
///
/// Returns `Ok(true)` if autostart was enabled with the current exe path,
/// or `Ok(false)` if it was disabled.
///
/// If an entry existed with an outdated path (e.g. executable was moved),
/// toggling it off cleans the stale entry, and toggling it back on records
/// the new current path.
pub fn toggle_autostart() -> Result<bool, String> {
    if is_autostart_enabled() {
        disable_autostart()?;
        Ok(false)
    } else {
        enable_autostart()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_run_command_quoted_paths() {
        // Path with spaces
        let p1 = r"C:\Program Files\godnotch\godnotch.exe";
        assert_eq!(
            build_run_command(p1),
            r#""C:\Program Files\godnotch\godnotch.exe""#
        );

        // Already quoted path
        let p2 = r#""C:\Tools\godnotch.exe""#;
        assert_eq!(build_run_command(p2), r#""C:\Tools\godnotch.exe""#);

        // Path with Unicode characters and spaces
        let p3 = r"C:\Pengguna\Bagus\Aplikasi\godnotch_🚀\godnotch.exe";
        assert_eq!(
            build_run_command(p3),
            r#""C:\Pengguna\Bagus\Aplikasi\godnotch_🚀\godnotch.exe""#
        );

        // Long path (> 260 chars)
        let long_segment = "a".repeat(300);
        let p4 = format!(r"C:\{long_segment}\godnotch.exe");
        assert_eq!(build_run_command(&p4), format!("\"{p4}\""));
    }

    #[test]
    fn test_parse_run_command() {
        assert_eq!(
            parse_run_command(r#""C:\Program Files\godnotch\godnotch.exe""#),
            r"C:\Program Files\godnotch\godnotch.exe"
        );
        assert_eq!(parse_run_command(r"C:\godnotch.exe"), r"C:\godnotch.exe");
        assert_eq!(
            parse_run_command(r#"  "D:\godnotch.exe"  "#),
            r"D:\godnotch.exe"
        );
    }

    #[test]
    fn test_is_same_path_case_and_separators() {
        // Identical paths
        assert!(is_same_path(r"C:\app\godnotch.exe", r"C:\app\godnotch.exe"));

        // Case insensitivity
        assert!(is_same_path(r"C:\app\godnotch.exe", r"c:\APP\GODNOTCH.EXE"));

        // Slash normalization ('/' vs '\')
        assert!(is_same_path(r"C:/app/godnotch.exe", r"C:\app\godnotch.exe"));

        // Quoted vs unquoted
        assert!(is_same_path(
            r#""C:\Program Files\godnotch.exe""#,
            r"C:\Program Files\godnotch.exe"
        ));

        // Different paths
        assert!(!is_same_path(
            r"C:\app\godnotch.exe",
            r"C:\other\godnotch.exe"
        ));
    }

    #[test]
    fn test_registry_autostart_isolated_roundtrip() {
        // Use an isolated test key name specific to this process PID
        let test_val_name = format!("godnotch-test-{}", std::process::id());

        // RAII guard ensuring the test value is deleted even if assertions panic
        struct RegistryCleaner(String);
        impl Drop for RegistryCleaner {
            fn drop(&mut self) {
                let _ = delete_autostart_value(&self.0);
            }
        }
        let _cleaner = RegistryCleaner(test_val_name.clone());

        // 1. Initially must not exist
        let _ = delete_autostart_value(&test_val_name);
        assert_eq!(get_autostart_value(&test_val_name), None);

        // 2. Set test value with quoted path
        let test_path = r"C:\Test Folder\godnotch.exe";
        let set_res = set_autostart_value(&test_val_name, test_path);
        if let Err(ref e) = set_res {
            if e.contains("Access is denied") {
                println!(
                    "[SKIP] Registry write restricted in current sandbox/permission environment: {e}"
                );
                return;
            }
        }
        assert!(
            set_res.is_ok(),
            "Setting registry value must succeed: {set_res:?}"
        );

        // 3. Query value back and verify content
        let read_val = get_autostart_value(&test_val_name);
        assert_eq!(
            read_val,
            Some(r#""C:\Test Folder\godnotch.exe""#.to_string())
        );

        // 4. Delete value
        let del_res = delete_autostart_value(&test_val_name);
        assert!(
            del_res.is_ok(),
            "Deleting registry value must succeed: {del_res:?}"
        );

        // 5. Must now be None
        assert_eq!(get_autostart_value(&test_val_name), None);
    }
}
