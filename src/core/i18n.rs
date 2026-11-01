//! Internationalization (i18n) and time/date formatting module.
//!
//! Provides localized string lookup tables (default Indonesian, optional English),
//! Windows 12/24-hour time formatting, and full date formatting functions.

/// Supported UI and locale languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // Fase 1 T1.4
pub enum Language {
    /// Indonesian (default).
    #[default]
    Indonesian,
    /// English.
    English,
}

impl Language {
    /// Determines the language from a Windows BCP-47 locale name (e.g. "id-ID" -> Indonesian).
    #[must_use]
    #[allow(dead_code)] // Fase 1 T1.4
    pub fn from_locale_name(locale: &str) -> Self {
        if locale.to_ascii_lowercase().starts_with("id") {
            Language::Indonesian
        } else {
            Language::English
        }
    }
}

/// Time display format (12-hour with AM/PM or 24-hour military).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // Fase 1 T1.4
pub enum TimeFormat {
    /// 12-hour format with AM/PM (e.g. "12:05 AM", "2:30 PM").
    Hour12,
    /// 24-hour format (e.g. "00:05", "14:30").
    #[default]
    Hour24,
}

const DAYS_ID: [&str; 7] = [
    "Minggu", "Senin", "Selasa", "Rabu", "Kamis", "Jumat", "Sabtu",
];
const DAYS_EN: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const MONTHS_ID: [&str; 12] = [
    "Januari",
    "Februari",
    "Maret",
    "April",
    "Mei",
    "Juni",
    "Juli",
    "Agustus",
    "September",
    "Oktober",
    "November",
    "Desember",
];
const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Returns the localized day of week name.
/// `day_of_week`: 0 = Sunday, 1 = Monday, ..., 6 = Saturday (matching Win32 SYSTEMTIME.wDayOfWeek).
#[must_use]
#[allow(dead_code)] // Fase 1 T1.4
pub fn day_name(lang: Language, day_of_week: u16) -> &'static str {
    let idx = (day_of_week as usize) % 7;
    match lang {
        Language::Indonesian => DAYS_ID[idx],
        Language::English => DAYS_EN[idx],
    }
}

/// Returns the localized month name.
/// `month`: 1 = January, ..., 12 = December (matching Win32 SYSTEMTIME.wMonth).
#[must_use]
#[allow(dead_code)] // Fase 1 T1.4
pub fn month_name(lang: Language, month: u16) -> &'static str {
    let idx = if month == 0 {
        0
    } else {
        ((month - 1) as usize) % 12
    };
    match lang {
        Language::Indonesian => MONTHS_ID[idx],
        Language::English => MONTHS_EN[idx],
    }
}

/// Formats a complete localized date string:
/// - ID: "Sabtu, 31 Oktober 2026"
/// - EN: "Saturday, October 31, 2026"
#[must_use]
#[allow(dead_code)] // Fase 1 T1.4
pub fn format_date(lang: Language, day_of_week: u16, day: u16, month: u16, year: u16) -> String {
    let d_name = day_name(lang, day_of_week);
    let m_name = month_name(lang, month);
    match lang {
        Language::Indonesian => format!("{d_name}, {day} {m_name} {year}"),
        Language::English => format!("{d_name}, {m_name} {day}, {year}"),
    }
}

/// Formats a time string according to the specified 12-hour or 24-hour format:
/// - 24-hour: "HH:mm" (e.g. "00:05", "12:00", "23:59")
/// - 12-hour: "h:mm AM/PM" (e.g. "12:05 AM", "12:00 PM", "11:59 PM")
#[must_use]
#[allow(dead_code)] // Fase 1 T1.4
pub fn format_time(format: TimeFormat, hour: u16, minute: u16) -> String {
    match format {
        TimeFormat::Hour24 => format!("{:02}:{:02}", hour, minute),
        TimeFormat::Hour12 => {
            let (h12, am_pm) = match hour {
                0 => (12, "AM"),
                1..=11 => (hour, "AM"),
                12 => (12, "PM"),
                13..=23 => (hour - 12, "PM"),
                _ => (hour % 12, "AM"),
            };
            format!("{}:{:02} {}", h12, minute, am_pm)
        }
    }
}

/// Returns the localized label for the "Start with Windows" tray menu option.
#[must_use]
#[allow(dead_code)] // Fase 1 T1.5
pub fn tray_autostart_label(lang: Language) -> &'static str {
    match lang {
        Language::Indonesian => "Mulai bersama Windows",
        Language::English => "Start with Windows",
    }
}

/// Returns the localized label for the "Hide on fullscreen" tray menu option.
#[must_use]
pub fn tray_hide_on_fullscreen_label(lang: Language) -> &'static str {
    match lang {
        Language::Indonesian => "Sembunyikan saat layar penuh",
        Language::English => "Hide on fullscreen",
    }
}

/// Returns the localized label for the "Open settings file" tray menu option.
#[must_use]
pub fn tray_open_settings_file_label(lang: Language) -> &'static str {
    match lang {
        Language::Indonesian => "Buka berkas pengaturan",
        Language::English => "Open settings file",
    }
}

/// Returns the localized label for the "Reload settings" tray menu option.
#[must_use]
pub fn tray_reload_settings_label(lang: Language) -> &'static str {
    match lang {
        Language::Indonesian => "Muat ulang pengaturan",
        Language::English => "Reload settings",
    }
}

/// Returns the localized label for the "Exit" tray menu option.
#[must_use]
#[allow(dead_code)] // Fase 1 T1.5
pub fn tray_exit_label(lang: Language) -> &'static str {
    match lang {
        Language::Indonesian => "Keluar",
        Language::English => "Exit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tray_menu_labels() {
        assert_eq!(
            tray_autostart_label(Language::Indonesian),
            "Mulai bersama Windows"
        );
        assert_eq!(
            tray_hide_on_fullscreen_label(Language::Indonesian),
            "Sembunyikan saat layar penuh"
        );
        assert_eq!(
            tray_open_settings_file_label(Language::Indonesian),
            "Buka berkas pengaturan"
        );
        assert_eq!(
            tray_reload_settings_label(Language::Indonesian),
            "Muat ulang pengaturan"
        );
        assert_eq!(tray_exit_label(Language::Indonesian), "Keluar");

        assert_eq!(
            tray_autostart_label(Language::English),
            "Start with Windows"
        );
        assert_eq!(
            tray_hide_on_fullscreen_label(Language::English),
            "Hide on fullscreen"
        );
        assert_eq!(
            tray_open_settings_file_label(Language::English),
            "Open settings file"
        );
        assert_eq!(
            tray_reload_settings_label(Language::English),
            "Reload settings"
        );
        assert_eq!(tray_exit_label(Language::English), "Exit");
    }

    #[test]
    fn test_language_from_locale_name() {
        assert_eq!(Language::from_locale_name("id-ID"), Language::Indonesian);
        assert_eq!(Language::from_locale_name("id"), Language::Indonesian);
        assert_eq!(Language::from_locale_name("en-US"), Language::English);
        assert_eq!(Language::from_locale_name("en-GB"), Language::English);
        assert_eq!(Language::from_locale_name("ja-JP"), Language::English);
    }

    #[test]
    fn test_format_date_id_and_en() {
        // Saturday, 31 October 2026 (day_of_week = 6, day = 31, month = 10, year = 2026)
        let date_id = format_date(Language::Indonesian, 6, 31, 10, 2026);
        assert_eq!(date_id, "Sabtu, 31 Oktober 2026");

        let date_en = format_date(Language::English, 6, 31, 10, 2026);
        assert_eq!(date_en, "Saturday, October 31, 2026");

        // Wednesday, 30 September 2026 (day_of_week = 3, day = 30, month = 9, year = 2026)
        let longest_id = format_date(Language::Indonesian, 3, 30, 9, 2026);
        assert_eq!(longest_id, "Rabu, 30 September 2026");

        let longest_en = format_date(Language::English, 3, 30, 9, 2026);
        assert_eq!(longest_en, "Wednesday, September 30, 2026");
    }

    #[test]
    fn test_format_time_24_hour() {
        assert_eq!(format_time(TimeFormat::Hour24, 0, 5), "00:05");
        assert_eq!(format_time(TimeFormat::Hour24, 0, 0), "00:00");
        assert_eq!(format_time(TimeFormat::Hour24, 9, 15), "09:15");
        assert_eq!(format_time(TimeFormat::Hour24, 12, 0), "12:00");
        assert_eq!(format_time(TimeFormat::Hour24, 12, 30), "12:30");
        assert_eq!(format_time(TimeFormat::Hour24, 23, 59), "23:59");
    }

    #[test]
    fn test_format_time_12_hour() {
        // Midnight cases
        assert_eq!(format_time(TimeFormat::Hour12, 0, 5), "12:05 AM");
        assert_eq!(format_time(TimeFormat::Hour12, 0, 0), "12:00 AM");

        // Morning cases
        assert_eq!(format_time(TimeFormat::Hour12, 1, 0), "1:00 AM");
        assert_eq!(format_time(TimeFormat::Hour12, 9, 15), "9:15 AM");
        assert_eq!(format_time(TimeFormat::Hour12, 11, 59), "11:59 AM");

        // Noon cases
        assert_eq!(format_time(TimeFormat::Hour12, 12, 0), "12:00 PM");
        assert_eq!(format_time(TimeFormat::Hour12, 12, 5), "12:05 PM");

        // Afternoon/Evening cases
        assert_eq!(format_time(TimeFormat::Hour12, 13, 0), "1:00 PM");
        assert_eq!(format_time(TimeFormat::Hour12, 23, 59), "11:59 PM");
    }
}
