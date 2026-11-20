//! Pure, bounded touchpad gesture plans and explicit native capability checks.
//!
//! No native gesture backend is implemented. Planning is available on every
//! std target, but execution/cancellation return an unsupported-capability
//! error. These APIs never substitute pointer movement or mouse-wheel input.
//! Swipe deltas are abstract gesture coordinates; a future native backend must
//! document its coordinate mapping and report support before accepting input.

pub const MAX_DURATION_MS: u32 = 60_000;
pub const MAX_STEPS: u32 = 4096;
pub const MAX_PROGRAM_SAMPLES: usize = 65_536;
pub const MAX_DELTA: f64 = 1_000_000.0;
pub const MAX_ROTATION_DEGREES: f64 = 3600.0;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    Swipe {
        dx: f64,
        dy: f64,
    },
    /// Total multiplicative scale relative to the gesture's initial size.
    Pinch {
        scale: f64,
    },
    Rotate {
        degrees: f64,
    },
}

impl Gesture {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Swipe { .. } => "swipe",
            Self::Pinch { .. } => "pinch",
            Self::Rotate { .. } => "rotate",
        }
    }

    pub fn validate(self) -> Result<()> {
        match self {
            Self::Swipe { dx, dy }
                if !dx.is_finite()
                    || !dy.is_finite()
                    || dx.abs() > MAX_DELTA
                    || dy.abs() > MAX_DELTA =>
            {
                Err("touchpad swipe dx/dy must be finite and in -1000000..1000000".into())
            }
            Self::Pinch { scale } if !scale.is_finite() || !(0.01..=100.0).contains(&scale) => {
                Err("touchpad pinch scale must be finite and in 0.01..100".into())
            }
            Self::Rotate { degrees }
                if !degrees.is_finite() || degrees.abs() > MAX_ROTATION_DEGREES =>
            {
                Err("touchpad rotation degrees must be finite and in -3600..3600".into())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GestureOptions {
    /// Planning supports 2..5 fingers; this does not assert hardware support.
    pub fingers: u8,
    pub duration_ms: u32,
    /// Update intervals. Defaults to approximately 125 Hz, within hard limits.
    pub steps: Option<u32>,
}

impl Default for GestureOptions {
    fn default() -> Self {
        Self {
            fingers: 2,
            duration_ms: 350,
            steps: None,
        }
    }
}

impl GestureOptions {
    pub fn validate(self) -> Result<()> {
        if !(2..=5).contains(&self.fingers) {
            return Err("touchpad fingers must be in 2..5".into());
        }
        if self.duration_ms > MAX_DURATION_MS {
            return Err("touchpad duration must be in 0..60000 milliseconds".into());
        }
        let steps = self.sample_steps();
        if !(1..=MAX_STEPS).contains(&steps) {
            return Err("touchpad steps must be in 1..4096".into());
        }
        if (self.duration_ms == 0 && steps != 1)
            || (self.duration_ms > 0 && steps > self.duration_ms)
        {
            return Err("touchpad steps must fit duration in milliseconds".into());
        }
        Ok(())
    }

    pub fn sample_steps(self) -> u32 {
        self.steps
            .unwrap_or_else(|| (self.duration_ms / 8).clamp(1, MAX_STEPS))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GesturePhase {
    Begin,
    Update,
    End,
    Cancel,
}

/// One protocol-neutral gesture event. Update translations/angles add; update
/// scales multiply. Begin/End/Cancel have zero deltas and scale 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GestureSample {
    pub phase: GesturePhase,
    pub at_ms: u32,
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
    pub rotation_degrees: f64,
}

impl GestureSample {
    fn stationary(phase: GesturePhase, at_ms: u32) -> Self {
        Self {
            phase,
            at_ms,
            dx: 0.0,
            dy: 0.0,
            scale: 1.0,
            rotation_degrees: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GesturePlan {
    gesture: Gesture,
    options: GestureOptions,
    samples: Vec<GestureSample>,
}

impl GesturePlan {
    /// Build a complete finite plan without observing or changing any device.
    pub fn new(gesture: Gesture, options: GestureOptions) -> Result<Self> {
        gesture.validate()?;
        options.validate()?;
        let steps = options.sample_steps();
        let mut samples = Vec::with_capacity(steps as usize + 2);
        samples.push(GestureSample::stationary(GesturePhase::Begin, 0));
        let (mut x, mut y, mut scale, mut degrees) = (0.0, 0.0, 1.0, 0.0);
        for step in 1..=steps {
            let t = f64::from(step) / f64::from(steps);
            let progress = t * t * (3.0 - 2.0 * t);
            let at_ms =
                (u64::from(options.duration_ms) * u64::from(step) / u64::from(steps)) as u32;
            let mut sample = GestureSample::stationary(GesturePhase::Update, at_ms);
            match gesture {
                Gesture::Swipe { dx, dy } => {
                    let next_x = if step == steps { dx } else { dx * progress };
                    let next_y = if step == steps { dy } else { dy * progress };
                    sample.dx = next_x - x;
                    sample.dy = next_y - y;
                    (x, y) = (next_x, next_y);
                }
                Gesture::Pinch { scale: total } => {
                    let next = if step == steps {
                        total
                    } else {
                        1.0 + (total - 1.0) * progress
                    };
                    sample.scale = next / scale;
                    scale = next;
                }
                Gesture::Rotate { degrees: total } => {
                    let next = if step == steps {
                        total
                    } else {
                        total * progress
                    };
                    sample.rotation_degrees = next - degrees;
                    degrees = next;
                }
            }
            samples.push(sample);
        }
        samples.push(GestureSample::stationary(
            GesturePhase::End,
            options.duration_ms,
        ));
        Ok(Self {
            gesture,
            options,
            samples,
        })
    }

    pub const fn gesture(&self) -> Gesture {
        self.gesture
    }
    pub const fn options(&self) -> GestureOptions {
        self.options
    }
    pub const fn duration_ms(&self) -> u32 {
        self.options.duration_ms
    }
    pub fn samples(&self) -> &[GestureSample] {
        &self.samples
    }

    /// A caller inspecting/planning a cancelled trajectory can construct its
    /// terminal event. This does not submit cancellation to a native device.
    pub fn cancel_sample(&self, at_ms: u32) -> Result<GestureSample> {
        if at_ms > self.options.duration_ms {
            return Err("touchpad cancellation time exceeds the planned duration".into());
        }
        Ok(GestureSample::stationary(GesturePhase::Cancel, at_ms))
    }

    pub fn to_json(&self) -> String {
        let detail = match self.gesture {
            Gesture::Swipe { dx, dy } => format!("\"dx\":{dx},\"dy\":{dy}"),
            Gesture::Pinch { scale } => format!("\"scale\":{scale}"),
            Gesture::Rotate { degrees } => format!("\"degrees\":{degrees}"),
        };
        format!(
            "{{\"gesture\":\"{}\",\"fingers\":{},\"duration_ms\":{},\"samples\":{},\"native_supported\":false,{detail}}}",
            self.gesture.name(),
            self.options.fingers,
            self.options.duration_ms,
            self.samples.len(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub native: bool,
    pub backend: Option<&'static str>,
    pub swipe: bool,
    pub pinch: bool,
    pub rotate: bool,
    pub cancel: bool,
}

pub const fn capabilities() -> Capabilities {
    Capabilities {
        native: false,
        backend: None,
        swipe: false,
        pinch: false,
        rotate: false,
        cancel: false,
    }
}

impl Capabilities {
    pub fn to_json(self) -> String {
        // Native capability discovery has no backend yet. Do not infer gesture
        // injection from mouse, wheel, XInput, virtual-keyboard or GUI support.
        "{\"native\":false,\"backend\":null,\"swipe\":false,\"pinch\":false,\"rotate\":false,\"cancel\":false,\"planning\":true,\"mouse_wheel_fallback\":false,\"reason\":\"No native touchpad gesture injection backend is implemented\"}".into()
    }
}

pub fn capabilities_json() -> String {
    capabilities().to_json()
}

fn unsupported(operation: &str) -> String {
    format!(
        "unsupported_capability: touchpad.{operation} requires a native gesture backend; none is implemented; mouse and wheel substitution is disabled"
    )
}

/// Native execution is unavailable. A valid plan never implies device support.
pub fn execute(plan: &GesturePlan) -> Result<()> {
    Err(unsupported(plan.gesture.name()))
}

/// No native gesture stream exists to cancel on the currently implemented backends.
pub fn cancel() -> Result<()> {
    Err(unsupported("cancel"))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Capabilities,
    Gesture(GesturePlan),
    Cancel,
}

impl Command {
    pub const fn is_mutating(&self) -> bool {
        !matches!(self, Self::Capabilities)
    }

    pub const fn scheduled_duration_ms(&self) -> u32 {
        match self {
            Self::Gesture(plan) => plan.duration_ms(),
            _ => 0,
        }
    }

    /// Run for every command in a program before creating mice or sending any
    /// input. Capability queries and pure planning remain available.
    pub fn require_native(&self) -> Result<()> {
        match self {
            Self::Capabilities => Ok(()),
            Self::Gesture(plan) => Err(unsupported(plan.gesture.name())),
            Self::Cancel => Err(unsupported("cancel")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= expected.abs().max(1.0) * 1e-10,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn swipe_samples_sum_to_requested_delta_and_have_terminal_phases() {
        let options = GestureOptions {
            fingers: 3,
            duration_ms: 120,
            steps: Some(15),
        };
        let plan = GesturePlan::new(
            Gesture::Swipe {
                dx: 217.5,
                dy: -38.25,
            },
            options,
        )
        .unwrap();
        assert_eq!(plan.options(), options);
        assert_eq!(plan.duration_ms(), 120);
        assert_eq!(plan.samples().len(), 17);
        assert_eq!(
            plan.samples()[0],
            GestureSample::stationary(GesturePhase::Begin, 0)
        );
        assert_eq!(
            plan.samples()[16],
            GestureSample::stationary(GesturePhase::End, 120)
        );
        close(plan.samples().iter().map(|sample| sample.dx).sum(), 217.5);
        close(plan.samples().iter().map(|sample| sample.dy).sum(), -38.25);
        for (index, sample) in plan.samples()[1..16].iter().enumerate() {
            assert_eq!(sample.phase, GesturePhase::Update);
            assert_eq!(sample.at_ms, (index as u32 + 1) * 8);
            assert!(sample.dx >= 0.0 && sample.dy <= 0.0);
            assert_eq!(sample.scale, 1.0);
            assert_eq!(sample.rotation_degrees, 0.0);
        }
    }

    #[test]
    fn pinch_samples_multiply_to_total_scale() {
        for scale in [0.01, 0.5, 1.0, 2.0, 100.0] {
            let plan =
                GesturePlan::new(Gesture::Pinch { scale }, GestureOptions::default()).unwrap();
            close(
                plan.samples().iter().map(|sample| sample.scale).product(),
                scale,
            );
            assert!(plan.samples().iter().all(|sample| sample.scale.is_finite()
                && sample.scale > 0.0
                && sample.dx == 0.0
                && sample.dy == 0.0
                && sample.rotation_degrees == 0.0));
        }
    }

    #[test]
    fn rotation_samples_sum_to_total_angle() {
        for degrees in [-3600.0, -90.0, 0.0, 15.0, 3600.0] {
            let plan =
                GesturePlan::new(Gesture::Rotate { degrees }, GestureOptions::default()).unwrap();
            close(
                plan.samples()
                    .iter()
                    .map(|sample| sample.rotation_degrees)
                    .sum(),
                degrees,
            );
            assert!(
                plan.samples()
                    .iter()
                    .all(|sample| sample.dx == 0.0 && sample.dy == 0.0 && sample.scale == 1.0)
            );
        }
    }

    #[test]
    fn instant_and_maximum_plans_remain_bounded() {
        let instant = GesturePlan::new(
            Gesture::Swipe {
                dx: MAX_DELTA,
                dy: -MAX_DELTA,
            },
            GestureOptions {
                duration_ms: 0,
                ..GestureOptions::default()
            },
        )
        .unwrap();
        assert_eq!(instant.samples().len(), 3);
        assert!(instant.samples().iter().all(|sample| sample.at_ms == 0));
        assert_eq!(instant.samples()[1].dx, MAX_DELTA);
        let largest = GesturePlan::new(
            Gesture::Pinch { scale: 100.0 },
            GestureOptions {
                duration_ms: MAX_DURATION_MS,
                steps: Some(MAX_STEPS),
                fingers: 5,
            },
        )
        .unwrap();
        assert_eq!(largest.samples().len(), MAX_STEPS as usize + 2);
        assert_eq!(largest.samples().last().unwrap().at_ms, MAX_DURATION_MS);
    }

    #[test]
    fn invalid_options_and_nonfinite_requests_are_rejected_before_sampling() {
        for options in [
            GestureOptions {
                fingers: 1,
                ..GestureOptions::default()
            },
            GestureOptions {
                fingers: 6,
                ..GestureOptions::default()
            },
            GestureOptions {
                duration_ms: MAX_DURATION_MS + 1,
                ..GestureOptions::default()
            },
            GestureOptions {
                steps: Some(0),
                ..GestureOptions::default()
            },
            GestureOptions {
                steps: Some(MAX_STEPS + 1),
                ..GestureOptions::default()
            },
            GestureOptions {
                duration_ms: 10,
                steps: Some(11),
                ..GestureOptions::default()
            },
            GestureOptions {
                duration_ms: 0,
                steps: Some(2),
                ..GestureOptions::default()
            },
        ] {
            assert!(GesturePlan::new(Gesture::Pinch { scale: 1.0 }, options).is_err());
        }
        for gesture in [
            Gesture::Swipe {
                dx: f64::NAN,
                dy: 0.0,
            },
            Gesture::Swipe {
                dx: 0.0,
                dy: f64::INFINITY,
            },
            Gesture::Swipe {
                dx: MAX_DELTA + 1.0,
                dy: 0.0,
            },
            Gesture::Pinch { scale: 0.0 },
            Gesture::Pinch { scale: 100.1 },
            Gesture::Pinch { scale: f64::NAN },
            Gesture::Rotate {
                degrees: f64::NEG_INFINITY,
            },
            Gesture::Rotate {
                degrees: MAX_ROTATION_DEGREES + 1.0,
            },
        ] {
            assert!(GesturePlan::new(gesture, GestureOptions::default()).is_err());
        }
    }

    #[test]
    fn planned_cancellation_has_no_final_motion_delta() {
        let plan = GesturePlan::new(
            Gesture::Swipe {
                dx: 100.0,
                dy: 20.0,
            },
            GestureOptions::default(),
        )
        .unwrap();
        for time in [0, 123, plan.duration_ms()] {
            assert_eq!(
                plan.cancel_sample(time).unwrap(),
                GestureSample::stationary(GesturePhase::Cancel, time)
            );
        }
        assert!(plan.cancel_sample(plan.duration_ms() + 1).is_err());
    }

    #[test]
    fn native_capability_rejection_is_explicit_for_every_mutating_command() {
        let capabilities = capabilities();
        assert!(
            !capabilities.native
                && !capabilities.swipe
                && !capabilities.pinch
                && !capabilities.rotate
                && !capabilities.cancel
        );
        assert_eq!(capabilities.backend, None);
        assert!(
            capabilities
                .to_json()
                .contains("\"mouse_wheel_fallback\":false")
        );
        assert!(Command::Capabilities.require_native().is_ok());
        assert!(!Command::Capabilities.is_mutating());
        assert_eq!(Command::Capabilities.scheduled_duration_ms(), 0);
        for gesture in [
            Gesture::Swipe { dx: 1.0, dy: 0.0 },
            Gesture::Pinch { scale: 1.1 },
            Gesture::Rotate { degrees: 15.0 },
        ] {
            let plan = GesturePlan::new(gesture, GestureOptions::default()).unwrap();
            assert!(
                execute(&plan)
                    .unwrap_err()
                    .starts_with("unsupported_capability:")
            );
            let command = Command::Gesture(plan);
            assert!(command.is_mutating());
            assert_eq!(command.scheduled_duration_ms(), 350);
            assert!(
                command
                    .require_native()
                    .unwrap_err()
                    .contains(gesture.name())
            );
        }
        assert!(Command::Cancel.is_mutating());
        assert!(Command::Cancel.require_native().is_err());
        assert!(cancel().unwrap_err().starts_with("unsupported_capability:"));
    }
}
