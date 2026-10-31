//! Multi-axis motion orchestrator for godnotch.
//!
//! Manages decoupled springs for width and height axes with distinct dynamic
//! presets for Opening and Closing states. Corner radius is driven by the
//! clamped vertical height progression.

use crate::core::spring::Spring;

/// Motion preset containing spring stiffness (k) and damping ratio (zeta).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionPreset {
    pub k: f32,
    pub zeta: f32,
}

// ---------------------------------------------------------------------------
// Centralized Motion Preset Table
// ---------------------------------------------------------------------------
// State   | Axis   | Stiffness (k) | Damping (zeta) | Character
// --------+--------+---------------+----------------+-----------------------
// OPEN    | Width  | 360.0         | 0.68           | Elastic spring bounce
// OPEN    | Height | 480.0         | 0.75           | Snappy responsive drop
// CLOSE   | Width  | 420.0         | 0.92           | Controlled retraction
// CLOSE   | Height | 420.0         | 0.92           | Smooth subtle tuck
// ---------------------------------------------------------------------------

pub const PRESET_OPEN_WIDTH: MotionPreset = MotionPreset {
    k: 360.0,
    zeta: 0.68,
};
pub const PRESET_OPEN_HEIGHT: MotionPreset = MotionPreset {
    k: 480.0,
    zeta: 0.75,
};
pub const PRESET_CLOSE_WIDTH: MotionPreset = MotionPreset {
    k: 420.0,
    zeta: 0.92,
};
pub const PRESET_CLOSE_HEIGHT: MotionPreset = MotionPreset {
    k: 420.0,
    zeta: 0.92,
};

/// Dynamic physical pixel motion ranges based on notch dimensions (Closed: 164x30 -> Open: 360x112).
pub const MOTION_RANGE_WIDTH: f32 = 196.0; // 360.0 - 164.0
pub const MOTION_RANGE_HEIGHT: f32 = 82.0; // 112.0 - 30.0

#[allow(dead_code)] // Fase 1
pub const CLOSED_CORNER_RADIUS: f32 = 9.0;
#[allow(dead_code)] // Fase 1
pub const OPEN_CORNER_RADIUS: f32 = 24.0;

/// State direction of the notch motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionDirection {
    Opening,
    Closing,
}

/// Dual-axis motion system animating width and height independently.
#[derive(Debug, Clone, PartialEq)]
pub struct Motion {
    pub width_spring: Spring,
    pub height_spring: Spring,
    pub direction: MotionDirection,
}

impl Motion {
    /// Creates a new motion system initialized at the closed state (0.0).
    #[must_use]
    pub fn new() -> Self {
        Self {
            width_spring: Spring::with_params(0.0, PRESET_CLOSE_WIDTH.k, PRESET_CLOSE_WIDTH.zeta),
            height_spring: Spring::with_params(
                0.0,
                PRESET_CLOSE_HEIGHT.k,
                PRESET_CLOSE_HEIGHT.zeta,
            ),
            direction: MotionDirection::Closing,
        }
    }

    /// Directs the motion toward opening (1.0), applying the OPEN presets
    /// while strictly preserving current positions and velocities.
    pub fn open(&mut self) {
        self.direction = MotionDirection::Opening;
        self.width_spring.k = PRESET_OPEN_WIDTH.k;
        self.width_spring.zeta = PRESET_OPEN_WIDTH.zeta;
        self.width_spring.set_target(1.0);

        self.height_spring.k = PRESET_OPEN_HEIGHT.k;
        self.height_spring.zeta = PRESET_OPEN_HEIGHT.zeta;
        self.height_spring.set_target(1.0);
    }

    /// Directs the motion toward closing (0.0), applying the CLOSE presets
    /// while strictly preserving current positions and velocities.
    pub fn close(&mut self) {
        self.direction = MotionDirection::Closing;
        self.width_spring.k = PRESET_CLOSE_WIDTH.k;
        self.width_spring.zeta = PRESET_CLOSE_WIDTH.zeta;
        self.width_spring.set_target(0.0);

        self.height_spring.k = PRESET_CLOSE_HEIGHT.k;
        self.height_spring.zeta = PRESET_CLOSE_HEIGHT.zeta;
        self.height_spring.set_target(0.0);
    }

    /// Sets a generic target (1.0 = Open, 0.0 = Close).
    #[allow(dead_code)] // Fase 1
    pub fn set_target(&mut self, target: f32) {
        if target >= 0.5 {
            self.open();
        } else {
            self.close();
        }
    }

    /// Advances both springs by `dt` seconds using exact analytical integration.
    /// Returns `true` if both width and height have settled within pixel criteria.
    pub fn step(&mut self, dt: f32) -> bool {
        let w_settled = self.width_spring.step(dt, MOTION_RANGE_WIDTH);
        let h_settled = self.height_spring.step(dt, MOTION_RANGE_HEIGHT);
        w_settled && h_settled
    }

    /// Returns `true` if both axes are settled.
    #[must_use]
    #[allow(dead_code)] // Fase 1/2 API
    pub fn is_settled(&self) -> bool {
        self.width_spring.is_settled(MOTION_RANGE_WIDTH)
            && self.height_spring.is_settled(MOTION_RANGE_HEIGHT)
    }

    /// Current corner radius driven by clamped height progress [0.0, 1.0].
    #[must_use]
    #[allow(dead_code)] // Fase 1
    pub fn corner_radius(&self) -> f32 {
        let p_h = self.height_spring.pos.clamp(0.0, 1.0);
        CLOSED_CORNER_RADIUS + p_h * (OPEN_CORNER_RADIUS - CLOSED_CORNER_RADIUS)
    }

    /// Width progress (can exceed 1.0 during overshoot).
    #[must_use]
    pub fn width_progress(&self) -> f32 {
        self.width_spring.pos
    }

    /// Height progress (can exceed 1.0 during overshoot).
    #[must_use]
    pub fn height_progress(&self) -> f32 {
        self.height_spring.pos
    }
}

impl Default for Motion {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_presets_applied_correctly() {
        let mut motion = Motion::new();
        assert_eq!(motion.width_spring.k, PRESET_CLOSE_WIDTH.k);
        assert_eq!(motion.width_spring.zeta, PRESET_CLOSE_WIDTH.zeta);
        assert_eq!(motion.height_spring.k, PRESET_CLOSE_HEIGHT.k);
        assert_eq!(motion.height_spring.zeta, PRESET_CLOSE_HEIGHT.zeta);

        motion.open();
        assert_eq!(motion.width_spring.k, PRESET_OPEN_WIDTH.k);
        assert_eq!(motion.width_spring.zeta, PRESET_OPEN_WIDTH.zeta);
        assert_eq!(motion.height_spring.k, PRESET_OPEN_HEIGHT.k);
        assert_eq!(motion.height_spring.zeta, PRESET_OPEN_HEIGHT.zeta);

        motion.close();
        assert_eq!(motion.width_spring.k, PRESET_CLOSE_WIDTH.k);
        assert_eq!(motion.width_spring.zeta, PRESET_CLOSE_WIDTH.zeta);
        assert_eq!(motion.height_spring.k, PRESET_CLOSE_HEIGHT.k);
        assert_eq!(motion.height_spring.zeta, PRESET_CLOSE_HEIGHT.zeta);
    }

    #[test]
    fn test_no_size_jump_on_reversal() {
        let mut motion = Motion::new();
        motion.open();

        // Advance 60ms
        motion.step(0.06);

        let w_pos = motion.width_progress();
        let h_pos = motion.height_progress();
        let w_vel = motion.width_spring.vel;
        let h_vel = motion.height_spring.vel;

        // Reverse to close
        motion.close();

        assert_eq!(
            motion.width_progress(),
            w_pos,
            "Width must not jump on reversal"
        );
        assert_eq!(
            motion.height_progress(),
            h_pos,
            "Height must not jump on reversal"
        );
        assert_eq!(
            motion.width_spring.vel, w_vel,
            "Width velocity must be preserved"
        );
        assert_eq!(
            motion.height_spring.vel, h_vel,
            "Height velocity must be preserved"
        );
    }

    #[test]
    fn test_settling_times() {
        // Test BUKA settling time (target: <= 600 ms)
        let mut motion_open = Motion::new();
        motion_open.open();

        let dt = 0.008; // 125 Hz simulation step
        let mut open_time = 0.0_f32;
        while !motion_open.step(dt) && open_time < 2.0 {
            open_time += dt;
        }

        assert!(
            open_time <= 0.600,
            "OPEN settling time must be <= 600ms, measured: {:.1}ms",
            open_time * 1000.0
        );

        // Test TUTUP settling time (target: <= 500 ms)
        let mut motion_close = Motion::new();
        motion_close.open();
        // Settle open first
        while !motion_close.step(dt) {}

        motion_close.close();
        let mut close_time = 0.0_f32;
        while !motion_close.step(dt) && close_time < 2.0 {
            close_time += dt;
        }

        assert!(
            close_time <= 0.500,
            "CLOSE settling time must be <= 500ms, measured: {:.1}ms",
            close_time * 1000.0
        );
    }
}
