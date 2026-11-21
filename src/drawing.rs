//! Bounded drawing primitives and explicitly observed window-local anchors.
//!
//! An anchor records geometry; it never follows a renamed or moved window.
use crate::{
    motion::{MAX_COORDINATE, Point},
    mouse::Mouse,
    observation::Cancellation,
    target::{Bounds, TargetWindow},
};
use std::time::{Duration, Instant};

pub const MAX_POINTS: usize = 4096;
pub const MAX_PROGRAM_POINTS: usize = 65_536;

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.len() > 128 || name.contains('\0') {
        Err("anchor name must contain 1..128 non-NUL bytes".into())
    } else {
        Ok(())
    }
}

fn validate_point(point: Point) -> Result<(), String> {
    if [point.x, point.y]
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE)
    {
        Err("drawing points must be finite and within the supported coordinate range".into())
    } else {
        Ok(())
    }
}

/// Parse every vertex before input. Empty segments and extra coordinates fail.
pub fn parse_points(source: &str) -> Result<Vec<Point>, String> {
    if source.len() > 262_144 {
        return Err("drawing path exceeds 256 KiB".into());
    }
    let mut points = Vec::new();
    for pair in source.split(';') {
        if points.len() == MAX_POINTS {
            return Err("drawing path exceeds 4096 points".into());
        }
        let (x, y) = pair
            .trim()
            .split_once(',')
            .ok_or("drawing points must be x,y pairs separated by semicolons")?;
        let point = Point {
            x: x.trim()
                .parse()
                .map_err(|_| "invalid drawing x coordinate")?,
            y: y.trim()
                .parse()
                .map_err(|_| "invalid drawing y coordinate")?,
        };
        validate_point(point)?;
        points.push(point);
    }
    if points.len() < 2 {
        return Err("a stroke requires at least two points".into());
    }
    Ok(points)
}

/// A two-point outline. Geometry is checked before a pointer is acquired.
pub fn line(from: Point, to: Point) -> Result<Vec<Point>, String> {
    validate_point(from)?;
    validate_point(to)?;
    Ok(vec![from, to])
}

/// Three numeric vertices followed by an exact copy of the first vertex.
pub fn triangle(a: Point, b: Point, c: Point) -> Result<Vec<Point>, String> {
    for point in [a, b, c] {
        validate_point(point)?;
    }
    Ok(vec![a, b, c, a])
}

/// Rectangle origin is its top-left corner; the last point closes the outline.
pub fn rect(origin: Point, width: f64, height: f64) -> Result<Vec<Point>, String> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("draw.rect requires positive finite width and height".into());
    }
    let points = vec![
        origin,
        Point {
            x: origin.x + width,
            y: origin.y,
        },
        Point {
            x: origin.x + width,
            y: origin.y + height,
        },
        Point {
            x: origin.x,
            y: origin.y + height,
        },
        origin,
    ];
    for point in &points {
        validate_point(*point)?;
    }
    Ok(points)
}

/// `steps` is the number of segments; copying the first point closes exactly.
pub fn circle(center: Point, radius: f64, steps: usize) -> Result<Vec<Point>, String> {
    if !radius.is_finite() || radius <= 0.0 {
        return Err("draw.circle requires a positive finite radius".into());
    }
    if !(3..MAX_POINTS).contains(&steps) {
        return Err("draw.circle steps must be 3..4095, including room for closure".into());
    }
    validate_point(center)?;
    // Check the full extent even when an odd segment count skips an extremum.
    validate_point(Point {
        x: center.x - radius,
        y: center.y - radius,
    })?;
    validate_point(Point {
        x: center.x + radius,
        y: center.y + radius,
    })?;
    let mut points = Vec::with_capacity(steps + 1);
    for index in 0..steps {
        let angle = std::f64::consts::TAU * index as f64 / steps as f64;
        let point = Point {
            x: center.x + radius * angle.cos(),
            y: center.y + radius * angle.sin(),
        };
        validate_point(point)?;
        points.push(point);
    }
    points.push(points[0]);
    Ok(points)
}

/// Close an explicitly supplied polygon without adding a second closing point.
pub fn polygon(source: &str) -> Result<Vec<Point>, String> {
    let mut points = parse_points(source)?;
    let closed = points.first() == points.last();
    let vertices = points.len() - usize::from(closed);
    if vertices < 3 {
        return Err("draw.polygon requires at least three vertices".into());
    }
    if !closed {
        if points.len() == MAX_POINTS {
            return Err("drawing path exceeds 4096 points including closure".into());
        }
        points.push(points[0]);
    }
    Ok(points)
}

/// A region relative to one observed window, in the backend's native units.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub window: TargetWindow,
    pub region: Bounds,
}

impl Anchor {
    pub fn new(window: TargetWindow, region: Bounds) -> Result<Self, String> {
        Self::validate_region(region)?;
        if !window.focused {
            return Err("stale_anchor: focus the observed window before setting an anchor".into());
        }
        if region.x + region.width > window.bounds.width
            || region.y + region.height > window.bounds.height
        {
            return Err("anchor region lies outside the observed window".into());
        }
        Ok(Self { window, region })
    }

    pub fn validate_region(region: Bounds) -> Result<(), String> {
        if [region.x, region.y, region.width, region.height]
            .iter()
            .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE)
            || region.x < 0.0
            || region.y < 0.0
            || region.width <= 0.0
            || region.height <= 0.0
        {
            return Err("anchor region requires nonnegative x/y and positive width/height".into());
        }
        Ok(())
    }

    pub fn verify(&self, snapshot: &str) -> Result<(), String> {
        let current = TargetWindow::from_windows(snapshot, &self.window.id)
            .map_err(|error| format!("stale_anchor: {error}; observe and set the anchor again"))?;
        if current != self.window || !current.focused {
            return Err("stale_anchor: window identity, geometry, scale or focus changed; observe and set the anchor again".into());
        }
        Ok(())
    }

    pub fn resolve(&self, points: &[Point]) -> Result<Vec<Point>, String> {
        points
            .iter()
            .map(|point| {
                validate_point(*point)?;
                if point.x < 0.0
                    || point.y < 0.0
                    || point.x >= self.region.width
                    || point.y >= self.region.height
                {
                    return Err("drawing point lies outside its anchor region".into());
                }
                let point = Point {
                    x: self.window.bounds.x + self.region.x + point.x,
                    y: self.window.bounds.y + self.region.y + point.y,
                };
                validate_point(point)?;
                Ok(point)
            })
            .collect()
    }
}

pub(crate) trait Input {
    fn move_to(&mut self, point: Point) -> Result<(), String>;
    fn button(&mut self, button: &str, action: &str) -> Result<(), String>;
    fn holds(&self, button: &str) -> bool;
}

impl Input for Mouse {
    fn move_to(&mut self, point: Point) -> Result<(), String> {
        self.submit(point)
    }
    fn button(&mut self, button: &str, action: &str) -> Result<(), String> {
        Mouse::button(self, button, action)
    }
    fn holds(&self, button: &str) -> bool {
        self.holds_button(button)
    }
}

/// Always attempt release after an attempted press, including cancelled input.
/// Cleanup intentionally does not use the cancelled token, like Mouse::close.
fn release_result<T>(
    input: &mut impl Input,
    button: &str,
    result: Result<T, String>,
) -> Result<T, String> {
    match (result, input.button(button, "up")) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(format!("button release failed: {cleanup}")),
        (Err(error), Err(cleanup)) => Err(format!("{error}; button release failed: {cleanup}")),
    }
}

pub(crate) fn stroke(
    input: &mut impl Input,
    points: &[Point],
    duration_ms: u32,
    settle_ms: u32,
    button: &str,
    cancellation: &Cancellation,
) -> Result<usize, String> {
    if points.len() < 2 || points.len() > MAX_POINTS {
        return Err("a stroke requires 2..4096 points".into());
    }
    for point in points {
        validate_point(*point)?;
    }
    if input.holds(button) {
        return Err("a stroke cannot take over an already held button".into());
    }
    cancellation.dispatch(|| input.move_to(points[0]))?;
    cancellation.sleep(Duration::from_millis(u64::from(settle_ms)))?;
    let mut pressed = false;
    let result = (|| {
        cancellation.dispatch(|| {
            pressed = true;
            input.button(button, "down")
        })?;
        cancellation.sleep(Duration::from_millis(u64::from(settle_ms)))?;
        let start = Instant::now();
        for (index, point) in points.iter().enumerate().skip(1) {
            let at_ms = u64::from(duration_ms) * index as u64 / (points.len() - 1) as u64;
            if let Some(delay) = Duration::from_millis(at_ms).checked_sub(start.elapsed()) {
                cancellation.sleep(delay)?;
            }
            cancellation.dispatch(|| input.move_to(*point))?;
        }
        Ok(points.len())
    })();
    if pressed {
        release_result(input, button, result)
    } else {
        result
    }
}

/// Sample and validate the complete target path before its first input event.
pub(crate) fn move_path(
    input: &mut impl Input,
    path: impl Iterator<Item = crate::motion::Sample> + Clone,
    cancellation: &Cancellation,
) -> Result<usize, String> {
    for sample in path.clone() {
        validate_point(sample.point)?;
    }
    let start = Instant::now();
    let mut samples = 0;
    for sample in path {
        if let Some(delay) =
            Duration::from_millis(u64::from(sample.at_ms)).checked_sub(start.elapsed())
        {
            cancellation.sleep(delay)?;
        }
        cancellation.dispatch(|| input.move_to(sample.point))?;
        samples += 1;
    }
    Ok(samples)
}

pub(crate) fn click_at(
    input: &mut impl Input,
    path: impl Iterator<Item = crate::motion::Sample> + Clone,
    settle_ms: u32,
    button: &str,
    cancellation: &Cancellation,
) -> Result<usize, String> {
    if input.holds(button) {
        return Err("cannot click a button that this mouse already holds".into());
    }
    let samples = move_path(input, path, cancellation)?;
    cancellation.sleep(Duration::from_millis(u64::from(settle_ms)))?;
    let mut pressed = false;
    let result = cancellation.dispatch(|| {
        pressed = true;
        input.button(button, "down")?;
        Ok(samples)
    });
    if pressed {
        release_result(input, button, result)
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Recorder {
        points: Vec<Point>,
        buttons: Vec<String>,
        held: bool,
        fail_move: Option<usize>,
        fail_press: bool,
        on_press: Option<std::sync::mpsc::Sender<()>>,
        on_move: Option<std::sync::mpsc::Sender<()>>,
    }
    impl Input for Recorder {
        fn move_to(&mut self, point: Point) -> Result<(), String> {
            self.points.push(point);
            if let Some(sender) = &self.on_move {
                let _ = sender.send(());
            }
            if self.fail_move == Some(self.points.len()) {
                Err("move failed".into())
            } else {
                Ok(())
            }
        }
        fn button(&mut self, _: &str, action: &str) -> Result<(), String> {
            self.buttons.push(action.into());
            self.held = action == "down";
            if action == "down"
                && let Some(sender) = &self.on_press
            {
                let _ = sender.send(());
            }
            if action == "down" && self.fail_press {
                Err("press acknowledgement failed".into())
            } else {
                Ok(())
            }
        }
        fn holds(&self, _: &str) -> bool {
            self.held
        }
    }

    #[test]
    fn stroke_preserves_every_vertex_and_releases_after_failure() {
        let points = parse_points("1,2;9,2;9,9;1,2").unwrap();
        let mut input = Recorder::default();
        assert_eq!(
            stroke(&mut input, &points, 0, 0, "left", &Cancellation::default()).unwrap(),
            4
        );
        assert_eq!(input.points, points);
        assert_eq!(input.buttons, ["down", "up"]);
        for fail_move in [Some(2), Some(3)] {
            let mut input = Recorder {
                fail_move,
                ..Recorder::default()
            };
            assert!(stroke(&mut input, &points, 0, 0, "left", &Cancellation::default()).is_err());
            assert_eq!(input.buttons, ["down", "up"]);
            assert!(!input.held);
        }
        let mut input = Recorder {
            fail_press: true,
            ..Recorder::default()
        };
        assert!(stroke(&mut input, &points, 0, 0, "left", &Cancellation::default()).is_err());
        assert_eq!(input.buttons, ["down", "up"]);
    }

    #[test]
    fn invalid_and_cancelled_paths_send_no_input() {
        for source in [
            "",
            "1,2",
            "1,2;",
            "1,2;3,4,5",
            "1,2;NaN,4",
            "1,2;16777217,0",
        ] {
            assert!(parse_points(source).is_err(), "{source}");
        }
        let points = parse_points("1,2;3,4").unwrap();
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let mut input = Recorder::default();
        assert!(stroke(&mut input, &points, 0, 0, "left", &cancellation).is_err());
        assert!(input.points.is_empty());
        assert!(input.buttons.is_empty());
        input.held = true;
        assert!(stroke(&mut input, &points, 0, 0, "left", &Cancellation::default()).is_err());
        assert!(input.points.is_empty());
    }

    #[test]
    fn cancellation_during_a_stroke_releases_its_button() {
        let cancellation = Cancellation::default();
        let other = cancellation.clone();
        let (pressed, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            receive.recv().unwrap();
            other.cancel();
        });
        let mut input = Recorder {
            on_press: Some(pressed),
            ..Recorder::default()
        };
        let result = stroke(
            &mut input,
            &parse_points("1,2;9,9").unwrap(),
            1000,
            0,
            "left",
            &cancellation,
        );
        thread.join().unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(input.buttons, ["down", "up"]);
        assert_eq!(input.points.len(), 1);
    }

    #[test]
    fn click_moves_every_sample_before_press_and_releases_failed_presses() {
        use crate::motion::{Curve, Motion};
        let from = Point { x: 0.0, y: 0.0 };
        let to = Point { x: 10.0, y: 20.0 };
        let path = Motion::new(from, to, 8, 2, Curve::Smooth, 0.6, 1).unwrap();
        let expected: Vec<_> = path.clone().map(|sample| sample.point).collect();
        let mut input = Recorder::default();
        assert_eq!(
            click_at(
                &mut input,
                path.clone(),
                0,
                "left",
                &Cancellation::default()
            )
            .unwrap(),
            3
        );
        assert_eq!(input.points, expected);
        assert_eq!(input.buttons, ["down", "up"]);
        let mut input = Recorder {
            fail_move: Some(2),
            ..Recorder::default()
        };
        assert!(click_at(&mut input, path, 0, "left", &Cancellation::default()).is_err());
        assert!(input.buttons.is_empty());
        let instant = Motion::new(to, to, 0, 1, Curve::Smooth, 0.6, 1)
            .unwrap()
            .skip(1);
        let mut input = Recorder {
            fail_press: true,
            ..Recorder::default()
        };
        assert!(
            click_at(
                &mut input,
                instant.clone(),
                0,
                "left",
                &Cancellation::default()
            )
            .is_err()
        );
        assert_eq!(input.points, [to]);
        assert_eq!(input.buttons, ["down", "up"]);
        let mut input = Recorder {
            held: true,
            ..Recorder::default()
        };
        assert!(click_at(&mut input, instant, 0, "left", &Cancellation::default()).is_err());
        assert!(input.points.is_empty());
        assert!(input.buttons.is_empty());
    }

    #[test]
    fn cancelled_click_motion_never_reaches_the_press() {
        use crate::motion::{Curve, Motion};
        let cancellation = Cancellation::default();
        let other = cancellation.clone();
        let (moved, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            receive.recv().unwrap();
            other.cancel();
        });
        let mut input = Recorder {
            on_move: Some(moved),
            ..Recorder::default()
        };
        let path = Motion::new(
            Point::default(),
            Point { x: 10.0, y: 20.0 },
            1000,
            2,
            Curve::Smooth,
            0.6,
            1,
        )
        .unwrap();
        let result = click_at(&mut input, path, 0, "left", &cancellation);
        thread.join().unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(input.points.len(), 1);
        assert!(input.buttons.is_empty());
    }

    #[test]
    fn invalid_final_click_sample_rejects_the_entire_motion() {
        use crate::motion::Sample;
        let points = [
            Sample {
                point: Point::default(),
                at_ms: 0,
            },
            Sample {
                point: Point {
                    x: MAX_COORDINATE + 1.0,
                    y: 0.0,
                },
                at_ms: 1,
            },
        ];
        let mut input = Recorder::default();
        assert!(
            click_at(
                &mut input,
                points.into_iter(),
                0,
                "left",
                &Cancellation::default()
            )
            .is_err()
        );
        assert!(input.points.is_empty());
        assert!(input.buttons.is_empty());
    }
}
