//! Finite, simultaneous contacts in CSS viewport coordinates on the selected page.
use super::*;
use crate::motion::{Curve, FunctionalMotion, PathOptions, Point};
use crate::observation::Cancellation;

#[derive(Debug)]
pub(crate) struct TouchGesture {
    paths: Vec<Vec<Point>>,
    pub(crate) duration_ms: u64,
    steps: u32,
}

impl TouchGesture {
    pub(crate) fn parse(
        paths: &str,
        duration_ms: u64,
        steps: u32,
        curve: Curve,
        jitter: f64,
        seed: u64,
        speed_variation: f64,
    ) -> Result<Self> {
        if !jitter.is_finite() || !(0.0..=32.0).contains(&jitter) {
            return Err("touch jitter must be in 0..32 CSS pixels".into());
        }
        if paths.len() > 512 || duration_ms > 10_000 || !(1..=120).contains(&steps) {
            return Err("touch gesture exceeds 512 bytes, 10000ms or 1..120 steps".into());
        }
        fn point(s: &str) -> Result<Point> {
            let (x, y) = s.split_once(',').ok_or("touch point requires x,y")?;
            let parse = |v: &str| {
                v.trim()
                    .parse::<u16>()
                    .map_err(|_| "touch coordinates must be integers in 0..65535".to_string())
            };
            Ok(Point {
                x: f64::from(parse(x)?),
                y: f64::from(parse(y)?),
            })
        }
        let mut contacts = Vec::new();
        for path in paths.split(';') {
            if contacts.len() == 10 {
                return Err("touch gesture supports 1..10 contacts".into());
            }
            let (start, end) = path.split_once(':').unwrap_or((path, path));
            let from = point(start)?;
            let to = point(end)?;
            // The dispatch loop owns real timing, including zero-duration gestures.
            let options = PathOptions {
                duration_ms: steps,
                steps,
                easing: curve,
                jitter: if from == to { 0.0 } else { jitter },
                seed: seed.wrapping_add(contacts.len() as u64),
                speed_variation,
            };
            let dx = to.x - from.x;
            let dy = to.y - from.y;
            let motion = FunctionalMotion::new(
                |u| {
                    let bow = if curve == Curve::Bezier {
                        0.6 * u * (1.0 - u)
                    } else {
                        0.0
                    };
                    Point {
                        x: from.x + dx * u - dy * bow,
                        y: from.y + dy * u + dx * bow,
                    }
                },
                options,
            )
            .map_err(|error| error.to_string())?;
            let samples = motion
                .map(|sample| sample.map(|s| s.point))
                .collect::<std::result::Result<_, _>>()
                .map_err(|error| error.to_string())?;
            contacts.push(samples);
        }
        Ok(Self {
            paths: contacts,
            duration_ms,
            steps,
        })
    }

    fn points(&self, step: u32) -> String {
        use std::fmt::Write;
        let mut output = String::new();
        for (id, path) in self.paths.iter().enumerate() {
            let point = path[step as usize];
            if id > 0 {
                output.push(',');
            }
            write!(output, r#"{{"id":{id},"x":{},"y":{}}}"#, point.x, point.y).unwrap();
        }
        output
    }
}

impl Browser {
    pub(crate) fn touch_gesture(
        &mut self,
        gesture: TouchGesture,
        cancellation: &Cancellation,
    ) -> Result<String> {
        cancellation.check()?;
        let size = self.evaluate("({width: innerWidth, height: innerHeight})")?;
        let (width, height) = with_json(&size, |v| {
            Ok((number(v.get("width"))?, number(v.get("height"))?))
        })?;
        if gesture
            .paths
            .iter()
            .flatten()
            .any(|p| p.x < 0.0 || p.y < 0.0 || p.x >= width || p.y >= height)
        {
            return Err("touch point lies outside the selected page viewport".into());
        }
        let start = Instant::now();
        let duration = Duration::from_millis(gesture.duration_ms);
        let deadline = start + duration + Duration::from_secs(3);
        let mut dispatch = |kind: &str, points: String| {
            self.request_until(
                "Input.dispatchTouchEvent",
                &format!(r#"{{"type":"{kind}","touchPoints":[{points}]}}"#),
                true,
                deadline,
                Some(cancellation),
            )
        };
        let result = (|| {
            dispatch("touchStart", gesture.points(0))?;
            let moving = gesture.paths.iter().any(|path| path.first() != path.last());
            if moving {
                for step in 1..=gesture.steps {
                    let due = start + duration * step / gesture.steps;
                    cancellation.sleep(due.saturating_duration_since(Instant::now()))?;
                    dispatch("touchMove", gesture.points(step))?;
                }
            } else {
                cancellation.sleep((start + duration).saturating_duration_since(Instant::now()))?;
            }
            dispatch("touchEnd", String::new())?;
            Ok(format!(
                r#"{{"contacts":{},"duration_ms":{},"completed":true}}"#,
                gesture.paths.len(),
                gesture.duration_ms
            ))
        })();
        if let Err(reason) = result {
            let clean = self.request_until(
                "Input.dispatchTouchEvent",
                r#"{"type":"touchCancel","touchPoints":[]}"#,
                true,
                Instant::now() + Duration::from_secs(1),
                None,
            );
            return match clean {
                Ok(_) => Err(reason),
                Err(e) => Err(format!("{reason}; touch cancellation was unconfirmed: {e}")),
            };
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_use_stable_ids_and_bounded_coordinates() {
        let g = TouchGesture::parse(
            "10,20:20,40;70,80:50,60",
            100,
            2,
            Curve::Linear,
            0.0,
            1,
            0.0,
        )
        .unwrap();
        assert_eq!(
            g.points(1),
            r#"{"id":0,"x":15,"y":30},{"id":1,"x":60,"y":70}"#
        );
        for paths in ["", "-1,0", "0,0:1", "70000,1", "1,2;"] {
            assert!(TouchGesture::parse(paths, 100, 2, Curve::Linear, 0.0, 1, 0.0).is_err());
        }
        assert!(
            TouchGesture::parse(&["1,1"; 11].join(";"), 100, 2, Curve::Linear, 0.0, 1, 0.0)
                .is_err()
        );
        assert!(TouchGesture::parse("1,1", 100, 0, Curve::Linear, 0.0, 1, 0.0).is_err());
    }

    #[test]
    fn seeded_contacts_have_reproducible_interiors_and_exact_endpoints() {
        let make = |seed| {
            TouchGesture::parse("50,50:100,100;70,80", 100, 8, Curve::Bezier, 3.0, seed, 0.8)
                .unwrap()
        };
        let a = make(42);
        assert_eq!(a.paths, make(42).paths);
        assert_ne!(a.paths[0][3], make(43).paths[0][3]);
        assert_eq!(a.paths[0][0], Point { x: 50.0, y: 50.0 });
        assert_eq!(a.paths[0][8], Point { x: 100.0, y: 100.0 });
        assert!(a.paths[1].iter().all(|p| p == &Point { x: 70.0, y: 80.0 }));
        assert!(TouchGesture::parse("40,40", 100, 6, Curve::Smooth, 32.1, 1, 0.0).is_err());
        let ease = TouchGesture::parse("0,0:80,0", 100, 4, Curve::EaseInOut, 0.0, 1, 0.0).unwrap();
        assert!(ease.paths[0][1].x < 20.0);
        assert_eq!(ease.paths[0][2].x, 40.0);
    }
}
