//! User settings model, serialization, and strict custom JSON parser.
//!
//! # Pure Logic Guarantee
//! This module contains zero Win32 calls and zero external dependencies (no `serde`).
//! It parses a versioned, flat configuration schema with strict validation bounds:
//! - Total size <= 64 KB
//! - Object/array nesting depth <= 8
//! - String length <= 256 characters
//! - Unknown fields ignored, duplicate keys resolve to last-wins
//! - Out-of-bounds numbers are clamped; invalid enum strings fall back per-field.

#![allow(dead_code)] // Fase 1 T1.7

use std::fmt;

/// Maximum allowable settings file size (64 KB).
pub const MAX_FILE_SIZE: usize = 64 * 1024;

/// Maximum allowable nesting depth for JSON structures.
pub const MAX_DEPTH: usize = 8;

/// Maximum allowable string length in characters.
pub const MAX_STRING_LEN: usize = 256;

/// Minimum allowable open delay in milliseconds.
pub const MIN_OPEN_DELAY_MS: u32 = 0;
/// Maximum allowable open delay in milliseconds.
pub const MAX_OPEN_DELAY_MS: u32 = 500;
/// Default open delay in milliseconds.
pub const DEFAULT_OPEN_DELAY_MS: u32 = 80;

/// Minimum allowable close delay in milliseconds.
pub const MIN_CLOSE_DELAY_MS: u32 = 100;
/// Maximum allowable close delay in milliseconds.
pub const MAX_CLOSE_DELAY_MS: u32 = 2000;
/// Default close delay in milliseconds.
pub const DEFAULT_CLOSE_DELAY_MS: u32 = 300;

/// Supported language configuration options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguageConfig {
    /// Automatically follow Windows system locale (default).
    #[default]
    Auto,
    /// Indonesian (`"id"`).
    Id,
    /// English (`"en"`).
    En,
}

impl LanguageConfig {
    /// Returns the string representation used in JSON.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Id => "id",
            Self::En => "en",
        }
    }
}

/// Supported time format configuration options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeFormatConfig {
    /// Automatically follow Windows time formatting (default).
    #[default]
    Auto,
    /// 12-hour format with AM/PM (`"12"`).
    Hour12,
    /// 24-hour military format (`"24"`).
    Hour24,
}

impl TimeFormatConfig {
    /// Returns the string representation used in JSON.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hour12 => "12",
            Self::Hour24 => "24",
        }
    }
}

/// Persistent user settings model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Schema version (currently 1).
    pub version: u32,
    /// Language preference.
    pub language: LanguageConfig,
    /// Time format preference.
    pub time_format: TimeFormatConfig,
    /// Whether notch hides when fullscreen applications/games/videos are active.
    pub hide_on_fullscreen: bool,
    /// Delay before expanding notch on hover, in milliseconds (0..=500).
    pub open_delay_ms: u32,
    /// Delay before collapsing notch on mouse leave, in milliseconds (100..=2000).
    pub close_delay_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            language: LanguageConfig::Auto,
            time_format: TimeFormatConfig::Auto,
            hide_on_fullscreen: true,
            open_delay_ms: DEFAULT_OPEN_DELAY_MS,
            close_delay_ms: DEFAULT_CLOSE_DELAY_MS,
        }
    }
}

impl Settings {
    /// Serializes the settings to a deterministic, human-readable JSON string.
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            "{{\n  \"version\": {},\n  \"language\": \"{}\",\n  \"time_format\": \"{}\",\n  \"hide_on_fullscreen\": {},\n  \"open_delay_ms\": {},\n  \"close_delay_ms\": {}\n}}\n",
            self.version,
            self.language.as_str(),
            self.time_format.as_str(),
            self.hide_on_fullscreen,
            self.open_delay_ms
                .clamp(MIN_OPEN_DELAY_MS, MAX_OPEN_DELAY_MS),
            self.close_delay_ms
                .clamp(MIN_CLOSE_DELAY_MS, MAX_CLOSE_DELAY_MS),
        )
    }

    /// Parses settings from raw bytes, validating size, UTF-8, and schema constraints.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() > MAX_FILE_SIZE {
            return Err(ParseError::ExceedsMaxSize(bytes.len()));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidUtf8)?;
        Self::from_json(text)
    }

    /// Parses settings from a JSON string.
    pub fn from_json(content: &str) -> Result<Self, ParseError> {
        if content.len() > MAX_FILE_SIZE {
            return Err(ParseError::ExceedsMaxSize(content.len()));
        }

        // Accept and strip UTF-8 BOM if present
        let trimmed = content.strip_prefix('\u{FEFF}').unwrap_or(content).trim();
        if trimmed.is_empty() {
            return Err(ParseError::Empty);
        }

        let parsed_val = parse_json_value(trimmed)?;
        let entries = match parsed_val {
            JsonValue::Object(map) => map,
            _ => return Err(ParseError::NotAnObject),
        };

        let mut settings = Self::default();

        for (key, val) in entries {
            match key.as_str() {
                "version" => {
                    if let JsonValue::Number(n) = val
                        && (0.0..=(u32::MAX as f64)).contains(&n)
                    {
                        settings.version = n as u32;
                    }
                }
                "language" => {
                    if let JsonValue::String(s) = val {
                        settings.language = match s.trim() {
                            "id" => LanguageConfig::Id,
                            "en" => LanguageConfig::En,
                            _ => LanguageConfig::Auto,
                        };
                    }
                }
                "time_format" => {
                    if let JsonValue::String(s) = val {
                        settings.time_format = match s.trim() {
                            "12" => TimeFormatConfig::Hour12,
                            "24" => TimeFormatConfig::Hour24,
                            _ => TimeFormatConfig::Auto,
                        };
                    }
                }
                "hide_on_fullscreen" => {
                    if let JsonValue::Bool(b) = val {
                        settings.hide_on_fullscreen = b;
                    }
                }
                "open_delay_ms" => {
                    if let JsonValue::Number(n) = val
                        && n.is_finite()
                    {
                        let clamped = (n.round() as i64)
                            .clamp(MIN_OPEN_DELAY_MS as i64, MAX_OPEN_DELAY_MS as i64)
                            as u32;
                        settings.open_delay_ms = clamped;
                    }
                }
                "close_delay_ms" => {
                    if let JsonValue::Number(n) = val
                        && n.is_finite()
                    {
                        let clamped = (n.round() as i64)
                            .clamp(MIN_CLOSE_DELAY_MS as i64, MAX_CLOSE_DELAY_MS as i64)
                            as u32;
                        settings.close_delay_ms = clamped;
                    }
                }
                _ => {
                    // Unknown fields are safely ignored per specification
                }
            }
        }

        Ok(settings)
    }
}

/// Parsing errors indicating corrupt or invalid JSON inputs.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ParseError {
    /// Input is empty or contains only whitespace.
    Empty,
    /// Input is not valid UTF-8.
    InvalidUtf8,
    /// Total file size exceeds `MAX_FILE_SIZE` (64 KB).
    ExceedsMaxSize(usize),
    /// String length exceeds `MAX_STRING_LEN` (256 characters).
    StringTooLong(usize),
    /// Nesting depth exceeds `MAX_DEPTH` (8 levels).
    DepthExceeded,
    /// Unexpected end of file / truncated JSON.
    UnexpectedEof,
    /// Invalid character or syntax.
    InvalidSyntax(String),
    /// Number is invalid, out of range, or NaN/infinity.
    InvalidNumber,
    /// Invalid escape sequence in string literal.
    InvalidEscape,
    /// Trailing characters after root value.
    TrailingCharacters,
    /// Top-level value is not a JSON object.
    NotAnObject,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "JSON input is empty"),
            Self::InvalidUtf8 => write!(f, "JSON input contains invalid UTF-8"),
            Self::ExceedsMaxSize(len) => {
                write!(f, "JSON exceeds max size 64KB (length: {len} bytes)")
            }
            Self::StringTooLong(len) => {
                write!(f, "JSON string exceeds max 256 characters (length: {len})")
            }
            Self::DepthExceeded => write!(f, "JSON nesting depth exceeds maximum of 8"),
            Self::UnexpectedEof => write!(f, "Unexpected end of JSON input"),
            Self::InvalidSyntax(msg) => write!(f, "Invalid JSON syntax: {msg}"),
            Self::InvalidNumber => write!(f, "Invalid number literal in JSON"),
            Self::InvalidEscape => write!(f, "Invalid escape sequence in JSON string"),
            Self::TrailingCharacters => write!(f, "Trailing non-whitespace characters in JSON"),
            Self::NotAnObject => write!(f, "Root JSON value is not an object"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Minimal abstract JSON value representation.
#[derive(Debug, PartialEq, Clone)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

/// Parses a JSON value from a string slice with strict depth and bounds checking.
pub fn parse_json_value(input: &str) -> Result<JsonValue, ParseError> {
    let bytes = input.as_bytes();
    let mut parser = Parser {
        bytes,
        cursor: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    if parser.is_eof() {
        return Err(ParseError::Empty);
    }
    let val = parser.parse_value()?;
    parser.skip_whitespace();
    if !parser.is_eof() {
        return Err(ParseError::TrailingCharacters);
    }
    Ok(val)
}

struct Parser<'a> {
    bytes: &'a [u8],
    cursor: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn is_eof(&self) -> bool {
        self.cursor >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        if self.cursor < self.bytes.len() {
            let b = self.bytes[self.cursor];
            self.cursor += 1;
            Some(b)
        } else {
            None
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                self.cursor += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, ParseError> {
        self.skip_whitespace();
        let b = self.peek().ok_or(ParseError::UnexpectedEof)?;
        match b {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(JsonValue::String),
            b't' | b'f' => self.parse_bool().map(JsonValue::Bool),
            b'n' => self.parse_null(),
            b'-' | b'0'..=b'9' => self.parse_number().map(JsonValue::Number),
            _ => Err(ParseError::InvalidSyntax(format!(
                "Unexpected character '{}' at offset {}",
                b as char, self.cursor
            ))),
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(ParseError::DepthExceeded);
        }
        self.depth += 1;
        self.cursor += 1; // consume '{'

        let mut entries = Vec::new();
        self.skip_whitespace();

        if let Some(b'}') = self.peek() {
            self.cursor += 1;
            self.depth -= 1;
            return Ok(JsonValue::Object(entries));
        }

        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                self.depth -= 1;
                return Err(ParseError::InvalidSyntax(
                    "Expected string key in object".into(),
                ));
            }
            let key = self.parse_string()?;
            self.skip_whitespace();

            if self.advance() != Some(b':') {
                self.depth -= 1;
                return Err(ParseError::InvalidSyntax(
                    "Expected ':' after object key".into(),
                ));
            }

            let val = self.parse_value()?;
            entries.push((key, val));

            self.skip_whitespace();
            match self.peek() {
                Some(b'}') => {
                    self.cursor += 1;
                    break;
                }
                Some(b',') => {
                    self.cursor += 1;
                }
                Some(other) => {
                    self.depth -= 1;
                    return Err(ParseError::InvalidSyntax(format!(
                        "Expected ',' or '}}' in object, found '{}'",
                        other as char
                    )));
                }
                None => {
                    self.depth -= 1;
                    return Err(ParseError::UnexpectedEof);
                }
            }
        }

        self.depth -= 1;
        Ok(JsonValue::Object(entries))
    }

    fn parse_array(&mut self) -> Result<JsonValue, ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(ParseError::DepthExceeded);
        }
        self.depth += 1;
        self.cursor += 1; // consume '['

        let mut elements = Vec::new();
        self.skip_whitespace();

        if let Some(b']') = self.peek() {
            self.cursor += 1;
            self.depth -= 1;
            return Ok(JsonValue::Array(elements));
        }

        loop {
            let val = self.parse_value()?;
            elements.push(val);

            self.skip_whitespace();
            match self.peek() {
                Some(b']') => {
                    self.cursor += 1;
                    break;
                }
                Some(b',') => {
                    self.cursor += 1;
                }
                Some(other) => {
                    self.depth -= 1;
                    return Err(ParseError::InvalidSyntax(format!(
                        "Expected ',' or ']' in array, found '{}'",
                        other as char
                    )));
                }
                None => {
                    self.depth -= 1;
                    return Err(ParseError::UnexpectedEof);
                }
            }
        }

        self.depth -= 1;
        Ok(JsonValue::Array(elements))
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        if self.advance() != Some(b'"') {
            return Err(ParseError::InvalidSyntax("Expected opening '\"'".into()));
        }

        let mut out = String::new();
        let mut char_count = 0usize;

        while let Some(b) = self.advance() {
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.advance().ok_or(ParseError::UnexpectedEof)?;
                    let ch = match esc {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\x08',
                        b'f' => '\x0C',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.parse_unicode_escape()?,
                        _ => return Err(ParseError::InvalidEscape),
                    };
                    char_count += 1;
                    if char_count > MAX_STRING_LEN {
                        return Err(ParseError::StringTooLong(char_count));
                    }
                    out.push(ch);
                }
                0x00..=0x1F => {
                    // Raw control characters are disallowed in JSON string literals
                    return Err(ParseError::InvalidSyntax(
                        "Unescaped control character in string literal".into(),
                    ));
                }
                _ => {
                    // Handle potential multi-byte UTF-8 character start
                    self.cursor -= 1;
                    let remaining = &self.bytes[self.cursor..];
                    let s = std::str::from_utf8(remaining).map_err(|_| ParseError::InvalidUtf8)?;
                    let ch = s.chars().next().ok_or(ParseError::UnexpectedEof)?;
                    self.cursor += ch.len_utf8();
                    if ch == '"' {
                        return Ok(out);
                    }
                    char_count += 1;
                    if char_count > MAX_STRING_LEN {
                        return Err(ParseError::StringTooLong(char_count));
                    }
                    out.push(ch);
                }
            }
        }

        Err(ParseError::UnexpectedEof)
    }

    fn parse_unicode_escape(&mut self) -> Result<char, ParseError> {
        let cp = self.parse_hex_u16()?;
        // Check for UTF-16 surrogate pairs
        if (0xD800..=0xDBFF).contains(&cp) {
            // High surrogate, look for \uDC00..\uDFFF
            if self.advance() == Some(b'\\') && self.advance() == Some(b'u') {
                let low = self.parse_hex_u16()?;
                if (0xDC00..=0xDFFF).contains(&low) {
                    let combined = 0x10000 + (((cp as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                    return char::from_u32(combined).ok_or(ParseError::InvalidEscape);
                }
            }
            return Err(ParseError::InvalidEscape);
        }

        char::from_u32(cp as u32).ok_or(ParseError::InvalidEscape)
    }

    fn parse_hex_u16(&mut self) -> Result<u16, ParseError> {
        let mut val = 0u16;
        for _ in 0..4 {
            let b = self.advance().ok_or(ParseError::UnexpectedEof)?;
            let digit = match b {
                b'0'..=b'9' => (b - b'0') as u16,
                b'a'..=b'f' => (b - b'a' + 10) as u16,
                b'A'..=b'F' => (b - b'A' + 10) as u16,
                _ => return Err(ParseError::InvalidEscape),
            };
            val = (val << 4) | digit;
        }
        Ok(val)
    }

    fn parse_bool(&mut self) -> Result<bool, ParseError> {
        if self.bytes[self.cursor..].starts_with(b"true") {
            self.cursor += 4;
            Ok(true)
        } else if self.bytes[self.cursor..].starts_with(b"false") {
            self.cursor += 5;
            Ok(false)
        } else {
            Err(ParseError::InvalidSyntax("Invalid boolean literal".into()))
        }
    }

    fn parse_null(&mut self) -> Result<JsonValue, ParseError> {
        if self.bytes[self.cursor..].starts_with(b"null") {
            self.cursor += 4;
            Ok(JsonValue::Null)
        } else {
            Err(ParseError::InvalidSyntax("Invalid null literal".into()))
        }
    }

    fn parse_number(&mut self) -> Result<f64, ParseError> {
        let start = self.cursor;

        if let Some(b'-') = self.peek() {
            self.cursor += 1;
        }

        let int_start = self.cursor;
        while let Some(b'0'..=b'9') = self.peek() {
            self.cursor += 1;
        }
        if self.cursor == int_start {
            return Err(ParseError::InvalidNumber);
        }

        if let Some(b'.') = self.peek() {
            self.cursor += 1;
            let frac_start = self.cursor;
            while let Some(b'0'..=b'9') = self.peek() {
                self.cursor += 1;
            }
            if self.cursor == frac_start {
                return Err(ParseError::InvalidNumber);
            }
        }

        if let Some(b'e' | b'E') = self.peek() {
            self.cursor += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.cursor += 1;
            }
            let exp_start = self.cursor;
            while let Some(b'0'..=b'9') = self.peek() {
                self.cursor += 1;
            }
            if self.cursor == exp_start {
                return Err(ParseError::InvalidNumber);
            }
        }

        let num_slice = &self.bytes[start..self.cursor];
        let num_str = std::str::from_utf8(num_slice).map_err(|_| ParseError::InvalidNumber)?;
        let val: f64 = num_str.parse().map_err(|_| ParseError::InvalidNumber)?;
        if !val.is_finite() {
            return Err(ParseError::InvalidNumber);
        }
        Ok(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_full_settings() {
        let json = r#"{
            "version": 1,
            "language": "id",
            "time_format": "12",
            "hide_on_fullscreen": false,
            "open_delay_ms": 120,
            "close_delay_ms": 450
        }"#;

        let settings = Settings::from_json(json).expect("Full valid settings must parse");
        assert_eq!(settings.version, 1);
        assert_eq!(settings.language, LanguageConfig::Id);
        assert_eq!(settings.time_format, TimeFormatConfig::Hour12);
        assert!(!settings.hide_on_fullscreen);
        assert_eq!(settings.open_delay_ms, 120);
        assert_eq!(settings.close_delay_ms, 450);
    }

    #[test]
    fn test_empty_object_gives_defaults() {
        let settings = Settings::from_json("{}").expect("Empty object must succeed");
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn test_missing_fields_give_defaults() {
        let json = r#"{"language": "en"}"#;
        let settings = Settings::from_json(json).expect("Partial settings must succeed");
        assert_eq!(settings.language, LanguageConfig::En);
        assert_eq!(settings.time_format, TimeFormatConfig::Auto);
        assert!(settings.hide_on_fullscreen);
        assert_eq!(settings.open_delay_ms, DEFAULT_OPEN_DELAY_MS);
        assert_eq!(settings.close_delay_ms, DEFAULT_CLOSE_DELAY_MS);
    }

    #[test]
    fn test_unknown_fields_are_ignored() {
        let json = r#"{
            "version": 1,
            "extra_key": "ignore me",
            "nested_extra": {"foo": [1, 2, 3]},
            "open_delay_ms": 150
        }"#;
        let settings = Settings::from_json(json).expect("Unknown fields must be ignored");
        assert_eq!(settings.open_delay_ms, 150);
        assert_eq!(settings.language, LanguageConfig::Auto);
    }

    #[test]
    fn test_out_of_range_numbers_are_clamped() {
        let json = r#"{
            "open_delay_ms": 9999,
            "close_delay_ms": -500
        }"#;
        let settings = Settings::from_json(json).expect("Out of range numbers must be clamped");
        assert_eq!(settings.open_delay_ms, MAX_OPEN_DELAY_MS);
        assert_eq!(settings.close_delay_ms, MIN_CLOSE_DELAY_MS);

        let json_low = r#"{
            "open_delay_ms": -10,
            "close_delay_ms": 50000
        }"#;
        let settings_low = Settings::from_json(json_low).expect("Clamping check");
        assert_eq!(settings_low.open_delay_ms, MIN_OPEN_DELAY_MS);
        assert_eq!(settings_low.close_delay_ms, MAX_CLOSE_DELAY_MS);
    }

    #[test]
    fn test_unknown_enum_falls_back_per_field() {
        let json = r#"{
            "language": "esperanto",
            "time_format": "binary",
            "open_delay_ms": 200
        }"#;
        let settings = Settings::from_json(json).expect("Unknown enums must fallback to default");
        assert_eq!(settings.language, LanguageConfig::Auto);
        assert_eq!(settings.time_format, TimeFormatConfig::Auto);
        assert_eq!(settings.open_delay_ms, 200);
    }

    #[test]
    fn test_duplicate_keys_last_wins() {
        let json = r#"{
            "language": "en",
            "language": "id"
        }"#;
        let settings = Settings::from_json(json).expect("Duplicate keys last wins");
        assert_eq!(settings.language, LanguageConfig::Id);
    }

    #[test]
    fn test_utf8_bom_accepted() {
        let json = "\u{FEFF}{\"language\": \"en\"}";
        let settings = Settings::from_json(json).expect("UTF-8 BOM must be stripped and accepted");
        assert_eq!(settings.language, LanguageConfig::En);
    }

    #[test]
    fn test_depth_limit_exceeded() {
        let deeply_nested = "[[[[[[[[[1]]]]]]]]]";
        let res = parse_json_value(deeply_nested);
        assert_eq!(res.unwrap_err(), ParseError::DepthExceeded);
    }

    #[test]
    fn test_string_length_exceeded() {
        let long_str = format!("\"{}\"", "a".repeat(257));
        let res = parse_json_value(&long_str);
        assert_eq!(res.unwrap_err(), ParseError::StringTooLong(257));
    }

    #[test]
    fn test_file_size_exceeded() {
        let big = vec![b' '; MAX_FILE_SIZE + 1];
        let res = Settings::from_bytes(&big);
        assert_eq!(
            res.unwrap_err(),
            ParseError::ExceedsMaxSize(MAX_FILE_SIZE + 1)
        );
    }

    #[test]
    fn test_invalid_utf8_rejected() {
        let invalid = [0xFF, 0xFE, 0xFD];
        let res = Settings::from_bytes(&invalid);
        assert_eq!(res.unwrap_err(), ParseError::InvalidUtf8);
    }

    #[test]
    fn test_truncated_json_rejected() {
        let truncated = r#"{"version": 1, "language":"#;
        assert_eq!(
            Settings::from_json(truncated).unwrap_err(),
            ParseError::UnexpectedEof
        );
    }

    #[test]
    fn test_empty_or_whitespace_rejected() {
        assert_eq!(Settings::from_json("").unwrap_err(), ParseError::Empty);
        assert_eq!(
            Settings::from_json("   \t\r\n").unwrap_err(),
            ParseError::Empty
        );
    }

    #[test]
    fn test_wrong_types_graceful_fallback() {
        // String given for bool, boolean given for number, number for string
        let json = r#"{
            "hide_on_fullscreen": "yes",
            "open_delay_ms": false,
            "language": 123
        }"#;
        let settings =
            Settings::from_json(json).expect("Wrong field types must fall back gracefully");
        assert!(settings.hide_on_fullscreen); // default true
        assert_eq!(settings.open_delay_ms, DEFAULT_OPEN_DELAY_MS);
        assert_eq!(settings.language, LanguageConfig::Auto);
    }

    #[test]
    fn test_root_not_an_object_rejected() {
        assert_eq!(
            Settings::from_json("[1, 2, 3]").unwrap_err(),
            ParseError::NotAnObject
        );
        assert_eq!(
            Settings::from_json("\"just a string\"").unwrap_err(),
            ParseError::NotAnObject
        );
        assert_eq!(
            Settings::from_json("12345").unwrap_err(),
            ParseError::NotAnObject
        );
    }

    #[test]
    fn test_roundtrip_serialization() {
        let original = Settings {
            version: 1,
            language: LanguageConfig::Id,
            time_format: TimeFormatConfig::Hour24,
            hide_on_fullscreen: false,
            open_delay_ms: 150,
            close_delay_ms: 600,
        };

        let serialized = original.to_json();
        let parsed =
            Settings::from_json(&serialized).expect("Serialized settings must parse cleanly");
        assert_eq!(original, parsed);
    }

    struct SimpleRng(u64);
    impl SimpleRng {
        fn new(seed: u64) -> Self {
            Self(seed)
        }
        fn next_u32(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 as u32
        }
    }

    #[test]
    fn test_pseudo_random_fuzz_never_panics() {
        let mut rng = SimpleRng::new(0xDEAD_BEEF_CAFE_BABE);
        let mut buf = [0u8; 128];

        for _ in 0..2000 {
            let len = (rng.next_u32() as usize % buf.len()) + 1;
            for byte in buf.iter_mut().take(len) {
                *byte = rng.next_u32() as u8;
            }

            // from_bytes must never panic, regardless of input bytes
            if let Ok(settings) = Settings::from_bytes(&buf[..len]) {
                assert!(settings.open_delay_ms <= MAX_OPEN_DELAY_MS);
                assert!(settings.close_delay_ms >= MIN_CLOSE_DELAY_MS);
                assert!(settings.close_delay_ms <= MAX_CLOSE_DELAY_MS);
            }
        }
    }
}
