//! A small, reusable spring-physics animator.
//!
//! Springs are integrated with fixed sub-steps (semi-implicit Euler), so the
//! motion is frame-rate independent: a 60 Hz and a 144 Hz display trace the
//! same curve.

/// Physical parameters of a damped harmonic oscillator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
}

impl SpringParams {
    /// Critically damped spring: the fastest motion that never overshoots.
    pub fn critical(stiffness: f32) -> Self {
        let mass = 1.0;
        Self {
            stiffness,
            damping: 2.0 * (stiffness * mass).sqrt(),
            mass,
        }
    }

    /// Under-damped spring that overshoots its target by `overshoot`
    /// (a fraction, e.g. `0.04` for 4%) when released from rest.
    pub fn with_overshoot(stiffness: f32, overshoot: f32) -> Self {
        let mass = 1.0;
        let zeta = damping_ratio_for_overshoot(overshoot);
        Self {
            stiffness,
            damping: 2.0 * zeta * (stiffness * mass).sqrt(),
            mass,
        }
    }

    /// The default UI spring: critically damped and fairly snappy.
    pub fn snappy() -> Self {
        Self::critical(420.0)
    }

    // Calibrated against caelestia's M3 Expressive béziers by sampling them
    // (see `springs_match_the_caelestia_beziers`):
    //   expressiveDefaultSpatial (0.38, 1.21, 0.22, 1), 500 ms: peak
    //     overshoot 1.39% at 280 ms, settles to 2% by 192 ms.
    //   expressiveFastSpatial (0.42, 1.67, 0.21, 0.9), 350 ms: peak
    //     overshoot 9.21% at 135 ms, settles to 2% by 219 ms.
    // A spring's settle time falls as stiffness rises: k = 400 gives 190 ms
    // (peak at 265 ms) and k = 750 gives 217 ms (peak at 144 ms). The
    // overshoot arguments (1.68%, 9.76%) are above the targets (1.39%,
    // 9.21%) because the 1/480 s semi-implicit Euler step damps the peak.

    /// Shell open/close.
    pub fn spatial() -> Self {
        Self::with_overshoot(400.0, 0.0168)
    }

    /// Hover swell, pill width and placement.
    pub fn fast_spatial() -> Self {
        Self::with_overshoot(750.0, 0.0976)
    }

    /// Damping ratio (zeta). 1.0 is critical, below 1.0 overshoots.
    pub fn damping_ratio(&self) -> f32 {
        self.damping / (2.0 * (self.stiffness * self.mass).sqrt())
    }
}

impl Default for SpringParams {
    fn default() -> Self {
        Self::snappy()
    }
}

/// Damping ratio that yields the given peak overshoot for a step response.
pub fn damping_ratio_for_overshoot(overshoot: f32) -> f32 {
    if overshoot <= 0.0 {
        return 1.0;
    }
    let ln = overshoot.min(0.99).ln();
    let pi = std::f32::consts::PI;
    (-ln / (pi * pi + ln * ln).sqrt()).clamp(0.0, 1.0)
}

const SUBSTEP: f32 = 1.0 / 480.0;
const MAX_DT: f32 = 1.0 / 15.0;
const REST_DISTANCE: f32 = 1e-3;
const REST_VELOCITY: f32 = 1e-2;

/// A one-dimensional spring that chases `target`.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub params: SpringParams,
}

impl Spring {
    pub fn new(value: f32, params: SpringParams) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            params,
        }
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Jumps to `value` with no motion.
    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// Advances the simulation by `dt` seconds. Large gaps (the app was idle)
    /// are clamped so a spring never "teleports" through its whole motion in
    /// one hidden frame. Returns `true` while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.is_settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let mut remaining = dt.clamp(0.0, MAX_DT);
        let SpringParams {
            stiffness,
            damping,
            mass,
        } = self.params;
        let mass = mass.max(1e-4);
        while remaining > 0.0 {
            let h = remaining.min(SUBSTEP);
            let force = -stiffness * (self.value - self.target) - damping * self.velocity;
            self.velocity += force / mass * h;
            self.value += self.velocity * h;
            remaining -= h;
        }
        if self.is_settled() {
            self.value = self.target;
            self.velocity = 0.0;
            false
        } else {
            true
        }
    }

    pub fn is_settled(&self) -> bool {
        (self.value - self.target).abs() < REST_DISTANCE && self.velocity.abs() < REST_VELOCITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(spring: &mut Spring, seconds: f32, dt: f32) -> (f32, f32) {
        let mut peak = spring.value;
        let mut t = 0.0;
        while t < seconds {
            spring.step(dt);
            peak = peak.max(spring.value);
            t += dt;
        }
        (spring.value, peak)
    }

    #[test]
    fn critical_spring_settles_without_overshoot() {
        let mut s = Spring::new(0.0, SpringParams::critical(300.0));
        s.set_target(1.0);
        let (end, peak) = run(&mut s, 2.0, 1.0 / 60.0);
        assert!((end - 1.0).abs() < 1e-3, "end = {end}");
        assert!(peak <= 1.0 + 1e-4, "peak = {peak}");
        assert!(s.is_settled());
    }

    #[test]
    fn overshoot_spring_overshoots_about_four_percent() {
        let mut s = Spring::new(0.0, SpringParams::with_overshoot(260.0, 0.04));
        s.set_target(1.0);
        let (end, peak) = run(&mut s, 3.0, 1.0 / 240.0);
        assert!((end - 1.0).abs() < 1e-3);
        assert!((peak - 1.04).abs() < 0.006, "peak = {peak}");
    }

    /// Peak overshoot and the last time the curve is more than 2% off 1,
    /// for a cubic-bezier easing `(x1, y1, x2, y2)` over `secs`.
    fn bezier_stats(c: [f32; 4], secs: f32) -> (f32, f32) {
        let b = |t: f32, a: f32, b: f32| {
            3.0 * (1.0 - t).powi(2) * t * a + 3.0 * (1.0 - t) * t * t * b + t.powi(3)
        };
        let (mut peak, mut settle) = (0.0f32, 0.0f32);
        for i in 0..=20_000 {
            let t = i as f32 / 20_000.0;
            let (x, y) = (b(t, c[0], c[2]), b(t, c[1], c[3]));
            peak = peak.max(y - 1.0);
            if (y - 1.0).abs() > 0.02 {
                settle = x * secs;
            }
        }
        (peak, settle)
    }

    fn spring_stats(params: SpringParams) -> (f32, f32) {
        let mut s = Spring::new(0.0, params);
        s.set_target(1.0);
        let (mut peak, mut settle, mut t) = (0.0f32, 0.0f32, 0.0f32);
        while t < 2.0 {
            s.step(1.0 / 480.0);
            t += 1.0 / 480.0;
            peak = peak.max(s.value - 1.0);
            if (s.value - 1.0).abs() > 0.02 {
                settle = t;
            }
        }
        (peak, settle)
    }

    #[test]
    fn springs_match_the_caelestia_beziers() {
        for (spring, bezier, secs) in [
            (SpringParams::spatial(), [0.38, 1.21, 0.22, 1.0], 0.5),
            (SpringParams::fast_spatial(), [0.42, 1.67, 0.21, 0.9], 0.35),
        ] {
            let (bo, bs) = bezier_stats(bezier, secs);
            let (so, ss) = spring_stats(spring);
            assert!((so - bo).abs() <= 0.0015, "overshoot {so} vs {bo}");
            assert!((ss - bs).abs() < 0.03, "settle {ss} vs {bs}");
        }
    }

    #[test]
    fn frame_rate_independent() {
        let mut a = Spring::new(0.0, SpringParams::spatial());
        let mut b = a;
        a.set_target(1.0);
        b.set_target(1.0);
        run(&mut a, 0.1, 1.0 / 60.0);
        run(&mut b, 0.1, 1.0 / 144.0);
        // Frame boundaries do not line up exactly, so allow a small slack.
        assert!(
            (a.value - b.value).abs() < 0.03,
            "{} vs {}",
            a.value,
            b.value
        );
    }

    #[test]
    fn huge_dt_is_clamped() {
        let mut s = Spring::new(0.0, SpringParams::snappy());
        s.set_target(100.0);
        s.step(10.0);
        assert!(s.value < 100.0);
        assert!(s.value.is_finite());
    }

    #[test]
    fn settled_spring_reports_idle() {
        let mut s = Spring::new(5.0, SpringParams::snappy());
        assert!(!s.step(1.0 / 60.0));
        s.snap(2.0);
        assert!(!s.step(1.0 / 60.0));
        assert_eq!(s.value, 2.0);
    }

    #[test]
    fn damping_ratio_round_trip() {
        assert!((damping_ratio_for_overshoot(0.04) - 0.7156).abs() < 1e-3);
        assert_eq!(damping_ratio_for_overshoot(0.0), 1.0);
        let p = SpringParams::critical(100.0);
        assert!((p.damping_ratio() - 1.0).abs() < 1e-6);
    }
}
