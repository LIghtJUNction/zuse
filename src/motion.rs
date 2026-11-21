//! Deterministic, allocation-free pointer paths and bounded random timing.
//!
//! Sampling is independent of wall-clock time. A runtime schedules each sample
//! against the start instant using `Sample::at_ms`, keeping delays from drifting.

use core::fmt;

pub const MAX_DURATION_MS: u32 = 3_600_000;
pub const MAX_STEPS: u32 = 65_536;
pub const MAX_COORDINATE: f64 = 16_777_216.0;
pub const MAX_JITTER: f64 = 1_000.0;
/// Shared defaults for moving to a target, distinct from drawing geometry.
pub const DEFAULT_MOVE_DURATION_MS: u32 = 350;
pub const DEFAULT_MOVE_JITTER: f64 = 0.6;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Curve {
    Linear,
    /// Cubic smoothstep timing, with zero endpoint velocity.
    #[default]
    Smooth,
    /// Cubic ease-in/ease-out timing.
    EaseInOut,
    /// A curved cubic Bezier path with smoothstep timing.
    Bezier,
}

impl Curve {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "linear" => Some(Self::Linear),
            "smooth" | "smoothstep" => Some(Self::Smooth),
            "ease" | "ease-in-out" | "ease_in_out" => Some(Self::EaseInOut),
            "bezier" => Some(Self::Bezier),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub point: Point,
    /// Absolute offset from the start of this motion, in milliseconds.
    pub at_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionError {
    NonFinite,
    CoordinateRange,
    DurationRange,
    StepsRange,
    JitterRange,
    SpeedRange,
    DelayRange,
}

impl fmt::Display for MotionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid motion: {self:?}")
    }
}
impl core::error::Error for MotionError {}

/// A reproducible SplitMix64 generator. It is not a cryptographic RNG.
#[derive(Clone, Copy, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Uniform inclusive integer range, including the full `u64` domain.
    pub fn range_u64(&mut self, min: u64, max: u64) -> Result<u64, MotionError> {
        if min > max {
            return Err(MotionError::DelayRange);
        }
        let width = max.wrapping_sub(min).wrapping_add(1);
        if width == 0 {
            return Ok(self.next_u64());
        }
        let threshold = width.wrapping_neg() % width;
        loop {
            let sample = self.next_u64();
            if sample >= threshold {
                return Ok(min + sample % width);
            }
        }
    }

    /// A bounded inclusive delay; invalid bounds do not advance the generator.
    pub fn random_delay(&mut self, min_ms: u32, max_ms: u32) -> Result<u32, MotionError> {
        if min_ms > max_ms || max_ms > MAX_DURATION_MS {
            return Err(MotionError::DelayRange);
        }
        self.range_u64(min_ms as u64, max_ms as u64)
            .map(|n| n as u32)
    }

    fn signed_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (2.0 / 9_007_199_254_740_992.0) - 1.0
    }
}

/// A path iterator with `steps + 1` samples, including both exact endpoints.
/// `steps` counts intervals. Nonzero durations require at most one interval per
/// millisecond, so timestamps are strictly increasing. Zero duration permits one
/// interval, representing an immediate move with two zero-time endpoints.
#[derive(Clone, Debug)]
pub struct Motion {
    from: Point,
    to: Point,
    duration_ms: u32,
    steps: u32,
    curve: Curve,
    jitter: f64,
    next: u32,
    rng: Rng,
}

impl Motion {
    pub fn new(
        from: Point,
        to: Point,
        duration_ms: u32,
        steps: u32,
        curve: Curve,
        jitter: f64,
        seed: u64,
    ) -> Result<Self, MotionError> {
        if ![from.x, from.y, to.x, to.y, jitter]
            .iter()
            .all(|n| n.is_finite())
        {
            return Err(MotionError::NonFinite);
        }
        if [from.x, from.y, to.x, to.y]
            .iter()
            .any(|n| *n < -MAX_COORDINATE || *n > MAX_COORDINATE)
        {
            return Err(MotionError::CoordinateRange);
        }
        if duration_ms > MAX_DURATION_MS {
            return Err(MotionError::DurationRange);
        }
        if steps == 0
            || steps > MAX_STEPS
            || (duration_ms == 0 && steps != 1)
            || (duration_ms > 0 && steps > duration_ms)
        {
            return Err(MotionError::StepsRange);
        }
        if !(0.0..=MAX_JITTER).contains(&jitter) {
            return Err(MotionError::JitterRange);
        }
        Ok(Self {
            from,
            to,
            duration_ms,
            steps,
            curve,
            jitter,
            next: 0,
            rng: Rng::new(seed),
        })
    }
}

impl Iterator for Motion {
    type Item = Sample;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next > self.steps {
            return None;
        }
        let step = self.next;
        self.next += 1;
        let at_ms = ((self.duration_ms as u64 * step as u64) / self.steps as u64) as u32;
        if step == 0 {
            return Some(Sample {
                point: self.from,
                at_ms,
            });
        }
        if step == self.steps {
            return Some(Sample {
                point: self.to,
                at_ms,
            });
        }
        let t = step as f64 / self.steps as f64;
        let t = match self.curve {
            Curve::Linear => t,
            Curve::Smooth | Curve::Bezier => t * t * (3.0 - 2.0 * t),
            Curve::EaseInOut if t < 0.5 => 4.0 * t * t * t,
            Curve::EaseInOut => {
                let u = 1.0 - t;
                1.0 - 4.0 * u * u * u
            }
        };
        let dx = self.to.x - self.from.x;
        let dy = self.to.y - self.from.y;
        let mut point = Point {
            x: self.from.x + dx * t,
            y: self.from.y + dy * t,
        };
        if self.curve == Curve::Bezier {
            // Control points at 1/3 and 2/3 of the chord, both displaced by
            // one fifth of its perpendicular, yield this cubic simplification.
            let bow = 0.6 * t * (1.0 - t);
            point.x -= dy * bow;
            point.y += dx * bow;
        }
        if self.jitter > 0.0 {
            let amplitude = self.jitter * 4.0 * t * (1.0 - t);
            point.x += self.rng.signed_unit() * amplitude;
            point.y += self.rng.signed_unit() * amplitude;
        }
        Some(Sample { point, at_ms })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.steps + 1 - self.next) as usize;
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for Motion {}
impl core::iter::FusedIterator for Motion {}

/// Sampling parameters for a caller-defined pointer path. Defaults target ~120 Hz
/// for one second, with smooth acceleration and no variation or jitter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathOptions {
    pub duration_ms: u32,
    pub steps: u32,
    pub easing: Curve,
    pub jitter: f64,
    /// Seeded variation in progress per unit time, in 0..=1.
    pub speed_variation: f64,
    pub seed: u64,
}

impl Default for PathOptions {
    fn default() -> Self {
        Self {
            duration_ms: 1000,
            steps: 120,
            easing: Curve::Smooth,
            jitter: 0.0,
            speed_variation: 0.0,
            seed: 1,
        }
    }
}

/// An allocation-free time sampler of a Rust function from [0, 1] to a point.
/// Each sample has an absolute deadline. The caller owns input delivery,
/// cancellation, and any desktop authorization, independently of the path.
pub struct FunctionalMotion<F> {
    path: F,
    options: PathOptions,
    next: u32,
    speed_bias: f64,
}

impl<F: FnMut(f64) -> Point> FunctionalMotion<F> {
    pub fn new(path: F, options: PathOptions) -> Result<Self, MotionError> {
        let origin = Point::default();
        // Reuse duration, interval and jitter bounds from the segment sampler.
        Motion::new(
            origin,
            origin,
            options.duration_ms,
            options.steps,
            options.easing,
            options.jitter,
            options.seed,
        )?;
        if !options.speed_variation.is_finite() {
            return Err(MotionError::NonFinite);
        }
        if !(0.0..=1.0).contains(&options.speed_variation) {
            return Err(MotionError::SpeedRange);
        }
        let mut rng = Rng::new(options.seed ^ 0xd6e8feb86659fd93);
        let speed_bias = 0.8 * options.speed_variation * rng.signed_unit();
        Ok(Self {
            path,
            options,
            next: 0,
            speed_bias,
        })
    }

    fn smooth(v: f64) -> f64 {
        v * v * (3.0 - 2.0 * v)
    }

    fn noise(&self, index: u32) -> (f64, f64) {
        let mut rng =
            Rng::new(self.options.seed ^ u64::from(index).wrapping_mul(0x9e3779b97f4a7c15));
        (rng.signed_unit(), rng.signed_unit())
    }
}

impl<F: FnMut(f64) -> Point> Iterator for FunctionalMotion<F> {
    type Item = Result<Sample, MotionError>;

    fn next(&mut self) -> Option<Self::Item> {
        let o = self.options;
        let i = self.next;
        if i > o.steps {
            return None;
        }
        self.next += 1;
        let t = f64::from(i) / f64::from(o.steps);
        // The time warp is monotone: its derivative is at least 1 - |bias|.
        let phase = t + self.speed_bias * t * (1.0 - t);
        let u = match o.easing {
            Curve::Linear => phase,
            Curve::Smooth | Curve::Bezier => Self::smooth(phase),
            Curve::EaseInOut if phase < 0.5 => 4.0 * phase * phase * phase,
            Curve::EaseInOut => {
                let p = 1.0 - phase;
                1.0 - 4.0 * p * p * p
            }
        };
        let mut point = (self.path)(u);
        if i > 0 && i < o.steps && o.jitter > 0.0 {
            // Interpolated noise anchors, not a new independent jump each frame.
            let bucket = t * 8.0;
            let index = bucket as u32;
            let alpha = Self::smooth(bucket - f64::from(index));
            let (ax, ay) = self.noise(index);
            let (bx, by) = self.noise(index + 1);
            let amp = o.jitter * 4.0 * t * (1.0 - t);
            point.x += amp * (ax + (bx - ax) * alpha);
            point.y += amp * (ay + (by - ay) * alpha);
        }
        let error = if !point.x.is_finite() || !point.y.is_finite() {
            Some(MotionError::NonFinite)
        } else if point.x.abs() > MAX_COORDINATE || point.y.abs() > MAX_COORDINATE {
            Some(MotionError::CoordinateRange)
        } else {
            None
        };
        if let Some(error) = error {
            self.next = o.steps + 1;
            return Some(Err(error));
        }
        Some(Ok(Sample {
            point,
            at_ms: ((u64::from(o.duration_ms) * u64::from(i)) / u64::from(o.steps)) as u32,
        }))
    }
}

impl<F: FnMut(f64) -> Point> core::iter::FusedIterator for FunctionalMotion<F> {}
