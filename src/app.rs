//! Main application coordinator stub.
//!
//! Owns application state, dirty flag management, and event dispatching.
//! Scheduled for full implementation in Phase 1 (T1.1..T1.7).

/// Represents the central application state.
#[derive(Debug, Default)]
pub struct App {
    /// Indicates whether the notch needs to be redrawn on the next frame.
    #[allow(dead_code)] // Fase 1 T1.7
    pub dirty: bool,
}

impl App {
    /// Creates a new App instance in the initial clean state.
    #[must_use]
    pub const fn new() -> Self {
        Self { dirty: false }
    }
}
