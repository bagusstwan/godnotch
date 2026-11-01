//! Pure core scheduler engine with recurrence, misfire policies, and injectable clock.
//!
//! # Pure Logic Guarantee
//! This module contains zero Win32 calls and zero external dependencies.
//! Time is injected via the [`TimeSource`] trait, allowing 100% deterministic testing
//! of clock jumps, timezone changes (e.g. WIB to WITA), and DST transitions (spring-forward gap
//! and fall-back overlap).

#![allow(dead_code)] // Fase 2 T2.2

use std::fmt::{self, Write};

use crate::core::config::{JsonValue, MAX_FILE_SIZE, ParseError, parse_json_value};

/// Maximum number of schedule entries supported simultaneously (256).
pub const MAX_ENTRIES: usize = 256;

/// Minimum timer wakeup clamp in milliseconds (20 ms) to prevent high-frequency spin loops.
pub const MIN_WAKEUP_CLAMP_MS: u64 = 20;

/// Maximum timer wakeup clamp in milliseconds (4 hours = 14,400,000 ms).
/// Clamps long waits so that even across suspend/sleep or missed power broadcasts,
/// the engine awakens to re-synchronize with wall clock without polling.
pub const MAX_WAKEUP_CLAMP_MS: u64 = 4 * 60 * 60 * 1000;

/// Maximum allowable payload size in bytes (64 bytes).
pub const MAX_PAYLOAD_BYTES: usize = 64;

/// Backward clock jump tolerance in milliseconds before triggering automatic rescheduling (1000 ms).
pub const CLOCK_JUMP_BACKWARD_TOLERANCE_MS: u64 = 1000;

/// Local civil time breakdown used for timezone-aware recurrence calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millisecond: u32,
}

/// Result of converting local time to UTC, handling DST gaps and overlaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalToUtcResult {
    /// Exact 1-to-1 conversion to UTC milliseconds.
    Exact(u64),
    /// Local time falls into a spring-forward DST gap (nonexistent local time).
    /// Resolves to the first valid UTC moment immediately after the transition.
    Gap { first_valid_utc_ms: u64 },
    /// Local time falls into a fall-back DST overlap (occurs twice locally).
    /// Resolves to the earlier UTC occurrence.
    Ambiguous {
        earlier_utc_ms: u64,
        later_utc_ms: u64,
    },
}

impl LocalToUtcResult {
    /// Resolves to a concrete UTC timestamp according to godnotch scheduling rules:
    /// - `Exact`: exact timestamp
    /// - `Gap`: first valid moment after the jump
    /// - `Ambiguous`: earlier occurrence
    #[must_use]
    pub const fn resolve(&self) -> u64 {
        match self {
            Self::Exact(utc) => *utc,
            Self::Gap { first_valid_utc_ms } => *first_valid_utc_ms,
            Self::Ambiguous { earlier_utc_ms, .. } => *earlier_utc_ms,
        }
    }
}

/// Injectable time source abstraction for testing and platform time operations.
pub trait TimeSource: Send + Sync {
    /// Current wall-clock time in UTC milliseconds since Unix epoch.
    fn now_utc_ms(&self) -> u64;

    /// Converts a UTC timestamp (milliseconds) to local civil time in the current timezone.
    fn utc_to_local(&self, utc_ms: u64) -> LocalTime;

    /// Converts local civil time to UTC, reporting DST gaps or ambiguities.
    fn local_to_utc(&self, local: &LocalTime) -> LocalToUtcResult;
}

/// Recurrence pattern for a schedule entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recurrence {
    /// Fires once at the specified UTC timestamp, then is automatically removed.
    Once { at_utc_ms: u64 },
    /// Fires repeatedly at fixed intervals anchored from the actual fire time.
    Interval {
        every_ms: u64,
        anchor: IntervalAnchor,
    },
    /// Fires daily at the specified local hour and minute (0..=23, 0..=59).
    DailyAt { hour: u32, minute: u32 },
}

/// Anchor reference point for interval-based recurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalAnchor {
    /// Next due time is calculated relative to the moment the previous event fired.
    FromFire,
}

/// Policy applied when an event was missed (e.g. system sleep, app closed, or large clock jump).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MisfirePolicy {
    /// If late by more than `grace_ms`, drop without firing.
    /// If within `grace_ms`, fire once normally.
    Skip { grace_ms: u64 },
    /// If late by more than `grace_ms`, drop without firing.
    /// If within `grace_ms`, fire exactly once with coalesced `missed_count > 0`.
    FireOnce { grace_ms: u64 },
}

/// Bounded byte buffer payload attached to an entry (max 64 bytes).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Payload(Vec<u8>);

impl Payload {
    /// Creates an empty payload (0 bytes).
    #[must_use]
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    /// Creates a payload from a raw byte slice, rejecting if over 64 bytes.
    pub fn from_bytes(slice: &[u8]) -> Result<Self, ScheduleError> {
        if slice.len() > MAX_PAYLOAD_BYTES {
            return Err(ScheduleError::PayloadTooLarge(slice.len()));
        }
        Ok(Self(slice.to_vec()))
    }

    /// Encodes a single `u32` value as a 4-byte big-endian payload.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        Self(val.to_be_bytes().to_vec())
    }

    /// Decodes a 4-byte payload back into `u32`, or returns `None` if not exactly 4 bytes.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        if self.0.len() == 4 {
            Some(u32::from_be_bytes([
                self.0[0], self.0[1], self.0[2], self.0[3],
            ]))
        } else {
            None
        }
    }

    /// Returns the underlying byte slice.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the payload is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Serializes the payload into a hex string.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(self.0.len() * 2);
        for b in &self.0 {
            let _ = write!(s, "{:02x}", b);
        }
        s
    }

    /// Parses a hex string into a `Payload`.
    pub fn from_hex(s: &str) -> Result<Self, ScheduleError> {
        let trimmed = s.trim();
        if !trimmed.len().is_multiple_of(2) {
            return Err(ScheduleError::InvalidHex);
        }
        if trimmed.len() / 2 > MAX_PAYLOAD_BYTES {
            return Err(ScheduleError::PayloadTooLarge(trimmed.len() / 2));
        }
        let mut bytes = Vec::with_capacity(trimmed.len() / 2);
        for chunk in trimmed.as_bytes().chunks(2) {
            let high = hex_val(chunk[0]).ok_or(ScheduleError::InvalidHex)?;
            let low = hex_val(chunk[1]).ok_or(ScheduleError::InvalidHex)?;
            bytes.push((high << 4) | low);
        }
        Ok(Self(bytes))
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A persistent scheduled task entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleEntry {
    /// Stable, unique identifier for this schedule.
    pub id: u32,
    /// Category / module tag identifier.
    pub kind: u16,
    /// Recurrence pattern.
    pub recurrence: Recurrence,
    /// Policy applied when event is late or missed.
    pub misfire_policy: MisfirePolicy,
    /// Optional small payload (<= 64 bytes).
    pub payload: Payload,
    /// Next scheduled due timestamp in UTC milliseconds.
    pub next_due_utc_ms: u64,
    /// Timestamp in UTC milliseconds when this entry last fired, or `None` if never fired.
    pub last_fired_utc_ms: Option<u64>,
}

/// An event triggered by the scheduler during polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedEvent {
    /// ID of the schedule that fired.
    pub id: u32,
    /// Category / module tag identifier.
    pub kind: u16,
    /// Associated payload.
    pub payload: Payload,
    /// Timestamp in UTC milliseconds when the event was originally due.
    pub due_utc_ms: u64,
    /// Timestamp in UTC milliseconds when the event actually fired.
    pub fired_utc_ms: u64,
    /// Latency in milliseconds (`fired_utc_ms - due_utc_ms`).
    pub late_by_ms: u64,
    /// Number of missed interval cycles coalesced into this single event.
    pub missed_count: u32,
}

/// Errors returned by scheduler operations.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ScheduleError {
    /// Duplicate entry ID detected.
    DuplicateId(u32),
    /// Capacity limit of 256 entries reached.
    MaxEntriesExceeded,
    /// Invalid recurrence parameters (e.g. interval < 1000ms or invalid hour/minute).
    InvalidRecurrence(&'static str),
    /// Invalid misfire policy parameters.
    InvalidMisfirePolicy(&'static str),
    /// Payload exceeds maximum allowable limit (64 bytes).
    PayloadTooLarge(usize),
    /// Invalid hex string in payload.
    InvalidHex,
    /// Entry not found when attempting to replace.
    NotFound(u32),
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "Duplicate schedule ID: {id}"),
            Self::MaxEntriesExceeded => {
                write!(f, "Maximum capacity of {MAX_ENTRIES} entries exceeded")
            }
            Self::InvalidRecurrence(msg) => write!(f, "Invalid recurrence: {msg}"),
            Self::InvalidMisfirePolicy(msg) => write!(f, "Invalid misfire policy: {msg}"),
            Self::PayloadTooLarge(len) => {
                write!(f, "Payload exceeds max {MAX_PAYLOAD_BYTES} bytes: {len}")
            }
            Self::InvalidHex => write!(f, "Invalid hex string in payload"),
            Self::NotFound(id) => write!(f, "Schedule ID not found: {id}"),
        }
    }
}

impl std::error::Error for ScheduleError {}

/// Pure single-engine scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduler {
    entries: Vec<ScheduleEntry>,
    last_seen_utc_ms: u64,
}

impl Scheduler {
    /// Creates a new empty scheduler initialized to the given current UTC timestamp.
    #[must_use]
    pub const fn new(now_utc_ms: u64) -> Self {
        Self {
            entries: Vec::new(),
            last_seen_utc_ms: now_utc_ms,
        }
    }

    /// Returns the number of scheduled entries.
    #[must_use]
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a slice of all active entries.
    #[must_use]
    pub fn entries(&self) -> &[ScheduleEntry] {
        &self.entries
    }

    /// Looks up an entry by ID.
    #[must_use]
    pub fn get(&self, id: u32) -> Option<&ScheduleEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Adds a new schedule entry, validating uniqueness, bounds, and payload limits.
    pub fn add(&mut self, entry: ScheduleEntry) -> Result<(), ScheduleError> {
        if self.entries.len() >= MAX_ENTRIES {
            return Err(ScheduleError::MaxEntriesExceeded);
        }
        if self.entries.iter().any(|e| e.id == entry.id) {
            return Err(ScheduleError::DuplicateId(entry.id));
        }
        Self::validate_entry(&entry)?;
        self.entries.push(entry);
        Ok(())
    }

    /// Removes an entry by ID, returning `true` if found and removed.
    pub fn remove(&mut self, id: u32) -> bool {
        if let Some(pos) = self.entries.iter().position(|e| e.id == id) {
            self.entries.remove(pos);
            true
        } else {
            false
        }
    }

    /// Replaces an existing entry with updated parameters.
    pub fn replace(&mut self, entry: ScheduleEntry) -> Result<(), ScheduleError> {
        Self::validate_entry(&entry)?;
        if let Some(pos) = self.entries.iter().position(|e| e.id == entry.id) {
            self.entries[pos] = entry;
            Ok(())
        } else {
            Err(ScheduleError::NotFound(entry.id))
        }
    }

    /// Clears all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn validate_entry(entry: &ScheduleEntry) -> Result<(), ScheduleError> {
        match &entry.recurrence {
            Recurrence::Once { .. } => {}
            Recurrence::Interval { every_ms, .. } => {
                if *every_ms < 1000 {
                    return Err(ScheduleError::InvalidRecurrence(
                        "interval every_ms must be >= 1000",
                    ));
                }
            }
            Recurrence::DailyAt { hour, minute } => {
                if *hour > 23 || *minute > 59 {
                    return Err(ScheduleError::InvalidRecurrence(
                        "hour must be 0..=23 and minute must be 0..=59",
                    ));
                }
            }
        }
        if entry.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(ScheduleError::PayloadTooLarge(entry.payload.len()));
        }
        Ok(())
    }

    /// Evaluates due schedules at `now_utc_ms`, returning fired events sorted deterministically
    /// by `due_utc_ms` then `id`.
    pub fn poll(&mut self, now_utc_ms: u64, time_source: &dyn TimeSource) -> Vec<SchedEvent> {
        // Automatic backward clock jump detection: if clock went backward beyond tolerance,
        // reschedule all intervals and daily schedules from `now_utc_ms`.
        if now_utc_ms + CLOCK_JUMP_BACKWARD_TOLERANCE_MS < self.last_seen_utc_ms {
            self.reschedule_all(now_utc_ms, time_source);
        }
        self.last_seen_utc_ms = now_utc_ms;

        let mut events = Vec::new();
        let mut completed_ids = Vec::new();

        for entry in &mut self.entries {
            if now_utc_ms >= entry.next_due_utc_ms {
                let late_by_ms = now_utc_ms - entry.next_due_utc_ms;

                match &entry.misfire_policy {
                    MisfirePolicy::Skip { grace_ms } => {
                        if late_by_ms > *grace_ms {
                            // Missed beyond grace period: drop without firing
                            match &entry.recurrence {
                                Recurrence::Once { .. } => {
                                    completed_ids.push(entry.id);
                                }
                                Recurrence::Interval { every_ms, .. } => {
                                    entry.next_due_utc_ms = now_utc_ms + every_ms;
                                }
                                Recurrence::DailyAt { hour, minute } => {
                                    entry.next_due_utc_ms = compute_next_daily_at(
                                        now_utc_ms,
                                        *hour,
                                        *minute,
                                        time_source,
                                    );
                                }
                            }
                        } else {
                            // Within grace: fire 1 event
                            entry.last_fired_utc_ms = Some(now_utc_ms);
                            events.push(SchedEvent {
                                id: entry.id,
                                kind: entry.kind,
                                payload: entry.payload.clone(),
                                due_utc_ms: entry.next_due_utc_ms,
                                fired_utc_ms: now_utc_ms,
                                late_by_ms,
                                missed_count: 0,
                            });

                            match &entry.recurrence {
                                Recurrence::Once { .. } => {
                                    completed_ids.push(entry.id);
                                }
                                Recurrence::Interval { every_ms, .. } => {
                                    entry.next_due_utc_ms = now_utc_ms + every_ms;
                                }
                                Recurrence::DailyAt { hour, minute } => {
                                    entry.next_due_utc_ms = compute_next_daily_at(
                                        now_utc_ms,
                                        *hour,
                                        *minute,
                                        time_source,
                                    );
                                }
                            }
                        }
                    }
                    MisfirePolicy::FireOnce { grace_ms } => {
                        if late_by_ms > *grace_ms {
                            // Missed beyond grace period: drop without firing
                            match &entry.recurrence {
                                Recurrence::Once { .. } => {
                                    completed_ids.push(entry.id);
                                }
                                Recurrence::Interval { every_ms, .. } => {
                                    entry.next_due_utc_ms = now_utc_ms + every_ms;
                                }
                                Recurrence::DailyAt { hour, minute } => {
                                    entry.next_due_utc_ms = compute_next_daily_at(
                                        now_utc_ms,
                                        *hour,
                                        *minute,
                                        time_source,
                                    );
                                }
                            }
                        } else {
                            // Within grace: fire 1 coalesced event
                            entry.last_fired_utc_ms = Some(now_utc_ms);
                            let missed_count = match &entry.recurrence {
                                Recurrence::Interval { every_ms, .. } if *every_ms > 0 => {
                                    (late_by_ms / *every_ms) as u32
                                }
                                _ => 0,
                            };

                            events.push(SchedEvent {
                                id: entry.id,
                                kind: entry.kind,
                                payload: entry.payload.clone(),
                                due_utc_ms: entry.next_due_utc_ms,
                                fired_utc_ms: now_utc_ms,
                                late_by_ms,
                                missed_count,
                            });

                            match &entry.recurrence {
                                Recurrence::Once { .. } => {
                                    completed_ids.push(entry.id);
                                }
                                Recurrence::Interval { every_ms, .. } => {
                                    entry.next_due_utc_ms = now_utc_ms + every_ms;
                                }
                                Recurrence::DailyAt { hour, minute } => {
                                    entry.next_due_utc_ms = compute_next_daily_at(
                                        now_utc_ms,
                                        *hour,
                                        *minute,
                                        time_source,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Remove completed `Once` entries
        for id in completed_ids {
            self.remove(id);
        }

        // Deterministic sorting: due timestamp first, then ID
        events.sort_by(|a, b| {
            a.due_utc_ms
                .cmp(&b.due_utc_ms)
                .then_with(|| a.id.cmp(&b.id))
        });

        events
    }

    /// Calculates milliseconds until the next scheduled event, or `None` if no entries exist.
    #[must_use]
    pub fn next_wakeup(&self, now_utc_ms: u64) -> Option<u64> {
        if self.entries.is_empty() {
            return None;
        }
        let earliest = self.entries.iter().map(|e| e.next_due_utc_ms).min()?;
        Some(earliest.saturating_sub(now_utc_ms))
    }

    /// Calculates the clamped wakeup interval in milliseconds (clamped to `20 ms..=4 hours`),
    /// or `None` if no entries exist.
    #[must_use]
    pub fn next_wakeup_clamped(&self, now_utc_ms: u64) -> Option<u32> {
        let ms = self.next_wakeup(now_utc_ms)?;
        let clamped = ms.clamp(MIN_WAKEUP_CLAMP_MS, MAX_WAKEUP_CLAMP_MS) as u32;
        Some(clamped)
    }

    /// Reschedules all entries following a clock jump, timezone shift, or system resume.
    pub fn reschedule_all(&mut self, now_utc_ms: u64, time_source: &dyn TimeSource) {
        self.last_seen_utc_ms = now_utc_ms;

        for entry in &mut self.entries {
            match &entry.recurrence {
                Recurrence::Once { at_utc_ms } => {
                    if *at_utc_ms > now_utc_ms {
                        entry.next_due_utc_ms = *at_utc_ms;
                    }
                    // If in the past, leave entry.next_due_utc_ms alone so poll() applies misfire policy
                }
                Recurrence::Interval { every_ms, .. } => {
                    // Prevent absurd wait if clock jumped backwards:
                    if entry.next_due_utc_ms > now_utc_ms + *every_ms {
                        entry.next_due_utc_ms = now_utc_ms + *every_ms;
                    }
                }
                Recurrence::DailyAt { hour, minute } => {
                    // Recompute next daily occurrence in the updated timezone
                    entry.next_due_utc_ms =
                        compute_next_daily_at(now_utc_ms, *hour, *minute, time_source);
                }
            }
        }
    }

    /// Serializes all entries to a deterministic versioned JSON string.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(256 + self.entries.len() * 192);
        s.push_str("{\n  \"version\": 1,\n  \"entries\": [");
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("\n    {\n");
            let _ = writeln!(s, "      \"id\": {},", entry.id);
            let _ = writeln!(s, "      \"kind\": {},", entry.kind);
            s.push_str("      \"recurrence\": ");
            match &entry.recurrence {
                Recurrence::Once { at_utc_ms } => {
                    let _ = writeln!(s, "{{\"type\": \"once\", \"at_utc_ms\": {at_utc_ms}}},");
                }
                Recurrence::Interval { every_ms, anchor } => {
                    let anchor_str = match anchor {
                        IntervalAnchor::FromFire => "from_fire",
                    };
                    let _ = writeln!(
                        s,
                        "{{\"type\": \"interval\", \"every_ms\": {every_ms}, \"anchor\": \"{anchor_str}\"}},"
                    );
                }
                Recurrence::DailyAt { hour, minute } => {
                    let _ = writeln!(
                        s,
                        "{{\"type\": \"daily_at\", \"hour\": {hour}, \"minute\": {minute}}},"
                    );
                }
            }
            s.push_str("      \"misfire_policy\": ");
            match &entry.misfire_policy {
                MisfirePolicy::Skip { grace_ms } => {
                    let _ = writeln!(s, "{{\"type\": \"skip\", \"grace_ms\": {grace_ms}}},");
                }
                MisfirePolicy::FireOnce { grace_ms } => {
                    let _ = writeln!(s, "{{\"type\": \"fire_once\", \"grace_ms\": {grace_ms}}},");
                }
            }
            let _ = writeln!(s, "      \"payload\": \"{}\",", entry.payload.to_hex());
            let _ = writeln!(s, "      \"next_due_utc_ms\": {},", entry.next_due_utc_ms);
            match entry.last_fired_utc_ms {
                Some(ms) => {
                    let _ = writeln!(s, "      \"last_fired_utc_ms\": {ms}");
                }
                None => {
                    let _ = writeln!(s, "      \"last_fired_utc_ms\": null");
                }
            }
            s.push_str("    }");
        }
        if !self.entries.is_empty() {
            s.push('\n');
            s.push_str("  ]\n}\n");
        } else {
            s.push_str("]\n}\n");
        }
        s
    }

    /// Parses a `Scheduler` from raw JSON bytes, enforcing strict 64 KB and depth limits.
    pub fn from_bytes(bytes: &[u8], now_utc_ms: u64) -> Result<Self, ParseError> {
        if bytes.len() > MAX_FILE_SIZE {
            return Err(ParseError::ExceedsMaxSize(bytes.len()));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidUtf8)?;
        Self::from_json(text, now_utc_ms)
    }

    /// Parses a `Scheduler` from a JSON string. Invalid individual entries are dropped,
    /// while valid entries are preserved (up to 256 entries).
    pub fn from_json(content: &str, now_utc_ms: u64) -> Result<Self, ParseError> {
        if content.len() > MAX_FILE_SIZE {
            return Err(ParseError::ExceedsMaxSize(content.len()));
        }
        let trimmed = content.strip_prefix('\u{FEFF}').unwrap_or(content).trim();
        if trimmed.is_empty() {
            return Err(ParseError::Empty);
        }

        let root_val = parse_json_value(trimmed)?;
        let obj = match root_val {
            JsonValue::Object(map) => map,
            _ => return Err(ParseError::NotAnObject),
        };

        // Check schema version
        let mut version_found = false;
        let mut entries_val = None;
        for (k, v) in obj {
            if k == "version" {
                if let JsonValue::Number(n) = v
                    && (n.round() as u32) == 1
                {
                    version_found = true;
                }
            } else if k == "entries" {
                entries_val = Some(v);
            }
        }

        if !version_found {
            return Err(ParseError::InvalidSyntax(
                "unsupported or missing version".to_string(),
            ));
        }

        let raw_entries = match entries_val {
            Some(JsonValue::Array(arr)) => arr,
            _ => {
                return Err(ParseError::InvalidSyntax(
                    "missing or invalid entries array".to_string(),
                ));
            }
        };

        let mut scheduler = Self::new(now_utc_ms);
        for item in raw_entries {
            if scheduler.entries.len() >= MAX_ENTRIES {
                break;
            }
            if let JsonValue::Object(entry_fields) = item
                && let Some(parsed_entry) = Self::parse_single_entry(&entry_fields)
                && !scheduler.entries.iter().any(|e| e.id == parsed_entry.id)
                && Self::validate_entry(&parsed_entry).is_ok()
            {
                scheduler.entries.push(parsed_entry);
            }
        }

        Ok(scheduler)
    }

    fn parse_single_entry(fields: &[(String, JsonValue)]) -> Option<ScheduleEntry> {
        let mut id = None;
        let mut kind = None;
        let mut recurrence = None;
        let mut misfire_policy = None;
        let mut payload = Payload::empty();
        let mut next_due_utc_ms = None;
        let mut last_fired_utc_ms = None;

        for (k, v) in fields {
            match k.as_str() {
                "id" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= (u32::MAX as f64)
                    {
                        id = Some(*n as u32);
                    }
                }
                "kind" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= (u16::MAX as f64)
                    {
                        kind = Some(*n as u16);
                    }
                }
                "recurrence" => {
                    if let JsonValue::Object(rec_fields) = v {
                        recurrence = Self::parse_recurrence(rec_fields);
                    }
                }
                "misfire_policy" => {
                    if let JsonValue::Object(misp_fields) = v {
                        misfire_policy = Self::parse_misfire_policy(misp_fields);
                    }
                }
                "payload" => {
                    if let JsonValue::String(s) = v
                        && let Ok(p) = Payload::from_hex(s)
                    {
                        payload = p;
                    }
                }
                "next_due_utc_ms" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= (u64::MAX as f64)
                    {
                        next_due_utc_ms = Some(*n as u64);
                    }
                }
                "last_fired_utc_ms" => match v {
                    JsonValue::Number(n) if *n >= 0.0 && *n <= (u64::MAX as f64) => {
                        last_fired_utc_ms = Some(*n as u64);
                    }
                    JsonValue::Null => {
                        last_fired_utc_ms = None;
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Some(ScheduleEntry {
            id: id?,
            kind: kind?,
            recurrence: recurrence?,
            misfire_policy: misfire_policy?,
            payload,
            next_due_utc_ms: next_due_utc_ms?,
            last_fired_utc_ms,
        })
    }

    fn parse_recurrence(fields: &[(String, JsonValue)]) -> Option<Recurrence> {
        let mut rtype = None;
        let mut at_utc_ms = None;
        let mut every_ms = None;
        let mut hour = None;
        let mut minute = None;

        for (k, v) in fields {
            match k.as_str() {
                "type" => {
                    if let JsonValue::String(s) = v {
                        rtype = Some(s.as_str());
                    }
                }
                "at_utc_ms" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= (u64::MAX as f64)
                    {
                        at_utc_ms = Some(*n as u64);
                    }
                }
                "every_ms" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= (u64::MAX as f64)
                    {
                        every_ms = Some(*n as u64);
                    }
                }
                "hour" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= 23.0
                    {
                        hour = Some(*n as u32);
                    }
                }
                "minute" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= 59.0
                    {
                        minute = Some(*n as u32);
                    }
                }
                _ => {}
            }
        }

        match rtype? {
            "once" => Some(Recurrence::Once {
                at_utc_ms: at_utc_ms?,
            }),
            "interval" => {
                let ev = every_ms?;
                if ev < 1000 {
                    return None;
                }
                Some(Recurrence::Interval {
                    every_ms: ev,
                    anchor: IntervalAnchor::FromFire,
                })
            }
            "daily_at" => Some(Recurrence::DailyAt {
                hour: hour?,
                minute: minute?,
            }),
            _ => None,
        }
    }

    fn parse_misfire_policy(fields: &[(String, JsonValue)]) -> Option<MisfirePolicy> {
        let mut mtype = None;
        let mut grace_ms = None;

        for (k, v) in fields {
            match k.as_str() {
                "type" => {
                    if let JsonValue::String(s) = v {
                        mtype = Some(s.as_str());
                    }
                }
                "grace_ms" => {
                    if let JsonValue::Number(n) = v
                        && *n >= 0.0
                        && *n <= (u64::MAX as f64)
                    {
                        grace_ms = Some(*n as u64);
                    }
                }
                _ => {}
            }
        }

        let g = grace_ms?;
        match mtype? {
            "skip" => Some(MisfirePolicy::Skip { grace_ms: g }),
            "fire_once" => Some(MisfirePolicy::FireOnce { grace_ms: g }),
            _ => None,
        }
    }
}

/// Computes the next daily occurrence timestamp in UTC, using the injected `TimeSource`.
#[must_use]
pub fn compute_next_daily_at(
    now_utc_ms: u64,
    target_hour: u32,
    target_minute: u32,
    time_source: &dyn TimeSource,
) -> u64 {
    let local = time_source.utc_to_local(now_utc_ms);

    // Candidate 1: today
    let candidate_today = LocalTime {
        year: local.year,
        month: local.month,
        day: local.day,
        hour: target_hour,
        minute: target_minute,
        second: 0,
        millisecond: 0,
    };

    let target_today_utc = time_source.local_to_utc(&candidate_today).resolve();

    if target_today_utc > now_utc_ms {
        target_today_utc
    } else {
        // Target time has already passed today; schedule for tomorrow (or subsequent valid day)
        let mut days_ahead: i64 = 1;
        let (mut y, mut m, mut d) =
            add_days_to_date(local.year, local.month, local.day, days_ahead);
        let mut candidate_next = LocalTime {
            year: y,
            month: m,
            day: d,
            hour: target_hour,
            minute: target_minute,
            second: 0,
            millisecond: 0,
        };
        let mut target_next_utc = time_source.local_to_utc(&candidate_next).resolve();

        while target_next_utc <= now_utc_ms && days_ahead < 30 {
            days_ahead += 1;
            let (next_y, next_m, next_d) =
                add_days_to_date(local.year, local.month, local.day, days_ahead);
            y = next_y;
            m = next_m;
            d = next_d;
            candidate_next = LocalTime {
                year: y,
                month: m,
                day: d,
                hour: target_hour,
                minute: target_minute,
                second: 0,
                millisecond: 0,
            };
            target_next_utc = time_source.local_to_utc(&candidate_next).resolve();
        }

        target_next_utc
    }
}

/// Pure Gregorian date addition handling leap years, variable days in month, and year boundaries.
#[must_use]
pub fn add_days_to_date(year: i32, month: u32, day: u32, days: i64) -> (i32, u32, u32) {
    let d = days_from_civil(year, month, day);
    civil_from_days(d + days)
}

/// Converts civil date `(year, month, day)` to days since 1970-01-01 (Howard Hinnant algorithm).
#[must_use]
pub const fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { (y as i64) - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = if m > 2 {
        (153 * (m - 3) + 2) / 5 + d - 1
    } else {
        (153 * (m + 9) + 2) / 5 + d - 1
    };
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + (doe as i64) - 719468
}

/// Converts days since 1970-01-01 to civil date `(year, month, day)` (Howard Hinnant algorithm).
#[must_use]
pub const fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Returns `true` if `year` is a leap year.
#[must_use]
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Returns number of days in the given month and year.
#[must_use]
pub const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

    /// Mock time source with configurable wall clock, bias (UTC offset), and DST rules.
    pub struct MockTimeSource {
        now: AtomicU64,
        bias_ms: AtomicI64,
        gap_hour: Mutex<Option<(u32, u64)>>, // (hour, first_valid_utc_ms)
        ambiguous_hour: Mutex<Option<(u32, u64, u64)>>, // (hour, earlier_utc_ms, later_utc_ms)
    }

    impl MockTimeSource {
        pub fn new(initial_utc_ms: u64, bias_ms: i64) -> Self {
            Self {
                now: AtomicU64::new(initial_utc_ms),
                bias_ms: AtomicI64::new(bias_ms),
                gap_hour: Mutex::new(None),
                ambiguous_hour: Mutex::new(None),
            }
        }

        pub fn set_now(&self, utc_ms: u64) {
            self.now.store(utc_ms, Ordering::SeqCst);
        }

        pub fn advance(&self, delta_ms: u64) {
            self.now.fetch_add(delta_ms, Ordering::SeqCst);
        }

        pub fn set_bias_ms(&self, bias: i64) {
            self.bias_ms.store(bias, Ordering::SeqCst);
        }

        pub fn set_gap(&self, hour: u32, first_valid_utc_ms: u64) {
            *self.gap_hour.lock().unwrap() = Some((hour, first_valid_utc_ms));
        }

        pub fn set_ambiguous(&self, hour: u32, earlier_utc_ms: u64, later_utc_ms: u64) {
            *self.ambiguous_hour.lock().unwrap() = Some((hour, earlier_utc_ms, later_utc_ms));
        }
    }

    impl TimeSource for MockTimeSource {
        fn now_utc_ms(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }

        fn utc_to_local(&self, utc_ms: u64) -> LocalTime {
            let bias = self.bias_ms.load(Ordering::SeqCst);
            let local_ms = (utc_ms as i64) + bias;
            let days = local_ms.div_euclid(86_400_000);
            let rem_ms = local_ms.rem_euclid(86_400_000);
            let (year, month, day) = civil_from_days(days);
            let hour = (rem_ms / 3_600_000) as u32;
            let minute = ((rem_ms % 3_600_000) / 60_000) as u32;
            let second = ((rem_ms % 60_000) / 1000) as u32;
            let millisecond = (rem_ms % 1000) as u32;
            LocalTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
                millisecond,
            }
        }

        fn local_to_utc(&self, local: &LocalTime) -> LocalToUtcResult {
            if let Some((gap_h, first_valid)) = *self.gap_hour.lock().unwrap()
                && local.hour == gap_h
            {
                return LocalToUtcResult::Gap {
                    first_valid_utc_ms: first_valid,
                };
            }
            if let Some((amb_h, earlier, later)) = *self.ambiguous_hour.lock().unwrap()
                && local.hour == amb_h
            {
                return LocalToUtcResult::Ambiguous {
                    earlier_utc_ms: earlier,
                    later_utc_ms: later,
                };
            }

            let bias = self.bias_ms.load(Ordering::SeqCst);
            let days = days_from_civil(local.year, local.month, local.day);
            let local_ms = days * 86_400_000
                + (local.hour as i64) * 3_600_000
                + (local.minute as i64) * 60_000
                + (local.second as i64) * 1000
                + (local.millisecond as i64);
            let utc_ms = (local_ms - bias).max(0) as u64;
            LocalToUtcResult::Exact(utc_ms)
        }
    }

    // --- TEST GROUP 1: Once fires once and is removed; past Once respects Skip/FireOnce + grace ---
    #[test]
    fn test_group_1_once_firing_and_misfire_policies() {
        let t0 = 1_700_000_000_000;
        let clock = MockTimeSource::new(t0, 0);
        let mut sched = Scheduler::new(t0);

        // Standard future Once
        sched
            .add(ScheduleEntry {
                id: 1,
                kind: 10,
                recurrence: Recurrence::Once {
                    at_utc_ms: t0 + 5000,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 10_000 },
                payload: Payload::from_u32(42),
                next_due_utc_ms: t0 + 5000,
                last_fired_utc_ms: None,
            })
            .unwrap();

        // Polling before due
        clock.advance(2000);
        let evs = sched.poll(clock.now_utc_ms(), &clock);
        assert!(evs.is_empty());
        assert_eq!(sched.count(), 1);

        // Polling at due time
        clock.advance(3000); // at t0 + 5000
        let evs = sched.poll(clock.now_utc_ms(), &clock);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].id, 1);
        assert_eq!(evs[0].late_by_ms, 0);
        assert_eq!(evs[0].missed_count, 0);
        assert_eq!(evs[0].payload.as_u32(), Some(42));
        assert_eq!(sched.count(), 0, "Once entry must be removed after firing");

        // Polling again afterwards produces no events
        clock.advance(5000);
        assert!(sched.poll(clock.now_utc_ms(), &clock).is_empty());

        // Past Once: within grace (FireOnce)
        let mut sched2 = Scheduler::new(clock.now_utc_ms());
        sched2
            .add(ScheduleEntry {
                id: 2,
                kind: 10,
                recurrence: Recurrence::Once {
                    at_utc_ms: clock.now_utc_ms() - 3000,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
                payload: Payload::empty(),
                next_due_utc_ms: clock.now_utc_ms() - 3000,
                last_fired_utc_ms: None,
            })
            .unwrap();
        let evs = sched2.poll(clock.now_utc_ms(), &clock);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].id, 2);
        assert_eq!(evs[0].late_by_ms, 3000);
        assert_eq!(sched2.count(), 0);

        // Past Once: exceeded grace (FireOnce) -> dropped without firing
        sched2
            .add(ScheduleEntry {
                id: 3,
                kind: 10,
                recurrence: Recurrence::Once {
                    at_utc_ms: clock.now_utc_ms() - 10_000,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
                payload: Payload::empty(),
                next_due_utc_ms: clock.now_utc_ms() - 10_000,
                last_fired_utc_ms: None,
            })
            .unwrap();
        let evs = sched2.poll(clock.now_utc_ms(), &clock);
        assert!(
            evs.is_empty(),
            "Once past grace period must be dropped without firing"
        );
        assert_eq!(sched2.count(), 0);

        // Past Once: exceeded grace (Skip) -> dropped without firing
        sched2
            .add(ScheduleEntry {
                id: 4,
                kind: 10,
                recurrence: Recurrence::Once {
                    at_utc_ms: clock.now_utc_ms() - 10_000,
                },
                misfire_policy: MisfirePolicy::Skip { grace_ms: 5000 },
                payload: Payload::empty(),
                next_due_utc_ms: clock.now_utc_ms() - 10_000,
                last_fired_utc_ms: None,
            })
            .unwrap();
        assert!(sched2.poll(clock.now_utc_ms(), &clock).is_empty());
        assert_eq!(sched2.count(), 0);
    }

    // --- TEST GROUP 2: Interval repeating from fire, 3-hour sleep with 10-min interval ---
    #[test]
    fn test_group_2_interval_and_simulated_sleep_coalescing() {
        let t0 = 1_700_000_000_000;
        let clock = MockTimeSource::new(t0, 0);
        let mut sched = Scheduler::new(t0);

        let ten_min_ms = 10 * 60 * 1000; // 600,000 ms
        let grace_four_hours = 4 * 60 * 60 * 1000;

        // Interval with FireOnce policy and 4-hour grace
        sched
            .add(ScheduleEntry {
                id: 10,
                kind: 20,
                recurrence: Recurrence::Interval {
                    every_ms: ten_min_ms,
                    anchor: IntervalAnchor::FromFire,
                },
                misfire_policy: MisfirePolicy::FireOnce {
                    grace_ms: grace_four_hours,
                },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + ten_min_ms,
                last_fired_utc_ms: None,
            })
            .unwrap();

        // Simulate normal 1st fire at 10 minutes
        clock.advance(ten_min_ms);
        let evs = sched.poll(clock.now_utc_ms(), &clock);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].id, 10);
        assert_eq!(evs[0].missed_count, 0);
        let next_due = sched.get(10).unwrap().next_due_utc_ms;
        assert_eq!(next_due, clock.now_utc_ms() + ten_min_ms);

        // Simulate 3 hours of system sleep (3 * 3600 * 1000 = 10,800,000 ms)
        let three_hours_ms = 3 * 60 * 60 * 1000;
        clock.advance(three_hours_ms);

        // On resume: poll fires EXACTLY 1 event with coalesced missed_count
        let evs_resume = sched.poll(clock.now_utc_ms(), &clock);
        assert_eq!(
            evs_resume.len(),
            1,
            "Interval after sleep must produce exactly 1 event, not an event storm"
        );
        let ev = &evs_resume[0];
        assert_eq!(ev.id, 10);
        // During 3 hours (180 mins) after the next_due (10 mins in), 170 mins elapsed -> 17 intervals missed
        let expected_missed = (ev.late_by_ms / ten_min_ms) as u32;
        assert_eq!(ev.missed_count, expected_missed);
        assert!(ev.missed_count >= 17);

        // Next due is rescheduled from the resume fire time
        let next_due_after_resume = sched.get(10).unwrap().next_due_utc_ms;
        assert_eq!(
            next_due_after_resume,
            clock.now_utc_ms() + ten_min_ms,
            "Interval must be rescheduled from now"
        );

        // Repeat with Skip policy outside grace: 0 events fired
        let mut sched_skip = Scheduler::new(clock.now_utc_ms());
        sched_skip
            .add(ScheduleEntry {
                id: 11,
                kind: 20,
                recurrence: Recurrence::Interval {
                    every_ms: ten_min_ms,
                    anchor: IntervalAnchor::FromFire,
                },
                misfire_policy: MisfirePolicy::Skip { grace_ms: 60_000 }, // 1 min grace
                payload: Payload::empty(),
                next_due_utc_ms: clock.now_utc_ms() + ten_min_ms,
                last_fired_utc_ms: None,
            })
            .unwrap();

        clock.advance(three_hours_ms); // 3 hours sleep, far outside 1 min grace
        let evs_skip = sched_skip.poll(clock.now_utc_ms(), &clock);
        assert_eq!(
            evs_skip.len(),
            0,
            "Skip policy outside grace period must fire 0 events"
        );
        let next_due_skip = sched_skip.get(11).unwrap().next_due_utc_ms;
        assert_eq!(next_due_skip, clock.now_utc_ms() + ten_min_ms);
    }

    // --- TEST GROUP 3: DailyAt, timezone change WIB (+7) to WITA (+8), DST gap & ambiguous ---
    #[test]
    fn test_group_3_daily_at_and_timezone_shifts() {
        // Start on 2026-11-01 10:00:00 WIB (UTC = 03:00:00)
        // WIB is UTC+7 -> bias = +7 * 3,600,000 = +25,200,000 ms
        let wib_bias = 7 * 3600 * 1000;
        let days_2026_11_01 = days_from_civil(2026, 11, 1);
        let t0_utc = (days_2026_11_01 * 86_400_000 + 10 * 3600 * 1000 - wib_bias) as u64;

        let clock = MockTimeSource::new(t0_utc, wib_bias);
        let mut sched = Scheduler::new(t0_utc);

        // DailyAt 14:30 WIB (target today, in 4.5 hours)
        let next_due = compute_next_daily_at(t0_utc, 14, 30, &clock);
        let local_due = clock.utc_to_local(next_due);
        assert_eq!(local_due.year, 2026);
        assert_eq!(local_due.month, 11);
        assert_eq!(local_due.day, 1);
        assert_eq!(local_due.hour, 14);
        assert_eq!(local_due.minute, 30);

        sched
            .add(ScheduleEntry {
                id: 30,
                kind: 30,
                recurrence: Recurrence::DailyAt {
                    hour: 14,
                    minute: 30,
                },
                misfire_policy: MisfirePolicy::FireOnce {
                    grace_ms: 3_600_000,
                },
                payload: Payload::empty(),
                next_due_utc_ms: next_due,
                last_fired_utc_ms: None,
            })
            .unwrap();

        // Target already passed today (e.g. DailyAt 08:00 WIB when current is 10:00 WIB)
        let next_due_passed = compute_next_daily_at(t0_utc, 8, 0, &clock);
        let local_due_passed = clock.utc_to_local(next_due_passed);
        assert_eq!(
            local_due_passed.day, 2,
            "Passed target must be scheduled for tomorrow"
        );
        assert_eq!(local_due_passed.hour, 8);
        assert_eq!(local_due_passed.minute, 0);

        // Timezone shift from WIB (+7) to WITA (+8):
        // 14:30 in WITA (+8) corresponds to 06:30 UTC instead of 07:30 UTC (shifts due UTC earlier by 1 hour)
        let wita_bias = 8 * 3600 * 1000;
        clock.set_bias_ms(wita_bias);
        sched.reschedule_all(clock.now_utc_ms(), &clock);
        let shifted_due = sched.get(30).unwrap().next_due_utc_ms;
        assert_eq!(shifted_due, next_due - 3600 * 1000);
        let local_wita = clock.utc_to_local(shifted_due);
        assert_eq!(local_wita.hour, 14);
        assert_eq!(local_wita.minute, 30);

        // DST Spring-Forward Gap: local time 02:30 does not exist
        let gap_target_utc = t0_utc + 12 * 3600 * 1000;
        clock.set_gap(2, gap_target_utc);
        let dst_gap_due = compute_next_daily_at(t0_utc, 2, 30, &clock);
        assert_eq!(
            dst_gap_due, gap_target_utc,
            "DST gap must resolve to first valid UTC timestamp"
        );

        // DST Fall-Back Ambiguous: local time 01:30 occurs twice
        let earlier_utc = t0_utc + 15 * 3600 * 1000;
        let later_utc = t0_utc + 16 * 3600 * 1000;
        clock.set_ambiguous(1, earlier_utc, later_utc);
        let dst_amb_due = compute_next_daily_at(t0_utc, 1, 30, &clock);
        assert_eq!(
            dst_amb_due, earlier_utc,
            "DST ambiguous time must resolve to earlier occurrence"
        );

        // Rollover checks: leap year Feb 28 -> Feb 29 -> Mar 1
        let (y, m, d) = add_days_to_date(2024, 2, 28, 1);
        assert_eq!((y, m, d), (2024, 2, 29));
        let (y, m, d) = add_days_to_date(2024, 2, 29, 1);
        assert_eq!((y, m, d), (2024, 3, 1));
        // Non-leap year 2025 Feb 28 -> Mar 1
        let (y, m, d) = add_days_to_date(2025, 2, 28, 1);
        assert_eq!((y, m, d), (2025, 3, 1));
        // Year-end rollover Dec 31 -> Jan 1
        let (y, m, d) = add_days_to_date(2026, 12, 31, 1);
        assert_eq!((y, m, d), (2027, 1, 1));
    }

    // --- TEST GROUP 4: Clock jump backward and large forward ---
    #[test]
    fn test_group_4_clock_jump_handling() {
        let t0 = 1_700_000_000_000;
        let clock = MockTimeSource::new(t0, 0);
        let mut sched = Scheduler::new(t0);

        sched
            .add(ScheduleEntry {
                id: 40,
                kind: 1,
                recurrence: Recurrence::Interval {
                    every_ms: 10_000,
                    anchor: IntervalAnchor::FromFire,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 60_000 },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + 10_000,
                last_fired_utc_ms: None,
            })
            .unwrap();

        // Advance 10s -> fires normally
        clock.advance(10_000);
        let evs = sched.poll(clock.now_utc_ms(), &clock);
        assert_eq!(evs.len(), 1);

        // Clock jumps BACKWARD by 2 hours (e.g. user manually changed system clock)
        let two_hours_ms = 2 * 3600 * 1000;
        clock.set_now(clock.now_utc_ms() - two_hours_ms);

        // Calling poll: backward jump detected, reschedules without absurd wait or double fire
        let evs_after_jump = sched.poll(clock.now_utc_ms(), &clock);
        assert!(
            evs_after_jump.is_empty(),
            "Backward jump must not trigger immediate fire"
        );
        let entry = sched.get(40).unwrap();
        assert_eq!(
            entry.next_due_utc_ms,
            clock.now_utc_ms() + 10_000,
            "Next due must be recalculated from current clock"
        );

        // Large forward jump (exceeding grace): drops without firing
        clock.advance(24 * 3600 * 1000); // 1 day forward
        let evs_forward = sched.poll(clock.now_utc_ms(), &clock);
        assert!(
            evs_forward.is_empty(),
            "Large forward jump past grace must not fire"
        );
        assert_eq!(
            sched.get(40).unwrap().next_due_utc_ms,
            clock.now_utc_ms() + 10_000
        );
    }

    // --- TEST GROUP 5: Deterministic ordering, duplicate IDs, 256 entry cap, invalid values ---
    #[test]
    fn test_group_5_ordering_bounds_and_capacity() {
        let t0 = 1_700_000_000_000;
        let clock = MockTimeSource::new(t0, 0);
        let mut sched = Scheduler::new(t0);

        // Add out of order: due t0 + 1000 (id 5), due t0 + 500 (id 12), due t0 + 500 (id 3)
        sched
            .add(ScheduleEntry {
                id: 5,
                kind: 1,
                recurrence: Recurrence::Once {
                    at_utc_ms: t0 + 1000,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + 1000,
                last_fired_utc_ms: None,
            })
            .unwrap();

        sched
            .add(ScheduleEntry {
                id: 12,
                kind: 1,
                recurrence: Recurrence::Once {
                    at_utc_ms: t0 + 500,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + 500,
                last_fired_utc_ms: None,
            })
            .unwrap();

        sched
            .add(ScheduleEntry {
                id: 3,
                kind: 1,
                recurrence: Recurrence::Once {
                    at_utc_ms: t0 + 500,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + 500,
                last_fired_utc_ms: None,
            })
            .unwrap();

        // Duplicate ID rejected
        let dup_err = sched.add(ScheduleEntry {
            id: 5,
            kind: 1,
            recurrence: Recurrence::Once {
                at_utc_ms: t0 + 2000,
            },
            misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
            payload: Payload::empty(),
            next_due_utc_ms: t0 + 2000,
            last_fired_utc_ms: None,
        });
        assert_eq!(dup_err, Err(ScheduleError::DuplicateId(5)));

        // Invalid recurrence rejected
        let inv_interval = sched.add(ScheduleEntry {
            id: 99,
            kind: 1,
            recurrence: Recurrence::Interval {
                every_ms: 500, // < 1000 ms rejected
                anchor: IntervalAnchor::FromFire,
            },
            misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
            payload: Payload::empty(),
            next_due_utc_ms: t0 + 500,
            last_fired_utc_ms: None,
        });
        assert!(matches!(
            inv_interval,
            Err(ScheduleError::InvalidRecurrence(_))
        ));

        let inv_daily = sched.add(ScheduleEntry {
            id: 100,
            kind: 1,
            recurrence: Recurrence::DailyAt {
                hour: 25,
                minute: 10,
            },
            misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
            payload: Payload::empty(),
            next_due_utc_ms: t0 + 500,
            last_fired_utc_ms: None,
        });
        assert!(matches!(
            inv_daily,
            Err(ScheduleError::InvalidRecurrence(_))
        ));

        // Payload > 64 bytes rejected
        let large_bytes = [0u8; 65];
        assert_eq!(
            Payload::from_bytes(&large_bytes),
            Err(ScheduleError::PayloadTooLarge(65))
        );

        // Advance clock and verify deterministic ordering (due asc, then id asc)
        clock.advance(1500);
        let evs = sched.poll(clock.now_utc_ms(), &clock);
        assert_eq!(evs.len(), 3);
        assert_eq!((evs[0].due_utc_ms, evs[0].id), (t0 + 500, 3));
        assert_eq!((evs[1].due_utc_ms, evs[1].id), (t0 + 500, 12));
        assert_eq!((evs[2].due_utc_ms, evs[2].id), (t0 + 1000, 5));

        // Capacity test: fill to 256 entries, 257th rejected
        sched.clear();
        for i in 0..256 {
            sched
                .add(ScheduleEntry {
                    id: i as u32,
                    kind: 1,
                    recurrence: Recurrence::Once {
                        at_utc_ms: t0 + 10_000 + i as u64,
                    },
                    misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
                    payload: Payload::empty(),
                    next_due_utc_ms: t0 + 10_000 + i as u64,
                    last_fired_utc_ms: None,
                })
                .unwrap();
        }
        assert_eq!(sched.count(), 256);
        let overflow_err = sched.add(ScheduleEntry {
            id: 1000,
            kind: 1,
            recurrence: Recurrence::Once {
                at_utc_ms: t0 + 50_000,
            },
            misfire_policy: MisfirePolicy::FireOnce { grace_ms: 5000 },
            payload: Payload::empty(),
            next_due_utc_ms: t0 + 50_000,
            last_fired_utc_ms: None,
        });
        assert_eq!(overflow_err, Err(ScheduleError::MaxEntriesExceeded));
    }

    // --- TEST GROUP 6: Serialization roundtrip, partial corrupt tolerance, edge cases, fuzzing ---
    #[test]
    fn test_group_6_serialization_corruption_and_fuzz() {
        let t0 = 1_700_000_000_000;
        let mut sched = Scheduler::new(t0);

        sched
            .add(ScheduleEntry {
                id: 101,
                kind: 1,
                recurrence: Recurrence::Once {
                    at_utc_ms: t0 + 5000,
                },
                misfire_policy: MisfirePolicy::Skip { grace_ms: 2000 },
                payload: Payload::from_bytes(&[0xaa, 0xbb, 0xcc]).unwrap(),
                next_due_utc_ms: t0 + 5000,
                last_fired_utc_ms: None,
            })
            .unwrap();

        sched
            .add(ScheduleEntry {
                id: 102,
                kind: 2,
                recurrence: Recurrence::Interval {
                    every_ms: 60_000,
                    anchor: IntervalAnchor::FromFire,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 10_000 },
                payload: Payload::from_u32(0x12345678),
                next_due_utc_ms: t0 + 60_000,
                last_fired_utc_ms: Some(t0),
            })
            .unwrap();

        sched
            .add(ScheduleEntry {
                id: 103,
                kind: 3,
                recurrence: Recurrence::DailyAt {
                    hour: 22,
                    minute: 45,
                },
                misfire_policy: MisfirePolicy::FireOnce { grace_ms: 300_000 },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + 120_000,
                last_fired_utc_ms: None,
            })
            .unwrap();

        // Roundtrip serialization
        let json = sched.to_json();
        let restored = Scheduler::from_json(&json, t0).unwrap();
        assert_eq!(sched.entries(), restored.entries());

        // Partial corruption: array contains 1 invalid entry (bad hour 99) and 1 valid entry
        let partial_corrupt_json = r#"{
            "version": 1,
            "entries": [
                {
                    "id": 201,
                    "kind": 1,
                    "recurrence": {"type": "daily_at", "hour": 99, "minute": 10},
                    "misfire_policy": {"type": "skip", "grace_ms": 1000},
                    "payload": "",
                    "next_due_utc_ms": 1700000000000,
                    "last_fired_utc_ms": null
                },
                {
                    "id": 202,
                    "kind": 1,
                    "recurrence": {"type": "once", "at_utc_ms": 1700000005000},
                    "misfire_policy": {"type": "skip", "grace_ms": 1000},
                    "payload": "beef",
                    "next_due_utc_ms": 1700000005000,
                    "last_fired_utc_ms": null
                }
            ]
        }"#;
        let partial = Scheduler::from_json(partial_corrupt_json, t0).unwrap();
        assert_eq!(
            partial.count(),
            1,
            "Corrupt entry must be dropped while valid entry is kept"
        );
        assert_eq!(partial.entries()[0].id, 202);
        assert_eq!(
            partial.entries()[0].payload.as_bytes(),
            &[0xbe_u8, 0xef_u8][..]
        );

        // UTF-8 BOM tolerance
        let bom_json = format!("\u{FEFF}{json}");
        let from_bom = Scheduler::from_json(&bom_json, t0).unwrap();
        assert_eq!(sched.entries(), from_bom.entries());

        // Fatal errors
        assert_eq!(Scheduler::from_json("", t0), Err(ParseError::Empty));
        assert!(matches!(
            Scheduler::from_json("not json", t0),
            Err(ParseError::InvalidSyntax(_))
        ));
        assert_eq!(Scheduler::from_json("[]", t0), Err(ParseError::NotAnObject));

        // Exceeds 64 KB limit
        let huge_json = format!(
            "{{\"version\": 1, \"entries\": [], \"pad\": \"{}\"}}",
            "x".repeat(65_536)
        );
        assert!(matches!(
            Scheduler::from_json(&huge_json, t0),
            Err(ParseError::ExceedsMaxSize(_))
        ));

        // Deterministic pseudo-random fuzz testing with 2000 iterations (zero panics)
        let mut seed: u64 = 0x1234_5678_9abc_def0;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for _ in 0..2000 {
            let len = (rng() % 300) as usize;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                buf.push((rng() & 0xFF) as u8);
            }
            // Must not panic on arbitrary binary / mutated inputs
            let _ = Scheduler::from_bytes(&buf, t0);
        }
    }

    // --- TEST GROUP 7: next_wakeup and clamping ---
    #[test]
    fn test_group_7_next_wakeup_and_clamping() {
        let t0 = 1_700_000_000_000;
        let mut sched = Scheduler::new(t0);

        // Empty scheduler has no wakeup
        assert_eq!(sched.next_wakeup(t0), None);
        assert_eq!(sched.next_wakeup_clamped(t0), None);

        // Schedule due in 5 milliseconds -> clamped to MIN_WAKEUP_CLAMP_MS (20 ms)
        sched
            .add(ScheduleEntry {
                id: 1,
                kind: 1,
                recurrence: Recurrence::Once { at_utc_ms: t0 + 5 },
                misfire_policy: MisfirePolicy::Skip { grace_ms: 1000 },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + 5,
                last_fired_utc_ms: None,
            })
            .unwrap();
        assert_eq!(sched.next_wakeup(t0), Some(5));
        assert_eq!(sched.next_wakeup_clamped(t0), Some(20));

        // Schedule already due (now > due) -> clamped to 20 ms
        assert_eq!(sched.next_wakeup(t0 + 10), Some(0));
        assert_eq!(sched.next_wakeup_clamped(t0 + 10), Some(20));

        // Schedule due in 10 days -> clamped to MAX_WAKEUP_CLAMP_MS (4 hours = 14,400,000 ms)
        sched.clear();
        let ten_days_ms = 10 * 24 * 3600 * 1000;
        sched
            .add(ScheduleEntry {
                id: 2,
                kind: 1,
                recurrence: Recurrence::Once {
                    at_utc_ms: t0 + ten_days_ms,
                },
                misfire_policy: MisfirePolicy::Skip { grace_ms: 1000 },
                payload: Payload::empty(),
                next_due_utc_ms: t0 + ten_days_ms,
                last_fired_utc_ms: None,
            })
            .unwrap();
        assert_eq!(sched.next_wakeup(t0), Some(ten_days_ms));
        assert_eq!(sched.next_wakeup_clamped(t0), Some(14_400_000));
    }
}
