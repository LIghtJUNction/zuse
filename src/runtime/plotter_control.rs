//! A retained machine connection belongs to the Runtime and its serialized session jobs.
use super::{number, range, text};
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
    Connect { port: String, baud: u32 },
    Status,
    Disconnect,
    Jog { axis: char, delta: f64, feed: f64 },
    Pen { z: f64, feed: f64 },
    Program { lines: Vec<String>, check: bool },
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
            "plotter.status" => (Action::Status, 3000),
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
    port: SerialPort,
}

impl Selected {
    fn envelope(&self, fields: &str) -> String {
        format!(
            r#"{{"connected":true,"port":{},"baud":{},{}"#,
            quote(&self.path),
            self.baud,
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
            old.port.close()?;
        }
        return Ok(disconnected());
    }
    let end = machine_stream::deadline(command.timeout_ms)?;
    if let Action::Connect { port: path, baud } = command.action {
        if let Some(old) = selected.as_mut() {
            if old.port.matches_path(&path)? {
                if old.baud != baud {
                    return Err(
                        "run plotter.disconnect() before changing the selected port's baud rate"
                            .into(),
                    );
                }
                let status = machine_stream::inspect_connected(&mut old.port, end, cancel)?;
                return Ok(old.envelope(&status));
            }
            if old.path == path {
                return Err("the selected path now identifies a different device; run plotter.disconnect() before selecting its replacement".into());
            }
        }
        let mut port = cancel.dispatch(|| SerialPort::open_machine(&path, baud))?;
        let status = machine_stream::inspect_connected(&mut port, end, cancel)?;
        if let Some(mut old) = selected.take() {
            old.port.close()?;
        }
        let entry = Selected { path, baud, port };
        let result = entry.envelope(&status);
        *selected = Some(entry);
        return Ok(result);
    }
    let Some(device) = selected.as_mut() else {
        return if matches!(command.action, Action::Status) {
            Ok(disconnected())
        } else {
            Err("use plotter.connect first".into())
        };
    };
    let result = match command.action {
        Action::Status => machine_stream::inspect_connected(&mut device.port, end, cancel)?,
        Action::Jog { axis, delta, feed } => {
            machine_stream::jog_connected(&mut device.port, axis, delta, feed, end, cancel)?
        }
        Action::Pen { z, feed } => {
            let lines = machine_stream::plotter_lines(&format!("G21 G90\nG1 Z{z} F{feed}"))?;
            machine_stream::program_connected(&mut device.port, &lines, false, end, cancel)?;
            format!(r#"{{"state":"Idle","z_mm":{z},"feed_mm_min":{feed}}}"#)
        }
        Action::Program { lines, check } => {
            let count =
                machine_stream::program_connected(&mut device.port, &lines, check, end, cancel)?;
            let key = if check {
                "checked_lines"
            } else {
                "acknowledged_lines"
            };
            format!(r#"{{"state":"Idle","{key}":{count}}}"#)
        }
        Action::Connect { .. } | Action::Disconnect => unreachable!(),
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
        let job = runtime
            .execute("plotter.jog(axis=\"x\",delta=1)", Options::default())
            .unwrap();
        assert!(failed(&job) && job.contains("use plotter.connect first"));
    }
}
