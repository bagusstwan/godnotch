//! Persistent settings storage in `%APPDATA%\godnotch\settings.json`.
//!
//! # Safety & Atomicity Guarantees
//! - Atomic write: writes to `settings.json.tmp`, flushes buffers to disk, then calls
//!   `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
//! - Corrupt recovery: malformed or unparseable files are renamed to `settings.json.corrupt`
//!   and default settings are returned, preserving user edits for manual recovery without crash.
//! - First run: missing file returns `Settings::default()` without creating directories or files.
//! - Hermetic: all methods operate relative to a configurable base directory, enabling
//!   complete test isolation in temporary directories without ever touching `%APPDATA%`.

#![allow(dead_code)] // Fase 1 T1.7

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::core::config::Settings;

const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

#[link(name = "shell32")]
#[link(name = "ole32")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SHGetKnownFolderPath(
        rfid: *const windows::core::GUID,
        dwflags: u32,
        htoken: windows::Win32::Foundation::HANDLE,
        ppszpath: *mut windows::core::PWSTR,
    ) -> windows::core::HRESULT;

    fn CoTaskMemFree(pv: *const std::ffi::c_void);

    fn MoveFileExW(
        lpexistingfilename: *const u16,
        lpnewfilename: *const u16,
        dwflags: u32,
    ) -> windows::Win32::Foundation::BOOL;
}

/// Outcome of attempting to load settings from storage.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LoadOutcome {
    /// File did not exist; clean defaults returned without disk writes.
    DefaultNew,
    /// Settings successfully read and parsed from disk.
    LoadedSuccessfully,
    /// File was corrupt; renamed to `settings.json.corrupt` and defaults returned.
    CorruptRecoveredToDefault,
}

/// Errors that can occur during persistent settings operations.
#[derive(Debug)]
pub enum StorageError {
    /// Standard I/O error.
    Io(std::io::Error),
    /// Win32 API failure (`GetLastError`).
    Win32(u32),
    /// Could not resolve the Roaming AppData directory.
    NoBaseDir,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Win32(code) => write!(f, "Win32 error: {code}"),
            Self::NoBaseDir => write!(f, "Could not resolve AppData directory"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Manages atomic loading and saving of `Settings` to disk.
#[derive(Debug, Clone)]
pub struct SettingsStorage {
    base_dir: PathBuf,
}

impl SettingsStorage {
    /// Creates a storage manager targeting the official `%APPDATA%\godnotch` directory.
    #[must_use]
    pub fn new_default() -> Option<Self> {
        get_appdata_godnotch_dir().map(|base_dir| Self { base_dir })
    }

    /// Creates a storage manager targeting a custom directory (essential for unit testing).
    #[must_use]
    pub fn new_custom(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Returns a reference to the base directory.
    #[must_use]
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Path to `settings.json`.
    #[must_use]
    pub fn settings_path(&self) -> PathBuf {
        self.base_dir.join("settings.json")
    }

    /// Path to `settings.json.tmp`.
    #[must_use]
    pub fn temp_path(&self) -> PathBuf {
        self.base_dir.join("settings.json.tmp")
    }

    /// Path to `settings.json.corrupt`.
    #[must_use]
    pub fn corrupt_path(&self) -> PathBuf {
        self.base_dir.join("settings.json.corrupt")
    }

    /// Cleans up any stale temporary `.tmp` file left by an interrupted previous write.
    pub fn cleanup_temp(&self) {
        let tmp = self.temp_path();
        if tmp.exists() {
            let _ = std::fs::remove_file(tmp);
        }
    }

    /// Loads settings from disk.
    ///
    /// - If `settings.json` is missing: returns `(Settings::default(), LoadOutcome::DefaultNew)`
    ///   without creating any directories or files.
    /// - If `settings.json` is corrupt: renames it to `settings.json.corrupt` and returns
    ///   `(Settings::default(), LoadOutcome::CorruptRecoveredToDefault)`.
    /// - If valid: returns `(settings, LoadOutcome::LoadedSuccessfully)`.
    #[must_use]
    pub fn load(&self) -> (Settings, LoadOutcome) {
        self.cleanup_temp();

        let path = self.settings_path();
        if !path.exists() {
            return (Settings::default(), LoadOutcome::DefaultNew);
        }

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_e) => {
                #[cfg(debug_assertions)]
                eprintln!("[godnotch] Failed to read settings file: {_e}");
                return (Settings::default(), LoadOutcome::DefaultNew);
            }
        };

        match Settings::from_bytes(&bytes) {
            Ok(settings) => (settings, LoadOutcome::LoadedSuccessfully),
            Err(_err) => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[godnotch] Settings file is corrupt: {_err}. Moving to settings.json.corrupt"
                );

                // Preserve user edits by moving to .corrupt (overwriting any previous .corrupt)
                let corrupt = self.corrupt_path();
                let _ = atomic_rename(&path, &corrupt);

                (Settings::default(), LoadOutcome::CorruptRecoveredToDefault)
            }
        }
    }

    /// Atomically saves settings to disk.
    ///
    /// 1. Creates the base directory if it does not yet exist.
    /// 2. Writes serialized JSON to `settings.json.tmp` and flushes buffers (`sync_all`).
    /// 3. Atomically replaces `settings.json` via `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
    pub fn save(&self, settings: &Settings) -> Result<(), StorageError> {
        // Create folder only when first write occurs
        if !self.base_dir.exists() {
            std::fs::create_dir_all(&self.base_dir)?;
        }

        let tmp_path = self.temp_path();
        let target_path = self.settings_path();

        let json = settings.to_json();

        // Write and sync temp file
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }

        // Atomically rename tmp to destination
        if let Err(e) = atomic_rename(&tmp_path, &target_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }

        Ok(())
    }
}

/// Atomically replaces destination file using Win32 `MoveFileExW`.
fn atomic_rename(from: &Path, to: &Path) -> Result<(), StorageError> {
    let from_wide: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let to_wide: Vec<u16> = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: MoveFileExW replaces destination atomically and writes through to disk.
    let res = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if res.as_bool() {
        Ok(())
    } else {
        let err = unsafe { windows::Win32::Foundation::GetLastError() };
        Err(StorageError::Win32(err.0))
    }
}

/// Resolves the canonical `%APPDATA%\godnotch` directory using Win32 `SHGetKnownFolderPath`.
#[must_use]
pub fn get_appdata_godnotch_dir() -> Option<PathBuf> {
    // FOLDERID_RoamingAppData: {3EB685FD-9864-4FFB-B24F-4036063DEC0C}
    const FOLDERID_ROAMING_APP_DATA: windows::core::GUID =
        windows::core::GUID::from_u128(0x3eb685fd_9864_4ffb_b24f_4036063dec0c);

    let mut path_ptr = windows::core::PWSTR::null();

    // SAFETY: Querying canonical Windows Roaming AppData folder path.
    let hr = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_ROAMING_APP_DATA,
            0,
            windows::Win32::Foundation::HANDLE::default(),
            &mut path_ptr,
        )
    };

    if hr.is_ok() && !path_ptr.0.is_null() {
        // SAFETY: Reading null-terminated UTF-16 string returned by shell API.
        let path_str = unsafe {
            let len = (0..).take_while(|&i| *path_ptr.0.add(i) != 0).count();
            let slice = std::slice::from_raw_parts(path_ptr.0, len);
            String::from_utf16_lossy(slice)
        };
        // SAFETY: Freeing memory allocated by SHGetKnownFolderPath via CoTaskMemFree.
        unsafe {
            CoTaskMemFree(path_ptr.0 as *const _);
        }
        Some(PathBuf::from(path_str).join("godnotch"))
    } else {
        // Fallback to APPDATA environment variable
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("godnotch"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{LanguageConfig, TimeFormatConfig};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("godnotch_test_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_first_run_does_not_create_files_or_dir() {
        let test_dir = TestDir::new("first_run");
        let storage = SettingsStorage::new_custom(test_dir.path.clone());

        assert!(
            !test_dir.path.exists(),
            "Base dir must not exist before load"
        );
        let (settings, outcome) = storage.load();

        assert_eq!(outcome, LoadOutcome::DefaultNew);
        assert_eq!(settings, Settings::default());
        assert!(
            !test_dir.path.exists(),
            "Base dir must not be created on first run load"
        );
        assert!(
            !storage.settings_path().exists(),
            "settings.json must not exist"
        );
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let test_dir = TestDir::new("roundtrip");
        let storage = SettingsStorage::new_custom(test_dir.path.clone());

        let custom_settings = Settings {
            language: LanguageConfig::Id,
            time_format: TimeFormatConfig::Hour12,
            open_delay_ms: 150,
            close_delay_ms: 600,
            hide_on_fullscreen: false,
            ..Settings::default()
        };

        storage.save(&custom_settings).expect("Save must succeed");
        assert!(
            test_dir.path.exists(),
            "Base dir must be created on first save"
        );
        assert!(
            storage.settings_path().exists(),
            "settings.json must exist after save"
        );
        assert!(
            !storage.temp_path().exists(),
            "tmp file must be cleaned up after atomic save"
        );

        let (loaded, outcome) = storage.load();
        assert_eq!(outcome, LoadOutcome::LoadedSuccessfully);
        assert_eq!(loaded, custom_settings);
    }

    #[test]
    fn test_leftover_temp_cleaned_and_not_used() {
        let test_dir = TestDir::new("leftover_tmp");
        let storage = SettingsStorage::new_custom(test_dir.path.clone());
        std::fs::create_dir_all(&test_dir.path).unwrap();

        // Create a stale .tmp file
        let tmp = storage.temp_path();
        std::fs::write(&tmp, b"stale tmp data").unwrap();
        assert!(tmp.exists());

        // Load must clean up tmp and not use it
        let (settings, outcome) = storage.load();
        assert_eq!(outcome, LoadOutcome::DefaultNew);
        assert_eq!(settings, Settings::default());
        assert!(!tmp.exists(), "Stale tmp must be deleted during load");
    }

    #[test]
    fn test_corrupt_file_moved_to_corrupt_and_returns_default() {
        let test_dir = TestDir::new("corrupt_recovery");
        let storage = SettingsStorage::new_custom(test_dir.path.clone());
        std::fs::create_dir_all(&test_dir.path).unwrap();

        let malformed_content = b"{\"version\": 1, \"language\":";
        std::fs::write(storage.settings_path(), malformed_content).unwrap();

        let (settings, outcome) = storage.load();
        assert_eq!(outcome, LoadOutcome::CorruptRecoveredToDefault);
        assert_eq!(settings, Settings::default());

        assert!(
            !storage.settings_path().exists(),
            "Original corrupt file must be moved"
        );
        assert!(
            storage.corrupt_path().exists(),
            "settings.json.corrupt must exist"
        );

        let corrupt_content = std::fs::read(storage.corrupt_path()).unwrap();
        assert_eq!(
            corrupt_content, malformed_content,
            "Corrupt file content must be preserved"
        );
    }

    #[test]
    fn test_write_failure_preserves_old_file_and_does_not_panic() {
        let test_dir = TestDir::new("write_failure");
        let storage = SettingsStorage::new_custom(test_dir.path.clone());

        let initial_settings = Settings {
            language: LanguageConfig::En,
            ..Settings::default()
        };
        storage
            .save(&initial_settings)
            .expect("Initial save must succeed");

        // Force write failure by creating `settings.json.tmp` as a directory
        let tmp_path = storage.temp_path();
        std::fs::create_dir(&tmp_path).unwrap();

        let new_settings = Settings {
            language: LanguageConfig::Id,
            ..Settings::default()
        };

        let res = storage.save(&new_settings);
        assert!(
            res.is_err(),
            "Save must fail when tmp path cannot be opened"
        );

        // Clean up blocking directory
        let _ = std::fs::remove_dir(&tmp_path);

        // Verify old file was preserved and not corrupted
        let (loaded, outcome) = storage.load();
        assert_eq!(outcome, LoadOutcome::LoadedSuccessfully);
        assert_eq!(loaded, initial_settings);
    }

    #[test]
    fn test_canonical_appdata_resolution() {
        let appdata = get_appdata_godnotch_dir();
        assert!(
            appdata.is_some(),
            "Canonical AppData godnotch dir must resolve"
        );
        let path = appdata.unwrap();
        println!("[APPDATA_TEST] Resolved path: {}", path.display());
        assert!(path.ends_with("godnotch"));
    }
}
