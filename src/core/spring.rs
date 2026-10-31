//! Pure spring physics simulation for smooth notch animations.
//!
//! Provides a damped harmonic oscillator solved via semi-implicit Euler integration.
//! This module is independent of Win32 and can be thoroughly unit-tested on any platform.

/// Default spring stiffness (k).
///
/// SAD section 6 specifies k in 300.0..450.0; UI-UX-SPEC section 3 uses 380.0.
pub const DEFAULT_K: f32 = 380.0;

/// Default damping ratio (zeta).
///
/// SAD section 6 specifies zeta in 0.55..0.70; UI-UX-SPEC section 3 requires overshoot <= 6%.
/// With zeta = 0.68 and k = 380.0, the analytical overshoot is ~5.4% (strictly <= 6%).
pub const DEFAULT_ZETA: f32 = 0.68;

/// Position threshold below which motion is considered settled.
///
/// Threshold derivation:
/// Maximum physical motion range between Closed (190 px) and Open (420 px) is 230 px width (96 px height).
/// To ensure the resting error is strictly less than 0.5 physical pixels:
/// At 100% DPI: 230 px * 0.001 = 0.23 px (< 0.5 px).
/// At 200% DPI: 460 px * 0.001 = 0.46 px (< 0.5 px).
pub const EPSILON_POS: f32 = 0.001;

/// Velocity threshold below which motion is considered settled.
pub const EPSILON_VEL: f32 = 0.01;

/// Maximum integration timestep (seconds) per step to preserve numerical stability.
pub const MAX_DT: f32 = 0.04;

/// Pure spring simulation state.
#[derive(Debug, Clone, PartialEq)]
pub struct Spring {
    /// Current position.
    pub pos: f32,
    /// Current velocity.
    pub vel: f32,
    /// Target position.
    pub target: f32,
    /// Stiffness constant (k).
    pub k: f32,
    /// Damping ratio (zeta).
    pub zeta: f32,
    /// Settled flag indicating if movement has finished.
    settled: bool,
}

impl Spring {
    /// Creates a new spring initialized at `initial_pos` with default stiffness and damping.
    #[must_use]
    pub fn new(initial_pos: f32) -> Self {
        Self::with_params(initial_pos, DEFAULT_K, DEFAULT_ZETA)
    }

    /// Creates a new spring with custom stiffness `k` and damping ratio `zeta`.
    #[must_use]
    pub fn with_params(initial_pos: f32, k: f32, zeta: f32) -> Self {
        Self {
            pos: initial_pos,
            vel: 0.0,
            target: initial_pos,
            k,
            zeta,
            settled: true,
        }
    }

    /// Sets a new target position. If the target differs from current state beyond epsilons,
    /// marks the spring as not settled.
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
        if (self.target - self.pos).abs() > EPSILON_POS || self.vel.abs() > EPSILON_VEL {
            self.settled = false;
        }
    }

    /// Resets the spring immediately to a fixed position, zeroing velocity and marking it settled.
    #[allow(dead_code)] // Fase 1/2 API
    pub fn reset(&mut self, pos: f32) {
        self.pos = pos;
        self.target = pos;
        self.vel = 0.0;
        self.settled = true;
    }

    /// Returns `true` if the spring has settled at its target position.
    #[must_use]
    #[allow(dead_code)] // Fase 1/2 API
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Advances the spring simulation by `dt` seconds using semi-implicit Euler integration.
    ///
    /// Clamps `dt` to `[0.0, MAX_DT]` to maintain numerical stability under variable framerates
    /// and prevent explosions from large time steps.
    ///
    /// Returns `true` if the spring is settled, `false` if it is still animating.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.settled {
            return true;
        }

        // Clamp dt safely: handle NaN, negative, and excessively large timesteps.
        let safe_dt = if dt.is_nan() || dt <= 0.0 {
            0.0
        } else {
            dt.min(MAX_DT)
        };

        if safe_dt == 0.0 {
            return self.settled;
        }

        // Damping coefficient c = 2 * zeta * sqrt(k)
        let c = 2.0 * self.zeta * self.k.sqrt();
        let a = self.k * (self.target - self.pos) - c * self.vel;

        // Semi-implicit Euler integration
        self.vel += a * safe_dt;
        self.pos += self.vel * safe_dt;

        // Check settling condition
        if (self.target - self.pos).abs() <= EPSILON_POS && self.vel.abs() <= EPSILON_VEL {
            self.pos = self.target;
            self.vel = 0.0;
            self.settled = true;
        }

        self.settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_settled() {
        let spring = Spring::new(0.0);
        assert!(spring.is_settled());
        assert_eq!(spring.pos, 0.0);
        assert_eq!(spring.target, 0.0);
        assert_eq!(spring.vel, 0.0);
    }

    #[test]
    fn test_set_target_unsettles() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);
        assert!(!spring.is_settled());
        assert_eq!(spring.target, 1.0);
    }

    #[test]
    fn test_convergence_to_target() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);

        let dt = 0.016; // ~60 fps
        let mut steps = 0;
        let max_steps = 100; // 1.6 seconds max, should settle within ~0.35s

        while !spring.step(dt) && steps < max_steps {
            steps += 1;
        }

        assert!(
            spring.is_settled(),
            "Spring should settle within {max_steps} steps"
        );
        assert_eq!(
            spring.pos, 1.0,
            "Position must snap exactly to target upon settling"
        );
        assert_eq!(spring.vel, 0.0, "Velocity must zero out upon settling");
    }

    #[test]
    fn test_settling_time_within_450ms() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);

        let dt = 0.016; // 16 ms frames (60 FPS)
        let mut total_time = 0.0_f32;

        while !spring.step(dt) {
            total_time += dt;
            assert!(
                total_time < 2.0,
                "Spring failed to settle within 2.0 seconds"
            );
        }

        assert!(
            total_time <= 0.450,
            "Settling time must be <= 450ms at k=380, zeta=0.68, but took {:.1}ms",
            total_time * 1000.0
        );
        assert_eq!(spring.pos, 1.0);
        assert!(spring.is_settled());
    }

    #[test]
    fn test_no_nan_or_inf() {
        let mut spring = Spring::new(0.0);
        spring.set_target(100.0);

        for _ in 0..100 {
            spring.step(0.016);
            assert!(spring.pos.is_finite(), "Position produced non-finite value");
            assert!(spring.vel.is_finite(), "Velocity produced non-finite value");
        }
    }

    #[test]
    fn test_overshoot_bounded() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);

        let dt = 0.016;
        let mut max_pos: f32 = 0.0;

        while !spring.is_settled() {
            spring.step(dt);
            if spring.pos > max_pos {
                max_pos = spring.pos;
            }
        }

        // With default zeta=0.68, overshoot is ~5.4%, which is <= 6% (UI-UX-SPEC section 3)
        assert!(
            max_pos > 1.0,
            "Expected slight bounce/overshoot, got {max_pos}"
        );
        let overshoot_pct = (max_pos - 1.0) * 100.0;
        assert!(
            overshoot_pct <= 6.0,
            "Overshoot must be <= 6.0%, got {overshoot_pct:.2}%"
        );
    }

    #[test]
    fn test_settled_flag_when_idle() {
        let mut spring = Spring::new(5.0);
        assert!(spring.is_settled());
        assert!(spring.step(0.016));
        assert_eq!(spring.pos, 5.0);
    }

    #[test]
    fn test_zero_dt() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);

        let initial_pos = spring.pos;
        let initial_vel = spring.vel;

        spring.step(0.0);

        assert_eq!(spring.pos, initial_pos);
        assert_eq!(spring.vel, initial_vel);
        assert!(!spring.is_settled());
    }

    #[test]
    fn test_very_large_dt() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);

        // A massive timestep like 1000 seconds should be clamped to MAX_DT
        spring.step(1000.0);
        assert!(spring.pos.is_finite());
        assert!(spring.vel.is_finite());
        assert!(spring.pos > 0.0);
        assert!(spring.pos < 1.0); // Only advanced by MAX_DT
    }

    #[test]
    fn test_nan_and_negative_dt() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);

        spring.step(-1.0);
        assert_eq!(spring.pos, 0.0);
        assert_eq!(spring.vel, 0.0);

        spring.step(f32::NAN);
        assert_eq!(spring.pos, 0.0);
        assert_eq!(spring.vel, 0.0);
    }

    #[test]
    fn test_reset() {
        let mut spring = Spring::new(0.0);
        spring.set_target(10.0);
        spring.step(0.016);

        spring.reset(2.0);
        assert!(spring.is_settled());
        assert_eq!(spring.pos, 2.0);
        assert_eq!(spring.target, 2.0);
        assert_eq!(spring.vel, 0.0);
    }
}
