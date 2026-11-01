//! Platform time source and scheduler integration for Windows.
//!
//! # Safety Guarantee
//! Contains Win32 calls converting between UTC timestamps and local civil time
//! according to the system's active timezone and daylight saving configuration.
//! All `unsafe` blocks are accompanied by `// SAFETY:` explanations.

#![allow(dead_code)] // Fase 2 T2.2

use crate::core::scheduler::{LocalTime, LocalToUtcResult, TimeSource};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Win32FileTime {
    dw_low_date_time: u32,
    dw_high_date_time: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn FileTimeToSystemTime(
        lp_file_time: *const Win32FileTime,
        lp_system_time: *mut windows::Win32::Foundation::SYSTEMTIME,
    ) -> windows::Win32::Foundation::BOOL;

    fn SystemTimeToTzSpecificLocalTime(
        lp_time_zone_information: *const std::ffi::c_void,
        lp_universal_time: *const windows::Win32::Foundation::SYSTEMTIME,
        lp_local_time: *mut windows::Win32::Foundation::SYSTEMTIME,
    ) -> windows::Win32::Foundation::BOOL;

    fn TzSpecificLocalTimeToSystemTime(
        lp_time_zone_information: *const std::ffi::c_void,
        lp_local_time: *const windows::Win32::Foundation::SYSTEMTIME,
        lp_universal_time: *mut windows::Win32::Foundation::SYSTEMTIME,
    ) -> windows::Win32::Foundation::BOOL;

    fn SystemTimeToFileTime(
        lp_system_time: *const windows::Win32::Foundation::SYSTEMTIME,
        lp_file_time: *mut Win32FileTime,
    ) -> windows::Win32::Foundation::BOOL;
}

const FILETIME_UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
const TICKS_PER_MILLISECOND: u64 = 10_000;

/// Platform time source using Windows Win32 timezone APIs.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlatformTimeSource;

impl TimeSource for PlatformTimeSource {
    fn now_utc_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn utc_to_local(&self, utc_ms: u64) -> LocalTime {
        let ticks = utc_ms
            .saturating_mul(TICKS_PER_MILLISECOND)
            .saturating_add(FILETIME_UNIX_EPOCH_TICKS);
        let ft = Win32FileTime {
            dw_low_date_time: (ticks & 0xFFFF_FFFF) as u32,
            dw_high_date_time: (ticks >> 32) as u32,
        };

        let mut st_utc = windows::Win32::Foundation::SYSTEMTIME::default();
        let mut st_local = windows::Win32::Foundation::SYSTEMTIME::default();

        // SAFETY: Converting FILETIME to UTC SYSTEMTIME, then converting to local time using active system timezone (null).
        unsafe {
            let _ = FileTimeToSystemTime(&ft, &mut st_utc);
            let _ = SystemTimeToTzSpecificLocalTime(std::ptr::null(), &st_utc, &mut st_local);
        }

        LocalTime {
            year: st_local.wYear as i32,
            month: st_local.wMonth as u32,
            day: st_local.wDay as u32,
            hour: st_local.wHour as u32,
            minute: st_local.wMinute as u32,
            second: st_local.wSecond as u32,
            millisecond: st_local.wMilliseconds as u32,
        }
    }

    fn local_to_utc(&self, local: &LocalTime) -> LocalToUtcResult {
        let st_local = windows::Win32::Foundation::SYSTEMTIME {
            wYear: local.year as u16,
            wMonth: local.month as u16,
            wDayOfWeek: 0,
            wDay: local.day as u16,
            wHour: local.hour as u16,
            wMinute: local.minute as u16,
            wSecond: local.second as u16,
            wMilliseconds: local.millisecond as u16,
        };
        let mut st_utc = windows::Win32::Foundation::SYSTEMTIME::default();

        // SAFETY: Converting local SYSTEMTIME to UTC SYSTEMTIME using active system timezone (null).
        let success =
            unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &st_local, &mut st_utc) };

        if success.as_bool() {
            let mut ft = Win32FileTime::default();
            // SAFETY: Converting UTC SYSTEMTIME to FILETIME.
            unsafe {
                let _ = SystemTimeToFileTime(&st_utc, &mut ft);
            }
            let ticks = ((ft.dw_high_date_time as u64) << 32) | (ft.dw_low_date_time as u64);
            let utc_ms = ticks.saturating_sub(FILETIME_UNIX_EPOCH_TICKS) / TICKS_PER_MILLISECOND;
            LocalToUtcResult::Exact(utc_ms)
        } else {
            // Spring-forward DST gap: fallback to current time
            LocalToUtcResult::Gap {
                first_valid_utc_ms: self.now_utc_ms(),
            }
        }
    }
}

/// Handles a fired demo schedule event: sounds `MessageBeep` and appends a line to
/// `%TEMP%\godnotch-sched-demo.log` (capped at 256 KB).
#[cfg(feature = "sched-demo")]
pub fn handle_demo_event(ev: &crate::core::scheduler::SchedEvent) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBeep(utype: u32) -> windows::Win32::Foundation::BOOL;
    }
    // SAFETY: Emits default system notification sound without stealing focus.
    unsafe {
        let _ = MessageBeep(0xFFFF_FFFF);
    }

    let temp_log = std::env::temp_dir().join("godnotch-sched-demo.log");
    const MAX_DEMO_LOG_BYTES: u64 = 256 * 1024;
    let current_len = std::fs::metadata(&temp_log).map(|m| m.len()).unwrap_or(0);
    if current_len < MAX_DEMO_LOG_BYTES {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&temp_log)
        {
            let line = format!(
                "{},{},{},{},{},{}\n",
                ev.fired_utc_ms, ev.due_utc_ms, ev.late_by_ms, ev.missed_count, ev.id, ev.kind
            );
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_time_source_roundtrip() {
        let clock = PlatformTimeSource;
        let now_utc = clock.now_utc_ms();
        assert!(now_utc > 1_700_000_000_000);

        let local = clock.utc_to_local(now_utc);
        assert!(local.year >= 2024);
        assert!((1..=12).contains(&local.month));
        assert!((1..=31).contains(&local.day));
        assert!(local.hour <= 23);
        assert!(local.minute <= 59);

        let res = clock.local_to_utc(&local);
        let utc_reconstructed = res.resolve();
        // Allow up to 1000 ms diff due to millisecond truncation in civil time
        let diff = (now_utc as i64 - utc_reconstructed as i64).abs();
        assert!(diff <= 1000, "Reconstructed UTC diff too large: {diff} ms");
    }
}
