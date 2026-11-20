//! Fully validated pointer plans for the host runtime and embedded callers.
//!
//! Planning does not read or move the pointer. Coordinates use the selected
//! desktop backend's screen space. A runtime supplies the current position,
//! resolves a target, and submits the resulting samples against one start time.

use crate::motion::{
    Curve, FunctionalMotion, MAX_COORDINATE, MAX_JITTER, PathOptions, Point, Sample,
};

pub const MAX_MOVE_DURATION_MS: u32 = 60_000;
pub const MAX_MOVE_STEPS: u32 = 4096;
pub const MAX_VIA_POINTS: usize = 64;

/// Options shared by coordinate paths and text/button target movement.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveOptions {
    /// Movement time in milliseconds. Exclusive with `speed`; defaults to 350.
    pub duration_ms: Option<u32>,
    /// Average speed in screen-coordinate units per second, measured over the
    /// final sampled path, including its curvature and jitter.
    pub speed: Option<f64>,
    /// Number of sampling intervals; the plan contains `steps + 1` samples.
    /// Defaults to approximately 125 Hz for duration, or 120 intervals for speed.
    pub steps: Option<u32>,
    pub curve: Curve,
    /// Maximum per-axis displacement during the path; zero at both endpoints.
    pub jitter: f64,
    /// Seeded monotone variation in path progress, in 0..=1.
    pub speed_variation: f64,
    pub seed: u64,
    /// Added to the resolved target center, in screen coordinates.
    pub offset: Point,
    /// Intermediate absolute screen coordinates. The path is sampled at the
    /// configured resolution; jitter can displace intermediate samples.
    pub via: Vec<Point>,
}

impl Default for MoveOptions {
    fn default() -> Self {
        Self {
            duration_ms: None,
            speed: None,
            steps: None,
            curve: Curve::Smooth,
            jitter: 0.0,
            speed_variation: 0.0,
            seed: 1,
            offset: Point::default(),
            via: Vec::new(),
        }
    }
}

impl MoveOptions {
    /// Check options before any target lookup or desktop input.
    pub fn validate(&self) -> Result<(), String> {
        if self.duration_ms.is_some() && self.speed.is_some() {
            return Err("pointer movement accepts either duration or speed".into());
        }
        if self.duration_ms.is_some_and(|n| n > MAX_MOVE_DURATION_MS) {
            return Err("pointer duration must be 0..60000 milliseconds".into());
        }
        if self.speed.is_some_and(|n| !n.is_finite() || n <= 0.0) {
            return Err("pointer speed must be finite and greater than zero".into());
        }
        let steps = self.sample_steps();
        if !(1..=MAX_MOVE_STEPS).contains(&steps) {
            return Err("pointer steps must be 1..4096".into());
        }
        if self.speed.is_none() {
            let duration = self.scheduled_duration_ms();
            if (duration == 0 && steps != 1) || (duration > 0 && steps > duration) {
                return Err("pointer steps must fit duration in milliseconds".into());
            }
        }
        if !self.jitter.is_finite() || !(0.0..=MAX_JITTER).contains(&self.jitter) {
            return Err("pointer jitter must be finite and in 0..1000".into());
        }
        if !self.speed_variation.is_finite() || !(0.0..=1.0).contains(&self.speed_variation) {
            return Err("pointer speed_variation must be finite and in 0..1".into());
        }
        validate_point(self.offset, "offset")?;
        if self.via.len() > MAX_VIA_POINTS {
            return Err("pointer via accepts at most 64 points".into());
        }
        for point in &self.via {
            validate_point(*point, "via point")?;
        }
        Ok(())
    }

    /// Static contribution to a program's scheduled-wait budget. Speed-based
    /// movement is charged after target resolution and complete path sampling.
    pub fn scheduled_duration_ms(&self) -> u32 {
        if self.speed.is_some() {
            0
        } else {
            self.duration_ms.unwrap_or(350)
        }
    }

    fn sample_steps(&self) -> u32 {
        self.steps.unwrap_or_else(|| {
            if self.speed.is_some() {
                120
            } else {
                (self.scheduled_duration_ms() / 8).clamp(1, MAX_MOVE_STEPS)
            }
        })
    }
}

/// An immutable, finite path with exact start and end points.
///
/// Times are offsets from one monotonic start instant. Duration-based paths
/// have strictly increasing times except an immediate two-endpoint move.
/// Speed-based paths can have several samples in one millisecond when moving
/// quickly; their times never decrease.
#[derive(Clone, Debug, PartialEq)]
pub struct MovePlan {
    from: Point,
    to: Point,
    samples: Vec<Sample>,
    duration_ms: u32,
    length: f64,
}

impl MovePlan {
    /// Plan a line or piecewise path from the current position, through `via`,
    /// to the target plus offset. Bezier curves bow each segment by the same
    /// rule as `motion::Motion`; other curves affect timing only.
    pub fn new(from: Point, target: Point, options: &MoveOptions) -> Result<Self, String> {
        options.validate()?;
        let to = endpoint(from, target, options.offset)?;
        let mut points = Vec::with_capacity(options.via.len() + 2);
        points.push(from);
        points.extend_from_slice(&options.via);
        points.push(to);
        let mut cumulative = Vec::with_capacity(points.len());
        cumulative.push(0.0);
        for pair in points.windows(2) {
            cumulative.push(cumulative.last().copied().unwrap_or(0.0) + distance(pair[0], pair[1]));
        }
        let total = cumulative.last().copied().unwrap_or(0.0);
        Self::sample(from, to, options, |u| {
            if u <= 0.0 || total == 0.0 {
                return from;
            }
            if u >= 1.0 {
                return to;
            }
            let position = u * total;
            for i in 0..points.len() - 1 {
                let length = cumulative[i + 1] - cumulative[i];
                if length == 0.0 || position > cumulative[i + 1] {
                    continue;
                }
                let t = ((position - cumulative[i]) / length).clamp(0.0, 1.0);
                let a = points[i];
                let b = points[i + 1];
                let dx = b.x - a.x;
                let dy = b.y - a.y;
                let mut point = Point {
                    x: a.x + dx * t,
                    y: a.y + dy * t,
                };
                if options.curve == Curve::Bezier {
                    let bow = 0.6 * t * (1.0 - t);
                    point.x -= dy * bow;
                    point.y += dx * bow;
                }
                return point;
            }
            to
        })
    }

    /// Sample a programmatic path. The callback receives eased progress in
    /// 0..=1, the actual start, and the target with offset already applied.
    /// It must return those exact endpoints at 0 and 1. It is called once per
    /// sample, and all samples are validated before a plan is returned.
    /// `via` is exclusive with a callback because the callback owns geometry.
    pub fn with_path<F>(
        from: Point,
        target: Point,
        options: &MoveOptions,
        mut path: F,
    ) -> Result<Self, String>
    where
        F: FnMut(f64, Point, Point) -> Point,
    {
        options.validate()?;
        if !options.via.is_empty() {
            return Err("pointer callback path cannot be combined with via".into());
        }
        let to = endpoint(from, target, options.offset)?;
        Self::sample(from, to, options, |u| path(u, from, to))
    }

    fn sample<F>(from: Point, to: Point, options: &MoveOptions, path: F) -> Result<Self, String>
    where
        F: FnMut(f64) -> Point,
    {
        let steps = options.sample_steps();
        let duration = if options.speed.is_some() {
            // Geometry is sampled once. The final duration is derived from its
            // length and does not rerun a stateful caller callback.
            steps
        } else {
            options.scheduled_duration_ms()
        };
        let mut samples = FunctionalMotion::new(
            path,
            PathOptions {
                duration_ms: duration,
                steps,
                easing: options.curve,
                jitter: options.jitter,
                speed_variation: options.speed_variation,
                seed: options.seed,
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
        if samples.first().map(|s| s.point) != Some(from)
            || samples.last().map(|s| s.point) != Some(to)
        {
            return Err("pointer path must preserve the exact start and target endpoints".into());
        }
        let length = samples
            .windows(2)
            .map(|pair| distance(pair[0].point, pair[1].point))
            .sum::<f64>();
        let duration_ms = if let Some(speed) = options.speed {
            let ms = if length == 0.0 {
                0.0
            } else {
                (length / speed * 1000.0).ceil().max(1.0)
            };
            if !ms.is_finite() || ms > f64::from(MAX_MOVE_DURATION_MS) {
                return Err("pointer path at this speed exceeds 60000 milliseconds".into());
            }
            let ms = ms as u32;
            for (i, sample) in samples.iter_mut().enumerate() {
                sample.at_ms = ((u64::from(ms) * i as u64) / u64::from(steps)) as u32;
            }
            ms
        } else {
            duration
        };
        Ok(Self {
            from,
            to,
            samples,
            duration_ms,
            length,
        })
    }

    pub fn from(&self) -> Point {
        self.from
    }

    pub fn to(&self) -> Point {
        self.to
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }

    /// Length of the sampled polyline, before the input backend rounds points
    /// to its integer coordinate units.
    pub fn length(&self) -> f64 {
        self.length
    }
}

fn endpoint(from: Point, target: Point, offset: Point) -> Result<Point, String> {
    validate_point(from, "start")?;
    validate_point(target, "target")?;
    let to = Point {
        x: target.x + offset.x,
        y: target.y + offset.y,
    };
    validate_point(to, "target plus offset")?;
    Ok(to)
}

fn validate_point(point: Point, name: &str) -> Result<(), String> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return Err(format!("pointer {name} must have finite coordinates"));
    }
    if point.x.abs() > MAX_COORDINATE || point.y.abs() > MAX_COORDINATE {
        return Err(format!("pointer {name} exceeds the coordinate range"));
    }
    Ok(())
}

fn distance(a: Point, b: Point) -> f64 {
    (b.x - a.x).hypot(b.y - a.y)
}
