//! Inclusive limits taught in work coordinates on one controller and work origin.
use super::{Connection, Identity, Port, Positions, QUERY_INTERVAL, Result, state};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) struct TravelBounds {
    min: [f64; 3],
    max: [f64; 3],
}

impl TravelBounds {
    pub(crate) fn new(min: [f64; 3], max: [f64; 3]) -> Result<Self> {
        for i in 0..3 {
            if !(min[i].is_finite() && max[i].is_finite() && min[i] < max[i]) {
                return Err(
                    "work bounds require finite, strictly ordered min and max for X, Y and Z"
                        .into(),
                );
            }
        }
        Ok(Self { min, max })
    }

    fn point(&self, point: [f64; 3]) -> Result<()> {
        for (axis, value) in point.into_iter().enumerate() {
            if !value.is_finite() || value < self.min[axis] || value > self.max[axis] {
                return Err(format!(
                    "{}={value} mm is outside taught bounds [{}..{}] mm",
                    ["X", "Y", "Z"][axis],
                    self.min[axis],
                    self.max[axis]
                ));
            }
        }
        Ok(())
    }

    fn program(&self, lines: &[String], mut position: [f64; 3]) -> Result<()> {
        self.point(position)
            .map_err(|e| format!("current position: {e}"))?;
        // plotter_lines has accepted only absolute, millimetre G0/G1 blocks. Omitted axes
        // keep their last values. The axis-aligned box contains the segment between its endpoints.
        for (index, line) in lines.iter().enumerate() {
            for (addr, word) in super::words(line) {
                let axis = match addr {
                    b'X' => 0,
                    b'Y' => 1,
                    b'Z' => 2,
                    _ => continue,
                };
                position[axis] = word
                    .parse::<f64>()
                    .map_err(|_| "invalid plotter coordinate")?;
            }
            self.point(position)
                .map_err(|e| format!("plotter block {}: {e}", index + 1))?;
        }
        Ok(())
    }
}

pub(crate) struct Workspace {
    limits: TravelBounds,
    identity: Identity,
    generation: std::cell::Cell<u64>,
    offset: [f64; 3],
}

fn same_offset(first: [f64; 3], second: [f64; 3]) -> bool {
    first
        .into_iter()
        .zip(second)
        .all(|(a, b)| (a - b).abs() <= 1e-9)
}

impl Workspace {
    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.generation.get() == generation
    }
    pub(super) fn require_generation(&self, generation: u64) -> Result<()> {
        if self.is_current(generation) {
            Ok(())
        } else {
            Err("controller reset observed; taught bounds remain installed but stale; physically re-reference and reapply bounds".into())
        }
    }
    pub(crate) fn receipt(&self) -> String {
        let [x0, y0, z0] = self.limits.min;
        let [x1, y1, z1] = self.limits.max;
        format!(
            r#"{{"bounds":{{"x_min":{x0},"x_max":{x1},"y_min":{y0},"y_max":{y1},"z_min":{z0},"z_max":{z1}}},"work_offset":{:?}}}"#,
            self.offset
        )
    }

    fn verify(&self, identity: &Identity, pose: &Pose) -> Result<()> {
        if self.identity != *identity {
            return Err("the selected controller identity changed; teach bounds again".into());
        }
        if !pose.offset.is_some_and(|o| same_offset(o, self.offset)) {
            return Err(
                "work origin changed or unavailable; teach or reapply bounds in the verified frame"
                    .into(),
            );
        }
        self.limits
            .point(pose.work)
            .map_err(|e| format!("current position: {e}"))
    }

    pub(super) fn jog(
        &self,
        identity: &Identity,
        pose: &Pose,
        axis: usize,
        target: f64,
    ) -> Result<()> {
        self.verify(identity, pose)?;
        let mut next = pose.work;
        next[axis] = target;
        self.limits.point(next)
    }
}

pub(super) struct Pose {
    pub(super) work: [f64; 3],
    pub(super) offset: Option<[f64; 3]>,
}

impl Positions {
    pub(super) fn offset_xyz(&self) -> Option<[f64; 3]> {
        if let Some(offset) = &self.offset {
            return Some([offset[0], offset[1], offset[2]]);
        }
        let (m, w) = (self.machine.as_ref()?, self.work.as_ref()?);
        Some(std::array::from_fn(|i| m[i] - w[i]))
    }
}

impl<P: Port> Connection<'_, P> {
    pub(super) fn idle_pose(&mut self, require_offset: bool) -> Result<Pose> {
        let scale = self.report_scale()?;
        let mut attempts = 0;
        loop {
            let frame = self.query_status(false)?;
            if state(&frame) != Some("Idle") {
                return Err("controller left Idle before motion or teaching".into());
            }
            let coords = Positions::parse(&frame)?;
            let offset = coords.offset_xyz().map(|v| v.map(|x| x * scale));
            if let (Some(x), Some(y), Some(z)) = (
                coords.work_axis(0),
                coords.work_axis(1),
                coords.work_axis(2),
            ) && (!require_offset || offset.is_some())
            {
                let work = [x * scale, y * scale, z * scale];
                if !work
                    .into_iter()
                    .chain(offset.into_iter().flatten())
                    .all(f64::is_finite)
                {
                    return Err("non-finite controller metric position".into());
                }
                return Ok(Pose { work, offset });
            }
            attempts += 1;
            let left = self.end.saturating_duration_since(Instant::now());
            if left <= QUERY_INTERVAL || (!require_offset && attempts >= 32) {
                return Err(if require_offset {
                    "fresh work XYZ and work offset unavailable; taught travel cannot be verified"
                } else {
                    "fresh WPos or matching MPos/WCO not reported; no jog sent"
                }
                .into());
            }
            self.cancel.sleep(QUERY_INTERVAL)?;
        }
    }

    pub(super) fn enforce_program(
        &mut self,
        bounds: Option<&Workspace>,
        id: &Identity,
        lines: &[String],
    ) -> Result<Option<Pose>> {
        if let Some(bounds) = bounds {
            bounds.require_generation(self.port.generation())?;
            let pose = self.idle_pose(true)?;
            bounds.require_generation(self.port.generation())?;
            bounds.verify(id, &pose)?;
            bounds.limits.program(lines, pose.work)?;
            return Ok(Some(pose));
        }
        Ok(None)
    }

    pub(super) fn verify_check_reference(
        &mut self,
        guard: &Workspace,
        id: &Identity,
        before: &Pose,
        old: u64,
    ) -> Result<()> {
        guard.require_generation(old)?;
        let after = self.port.generation();
        // One host-initiated exit can produce zero or one banner in the verified transition.
        if !(old + 1..=old + 2).contains(&after) {
            return Err("unexpected controller resets during Check".into());
        }
        if self.identify()? != *id {
            return Err("controller identity changed across Check".into());
        }
        let pose = self.idle_pose(true)?;
        guard.verify(id, &pose)?;
        if !same_offset(pose.work, before.work) || self.port.generation() != after {
            return Err("position or boot generation changed across Check".into());
        }
        guard.generation.set(after);
        Ok(())
    }
}

pub(super) fn bind<P: Port>(
    port: &mut P,
    limits: TravelBounds,
    expected: Option<[f64; 3]>,
    end: Instant,
    cancel: &super::Cancellation,
) -> Result<Workspace> {
    let mut c = Connection {
        port,
        end,
        cancel,
        pending: Vec::new(),
    };
    c.require_state("Idle", true)?;
    let generation = c.port.generation();
    let identity = c.identify()?;
    let pose = c.idle_pose(true)?;
    if generation != c.port.generation() {
        return Err("controller restarted during reference acquisition; physically re-reference and reapply bounds".into());
    }
    let offset = pose
        .offset
        .ok_or("work offset unavailable; bounds not applied")?;
    if expected.is_some_and(|e| !same_offset(e, offset)) {
        return Err(
            "current work origin does not match the taught reference; bounds not applied".into(),
        );
    }
    Ok(Workspace {
        limits,
        identity,
        generation: std::cell::Cell::new(generation),
        offset,
    })
}
