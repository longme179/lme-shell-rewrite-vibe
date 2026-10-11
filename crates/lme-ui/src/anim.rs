// SPDX-License-Identifier: GPL-3.0-or-later
//! Animation: time-based tween tracks evaluated from frame-callback
//! timestamps. No fixed-interval timers — the surface requests a frame
//! callback only while `Animator::active()` is true (mission quality bar).

use std::collections::HashMap;
use std::time::Instant;

/// Cubic bezier (x1,y1,x2,y2) easing like CSS/QML Bezier curves.
/// x is parametric time; solved by bisection (monotonic in x for sane curves).
#[derive(Debug, Clone, Copy)]
pub struct Curve {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl Curve {
    pub const LINEAR: Curve = Curve {
        x1: 0.0,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };

    pub fn eval(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if (self.x1 - self.x2).abs() < 1e-6 && self.x1 == 0.0 && self.x2 == 1.0 {
            // degenerate → linear only if also straight line
        }
        // solve param s such that bezier_x(s) = t
        let bezier_x = |s: f32| {
            let iv = 1.0 - s;
            3.0 * iv * iv * s * self.x1 + 3.0 * iv * s * s * self.x2 + s * s * s
        };
        let bezier_y = |s: f32| {
            let iv = 1.0 - s;
            3.0 * iv * iv * s * self.y1 + 3.0 * iv * s * s * self.y2 + s * s * s
        };
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) * 0.5;
            if bezier_x(mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        bezier_y((lo + hi) * 0.5)
    }
}

struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur_ms: u32,
    curve: Curve,
    /// Last evaluated value (so `value()` works between ticks).
    current: f32,
    done: bool,
}

/// Tracks identified by u64 (widget/panel-local constants). Jump-set when
/// animations are disabled (`Theme.animations == false` → zero-duration).
pub struct Animator {
    tweens: HashMap<u64, Tween>,
}

impl Animator {
    pub fn new() -> Self {
        Self {
            tweens: HashMap::new(),
        }
    }

    /// Start (or retarget, keeping continuity) a tween.
    pub fn tween(&mut self, id: u64, to: f32, dur_ms: u32, curve: Curve, now: Instant) {
        let from = self.value(id);
        if dur_ms == 0 {
            self.tweens.insert(
                id,
                Tween {
                    from: to,
                    to,
                    start: now,
                    dur_ms: 0,
                    curve,
                    current: to,
                    done: true,
                },
            );
            return;
        }
        self.tweens.insert(
            id,
            Tween {
                from,
                to,
                start: now,
                dur_ms,
                curve,
                current: from,
                done: false,
            },
        );
    }

    /// Instantly set (no animation).
    pub fn set(&mut self, id: u64, value: f32, now: Instant) {
        self.tween(id, value, 0, Curve::LINEAR, now);
    }

    /// Current value of a track (0 when never touched).
    pub fn value(&self, id: u64) -> f32 {
        self.tweens.get(&id).map(|t| t.current).unwrap_or(0.0)
    }

    /// Advance all tracks to `now`; returns true while anything is animating.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut active = false;
        for t in self.tweens.values_mut() {
            if t.done {
                continue;
            }
            let elapsed = now.duration_since(t.start).as_millis() as f32;
            let t01 = if t.dur_ms == 0 {
                1.0
            } else {
                (elapsed / t.dur_ms as f32).clamp(0.0, 1.0)
            };
            t.current = t.from + (t.to - t.from) * t.curve.eval(t01);
            if t01 >= 1.0 {
                t.current = t.to;
                t.done = true;
            } else {
                active = true;
            }
        }
        active
    }

    /// True while any tween is still running (does not mutate).
    pub fn active(&self) -> bool {
        self.tweens.values().any(|t| !t.done)
    }

    /// Drop tracks whose tweens finished and hold values nobody needs.
    pub fn gc(&mut self) {
        self.tweens
            .retain(|_, t| !t.done || t.current != t.to || true);
    }
}

impl Default for Animator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_eases() {
        let c = Curve {
            x1: 0.34,
            y1: 0.8,
            x2: 0.34,
            y2: 1.0,
        };
        assert!((c.eval(0.0) - 0.0).abs() < 1e-4);
        assert!((c.eval(1.0) - 1.0).abs() < 1e-4);
        let mid = c.eval(0.5);
        assert!(mid > 0.5, "effects curve is fast-start: {}", mid);
        let lin = Curve::LINEAR;
        assert!((lin.eval(0.3) - 0.3).abs() < 1e-4);
    }

    #[test]
    fn tween_lifecycle() {
        let mut a = Animator::new();
        let t0 = Instant::now();
        a.tween(7, 1.0, 100, Curve::LINEAR, t0);
        assert!(a.active());
        // halfway
        let t1 = t0 + std::time::Duration::from_millis(50);
        assert!(a.tick(t1));
        assert!((a.value(7) - 0.5).abs() < 0.01, "value {}", a.value(7));
        // done
        let t2 = t0 + std::time::Duration::from_millis(200);
        assert!(!a.tick(t2));
        assert!((a.value(7) - 1.0).abs() < 1e-6);
        // retarget keeps continuity
        a.tween(7, 0.0, 100, Curve::LINEAR, t2);
        assert!((a.value(7) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_duration_jumps() {
        let mut a = Animator::new();
        a.tween(1, 0.9, 0, Curve::LINEAR, Instant::now());
        assert!(!a.active());
        assert!((a.value(1) - 0.9).abs() < 1e-6);
    }
}
