//! Pure spring physics simulation with exact closed-form analytic solutions.
//!
//! Replaces numerical Euler integration with exact analytical solutions for
//! underdamped (zeta < 1), critically damped (zeta == 1), and overdamped (zeta > 1) systems.
//! Guarantees absolute frame-rate independence, energy conservation, and C1 continuity.

/// Default spring stiffness (k).
#[allow(dead_code)] // ADR 001 default parameter
pub const DEFAULT_K: f32 = 380.0;

/// Default damping ratio (zeta).
#[allow(dead_code)] // ADR 001 default parameter
pub const DEFAULT_ZETA: f32 = 0.68;

/// Maximum integration timestep (seconds) per step to prevent visual jumps after OS stall.
pub const MAX_DT: f32 = 0.10;

/// Position settling threshold: 0.25 physical pixels across motion range.
pub const SETTLE_POS_PX: f32 = 0.25;

/// Velocity settling threshold: 15.0 physical pixels/second across motion range.
pub const SETTLE_VEL_PX_PER_SEC: f32 = 15.0;

/// Computes the exact closed-form solution of a damped harmonic oscillator over timestep `dt`.
#[must_use]
pub fn solve_exact(pos: f32, vel: f32, target: f32, k: f32, zeta: f32, dt: f32) -> (f32, f32) {
    if dt <= 0.0 || dt.is_nan() {
        return (pos, vel);
    }

    let safe_k = if k.is_nan() || k < 1.0 { 1.0 } else { k };
    let safe_zeta = if zeta.is_nan() || zeta < 0.0 {
        0.0
    } else {
        zeta
    };

    let w = safe_k.sqrt();
    let x0_rel = pos - target;
    let v0 = vel;

    if safe_zeta < 0.999_99 {
        // Underdamped (zeta < 1)
        let wd = w * (1.0 - safe_zeta * safe_zeta).sqrt();
        let a = x0_rel;
        let b = (v0 + safe_zeta * w * x0_rel) / wd;
        let decay = (-safe_zeta * w * dt).exp();
        let cos_val = (wd * dt).cos();
        let sin_val = (wd * dt).sin();

        let x_rel = decay * (a * cos_val + b * sin_val);
        let v_rel = decay
            * ((b * wd - safe_zeta * w * a) * cos_val - (a * wd + safe_zeta * w * b) * sin_val);

        (target + x_rel, v_rel)
    } else if safe_zeta <= 1.000_01 {
        // Critically damped (zeta == 1)
        let a = x0_rel;
        let b = v0 + w * x0_rel;
        let decay = (-w * dt).exp();

        let x_rel = decay * (a + b * dt);
        let v_rel = decay * ((b - w * a) - w * b * dt);

        (target + x_rel, v_rel)
    } else {
        // Overdamped (zeta > 1)
        let wd = w * (safe_zeta * safe_zeta - 1.0).sqrt();
        let r1 = -safe_zeta * w + wd;
        let r2 = -safe_zeta * w - wd;

        let c1 = (v0 - x0_rel * r2) / (2.0 * wd);
        let c2 = (x0_rel * r1 - v0) / (2.0 * wd);

        let e1 = (r1 * dt).exp();
        let e2 = (r2 * dt).exp();

        let x_rel = c1 * e1 + c2 * e2;
        let v_rel = c1 * r1 * e1 + c2 * r2 * e2;

        (target + x_rel, v_rel)
    }
}

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
    pub settled: bool,
}

impl Spring {
    /// Creates a new spring initialized at `initial_pos` with default stiffness and damping.
    #[must_use]
    #[allow(dead_code)] // Fase 1/2 API
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

    /// Sets a new target position while strictly preserving current `pos` and `vel`.
    pub fn set_target(&mut self, target: f32) {
        if (self.target - target).abs() > 1e-7 {
            self.target = target;
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

    /// Returns true if the spring satisfies pixel-based resting criteria:
    /// |err| * range_px <= 0.25 px AND |vel| * range_px <= 15.0 px/s.
    #[must_use]
    pub fn is_settled(&self, range_px: f32) -> bool {
        if self.settled {
            return true;
        }
        let err = (self.target - self.pos).abs();
        let vel = self.vel.abs();
        let safe_range = if range_px.is_nan() || range_px <= 0.0 {
            1.0
        } else {
            range_px
        };
        err * safe_range <= SETTLE_POS_PX && vel * safe_range <= SETTLE_VEL_PX_PER_SEC
    }

    /// Advances the spring simulation by `dt` seconds using the exact closed-form analytic solution.
    ///
    /// Clamps `dt` to `[0.0, MAX_DT]` to maintain stability across frame hitches.
    /// Returns `true` if settled, `false` otherwise.
    pub fn step(&mut self, dt: f32, range_px: f32) -> bool {
        if self.settled {
            return true;
        }

        let dt = if dt.is_nan() || dt <= 0.0 {
            0.0
        } else {
            dt.min(MAX_DT)
        };

        if dt == 0.0 {
            return self.settled;
        }

        let (new_pos, new_vel) =
            solve_exact(self.pos, self.vel, self.target, self.k, self.zeta, dt);
        self.pos = new_pos;
        self.vel = new_vel;

        if self.is_settled(range_px) {
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
        assert!(spring.is_settled(200.0));
        assert_eq!(spring.pos, 0.0);
        assert_eq!(spring.target, 0.0);
        assert_eq!(spring.vel, 0.0);
    }

    #[test]
    fn test_set_target_preserves_continuity() {
        let mut spring = Spring::new(0.0);
        spring.set_target(1.0);
        spring.step(0.05, 200.0);

        let pos_before = spring.pos;
        let vel_before = spring.vel;

        // Retarget mid-motion
        spring.set_target(0.0);

        assert_eq!(
            spring.pos, pos_before,
            "Position must be strictly continuous after set_target"
        );
        assert_eq!(
            spring.vel, vel_before,
            "Velocity must be strictly continuous after set_target"
        );
        assert!(!spring.settled);
    }

    #[test]
    fn test_framerate_independence() {
        // Simulate 0.5s from 0.0 to 1.0 with various framerates
        let run_sim = |step_sizes: &[f32]| -> (f32, f32) {
            let mut s = Spring::with_params(0.0, 360.0, 0.68);
            s.set_target(1.0);
            for &dt in step_sizes {
                let (p, v) = solve_exact(s.pos, s.vel, s.target, s.k, s.zeta, dt);
                s.pos = p;
                s.vel = v;
            }
            (s.pos, s.vel)
        };

        let steps_30 = vec![1.0 / 30.0; 15]; // 0.5s
        let steps_60 = vec![1.0 / 60.0; 30]; // 0.5s
        let steps_144 = vec![0.5 / 72.0; 72]; // 0.5s
        let steps_240 = vec![0.5 / 120.0; 120]; // 0.5s

        // Variable jitter timesteps (8ms to 31ms) summing to 0.5s
        let jitter = vec![
            0.016, 0.008, 0.024, 0.031, 0.012, 0.018, 0.022, 0.009, 0.015, 0.027, 0.019, 0.021,
            0.014, 0.011, 0.025, 0.028, 0.013, 0.017, 0.023, 0.010, 0.026, 0.030, 0.020, 0.021,
            0.040,
        ]; // sum = 0.500s

        let (p30, v30) = run_sim(&steps_30);
        let (p60, v60) = run_sim(&steps_60);
        let (p144, _v144) = run_sim(&steps_144);
        let (p240, _v240) = run_sim(&steps_240);
        let (pjitter, vjitter) = run_sim(&jitter);

        assert!(
            (p30 - p60).abs() < 1e-4,
            "30fps vs 60fps pos error too large: {p30} vs {p60}"
        );
        assert!(
            (p60 - p144).abs() < 1e-4,
            "60fps vs 144fps pos error too large: {p60} vs {p144}"
        );
        assert!(
            (p144 - p240).abs() < 1e-4,
            "144fps vs 240fps pos error too large: {p144} vs {p240}"
        );
        assert!(
            (p60 - pjitter).abs() < 1e-4,
            "60fps vs jitter pos error too large: {p60} vs {pjitter}"
        );

        assert!(
            (v30 - v60).abs() < 1e-4,
            "30fps vs 60fps vel error too large"
        );
        assert!(
            (v60 - vjitter).abs() < 1e-4,
            "60fps vs jitter vel error too large"
        );
    }

    #[test]
    fn test_semigroup_property() {
        let dt_a = 0.017;
        let dt_b = 0.029;
        let dt_sum = dt_a + dt_b;

        let k = 360.0;
        let zeta = 0.68;
        let pos0 = 0.2;
        let vel0 = 1.5;
        let target = 1.0;

        // Two sequential steps
        let (pos_a, vel_a) = solve_exact(pos0, vel0, target, k, zeta, dt_a);
        let (pos_ab, vel_ab) = solve_exact(pos_a, vel_a, target, k, zeta, dt_b);

        // One single step
        let (pos_sum, vel_sum) = solve_exact(pos0, vel0, target, k, zeta, dt_sum);

        assert!(
            (pos_ab - pos_sum).abs() < 1e-5,
            "Semigroup property violated for pos: {pos_ab} vs {pos_sum}"
        );
        assert!(
            (vel_ab - vel_sum).abs() < 1e-5,
            "Semigroup property violated for vel: {vel_ab} vs {vel_sum}"
        );
    }

    #[test]
    fn test_overshoot_matches_theory() {
        let test_zeta = |zeta: f32| {
            let k = 380.0_f32;
            let w = k.sqrt();
            let wd = w * (1.0 - zeta * zeta).sqrt();
            let t_peak = std::f32::consts::PI / wd;

            // Theoretical overshoot fraction: exp(-pi * zeta / sqrt(1 - zeta^2))
            let theoretical_overshoot =
                (-std::f32::consts::PI * zeta / (1.0 - zeta * zeta).sqrt()).exp();

            let (pos_peak, vel_peak) = solve_exact(0.0, 0.0, 1.0, k, zeta, t_peak);
            let measured_overshoot = pos_peak - 1.0;

            let rel_error =
                ((measured_overshoot - theoretical_overshoot) / theoretical_overshoot).abs();
            assert!(
                rel_error < 0.005,
                "Overshoot for zeta={zeta} deviated from theory: measured={measured_overshoot}, theory={theoretical_overshoot}, rel_err={rel_error}"
            );
            assert!(
                vel_peak.abs() < 1e-4,
                "Velocity at peak must be 0, got {vel_peak}"
            );
        };

        test_zeta(0.68);
        test_zeta(0.75);
        test_zeta(0.92);
    }

    #[test]
    fn test_convergence_and_boundary_conditions() {
        // dt = 0
        let (p0, v0) = solve_exact(0.5, 2.0, 1.0, 380.0, 0.68, 0.0);
        assert_eq!(p0, 0.5);
        assert_eq!(v0, 2.0);

        // negative dt and NaN dt
        let (pn, vn) = solve_exact(0.5, 2.0, 1.0, 380.0, 0.68, -0.016);
        assert_eq!(pn, 0.5);
        assert_eq!(vn, 2.0);

        let (pnan, vnan) = solve_exact(0.5, 2.0, 1.0, 380.0, 0.68, f32::NAN);
        assert_eq!(pnan, 0.5);
        assert_eq!(vnan, 2.0);

        // Critical damping (zeta = 1.0)
        let (pcrit, vcrit) = solve_exact(0.0, 0.0, 1.0, 400.0, 1.0, 0.5);
        assert!(pcrit.is_finite());
        assert!(vcrit.is_finite());
        assert!(pcrit > 0.0 && pcrit <= 1.0);

        // Overdamping (zeta = 1.5)
        let (pover, vover) = solve_exact(0.0, 0.0, 1.0, 400.0, 1.5, 0.5);
        assert!(pover.is_finite());
        assert!(vover.is_finite());
        assert!(pover > 0.0 && pover <= 1.0);

        // Very large dt: must converge to target with zero velocity
        let (plarge, vlarge) = solve_exact(0.0, 5.0, 1.0, 380.0, 0.68, 10.0);
        assert!((plarge - 1.0).abs() < 1e-5);
        assert!(vlarge.abs() < 1e-5);
    }
}
