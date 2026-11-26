//! Write positioned font strokes with a borrowed system pointer or virtual touch contacts.
use crate::{
    drawing,
    motion::{MAX_COORDINATE, Point},
    mouse::{Mouse, MouseMode, MouseOptions},
    observation::Cancellation,
};

#[cfg(target_os = "linux")]
mod linux;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Absolute desktop coordinates, including negative coordinates on multi-monitor desktops.
    Mouse,
    /// Logical device coordinates. An indirect touchpad reports finger contacts to its consumer.
    Touchpad { width: u16, height: u16 },
    /// Logical device coordinates, mapped to an output by the compositor.
    Touchscreen { width: u16, height: u16 },
}

/// Check every stroke without opening a device and return the complete scheduled wait in milliseconds.
pub fn validate(
    target: Target,
    strokes: &[Vec<(f64, f64)>],
    interval_ms: u32,
) -> Result<u64, String> {
    let surface = match target {
        Target::Mouse => None,
        Target::Touchpad { width, height } | Target::Touchscreen { width, height } => {
            if width < 2 || height < 2 {
                return Err("touch surface width and height must both be at least 2".into());
            }
            Some((f64::from(width - 1), f64::from(height - 1)))
        }
    };
    if interval_ms > 1000 || strokes.len() > drawing::MAX_PROGRAM_POINTS {
        return Err("writing exceeds the input interval or stroke limit".into());
    }
    let mut count = 0;
    for path in strokes {
        if path.is_empty() || path.len() > drawing::MAX_POINTS {
            return Err("a handwriting stroke must contain 1..4096 points".into());
        }
        // A dot is a stationary contact with its own down and up events.
        count += path.len().max(2);
        if count > drawing::MAX_PROGRAM_POINTS {
            return Err("handwriting exceeds the 65536-point input limit".into());
        }
        for &(x, y) in path {
            if !x.is_finite()
                || !y.is_finite()
                || x.abs() > MAX_COORDINATE
                || y.abs() > MAX_COORDINATE
            {
                return Err("invalid handwriting input coordinate".into());
            }
            if surface.is_some_and(|(right, bottom)| x < 0.0 || x > right || y < 0.0 || y > bottom)
            {
                return Err("handwriting lies outside the selected touch surface".into());
            }
        }
    }
    // Per stroke the existing drawing executor waits once before and once after pressing.
    let waits = count + strokes.len();
    let discovery_ms = if surface.is_some() && !strokes.is_empty() {
        1000
    } else {
        0
    };
    let duration = waits as u64 * u64::from(interval_ms) + discovery_ms;
    if duration > 600_000 {
        return Err("handwriting input exceeds the 10-minute execution budget".into());
    }
    if !cfg!(target_os = "linux") && surface.is_some() && !strokes.is_empty() {
        return Err(
            "unsupported_capability: virtual handwriting touch input requires Linux uinput".into(),
        );
    }
    Ok(duration)
}

/// Validate all input before creating a device. The interval is milliseconds per vertex.
/// Returns after dispatching the final contact release. Application rendering is asynchronous.
pub fn write(
    target: Target,
    strokes: &[Vec<(f64, f64)>],
    interval_ms: u32,
    cancel: &Cancellation,
) -> Result<(), String> {
    validate(target, strokes, interval_ms)?;
    cancel.check()?;
    if strokes.is_empty() {
        return Ok(());
    }
    match target {
        Target::Mouse => {
            let mut mouse = Mouse::create(&MouseOptions {
                mode: MouseMode::User,
                appearance: None,
                ..MouseOptions::default()
            })?;
            dispatch(&mut mouse, strokes, interval_ms, cancel)
        }
        Target::Touchpad { width, height } | Target::Touchscreen { width, height } => {
            #[cfg(target_os = "linux")]
            {
                let mut device = linux::Surface::open(
                    width,
                    height,
                    matches!(target, Target::Touchscreen { .. }),
                )?;
                // Let desktop input consumers discover the newly registered device.
                cancel.sleep(std::time::Duration::from_secs(1))?;
                dispatch(&mut device, strokes, interval_ms, cancel)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (width, height);
                Err(
                    "unsupported_capability: virtual handwriting touch input requires Linux uinput"
                        .into(),
                )
            }
        }
    }
}

fn dispatch(
    input: &mut impl drawing::Input,
    paths: &[Vec<(f64, f64)>],
    interval_ms: u32,
    cancel: &Cancellation,
) -> Result<(), String> {
    for (index, path) in paths.iter().enumerate() {
        let mut points: Vec<Point> = path.iter().map(|&(x, y)| Point { x, y }).collect();
        if points.len() == 1 {
            points.push(points[0]);
        }
        let duration = (points.len() as u32 - 1) * interval_ms;
        drawing::stroke(input, &points, duration, interval_ms, "left", cancel)
            .map_err(|error| format!("handwriting stroke {}: {error}", index + 1))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        events: Vec<(String, f64, f64)>,
        pressed: bool,
        position: (f64, f64),
    }

    impl drawing::Input for Recorder {
        fn holds(&self, _: &str) -> bool {
            self.pressed
        }
        fn move_to(&mut self, point: Point) -> Result<(), String> {
            self.position = (point.x, point.y);
            self.events.push(("move".into(), point.x, point.y));
            Ok(())
        }
        fn button(&mut self, _: &str, action: &str) -> Result<(), String> {
            self.pressed = action == "down";
            self.events
                .push((action.into(), self.position.0, self.position.1));
            Ok(())
        }
    }

    #[test]
    fn separate_paths_and_dots_lift_before_repositioning() {
        let paths = vec![vec![(1., 2.), (3., 4.)], vec![(20., 25.)]];
        let mut input = Recorder::default();
        dispatch(&mut input, &paths, 0, &Cancellation::default()).unwrap();
        let phases: Vec<_> = input
            .events
            .iter()
            .map(|(s, x, y)| (s.as_str(), *x, *y))
            .collect();
        assert_eq!(
            phases,
            [
                ("move", 1., 2.),
                ("down", 1., 2.),
                ("move", 3., 4.),
                ("up", 3., 4.),
                ("move", 20., 25.),
                ("down", 20., 25.),
                ("move", 20., 25.),
                ("up", 20., 25.)
            ]
        );
    }

    #[test]
    fn permission_lost_mid_contact_releases_and_stops_following_strokes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = AtomicUsize::new(0);
        let token = Cancellation::with_dispatch_guard(move || {
            if attempts.fetch_add(1, Ordering::SeqCst) == 2 {
                Err("permit revoked".into())
            } else {
                Ok(Box::new(()))
            }
        });
        let mut input = Recorder::default();
        let result = dispatch(
            &mut input,
            &[vec![(1., 1.), (2., 2.)], vec![(8., 8.)]],
            0,
            &token,
        );
        assert!(result.unwrap_err().contains("permit revoked"));
        assert!(!input.pressed);
        assert_eq!(input.events.last().unwrap(), &("up".into(), 1., 1.));
        assert_eq!(input.events.len(), 3);
    }

    #[test]
    fn preflight_catches_late_errors_and_preserves_dots() {
        let target = Target::Touchscreen {
            width: 100,
            height: 50,
        };
        let mut strokes = vec![vec![(5., 8.)]];
        assert_eq!(validate(Target::Mouse, &strokes, 8).unwrap(), 24);
        #[cfg(target_os = "linux")]
        assert_eq!(validate(target, &strokes, 8).unwrap(), 1024);
        strokes.push(vec![(0., 0.), (5., 50.)]);
        assert!(validate(target, &strokes, 8).is_err());
        assert!(validate(Target::Mouse, &[vec![(0., f64::NAN)]], 8).is_err());
        assert!(validate(target, &[vec![(1., 1.); 605]], 1000).is_err());
        assert!(validate(Target::Mouse, &[vec![(-20., -40.)]], 0).is_ok());
    }
}
