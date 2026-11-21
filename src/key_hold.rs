//! Lifecycle shared by target-bound desktop and browser key holds.
use crate::observation::Cancellation;
use std::time::{Duration, Instant};

pub(crate) const MAX_MS: i64 = 60_000;
pub(crate) type Result<T> = std::result::Result<T, String>;

pub(crate) trait Target {
    fn verify(&mut self) -> Result<()>;
    /// Mark the attempt inside the dispatch permit, immediately before the write.
    fn press(&mut self, cancel: &Cancellation, attempted: &mut bool) -> Result<()>;
    /// Release only the original target, verifying it again before submission.
    fn release(&mut self) -> Result<()>;
}

pub(crate) fn run(target: &mut impl Target, ms: u64, cancel: &Cancellation) -> Result<()> {
    if !(1..=MAX_MS as u64).contains(&ms) {
        return Err("hold duration must be 1..60000 milliseconds".into());
    }
    cancel.check()?;
    target.verify()?;
    let mut attempted = false;
    let outcome = target.press(cancel, &mut attempted).and_then(|()| {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return cancel.check();
            }
            cancel.sleep(left.min(Duration::from_millis(25)))?;
            if Instant::now() < deadline {
                target.verify()?;
            }
        }
    });
    if !attempted
        || outcome
            .as_ref()
            .is_err_and(|e| e.starts_with("stale_target:"))
    {
        return outcome;
    }
    // The down may already have arrived, even when its acknowledgement was
    // interrupted. Only its matching up is permitted after cancellation.
    match (outcome, target.release()) {
        (result, Ok(())) => result,
        (Ok(()), Err(cleanup)) if cleanup.starts_with("stale_target:") => Err(cleanup),
        (Ok(()), Err(cleanup)) => Err(format!("key release unconfirmed: {cleanup}")),
        (Err(error), Err(cleanup)) => Err(format!("{error}; key release unconfirmed: {cleanup}")),
    }
}
