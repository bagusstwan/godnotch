//! Single instance synchronization via Win32 named mutex.
//!
//! Ensures only one instance of godnotch runs per user session.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{Error, PCWSTR};

/// RAII guard holding the single-instance named mutex handle.
/// Automatically releases and closes the mutex handle on drop.
#[derive(Debug)]
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// Attempts to acquire the single-instance mutex.
    ///
    /// Returns:
    /// - `Ok(Some(SingleInstance))` if this process is the first/only instance.
    /// - `Ok(None)` if another instance is already running.
    /// - `Err(windows::core::Error)` if creating the mutex failed.
    pub fn try_acquire(name: PCWSTR) -> Result<Option<Self>, Error> {
        // SAFETY: Calling CreateMutexW with default security descriptor, initial owner true,
        // and a valid wide string pointer for the named mutex.
        let handle = unsafe { CreateMutexW(None, true, name)? };

        // SAFETY: Calling GetLastError immediately after CreateMutexW to inspect if ERROR_ALREADY_EXISTS was set.
        let already_exists = unsafe { GetLastError() == ERROR_ALREADY_EXISTS };

        if already_exists {
            // SAFETY: We did not acquire unique ownership, close handle immediately.
            unsafe {
                let _ = CloseHandle(handle);
            }
            Ok(None)
        } else {
            Ok(Some(Self { handle }))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: Closing the valid mutex handle when the guard is dropped.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_instance_acquisition_and_conflict() {
        let name_str = format!("Local\\godnotch_test_mutex_{}\0", std::process::id());
        let wide: Vec<u16> = name_str.encode_utf16().collect();
        let name = PCWSTR(wide.as_ptr());

        let first = SingleInstance::try_acquire(name);
        assert!(first.is_ok());
        let first = first.unwrap();
        assert!(first.is_some(), "First instance must succeed");

        let second = SingleInstance::try_acquire(name);
        assert!(second.is_ok());
        let second = second.unwrap();
        assert!(
            second.is_none(),
            "Second instance with same mutex name must return None"
        );

        drop(first);

        let third = SingleInstance::try_acquire(name);
        assert!(third.is_ok());
        let third = third.unwrap();
        assert!(
            third.is_some(),
            "Should be able to acquire after previous guard was dropped"
        );
    }
}
