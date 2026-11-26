//! A retained machine connection belongs to the Runtime and its serialized session jobs.
use super::{boolean, number, range, text};
use crate::{
    json::quote,
    language::Call,
    machine_stream,
    observation::Cancellation,
    serial::{self, SerialPort},
};

type Result<T> = std::result::Result<T, String>;

const MAX_TIMEOUT_MS: i64 = 300_000;

pub(super) struct Command {
    action: Action,
    pub timeout_ms: u32,
}

enum Action {
    Connect {
        port: String,
        baud: u32,
    },
    Status {
        work: bool,
        offset: bool,
    },
    Bounds,
    SetBounds {
        limits: machine_stream::TravelBounds,
        expected: Option<[f64; 3]>,
        expected_generation: Option<u64>,
    },
    ClearBounds,
    Disconnect,
    Jog {
        axis: char,
        delta: f64,
        feed: f64,
    },
    Pen {
        z: f64,
        feed: f64,
    },
    Program {
        lines: Vec<String>,
        check: bool,
    },
}

impl Command {
    pub(super) fn prepare(call: &Call<'_>) -> Result<Self> {
        let (action, default_ms) = match call.name {
            "plotter.connect" => {
                let port = text(call, "port", "")?;
                serial::validate_path(&port)?;
                let baud = range(call, "baud", 115200, 1, 230400)? as u32;
                serial::validate_baud(baud)?;
                (Action::Connect { port, baud }, 5000)
            }
            "plotter.status" => {
                let offset = boolean(call, "offset", false);
                let work = boolean(call, "work", false) || offset;
                (
                    Action::Status { work, offset },
                    if work { 8000 } else { 3000 },
                )
            }
            "plotter.bounds" | "plotter.clear_bounds" => {
                return Ok(Self {
                    action: if call.name == "plotter.bounds" {
                        Action::Bounds
                    } else {
                        Action::ClearBounds
                    },
                    timeout_ms: 0,
                });
            }
            "plotter.set_bounds" => {
                let min = ["x_min", "y_min", "z_min"].map(|k| number(call, k, f64::NAN));
                let max = ["x_max", "y_max", "z_max"].map(|k| number(call, k, f64::NAN));
                let limits = machine_stream::TravelBounds::new(min, max)?;
                let keys = ["wco_x", "wco_y", "wco_z"];
                let expected = if keys.iter().any(|k| call.get(k).is_some()) {
                    let wco = keys.map(|k| number(call, k, f64::NAN));
                    if !wco.iter().all(|n| n.is_finite()) {
                        return Err(
                            "expected work origin requires finite wco_x, wco_y, wco_z together"
                                .into(),
                        );
                    }
                    Some(wco)
                } else {
                    None
                };
                let expected_generation = if call.get("expected_generation").is_some() {
                    Some(range(call, "expected_generation", 0, 0, i64::MAX)? as u64)
                } else {
                    None
                };
                (
                    Action::SetBounds {
                        limits,
                        expected,
                        expected_generation,
                    },
                    8000,
                )
            }
            "plotter.disconnect" => {
                return Ok(Self {
                    action: Action::Disconnect,
                    timeout_ms: 0,
                });
            }
            "plotter.jog" => {
                let axis = match text(call, "axis", "")?.as_str() {
                    "x" => 'X',
                    "y" => 'Y',
                    "z" => 'Z',
                    _ => return Err("plotter.jog axis must be x, y or z".into()),
                };
                let delta = number(call, "delta", 0.);
                if !delta.is_finite() || delta == 0. || delta.abs() > 10. {
                    return Err("plotter.jog delta must be nonzero and within ±10 mm".into());
                }
                let feed = feed(call)?;
                let travel_ms = (delta.abs() * 60_000.0 / feed).ceil();
                if travel_ms >= MAX_TIMEOUT_MS as f64 && call.get("timeout").is_none() {
                    return Err("plotter.jog travel reaches the 300 s deadline; shorten the move or increase the feed".into());
                }
                // Leave time for the initial queries, acceleration and final Idle report.
                let default_ms =
                    (travel_ms + 5_000.0).clamp(15_000.0, MAX_TIMEOUT_MS as f64) as i64;
                (Action::Jog { axis, delta, feed }, default_ms)
            }
            "plotter.pen" => {
                let z = number(call, "z", f64::NAN);
                if !z.is_finite() || !(z as f32).is_finite() {
                    return Err("plotter.pen requires a finite GRBL Z coordinate".into());
                }
                // Offline preflight includes GRBL's block length and numeric dialect.
                let feed = feed(call)?;
                machine_stream::check_grbl(&format!("G21 G90\nG1 Z{z} F{feed}"))?;
                (Action::Pen { z, feed }, 10000)
            }
            "plotter.send" | "plotter.check" => {
                let lines = machine_stream::plotter_lines(&text(call, "program", "")?)?;
                (
                    Action::Program {
                        lines,
                        check: call.name == "plotter.check",
                    },
                    30000,
                )
            }
            _ => return Err("unknown plotter command".into()),
        };
        Ok(Self {
            action,
            timeout_ms: range(call, "timeout", default_ms, 1, MAX_TIMEOUT_MS)? as u32,
        })
    }
}

fn feed(call: &Call<'_>) -> Result<f64> {
    let value = number(call, "feed", 100.);
    if !value.is_finite() || (value as f32) <= 0. || value > 500. {
        return Err("plotter feed must be greater than zero and at most 500 mm/min".into());
    }
    Ok(value)
}

pub(super) struct Selected {
    path: String,
    baud: u32,
    port: machine_stream::WatchedPort<SerialPort>,
    probe: machine_stream::ProbeEvidence,
    bounds: Option<machine_stream::Workspace>,
}

impl Selected {
    fn envelope(&self, fields: &str) -> String {
        let stale = if self
            .bounds
            .as_ref()
            .is_some_and(|b| !b.is_current(self.port.generation()))
        {
            r#","bounds_stale":true"#
        } else {
            ""
        };
        format!(
            r#"{{"connected":true,"port":{},"baud":{},"reference_generation":{}{stale},{}"#,
            quote(&self.path),
            self.baud,
            self.port.reference_generation(),
            &fields[1..]
        )
    }
}

pub(super) fn execute(
    selected: &mut Option<Selected>,
    command: Command,
    cancel: &Cancellation,
) -> Result<String> {
    let disconnected = || r#"{"connected":false}"#.to_owned();
    if let Action::Disconnect = command.action {
        if let Some(mut old) = selected.take() {
            old.port.inner.close()?;
        }
        return Ok(disconnected());
    }
    if matches!(command.action, Action::Bounds | Action::ClearBounds) {
        let Some(device) = selected.as_mut() else {
            return Ok(r#"{"connected":false,"bounds":null}"#.into());
        };
        if matches!(command.action, Action::ClearBounds) {
            device.bounds = None;
        }
        let fields = device.bounds.as_ref().map_or_else(
            || r#"{"bounds":null}"#.into(),
            machine_stream::Workspace::receipt,
        );
        return Ok(device.envelope(&fields));
    }
    let end = machine_stream::deadline(command.timeout_ms)?;
    if let Action::Connect { port: path, baud } = command.action {
        if let Some(old) = selected.as_mut() {
            if old.port.inner.matches_path(&path)? {
                if old.baud != baud {
                    return Err(
                        "run plotter.disconnect() before changing the selected port's baud rate"
                            .into(),
                    );
                }
                let status = machine_stream::inspect_connected(
                    &mut old.port,
                    end,
                    cancel,
                    false,
                    false,
                    &mut old.probe,
                )?;
                return Ok(old.envelope(&status));
            }
            if old.path == path {
                return Err("the selected path now identifies a different device; run plotter.disconnect() before selecting its replacement".into());
            }
        }
        let mut port = machine_stream::WatchedPort::new(
            cancel.dispatch(|| SerialPort::open_machine(&path, baud))?,
        );
        let mut probe = machine_stream::ProbeEvidence::default();
        let status =
            machine_stream::inspect_connected(&mut port, end, cancel, false, false, &mut probe)?;
        if let Some(mut old) = selected.take() {
            old.port.inner.close()?;
        }
        let entry = Selected {
            path,
            baud,
            port,
            probe,
            bounds: None,
        };
        let result = entry.envelope(&status);
        *selected = Some(entry);
        return Ok(result);
    }
    let Some(device) = selected.as_mut() else {
        return match command.action {
            Action::Status {work:true,..} => Ok(r#"{"connected":false,"work_position":null,"pressure":null,"probe_triggered":null,"probe_seen_active":false}"#.into()),
            Action::Status {work:false,..} => Ok(disconnected()),
            _ => Err("use plotter.connect first".into()),
        };
    };
    let result = match command.action {
        Action::SetBounds {
            limits,
            expected,
            expected_generation,
        } => {
            let bounds = machine_stream::bind_workspace(
                &mut device.port,
                limits,
                expected,
                expected_generation,
                end,
                cancel,
            )?;
            let receipt = bounds.receipt();
            device.bounds = Some(bounds);
            receipt
        }
        Action::Status { work, offset } => machine_stream::inspect_connected(
            &mut device.port,
            end,
            cancel,
            work,
            offset,
            &mut device.probe,
        )?,
        Action::Jog { axis, delta, feed } => machine_stream::jog_connected(
            &mut device.port,
            axis,
            delta,
            feed,
            end,
            cancel,
            device.bounds.as_ref(),
        )?,
        Action::Pen { z, feed } => {
            let lines = machine_stream::plotter_lines(&format!("G21 G90\nG1 Z{z} F{feed}"))?;
            machine_stream::program_connected(
                &mut device.port,
                &lines,
                false,
                end,
                cancel,
                device.bounds.as_ref(),
            )?;
            format!(r#"{{"state":"Idle","z_mm":{z},"feed_mm_min":{feed}}}"#)
        }
        Action::Program { lines, check } => {
            let count = machine_stream::program_connected(
                &mut device.port,
                &lines,
                check,
                end,
                cancel,
                device.bounds.as_ref(),
            )?;
            let key = if check {
                "checked_lines"
            } else {
                "acknowledged_lines"
            };
            format!(r#"{{"state":"Idle","{key}":{count}}}"#)
        }
        Action::Connect { .. } | Action::Disconnect | Action::Bounds | Action::ClearBounds => {
            unreachable!()
        }
    };
    Ok(device.envelope(&result))
}

#[cfg(test)]
mod tests {
    use crate::runtime::{Options, Runtime, check, failed};
    #[test]
    fn jog_deadlines_cover_travel_and_respect_the_explicit_timeout() {
        let timeout = |source: &str| {
            let call = crate::language::Parser::new(source)
                .next()
                .unwrap()
                .unwrap();
            super::Command::prepare(&call).map(|command| command.timeout_ms)
        };
        assert_eq!(
            timeout(r#"plotter.jog(axis="y",delta=-10,feed=40)"#),
            Ok(20_000)
        );
        assert_eq!(
            timeout(r#"plotter.jog(axis="x",delta=1,feed=100)"#),
            Ok(15_000)
        );
        assert_eq!(
            timeout(r#"plotter.jog(axis="x",delta=4.95,feed=1)"#),
            Ok(300_000)
        );
        assert!(
            timeout(r#"plotter.jog(axis="x",delta=10,feed=1)"#)
                .unwrap_err()
                .contains("shorten")
        );
        assert_eq!(
            timeout(r#"plotter.jog(axis="x",delta=10,feed=1,timeout=9000)"#),
            Ok(9_000)
        );
    }

    #[test]
    fn bounds_are_explicit_and_application_preflights_without_motion() {
        let set = "plotter.set_bounds(x_min=0,x_max=10,y_min=0,y_max=8,z_min=-2,z_max=5";
        let dry = Options {
            dry_run: true,
            ..Default::default()
        };
        let read = Options {
            read_only: true,
            ..dry
        };
        assert!(check("plotter.bounds()", read).is_ok());
        assert!(check("plotter.clear_bounds()", read).is_err());
        assert!(check(&format!("{set})"), read).is_err());
        assert!(check(&format!("{set},wco_x=1)"), dry).is_err());
        assert!(check(&format!("{set},expected_generation=-1)"), dry).is_err());
        assert!(check(&format!("{set},expected_generation=42)"), dry).is_ok());
        assert!(check(&format!("{set},wco_x=1,wco_y=2,wco_z=3)"), dry).is_ok());
        assert!(check(&format!("{set})").replace("x_max=10", "x_max=0"), dry).is_err());
        assert!(check("plotter.status(offset=true)", read).is_ok());
        let observed = Runtime::new()
            .execute("plotter.bounds()", Options::default())
            .unwrap();
        assert!(observed.contains(r#""connected":false,"bounds":null"#));
    }

    #[test]
    fn offline_dry_run_and_full_program_budget_precede_serial_io() {
        let dry = Options {
            dry_run: true,
            ..Default::default()
        };
        assert!(check("plotter.jog(axis=\"x\",delta=2,feed=200)", dry).is_ok());
        assert!(check("plotter.send(program=\"M3\")", dry).is_err());
        assert!(check("plotter.jog(axis=\"y\",delta=12)", dry).is_err());
        assert!(check("plotter.pen(z=0,feed=502)", dry).is_err());
        assert!(
            check(
                "plotter.status(timeout=4);plotter.status(timeout=4)",
                Options {
                    max_wait_ms: 7,
                    ..dry
                }
            )
            .is_err()
        );
        assert!(
            check(
                "plotter.status()",
                Options {
                    read_only: true,
                    ..dry
                }
            )
            .is_ok()
        );
        assert!(
            check(
                "plotter.disconnect()",
                Options {
                    read_only: true,
                    ..dry
                }
            )
            .is_err()
        );
        assert!(
            check(
                "plotter.check(program=\"G21 G90\\nG1 X1 F10\")",
                Options {
                    read_only: true,
                    ..dry
                }
            )
            .is_err()
        );
        let mut runtime = Runtime::new();
        let status = runtime
            .execute("plotter.status()", Options::default())
            .unwrap();
        assert!(!failed(&status) && status.contains(r#""connected":false"#));
        let calibration = runtime
            .execute(
                "plotter.status(work=true,timeout=100)",
                Options {
                    read_only: true,
                    max_wait_ms: 100,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(calibration.contains(
            r#""connected":false,"work_position":null,"pressure":null,"probe_triggered":null"#
        ));
        assert!(
            check(
                "plotter.status(work=true)",
                Options {
                    dry_run: true,
                    max_wait_ms: 7999,
                    ..Default::default()
                }
            )
            .is_err()
        );
        let job = runtime
            .execute("plotter.jog(axis=\"x\",delta=1)", Options::default())
            .unwrap();
        assert!(failed(&job) && job.contains("use plotter.connect first"));
    }
}
