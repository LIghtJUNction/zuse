//! Finite, simultaneous contacts in CSS viewport coordinates on the selected page.
use super::*;
use crate::observation::Cancellation;

#[derive(Debug)]
pub(crate) struct TouchGesture {
    paths: Vec<[(u16, u16); 2]>,
    pub(crate) duration_ms: u64,
    steps: u32,
}

impl TouchGesture {
    pub(crate) fn parse(paths: &str, duration_ms: u64, steps: u32) -> Result<Self> {
        if paths.len() > 512 || duration_ms > 10_000 || !(1..=120).contains(&steps) {
            return Err("touch gesture exceeds 512 bytes, 10000ms or 1..120 steps".into());
        }
        fn point(s: &str) -> Result<(u16, u16)> {
            let (x, y) = s.split_once(',').ok_or("touch point requires x,y")?;
            let parse = |v: &str| {
                v.trim()
                    .parse::<u16>()
                    .map_err(|_| "touch coordinates must be integers in 0..65535".to_string())
            };
            Ok((parse(x)?, parse(y)?))
        }
        let mut contacts = Vec::new();
        for path in paths.split(';') {
            if contacts.len() == 10 {
                return Err("touch gesture supports 1..10 contacts".into());
            }
            let (start, end) = path.split_once(':').unwrap_or((path, path));
            contacts.push([point(start)?, point(end)?]);
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
        for (id, &[start, end]) in self.paths.iter().enumerate() {
            let lerp = |from: u16, to: u16| {
                (u32::from(from) * (self.steps - step) + u32::from(to) * step) as f64
                    / self.steps as f64
            };
            if id > 0 {
                output.push(',');
            }
            write!(
                output,
                r#"{{"id":{id},"x":{},"y":{}}}"#,
                lerp(start.0, end.0),
                lerp(start.1, end.1)
            )
            .unwrap();
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
            .any(|&(x, y)| f64::from(x) >= width || f64::from(y) >= height)
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
            let moving = gesture.paths.iter().any(|[from, to]| from != to);
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
        let g = TouchGesture::parse("10,20:20,40;70,80:50,60", 100, 2).unwrap();
        assert_eq!(
            g.points(1),
            r#"{"id":0,"x":15,"y":30},{"id":1,"x":60,"y":70}"#
        );
        for paths in ["", "-1,0", "0,0:1", "70000,1", "1,2;"] {
            assert!(TouchGesture::parse(paths, 100, 2).is_err());
        }
        assert!(TouchGesture::parse(&["1,1"; 11].join(";"), 100, 2).is_err());
        assert!(TouchGesture::parse("1,1", 100, 0).is_err());
    }
}
