//! Bounded key input shared by native and browser adapters.
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

/// Native game controls deliberately exclude modifiers, locks and chords.
/// Browser holds keep their existing direction-only contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeKey {
    Direction(Direction),
    Slash,
    Period,
    M,
    N,
}

impl NativeKey {
    pub(crate) fn parse(key: &str) -> Result<Self> {
        match key.to_ascii_lowercase().as_str() {
            "slash" | "/" => Ok(Self::Slash),
            "period" | "." => Ok(Self::Period),
            "m" => Ok(Self::M),
            "n" => Ok(Self::N),
            _ => Direction::parse(key).map(Self::Direction).map_err(|_| {
                "native hold requires Left, Up, Right, Down, slash, period, m or n; modifiers, locks and chords are not supported".into()
            }),
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn native(self) -> &'static str {
        match self {
            Self::Direction(direction) => direction.native(),
            Self::Slash => "slash",
            Self::Period => "period",
            Self::M => "m",
            Self::N => "n",
        }
    }

    #[cfg(target_os = "linux")]
    fn label(self) -> &'static str {
        match self {
            Self::Direction(direction) => direction.browser(),
            Self::Slash => "/",
            Self::Period => ".",
            Self::M => "m",
            Self::N => "n",
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn modifier_keysym(symbol: u32) -> bool {
    matches!(symbol, 0xffe1..=0xffee | 0xfe01..=0xfe13 | 0xff7e | 0xff7f | 0xff14)
}

/// Permission to share the keyboard never transfers ownership of an already
/// pressed target key. Ordinary keys are classified in the actual native map.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn check_pressed_keys(
    allow_other_keys: bool,
    pressed_by_us: bool,
    target_code: u32,
    pressed: impl IntoIterator<Item = u32>,
    ordinary: impl Fn(u32) -> bool,
) -> Result<()> {
    for code in pressed {
        if code == target_code {
            if !pressed_by_us {
                return Err("unsupported_state: requested key is already held; it was not pressed or released by this call".into());
            }
        } else if !allow_other_keys || !ordinary(code) {
            let prefix = if pressed_by_us {
                "stale_target"
            } else {
                "unsupported_state"
            };
            let reason = if allow_other_keys {
                "a modifier, lock, layout or unsupported key is held during the key hold"
            } else {
                "another key is held; release it or explicitly set allow_other_keys=true"
            };
            return Err(format!("{prefix}: {reason}"));
        }
    }
    Ok(())
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

/// A read-only check may stop at the requested key-release time. That is
/// distinct from exhausting its own transport budget or losing a connection.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy)]
pub(crate) struct CheckDeadline {
    at: Instant,
    completes_hold: bool,
}

#[cfg(any(target_os = "linux", test))]
impl CheckDeadline {
    pub(crate) fn transport(at: Instant) -> Self {
        Self {
            at,
            completes_hold: false,
        }
    }

    pub(crate) fn for_hold(hold_end: Instant, transport_end: Instant, pressed_by_us: bool) -> Self {
        Self {
            at: hold_end.min(transport_end),
            completes_hold: pressed_by_us && hold_end <= transport_end,
        }
    }

    /// None ends only an observation whose deadline was the owned key's end.
    pub(crate) fn remaining(self, message: &str) -> Result<Option<Duration>> {
        match self
            .at
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
        {
            Some(left) => Ok(Some(left)),
            None => self.expired(message),
        }
    }

    pub(crate) fn expired<T>(self, message: &str) -> Result<Option<T>> {
        if self.completes_hold && Instant::now() >= self.at {
            Ok(None)
        } else {
            Err(message.into())
        }
    }

    pub(crate) fn io_failure<T>(self, error: std::io::Error, message: &str) -> Result<Option<T>> {
        if matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            self.expired(message)
        } else {
            Err(error.to_string())
        }
    }
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
    receipt_label(key.browser(), duration_ms, elapsed)
}

#[cfg(target_os = "linux")]
pub(crate) fn native_receipt(key: NativeKey, duration_ms: u64, elapsed: Duration) -> String {
    receipt_label(key.label(), duration_ms, elapsed)
}

fn receipt_label(key: &str, duration_ms: u64, elapsed: Duration) -> String {
    format!(
        "{{\"ok\":true,\"key\":\"{}\",\"duration_ms\":{duration_ms},\"held_ms\":{},\"released\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}}",
        key,
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
    fn native_game_keys_are_explicit_without_expanding_browser_controls() {
        for (name, expected) in [
            ("ArrowUp", NativeKey::Direction(Direction::Up)),
            ("slash", NativeKey::Slash),
            ("/", NativeKey::Slash),
            ("period", NativeKey::Period),
            (".", NativeKey::Period),
            ("M", NativeKey::M),
            ("n", NativeKey::N),
        ] {
            assert_eq!(NativeKey::parse(name).unwrap(), expected);
        }
        for key in [
            "Shift",
            "Shift_L",
            "Ctrl",
            "Control_L",
            "Num_Lock",
            "space",
            "w",
            "Up+slash",
            ">",
            "",
        ] {
            assert!(NativeKey::parse(key).is_err(), "{key}");
        }
        for key in ["slash", "period", "m", "n"] {
            assert!(
                Direction::parse(key).is_err(),
                "browser controls stay unchanged"
            );
        }
    }

    #[test]
    fn shared_keyboard_requires_opt_in_and_exclusive_ownership_of_our_key() {
        let ordinary = |code| matches!(code, 25 | 38 | 65 | 111);
        for pressed_by_us in [false, true] {
            assert!(check_pressed_keys(false, pressed_by_us, 111, [25], ordinary).is_err());
            assert!(check_pressed_keys(true, pressed_by_us, 111, [25, 38, 65], ordinary).is_ok());
            assert!(check_pressed_keys(true, pressed_by_us, 111, [25, 50], ordinary).is_err());
        }
        assert!(
            check_pressed_keys(true, false, 111, [25, 111], ordinary)
                .unwrap_err()
                .contains("requested key is already held")
        );
        assert!(check_pressed_keys(true, true, 111, [25, 111], ordinary).is_ok());
    }

    #[test]
    fn concurrent_hold_releases_only_its_key_and_preserves_foreign_keys() {
        use std::collections::BTreeSet;
        let mut keys = BTreeSet::from([25]); // User holds W before the call.
        let mut pressed_by_us = false;
        let mut emitted = Vec::new();
        run(25, &Cancellation::default(), |event| {
            match event {
                Event::Check { .. } => {
                    check_pressed_keys(true, pressed_by_us, 111, keys.iter().copied(), |code| {
                        code == 25
                    })?;
                }
                Event::Press { .. } => {
                    pressed_by_us = true;
                    keys.insert(111);
                    emitted.push((111, true));
                }
                Event::Release { .. } => {
                    keys.remove(&111);
                    emitted.push((111, false));
                }
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(keys, BTreeSet::from([25]));
        assert_eq!(emitted, [(111, true), (111, false)]);

        let mut input = 0;
        let error = run(25, &Cancellation::default(), |event| match event {
            Event::Check { .. } => check_pressed_keys(true, false, 111, [25, 111], |_| true),
            _ => {
                input += 1;
                Ok(())
            }
        })
        .unwrap_err();
        assert!(error.contains("already held"));
        assert_eq!(
            input, 0,
            "rejecting another owner's target cannot send keyup"
        );
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
    fn observation_cut_off_at_hold_end_releases_without_an_extra_check_budget() {
        let mut pressed = None;
        let mut released = None;
        let mut checks = 0;
        run(30, &Cancellation::default(), |event| {
            match event {
                Event::Press { .. } => pressed = Some(Instant::now()),
                Event::Release { .. } => released = Some(Instant::now()),
                Event::Check { deadline } if pressed.is_some() => {
                    checks += 1;
                    let check = CheckDeadline::for_hold(
                        deadline,
                        Instant::now() + Duration::from_secs(1),
                        true,
                    );
                    // Model an IPC read that reaches the key's end before its
                    // peer finishes. The hold end is not an IPC fault.
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    assert!(
                        check
                            .io_failure::<()>(std::io::ErrorKind::TimedOut.into(), "IPC timeout")?
                            .is_none()
                    );
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(checks, 1);
        let held = released.unwrap().duration_since(pressed.unwrap());
        assert!(held >= Duration::from_millis(25));
        assert!(
            held < Duration::from_millis(250),
            "the separate one-second budget must not extend the hold"
        );
    }

    #[test]
    fn hold_cut_off_never_hides_transport_or_prepress_failures() {
        let past = Instant::now() - Duration::from_millis(1);
        let future = Instant::now() + Duration::from_secs(1);
        let ending = CheckDeadline::for_hold(past, future, true);
        assert!(ending.remaining("timeout").unwrap().is_none());
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            assert!(ending.io_failure::<()>(kind.into(), "timeout").is_err());
        }
        let transport = CheckDeadline::for_hold(future, past, true);
        assert_eq!(
            transport.remaining("real transport timeout").unwrap_err(),
            "real transport timeout"
        );
        assert!(
            transport
                .io_failure::<()>(std::io::ErrorKind::TimedOut.into(), "timeout")
                .is_err()
        );
        assert!(
            CheckDeadline::for_hold(past, future, false)
                .remaining("prepress timeout")
                .is_err()
        );
    }

    #[test]
    fn stale_target_received_at_hold_end_still_fails_and_releases() {
        let mut pressed = false;
        let mut releases = 0;
        let error = run(20, &Cancellation::default(), |event| {
            match event {
                Event::Press { .. } => pressed = true,
                Event::Release { .. } => releases += 1,
                Event::Check { deadline } if pressed => {
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    return Err("stale_target: focus changed".into());
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error, "stale_target: focus changed");
        assert_eq!(releases, 1);
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
