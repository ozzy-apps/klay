//! Brush › Tablet Pressure curves (ZBrush): how pen pressure maps to Draw Size and Z / RGB
//! Intensity. A curve runs from (0, 0) to (1, 1) through points you place, smooth and never
//! turning back down (monotone cubic interpolation), so more pressure never gives less.

use glam::Vec2;

#[derive(Clone, Debug, PartialEq)]
pub struct PressureCurve {
    /// Points sorted by x; the first is at x = 0 and the last at x = 1.
    pub points: Vec<Vec2>,
}

impl Default for PressureCurve {
    fn default() -> Self {
        Self::linear()
    }
}

impl PressureCurve {
    /// ZBrush's default: output equals pressure.
    pub fn linear() -> Self {
        Self { points: vec![Vec2::ZERO, Vec2::ONE] }
    }

    /// The presets the Tablet Pressure section offers.
    pub fn preset(name: &str) -> Self {
        let p = |pts: &[(f32, f32)]| Self { points: pts.iter().map(|&(x, y)| Vec2::new(x, y)).collect() };
        match name {
            // Light strokes stay light: most of the range needs a firm press.
            "Soft" => p(&[(0.0, 0.0), (0.5, 0.22), (1.0, 1.0)]),
            // Full effect comes early.
            "Firm" => p(&[(0.0, 0.0), (0.5, 0.78), (1.0, 1.0)]),
            // Gentle at both ends, steep in the middle.
            "S" => p(&[(0.0, 0.0), (0.3, 0.1), (0.7, 0.9), (1.0, 1.0)]),
            _ => Self::linear(),
        }
    }

    pub const PRESETS: [&'static str; 4] = ["Linear", "Soft", "Firm", "S"];

    /// Output for pressure `p` (0..=1), in 0..=1.
    pub fn eval(&self, p: f32) -> f32 {
        let pts = &self.points;
        let x = p.clamp(0.0, 1.0);
        if pts.len() < 2 {
            return x;
        }
        let k = pts.windows(2).position(|w| x <= w[1].x).unwrap_or(pts.len() - 2);
        let (a, b) = (pts[k], pts[k + 1]);
        let h = (b.x - a.x).max(1e-6);
        let t = ((x - a.x) / h).clamp(0.0, 1.0);
        let (ma, mb) = (self.slope(k), self.slope(k + 1));
        // Cubic Hermite with Fritsch–Carlson slopes: monotone between monotone points.
        let (t2, t3) = (t * t, t * t * t);
        let y = (2.0 * t3 - 3.0 * t2 + 1.0) * a.y + (t3 - 2.0 * t2 + t) * h * ma + (-2.0 * t3 + 3.0 * t2) * b.y + (t3 - t2) * h * mb;
        y.clamp(0.0, 1.0)
    }

    /// Fritsch–Carlson tangent at point `i`.
    fn slope(&self, i: usize) -> f32 {
        let pts = &self.points;
        let secant = |j: usize| (pts[j + 1].y - pts[j].y) / (pts[j + 1].x - pts[j].x).max(1e-6);
        let n = pts.len();
        if i == 0 {
            return secant(0);
        }
        if i == n - 1 {
            return secant(n - 2);
        }
        let (d0, d1) = (secant(i - 1), secant(i));
        if d0 * d1 <= 0.0 {
            return 0.0;
        }
        // Harmonic mean keeps the curve from overshooting.
        2.0 / (1.0 / d0 + 1.0 / d1)
    }

    /// Put a point at `at` (inside 0..1, x strictly between its neighbours); returns its
    /// index, or `None` if too close to another.
    pub fn insert(&mut self, at: Vec2) -> Option<usize> {
        let at = at.clamp(Vec2::ZERO, Vec2::ONE);
        if self.points.iter().any(|p| (p.x - at.x).abs() < 0.03) {
            return None;
        }
        let i = self.points.iter().position(|p| p.x > at.x)?;
        self.points.insert(i, at);
        Some(i)
    }

    /// Move point `i` to `to`: the end points keep their x, the others stay between their
    /// neighbours.
    pub fn drag(&mut self, i: usize, to: Vec2) {
        let n = self.points.len();
        let y = to.y.clamp(0.0, 1.0);
        let x = if i == 0 {
            0.0
        } else if i == n - 1 {
            1.0
        } else {
            to.x.clamp(self.points[i - 1].x + 0.02, self.points[i + 1].x - 0.02)
        };
        self.points[i] = Vec2::new(x, y);
    }

    /// Remove point `i` unless it is an end point.
    pub fn remove(&mut self, i: usize) {
        if i > 0 && i + 1 < self.points.len() {
            self.points.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_the_identity_and_presets_bend_it() {
        let lin = PressureCurve::linear();
        for p in [0.0, 0.2, 0.5, 0.93, 1.0] {
            assert!((lin.eval(p) - p).abs() < 1e-5);
        }
        assert!(PressureCurve::preset("Soft").eval(0.5) < 0.3);
        assert!(PressureCurve::preset("Firm").eval(0.5) > 0.7);
        let s = PressureCurve::preset("S");
        assert!(s.eval(0.15) < 0.15 && s.eval(0.85) > 0.85);
        for name in PressureCurve::PRESETS {
            let c = PressureCurve::preset(name);
            assert_eq!((c.eval(0.0), c.eval(1.0)), (0.0, 1.0), "{name}");
            // Never decreasing.
            let ys: Vec<f32> = (0..=100).map(|k| c.eval(k as f32 / 100.0)).collect();
            assert!(ys.windows(2).all(|w| w[1] >= w[0] - 1e-6), "{name}");
        }
    }

    #[test]
    fn editing_points_keeps_the_curve_well_formed() {
        let mut c = PressureCurve::linear();
        let i = c.insert(Vec2::new(0.4, 0.8)).unwrap();
        assert_eq!(i, 1);
        assert!(c.insert(Vec2::new(0.41, 0.2)).is_none(), "too close");
        c.drag(1, Vec2::new(1.5, -1.0));
        assert!(c.points[1].x < 1.0 && c.points[1].y == 0.0);
        c.drag(0, Vec2::new(0.5, 0.3));
        assert_eq!(c.points[0], Vec2::new(0.0, 0.3));
        c.remove(0);
        c.remove(1);
        assert_eq!(c.points.len(), 2);
    }
}
