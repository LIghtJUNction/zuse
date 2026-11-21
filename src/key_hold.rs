//! Bounded direction-key input shared by native and browser adapters.
use crate::observation::Cancellation;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;
pub(crate) const MAX_HOLD_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Direction {
    Left,
    Up,
    Right,
    Down,
}

impl Direction {
    pub(crate) fn parse(key: &str) -> Result<Self> {
        match key.to_ascii_lowercase().as_str() {
            "left" | "arrowleft" => Ok(Self::Left),
            "up" | "arrowup" => Ok(Self::Up),
            "right" | "arrowright" => Ok(Self::Right),
            "down" | "arrowdown" => Ok(Self::Down),
            _ => Err("hold requires one direction key: Left, Up, Right or Down".into()),
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn native(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Up => "Up",
            Self::Right => "Right",
            Self::Down => "Down",
        }
    }
    pub(crate) fn browser(self) -> &'static str {
        match self {
            Self::Left => "ArrowLeft",
            Self::Up => "ArrowUp",
            Self::Right => "ArrowRight",
            Self::Down => "ArrowDown",
        }
    }
    pub(crate) fn virtual_key(self) -> u32 {
        match self {
            Self::Left => 37,
            Self::Up => 38,
            Self::Right => 39,
            Self::Down => 40,
        }
    }
}

pub(crate) fn validate_duration(duration_ms: u64) -> Result<()> {
    if !(1..=MAX_HOLD_MS).contains(&duration_ms) {
        return Err(format!("hold duration must be in 1..{MAX_HOLD_MS}ms"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum Event {
    Check { deadline: Instant },
    Press { deadline: Instant },
    Release { deadline: Instant },
}

struct Release<F: FnMut(Event) -> Result<()>> {
    event: F,
    armed: bool,
}
impl<F: FnMut(Event) -> Result<()>> Release<F> {
    fn release(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.armed) {
            return Ok(());
        }
        // Releasing our own key remains necessary after cancellation or input
        // permission revocation. It never presses a key or changes the target.
        (self.event)(Event::Release {
            deadline: Instant::now() + Duration::from_secs(2),
        })
    }
}
impl<F: FnMut(Event) -> Result<()>> Drop for Release<F> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Send once, wait without holding an input permit, and always release once.
/// Check may read target state; Press must only submit input, without waiting
/// for its acknowledgement. Cleanup has its own bounded protocol budget.
pub(crate) fn run(
    duration_ms: u64,
    cancellation: &Cancellation,
    event: impl FnMut(Event) -> Result<()>,
) -> Result<Duration> {
    validate_duration(duration_ms)?;
    cancellation.check()?;
    let mut guard = Release {
        event,
        armed: false,
    };
    (guard.event)(Event::Check {
        deadline: Instant::now() + Duration::from_secs(2),
    })?;
    let start = Instant::now();
    let deadline = start + Duration::from_millis(duration_ms);
    let result = (|| {
        cancellation.dispatch(|| {
            if Instant::now() >= deadline {
                return Err("timeout: key hold dispatch deadline reached".into());
            }
            // Arm before writing: a failed write can still deliver a prefix.
            guard.armed = true;
            (guard.event)(Event::Press { deadline })
        })?;
        loop {
            cancellation.check()?;
            // A long hold must also notice an embedding host revoking input.
            cancellation.dispatch(|| Ok(()))?;
            if Instant::now() >= deadline {
                break;
            }
            (guard.event)(Event::Check { deadline })?;
            cancellation.sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(20)),
            )?;
        }
        Ok(())
    })();
    let elapsed = start.elapsed();
    let released = guard.release();
    match (result, released) {
        (Ok(()), Ok(())) => Ok(elapsed),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("key release failed: {error}")),
        (Err(error), Err(cleanup)) => Err(format!("{error}; key release failed: {cleanup}")),
    }
}

pub(crate) fn receipt(key: Direction, duration_ms: u64, elapsed: Duration) -> String {
    format!(
        "{{\"ok\":true,\"key\":\"{}\",\"duration_ms\":{duration_ms},\"held_ms\":{},\"released\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}}",
        key.browser(),
        elapsed.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn directions_and_duration_are_bounded() {
        assert_eq!(Direction::parse("ArrowUp").unwrap(), Direction::Up);
        assert_eq!(Direction::parse("LEFT").unwrap(), Direction::Left);
        for key in ["", "Control+Up", "a", "Num_Lock"] {
            assert!(Direction::parse(key).is_err());
        }
        assert!(validate_duration(0).is_err());
        assert!(validate_duration(MAX_HOLD_MS + 1).is_err());
        assert!(validate_duration(MAX_HOLD_MS).is_ok());
    }

    #[test]
    fn duration_separates_press_and_release() {
        let mut events = Vec::new();
        run(30, &Cancellation::default(), |event| {
            if matches!(event, Event::Press { .. } | Event::Release { .. }) {
                events.push((matches!(event, Event::Press { .. }), Instant::now()));
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].0 && !events[1].0);
        assert!(events[1].1.duration_since(events[0].1) >= Duration::from_millis(25));
    }

    #[test]
    fn cancellation_releases_without_holding_the_dispatch_lock() {
        let token = Cancellation::default();
        let external = token.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let (pressed, ready) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            run(5_000, &token, |event| {
                if matches!(event, Event::Press { .. }) {
                    recorded.lock().unwrap().push("down");
                    pressed.send(()).unwrap();
                } else if matches!(event, Event::Release { .. }) {
                    recorded.lock().unwrap().push("up");
                }
                Ok(())
            })
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        let start = Instant::now();
        external.cancel();
        assert!(
            worker
                .join()
                .unwrap()
                .unwrap_err()
                .starts_with("cancelled:")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(*events.lock().unwrap(), ["down", "up"]);
    }

    #[test]
    fn changed_target_releases_once() {
        let mut pressed = false;
        let mut released = 0;
        let error = run(500, &Cancellation::default(), |event| {
            match event {
                Event::Press { .. } => pressed = true,
                Event::Release { .. } => released += 1,
                Event::Check { .. } if pressed => return Err("stale_target: moved".into()),
                _ => {}
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.starts_with("stale_target:"));
        assert_eq!(released, 1);
    }

    #[test]
    fn failed_press_attempt_still_releases() {
        let mut released = 0;
        assert!(
            run(500, &Cancellation::default(), |event| {
                match event {
                    Event::Press { .. } => return Err("partial write".into()),
                    Event::Release { .. } => released += 1,
                    _ => {}
                }
                Ok(())
            })
            .unwrap_err()
            .contains("partial write")
        );
        assert_eq!(released, 1);
    }

    #[test]
    fn revoked_permission_releases_owned_key() {
        let allowed = Arc::new(AtomicBool::new(true));
        let access = allowed.clone();
        let token = Cancellation::with_dispatch_guard(move || {
            if access.load(Ordering::Acquire) {
                Ok(Box::new(()))
            } else {
                Err("input permission revoked".into())
            }
        });
        let mut released = false;
        let error = run(500, &token, |event| {
            match event {
                Event::Press { .. } => allowed.store(false, Ordering::Release),
                Event::Release { .. } => released = true,
                _ => {}
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error, "input permission revoked");
        assert!(released);
    }

    #[test]
    fn refusal_before_press_has_no_release_side_effect() {
        let token = Cancellation::default();
        token.cancel();
        assert!(run(30, &token, |_| panic!("cancelled input must not run")).is_err());
        assert!(
            run(0, &Cancellation::default(), |_| panic!(
                "invalid input must not run"
            ))
            .is_err()
        );
    }

    #[test]
    fn slow_permission_acquisition_does_not_press_after_the_deadline() {
        let token = Cancellation::with_dispatch_guard(|| {
            std::thread::sleep(Duration::from_millis(20));
            Ok(Box::new(()))
        });
        let error = run(1, &token, |event| {
            assert!(matches!(event, Event::Check { .. }));
            Ok(())
        })
        .unwrap_err();
        assert!(error.starts_with("timeout:"));
    }
}
