//! Main application coordinator.
//!
//! Owns application-level state flags (e.g. dirty redraw flag, fullscreen suppression).
//! Scheduled for full implementation in Phase 1 (T1.1..T1.7).

/// Represents the central application state.
#[derive(Debug, Default)]
pub struct App {
    /// Indicates whether the notch needs to be redrawn on the next frame.
    #[allow(dead_code)] // Fase 1 T1.7
    pub dirty: bool,
    /// Indicates whether the notch is currently suppressed by an active fullscreen application.
    #[allow(dead_code)] // Fase 1 T1.6
    pub is_fullscreen_active: bool,
}

impl App {
    /// Creates a new App instance in the initial clean state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dirty: false,
            is_fullscreen_active: false,
        }
    }
}
