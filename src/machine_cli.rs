//! Controller selection is explicit. --dry-run validates without opening a device.
use zuse::{
    json::quote,
    machine_stream, plotter,
    runtime::{Options, Runtime},
};

pub(super) fn run(args: &[&str], options: Options) -> Result<String, String> {
    let usage = "zuse machine probe PORT BAUD | lathe-check FILE | lathe-run FILE ACTIVE_INI | grbl-check|grbl-send FILE PORT BAUD";
    match args {
        [] | ["help"] => Ok(format!("{{\"usage\":{}}}", quote(usage))),
        ["probe", port, baud] => {
            let baud: u32 = baud.parse().map_err(|_| "invalid serial baud rate")?;
            zuse::serial::validate_path(port)?;
            zuse::serial::validate_baud(baud)?;
            if options.dry_run {
                return Ok(r#"{"state":"planned"}"#.into());
            }
            let timeout = deadline(options)?;
            super::with_cancellation(&mut Runtime::new(), |machine| {
                machine_stream::probe_grbl(port, baud, timeout, &machine.cancellation())
            })
        }
        ["lathe-check", file] => preview(&super::read_source(file)?),
        ["lathe-run", file, ini] => {
            let program = super::read_source(file)?;
            let preview = preview(&program)?;
            if options.dry_run {
                return Ok(preview);
            }
            if options.read_only {
                return Err("lathe motion is blocked in read-only mode".into());
            }
            let timeout = deadline(options)?;
            super::with_cancellation(&mut Runtime::new(), |machine| {
                machine_stream::run_linuxcnc(ini, &program, timeout, &machine.cancellation())?;
                Ok("{\"controller\":\"linuxcnc\",\"state\":\"idle\"}".into())
            })
        }
        [action @ ("grbl-send" | "grbl-check"), file, port, baud] => {
            let program = super::read_source(file)?;
            let baud: u32 = baud.parse().map_err(|_| "invalid serial baud rate")?;
            zuse::serial::validate_baud(baud)?;
            zuse::serial::validate_path(port)?;
            if options.dry_run {
                let count = machine_stream::check_grbl(&program)?;
                return Ok(format!(
                    "{{\"controller\":\"grbl\",\"state\":\"planned\",\"lines\":{count}}}"
                ));
            }
            if options.read_only {
                return Err("controller commands require normal execution mode".into());
            }
            let timeout = deadline(options)?;
            super::with_cancellation(&mut Runtime::new(), |machine| {
                let checking = *action == "grbl-check";
                let dispatch = if checking {
                    machine_stream::check_on_grbl
                } else {
                    machine_stream::stream_grbl
                };
                let count = dispatch(port, baud, &program, timeout, &machine.cancellation())?;
                Ok(format!(
                    "{{\"controller\":\"grbl\",\"acknowledged_lines\":{count},\"checked_on_controller\":{checking},\"state\":\"idle\"}}"
                ))
            })
        }
        _ => Err(format!("usage: {usage}")),
    }
}

fn deadline(options: Options) -> Result<u32, String> {
    let millis = options.max_wait_ms.min(300_000) as u32;
    if millis == 0 {
        return Err("controller timeout must be positive".into());
    }
    Ok(millis)
}

fn preview(program: &str) -> Result<String, String> {
    let report = plotter::lathe_preview(program)?;
    Ok(format!(
        r#"{{"controller":"linuxcnc","state":"planned","preview":{}}}"#,
        quote(&report)
    ))
}
