//! Windows autostart configuration stub.
//!
//! Manages the `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` registry key
//! to toggle startup on user login without requiring administrative privileges.

/// Checks if godnotch is configured to start with Windows.
#[must_use]
pub fn is_autostart_enabled() -> bool {
    false
}

/// Toggles whether godnotch starts with Windows.
pub fn toggle_autostart() -> Result<bool, String> {
    Ok(false)
}
