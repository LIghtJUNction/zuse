//! Import the configured travel envelope only after verified GRBL homing.
use super::bounds::{TravelBounds, Workspace};
use super::{Cancellation, Connection, Port, Result, state};
use std::time::Instant;

struct Homing {
    travel: [f64; 3],
    direction: u8,
    pull_off: f64,
    steps: [f64; 3],
}

impl Homing {
    fn read(settings: &std::collections::BTreeMap<u16, f64>) -> Result<Self> {
        let get = |key| {
            settings
                .get(&key)
                .copied()
                .ok_or_else(|| format!("missing GRBL setting ${key}"))
        };
        if get(20)? != 1. || get(22)? != 1. {
            return Err("automatic travel calibration requires firmware soft limits ($20=1) and homing ($22=1); no motion sent".into());
        }
        let pull_off = get(27)?;
        let direction = get(23)?;
        let travel = [get(130)?, get(131)?, get(132)?];
        let steps = [get(100)?, get(101)?, get(102)?];
        if !((0.001..=20.).contains(&pull_off)
            && (0. ..=7.).contains(&direction)
            && direction.fract() == 0.
            && travel.iter().all(|n| *n > pull_off * 2. && *n <= 10_000.)
            && steps.iter().all(|n| (1. ..=100_000.).contains(n)))
        {
            return Err("invalid GRBL travel or homing configuration; no motion sent".into());
        }
        Ok(Self {
            travel,
            direction: direction as u8,
            pull_off,
            steps,
        })
    }

    fn bounds(&self, work: [f64; 3], offset: [f64; 3]) -> Result<TravelBounds> {
        for axis in 0..3 {
            let expected = if self.direction & (1 << axis) == 0 {
                -self.pull_off
            } else {
                -self.travel[axis] + self.pull_off
            };
            let actual = work[axis] + offset[axis];
            if (actual - expected).abs() > (0.51 / self.steps[axis] + 0.002) {
                return Err(
                    "reported position does not match the configured three-axis homing reference"
                        .into(),
                );
            }
        }
        // A small inward margin leaves room for the switches without excluding the pull-off pose.
        let margin = (self.pull_off / 4.).min(0.5);
        let bounds = TravelBounds::new(
            std::array::from_fn(|i| -self.travel[i] - offset[i] + margin),
            offset.map(|n| -n - margin),
        )?;
        bounds.point(work)?;
        Ok(bounds)
    }
}

pub(super) fn home<P: Port>(
    port: &mut P,
    end: Instant,
    cancel: &Cancellation,
) -> Result<Workspace> {
    let mut c = Connection {
        port,
        end,
        cancel,
        pending: Vec::new(),
    };
    let before = c.query_status(true)?;
    if !matches!(state(&before), Some("Idle" | "Alarm")) {
        return Err("automatic travel calibration requires Idle or Alarm; no motion sent".into());
    }
    let homing = Homing::read(&c.read_settings()?)?;
    let identity = c.identify()?;
    if !identity.firmware.starts_with("1.1") {
        return Err("automatic homing is supported only for the verified GRBL 1.1 coordinate convention; no motion sent".into());
    }
    c.check_startup_blocks()?;
    // Homing can change the reference even if it fails or gets cancelled.
    c.port.expect_reset();
    let generation = c.port.generation();
    let motion = c.send(b"$H\n").and_then(|()| c.ack(false));
    if let Err(error) = motion {
        if cancel.is_cancelled() || Instant::now() >= end {
            c.port.expect_reset();
            let stopped = c.port.write(&[0x18], 500).is_ok_and(|sent| sent == 1);
            if !stopped {
                return Err(format!(
                    "homing interrupted: {error}; stop could not be confirmed"
                ));
            }
        }
        return Err(format!(
            "homing interrupted: {error}; verify controller state before retrying"
        ));
    }
    let pose = c.idle_pose(true)?;
    let offset = pose
        .offset
        .ok_or("homing finished without a fresh work offset")?;
    if c.port.generation() != generation {
        return Err("controller reset during homing; bounds were not applied".into());
    }
    let limits = homing.bounds(pose.work, offset)?;
    Ok(Workspace {
        limits,
        identity,
        generation: std::cell::Cell::new(generation),
        offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_settings_refuse_and_reported_position_must_match_all_three_home_axes() {
        let settings = [(20, 0.), (22, 0.)].into();
        assert!(
            Homing::read(&settings)
                .err()
                .unwrap()
                .contains("no motion sent")
        );
        let homing = Homing {
            travel: [300., 200., 40.],
            direction: 1,
            pull_off: 1.,
            steps: [80.; 3],
        };
        assert!(homing.bounds([-299., -1., -1.], [0.; 3]).is_ok());
        assert!(homing.bounds([-299., -1., 0.], [0.; 3]).is_err());
        assert!(homing.bounds([0.; 3], [0.; 3]).is_err());
    }
}
