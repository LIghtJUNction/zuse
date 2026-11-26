//! GRBL and BtWriter simple send-response for generated XY pen programs on a dedicated serial port.
use crate::{device_io::ReadResult, json::quote, observation::Cancellation, serial::SerialPort};
use std::time::{Duration, Instant};

#[path = "linuxcnc.rs"]
mod linuxcnc;
pub use linuxcnc::run_linuxcnc;

/// Validate the complete pen program without opening a serial port.
pub fn check_grbl(program: &str) -> std::result::Result<usize, String> {
    plotter_lines(program).map(|lines| lines.len())
}

type Result<T> = std::result::Result<T, String>;
const MAX_PROGRAM: usize = 1024 * 1024;
const MAX_LINES: usize = 65_536;
const QUERY_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) fn deadline(timeout_ms: u32) -> Result<Instant> {
    if !(1..=300_000).contains(&timeout_ms) {
        return Err("controller deadline must be in 1..300000 milliseconds".into());
    }
    Ok(Instant::now() + Duration::from_millis(timeout_ms.into()))
}

trait Port {
    fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize>;
    fn read(&mut self, timeout_ms: u32) -> Result<ReadResult>;
}
impl Port for SerialPort {
    fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize> {
        SerialPort::write(self, bytes, timeout_ms)
    }
    fn read(&mut self, timeout_ms: u32) -> Result<ReadResult> {
        SerialPort::read(self, 256, timeout_ms)
    }
}

/// The input is the XY plotter export. Validate the entire stream before opening the port.
/// Idle is a controller receipt. A timeout or cancellation never retries a motion.
pub fn stream_grbl(
    path: &str,
    baud: u32,
    program: &str,
    timeout_ms: u32,
    cancel: &Cancellation,
) -> Result<usize> {
    let end = deadline(timeout_ms)?;
    let lines = plotter_lines(program)?;
    let mut port = cancel.dispatch(|| SerialPort::open_machine(path, baud))?;
    let outcome = transmit(&mut port, &lines, end, cancel);
    let close = port.close();
    match (outcome, close) {
        (Ok(n), Ok(())) => Ok(n),
        (Ok(n), Err(e)) => Err(format!(
            "{n} program lines acknowledged and Idle verified; serial close failed: {e}"
        )),
        (Err(e), _) => Err(e),
    }
}

/// Query controller status and build identity. No motion, configuration, reset, or unlock.
pub fn probe_grbl(path: &str, baud: u32, timeout_ms: u32, cancel: &Cancellation) -> Result<String> {
    let end = deadline(timeout_ms)?;
    let mut port = cancel.dispatch(|| SerialPort::open_machine(path, baud))?;
    let result = inspect_port(&mut port, end, cancel, false);
    let closed = port.close();
    let json = result?;
    closed?;
    Ok(json)
}

pub(crate) fn inspect_connected(
    port: &mut SerialPort,
    end: Instant,
    cancel: &Cancellation,
) -> Result<String> {
    inspect_port(port, end, cancel, true)
}

pub(crate) fn program_connected(
    port: &mut SerialPort,
    lines: &[String],
    check: bool,
    end: Instant,
    cancel: &Cancellation,
) -> Result<usize> {
    if check {
        check_on_port(port, lines, end, cancel)
    } else {
        transmit(port, lines, end, cancel)
    }
}

fn inspect_port<P: Port>(
    port: &mut P,
    end: Instant,
    cancel: &Cancellation,
    normalize: bool,
) -> Result<String> {
    let mut c = Connection {
        port,
        end,
        cancel,
        pending: Vec::new(),
    };
    let mut frame = c.query_status(true)?;
    let firmware = c.identify()?;
    let unit = if normalize {
        let unit = c.report_scale()?;
        frame = c.query_status(false)?;
        Some(unit)
    } else {
        None
    };
    let (state, fields) = frame[1..frame.len() - 1]
        .split_once('|')
        .ok_or("malformed GRBL status")?;
    let mut result = format!(
        r#"{{"controller":{},"firmware":{},"state":{}"#,
        quote(if firmware == "1.3a.20221114" {
            "BtWriter"
        } else {
            "grbl"
        }),
        quote(&firmware),
        quote(state)
    );
    if let Some(scale) = unit {
        result.push_str(&format!(
            r#","units":"mm","report_units":"{}""#,
            if scale == 1. { "mm" } else { "inch" }
        ));
    } else {
        result.push_str(r#","units":"controller_report""#);
    }
    let coords = Positions::parse(&frame)?;
    for (key, coords) in [
        ("machine_position", coords.machine.as_deref()),
        ("work_position", coords.work.as_deref()),
        ("work_offset", coords.offset.as_deref()),
    ] {
        if let Some(values) = coords {
            let mut xyz = values.to_vec();
            if let Some(scale) = unit {
                xyz.truncate(3); // GUI exposes linear X/Y/Z, optional rotary axes have other units.
                for n in &mut xyz {
                    *n *= scale;
                }
                if !xyz.iter().all(|n| n.is_finite()) {
                    return Err("GRBL metric coordinates overflow".into());
                }
            }
            result.push_str(&format!(r#",{}:{xyz:?}"#, quote(key)));
        }
    }
    if unit.is_some()
        && coords.work.is_none()
        && let (Some(m), Some(o)) = (&coords.machine, &coords.offset)
    {
        let xyz: Vec<f64> = (0..3).map(|i| (m[i] - o[i]) * unit.unwrap_or(1.)).collect();
        if !xyz.iter().all(|n| n.is_finite()) {
            return Err("GRBL work position overflow".into());
        }
        result.push_str(&format!(r#","work_position":{xyz:?}"#));
    }
    for f in fields.split('|') {
        if let Some(pins) = f.strip_prefix("Pn:")
            && pins.bytes().all(|b| b.is_ascii_uppercase())
        {
            result.push_str(&format!(r#","pins":{}"#, quote(pins)));
        }
    }
    result.push('}');
    Ok(result)
}

/// Parse a locally validated plot on the controller in GRBL Check mode.
/// Leaving Check makes the firmware soft-reset. The host never transmits a reset or unlock.
/// A successful receipt includes a fresh Idle report after that transition.
pub fn check_on_grbl(
    path: &str,
    baud: u32,
    program: &str,
    timeout_ms: u32,
    cancel: &Cancellation,
) -> Result<usize> {
    let end = deadline(timeout_ms)?;
    let lines = plotter_lines(program)?;
    let mut port = cancel.dispatch(|| SerialPort::open_machine(path, baud))?;
    let result = check_on_port(&mut port, &lines, end, cancel);
    let close = port.close();
    let count = result?;
    close.map_err(|e| {
        format!("{count} blocks checked and Idle verified; serial close failed: {e}")
    })?;
    Ok(count)
}

fn check_on_port<P: Port>(
    port: &mut P,
    lines: &[String],
    end: Instant,
    cancel: &Cancellation,
) -> Result<usize> {
    let mut c = Connection {
        port,
        end,
        cancel,
        pending: Vec::new(),
    };
    c.require_state("Idle", true)?;
    c.identify()?;
    c.check_startup_blocks()?;
    c.require_state("Idle", false)?;
    c.send(b"$C\n").and_then(|()| c.ack(false)).map_err(|e| {
        format!("Check entry not acknowledged; no program blocks sent, no toggle replayed: {e}")
    })?;

    let mut accepted = 0;
    let mut attempted = 0;
    let blocks = (|| {
        for line in lines {
            c.require_state("Check", false)?;
            attempted += 1;
            c.send(format!("{line}\n").as_bytes())?;
            c.checked_ack()?;
            accepted += 1;
        }
        Ok::<(), String>(())
    })();
    // This fresh status is required even after a block failed. A stale observation
    // or a timed-out/cancelled query never licenses another toggle.
    let exit = c
        .require_state("Check", false)
        .and_then(|()| c.leave_check());
    match (blocks, exit) {
        (Ok(()), Ok(())) => Ok(accepted),
        (Ok(()), Err(e)) => Err(format!(
            "{accepted} blocks accepted in Check; final Idle unverified: {e}"
        )),
        (Err(e), restore) => {
            let outcome = if attempted > accepted {
                "unconfirmed"
            } else {
                "not sent"
            };
            let restoration = match restore {
                Ok(()) => "restored Idle".to_owned(),
                Err(reason) => format!("controlled return to Idle unverified: {reason}"),
            };
            Err(format!(
                "{accepted} blocks accepted in Check; block {} {outcome}: {e}; {restoration}",
                accepted + 1
            ))
        }
    }
}

// Strip comments before transmission: GRBL recognizes real-time characters even in comments.
pub(crate) fn plotter_lines(program: &str) -> Result<Vec<String>> {
    if program.len() > MAX_PROGRAM {
        return Err("plotter program exceeds 1 MiB".into());
    }
    let mut lines = Vec::new();
    let (mut mm, mut absolute, mut moved, mut has_feed) = (false, false, false, false);
    for (index, raw) in program.lines().enumerate() {
        let mut comment = false;
        let mut code = String::new();
        for c in raw.trim().chars() {
            match c {
                ';' if !comment => break,
                '(' if !comment => comment = true,
                ')' if comment => comment = false,
                '(' | ')' => return Err(format!("malformed comment on line {}", index + 1)),
                _ if comment => {}
                ' ' | '\t' => {}
                'A'..='Z' | '0'..='9' | '-' | '+' | '.' => code.push(c),
                _ => return Err(format!("invalid character on plotter line {}", index + 1)),
            }
        }
        if comment {
            return Err(format!("unclosed comment on line {}", index + 1));
        }
        if code.is_empty() {
            continue;
        }
        if lines.len() == MAX_LINES || code.len() > 79 {
            return Err("GRBL plotter line count or 79-character block limit exceeded".into());
        }
        let mut seen = [false; 9];
        let mut cutting_feed = false;
        let mut block_motion = false;
        let mut axes = false;
        let mut rest = code.as_str();
        while !rest.is_empty() {
            let addr = rest.as_bytes()[0];
            let tail = &rest[1..];
            let next = tail
                .find(|c: char| c.is_ascii_uppercase())
                .unwrap_or(tail.len());
            let value = &tail[..next];
            let group = match (addr, value) {
                (b'G', "21") => {
                    mm = true;
                    0
                }
                (b'G', "90") => {
                    absolute = true;
                    1
                }
                (b'G', "94") => 2,
                (b'G', "17") => 3,
                (b'G', "0" | "00" | "1" | "01") => {
                    block_motion = true;
                    cutting_feed = matches!(value, "1" | "01");
                    4
                }
                (b'X' | b'Y' | b'Z' | b'F', v) => {
                    let number = v.parse::<f32>().map_err(|_| {
                        format!("invalid coordinate/feed on plotter line {}", index + 1)
                    })?;
                    if !number.is_finite() || (addr == b'F' && number <= 0.) {
                        return Err(format!(
                            "invalid coordinate/feed on plotter line {}",
                            index + 1
                        ));
                    }
                    axes |= addr != b'F';
                    has_feed |= addr == b'F';
                    match addr {
                        b'X' => 5,
                        b'Y' => 6,
                        b'Z' => 7,
                        _ => 8,
                    }
                }
                (b'M', "2" | "30") if code == "M2" || code == "M30" => 0,
                _ => return Err(format!("unsupported GRBL pen word on line {}", index + 1)),
            };
            if seen[group] {
                return Err(format!(
                    "duplicate/conflicting GRBL word on line {}",
                    index + 1
                ));
            }
            seen[group] = true;
            rest = &tail[next..];
        }
        if axes && !(block_motion && mm && absolute) {
            return Err(format!(
                "plotter line {} requires explicit G0/G1, G21 and G90",
                index + 1
            ));
        }
        if axes && cutting_feed && !has_feed {
            return Err(format!(
                "G1 on line {} requires a positive program feed",
                index + 1
            ));
        }
        moved |= axes;
        lines.push(code);
    }
    if !moved {
        return Err("empty GRBL pen trajectory".into());
    }
    if lines
        .iter()
        .take(lines.len() - 1)
        .any(|s| s == "M2" || s == "M30")
    {
        return Err("GRBL program end precedes additional commands".into());
    }
    Ok(lines)
}

struct Connection<'a, P> {
    port: &'a mut P,
    end: Instant,
    cancel: &'a Cancellation,
    pending: Vec<u8>,
}
impl<P: Port> Connection<'_, P> {
    fn time(&self) -> Result<u32> {
        self.cancel.check()?;
        let left = self.end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("total controller deadline expired".into());
        }
        Ok(left.as_millis().clamp(1, 250) as u32)
    }
    fn send(&mut self, data: &[u8]) -> Result<()> {
        let ms = self.time()?;
        let count = self.cancel.dispatch(|| self.port.write(data, ms))?;
        if count != data.len() {
            return Err(format!("short serial write {count}/{}", data.len()));
        }
        Ok(())
    }
    fn next(&mut self, until: Instant) -> Result<Option<String>> {
        loop {
            let ms = self.time()?;
            if let Some(i) = self.pending.iter().position(|&b| b == b'\n') {
                let mut bytes = self.pending.drain(..=i).collect::<Vec<_>>();
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
                if bytes.is_empty() {
                    continue;
                }
                if bytes.iter().any(|&b| !(32..=126).contains(&b)) {
                    return Err("invalid GRBL response bytes".into());
                }
                return Ok(Some(String::from_utf8(bytes).unwrap()));
            }
            if self.pending.len() > 512 {
                return Err("GRBL response exceeds 512 bytes".into());
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let r = self.port.read(ms.min(left.as_millis().max(1) as u32))?;
            self.pending.extend(r.bytes);
        }
    }
    fn query_status(&mut self, startup: bool) -> Result<String> {
        loop {
            self.send(b"?")?;
            let next_query = Instant::now() + QUERY_INTERVAL;
            while let Some(line) = self.next(next_query)? {
                if state(&line).is_some() {
                    return Ok(line);
                }
                if line.starts_with("[MSG:")
                    || (startup && (line.starts_with("Grbl 1.1") || line.starts_with("Grbl 1.3a")))
                {
                    continue;
                }
                return Err(format!("unexpected GRBL status response: {line}"));
            }
        }
    }
    fn idle(&mut self, startup: bool) -> Result<()> {
        loop {
            let frame = self.query_status(startup)?;
            match state(&frame) {
                Some("Idle") => return Ok(()),
                Some("Run") if !startup => {
                    self.cancel.sleep(
                        QUERY_INTERVAL.min(self.end.saturating_duration_since(Instant::now())),
                    )?;
                }
                Some(s) => return Err(format!("controller state {s}")),
                None => return Err("invalid GRBL status".into()),
            }
        }
    }
    fn require_state(&mut self, want: &str, startup: bool) -> Result<()> {
        let report = self.query_status(startup)?;
        match state(&report) {
            Some(got) if got == want => Ok(()),
            Some(got) => Err(format!("expected controller {want}, observed {got}")),
            None => Err("invalid controller status".into()),
        }
    }
    fn report_scale(&mut self) -> Result<f64> {
        // GRBL $13 controls realtime coordinate reporting independently of G20/G21.
        // Read the setting without changing the firmware's reporting configuration.
        self.send(b"$$\n")?;
        let mut units = None;
        loop {
            let line = self
                .next(self.end)?
                .ok_or("controller settings query timed out")?;
            if line == "ok" {
                break;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or("malformed GRBL settings reply")?;
            if key
                .strip_prefix('$')
                .and_then(|s| s.parse::<u16>().ok())
                .is_none()
            {
                return Err("malformed GRBL settings key".into());
            }
            if key == "$13" {
                if units.is_some() {
                    return Err("duplicate GRBL report units".into());
                }
                units = Some(match value.trim() {
                    "0" => 1.,
                    "1" => 25.4,
                    _ => return Err("unknown GRBL report units".into()),
                });
            }
        }
        units.ok_or_else(|| "missing GRBL $13 report units".into())
    }
    fn check_startup_blocks(&mut self) -> Result<()> {
        // A reset can execute stored startup blocks. Do not enter Check unless
        // startup records are readable and contain only the allowed modal setup.
        self.send(b"$N\n")?;
        let mut seen = [false; 2];
        loop {
            let line = self.next(self.end)?.ok_or("startup query timed out")?;
            if line == "ok" {
                if !seen.iter().all(|v| *v) || self.pending.iter().any(|b| !b.is_ascii_whitespace())
                {
                    return Err("startup blocks incomplete; Check mode not entered".into());
                }
                return Ok(());
            }
            let (slot, code) = line
                .split_once('=')
                .ok_or("startup blocks unavailable; Check mode not entered")?;
            let i = match slot {
                "$N0" => 0,
                "$N1" => 1,
                _ => return Err("unsupported startup records; Check mode not entered".into()),
            };
            if seen[i] {
                return Err("duplicate startup record; Check mode not entered".into());
            }
            seen[i] = true;
            let uppercase: String = code
                .bytes()
                .filter(|b| !b.is_ascii_whitespace())
                .map(|b| char::from(b.to_ascii_uppercase()))
                .collect();
            let mut words = uppercase.as_str();
            while !words.is_empty() {
                let Some(suffix) = ["G17", "G21", "G90", "G94"]
                    .iter()
                    .find_map(|word| words.strip_prefix(word))
                else {
                    return Err(format!(
                        "startup block {i} may cause an effect on firmware reset; Check mode not entered"
                    ));
                };
                words = suffix;
            }
        }
    }
    fn checked_ack(&mut self) -> Result<()> {
        loop {
            let line = self
                .next(self.end)?
                .ok_or("Check acknowledgement deadline expired")?;
            if line == "ok" {
                if self.pending.iter().any(|b| !b.is_ascii_whitespace()) {
                    return Err("unattributed data after Check acknowledgement".into());
                }
                return Ok(());
            }
            if line.starts_with("[MSG:")
                || line.starts_with("[echo:")
                || state(&line) == Some("Check")
            {
                continue;
            }
            return Err(format!("unexpected reply during controller check: {line}"));
        }
    }
    fn leave_check(&mut self) -> Result<()> {
        self.send(b"$C\n")?;
        // GRBL resets its parser on the transition; firmware variants differ
        // in whether the acknowledgement or the reboot banner arrives first.
        loop {
            let line = self
                .next(self.end)?
                .ok_or("Check exit response deadline expired")?;
            if line == "ok" || line == "[MSG:Disabled]" || line.starts_with("Grbl ") {
                break;
            }
            if line.starts_with("[MSG:") || state(&line) == Some("Check") {
                continue;
            }
            return Err(format!("Check exit not acknowledged: {line}"));
        }
        loop {
            self.send(b"?")?;
            let next_query = Instant::now() + QUERY_INTERVAL;
            while let Some(line) = self.next(next_query)? {
                if let Some(s) = state(&line) {
                    match s {
                        "Idle" => return Ok(()),
                        "Check" => continue,
                        _ => return Err(format!("after Check exit, controller state {s}")),
                    }
                }
                if line == "ok"
                    || line.starts_with("Grbl ")
                    || line.starts_with("[MSG:")
                    || (line.starts_with('>') && line.ends_with(":ok"))
                {
                    continue;
                }
                return Err(format!("unexpected status after Check exit: {line}"));
            }
        }
    }
    fn identify(&mut self) -> Result<String> {
        self.send(b"$I\n")?;
        self.ack(true)
    }
    fn ack(&mut self, info: bool) -> Result<String> {
        let mut version: Option<String> = None;
        let mut bt_writer = false;
        loop {
            let line = self
                .next(self.end)?
                .ok_or("GRBL response deadline expired")?;
            if line == "ok" {
                // Extra terminal replies cannot belong to a command we have not sent.
                if self.pending.iter().any(|b| !b.is_ascii_whitespace()) {
                    return Err("unattributed data after GRBL acknowledgement".into());
                }
                if !info {
                    return Ok(String::new());
                }
                return match version {
                    Some(v) if v == "1.3a.20221114" && bt_writer => Ok(v),
                    Some(v)
                        if v.strip_prefix("1.1").is_some_and(|tail| {
                            tail.is_empty()
                                || tail.starts_with('.')
                                || tail.as_bytes()[0].is_ascii_lowercase()
                        }) =>
                    {
                        Ok(v)
                    }
                    _ => Err(
                        "expected GRBL 1.1 or the identified BtWriter 1.3a.20221114 firmware"
                            .into(),
                    ),
                };
            }
            if info
                && let Some(build) = line.strip_prefix("[VER:").and_then(|s| s.strip_suffix(']'))
            {
                let (v, _) = build
                    .split_once(':')
                    .ok_or("malformed GRBL build identity")?;
                if version.is_some()
                    || v.is_empty()
                    || v.len() > 40
                    || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
                {
                    return Err("malformed or conflicting GRBL build identity".into());
                }
                version = Some(v.to_string());
                continue;
            }
            if info
                && line.starts_with("[MSG:Mode=BT:Name=BtWriter(")
                && line.contains("):Status=")
                && line.ends_with(']')
            {
                bt_writer = true;
                continue;
            }
            if (info && line.starts_with("[OPT:"))
                || line.starts_with("[MSG:")
                || line.starts_with("[echo:")
            {
                continue;
            }
            if matches!(state(&line), Some("Idle" | "Run")) {
                continue;
            }
            return Err(format!("GRBL replied {line}"));
        }
    }
}
/// Calculate a work-coordinate jog from fresh reports on the already selected device.
/// A single GRBL jog block neither latches a modal state nor resends uncertain motion.
pub(crate) fn jog_connected(
    port: &mut SerialPort,
    axis: char,
    delta: f64,
    feed: f64,
    end: Instant,
    cancel: &Cancellation,
) -> Result<String> {
    jog_on_port(port, axis, delta, feed, end, cancel)
}

fn jog_on_port<P: Port>(
    port: &mut P,
    axis: char,
    delta: f64,
    feed: f64,
    end: Instant,
    cancel: &Cancellation,
) -> Result<String> {
    let index = match axis {
        'X' => 0,
        'Y' => 1,
        'Z' => 2,
        _ => return Err("invalid jog axis".into()),
    };
    if !(delta.is_finite() && delta != 0. && delta.abs() <= 10. && feed > 0. && feed <= 500.) {
        return Err("invalid bounded plotter jog".into());
    }
    let mut c = Connection {
        port,
        end,
        cancel,
        pending: Vec::new(),
    };
    c.require_state("Idle", true)?;
    c.identify()?;
    let mm = c.report_scale()?;
    let mut position = None;
    // WCO is periodic. Never combine live MPos with a guessed or past-job offset.
    for attempt in 0..32 {
        let frame = c.query_status(false)?;
        if state(&frame) != Some("Idle") {
            return Err("controller left Idle before jog".into());
        }
        if let Some(v) = Positions::parse(&frame)?.work_axis(index) {
            position = Some(v * mm);
            break;
        }
        if attempt < 31 {
            c.cancel
                .sleep(QUERY_INTERVAL.min(end.saturating_duration_since(Instant::now())))?;
        }
    }
    let from = position.ok_or("fresh WPos or matching MPos/WCO not reported; no jog sent")?;
    let target = from + delta;
    if !(target as f32).is_finite() || target as f32 == from as f32 {
        return Err("jog delta cannot be expressed at this GRBL coordinate".into());
    }
    let block = format!("$J=G21G90{axis}{target:.6}F{feed:.6}\n");
    if block.len() > 80 || feed < 0.0000005 || (target - from).abs() < 0.0000005 {
        return Err("jog exceeds GRBL block precision or length".into());
    }
    c.send(block.as_bytes())
        .and_then(|()| c.ack(false))
        .map_err(|e| {
            format!("jog attempted; motion result unconfirmed, inspect before retry: {e}")
        })?;
    loop {
        let report = c
            .query_status(false)
            .map_err(|e| format!("jog acknowledged; final state unverified: {e}"))?;
        match state(&report) {
            Some("Idle") => break,
            Some("Jog" | "Run") => c
                .cancel
                .sleep(QUERY_INTERVAL.min(end.saturating_duration_since(Instant::now())))?,
            Some(s) => return Err(format!("jog acknowledged; controller state {s}")),
            None => return Err("jog acknowledged; invalid final state".into()),
        }
    }
    Ok(format!(
        r#"{{"state":"Idle","axis":{},"from_mm":{from},"target_mm":{target},"feed_mm_min":{feed}}}"#,
        quote(&axis.to_string())
    ))
}

#[derive(Default)]
struct Positions {
    work: Option<Vec<f64>>,
    machine: Option<Vec<f64>>,
    offset: Option<Vec<f64>>,
}

impl Positions {
    fn parse(frame: &str) -> Result<Self> {
        let mut coords = Self::default();
        for field in frame.trim_end_matches('>').split('|').skip(1) {
            let Some((key, value)) = field.split_once(':') else {
                continue;
            };
            let slot = match key {
                "WPos" => &mut coords.work,
                "MPos" => &mut coords.machine,
                "WCO" => &mut coords.offset,
                _ => continue,
            };
            let v: Vec<f64> = value
                .split(',')
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| "invalid GRBL coordinate")?;
            if slot.is_some() || !(3..=6).contains(&v.len()) || !v.iter().all(|v| v.is_finite()) {
                return Err("duplicate or invalid GRBL coordinate".into());
            }
            *slot = Some(v);
        }
        Ok(coords)
    }
    fn work_axis(&self, axis: usize) -> Option<f64> {
        self.work
            .as_ref()
            .map(|v| v[axis])
            .or_else(|| Some(self.machine.as_ref()?[axis] - self.offset.as_ref()?[axis]))
    }
}
fn state(line: &str) -> Option<&str> {
    let frame = line.strip_prefix('<')?.strip_suffix('>')?;
    let (state, fields) = frame.split_once('|')?;
    fields
        .split('|')
        .filter_map(|f| f.strip_prefix("MPos:").or_else(|| f.strip_prefix("WPos:")))
        .any(|coords| {
            let mut count = 0;
            let finite = coords.split(',').all(|v| {
                count += 1;
                v.parse::<f64>().is_ok_and(f64::is_finite)
            });
            finite && (3..=6).contains(&count)
        })
        .then_some(state)
}
fn transmit<P: Port>(
    port: &mut P,
    lines: &[String],
    end: Instant,
    cancel: &Cancellation,
) -> Result<usize> {
    let mut c = Connection {
        port,
        end,
        cancel,
        pending: Vec::new(),
    };
    c.idle(true)
        .and_then(|()| c.identify())
        .map_err(|e| format!("controller handshake before program: {e}"))?;
    // G94 establishes mm/min for this generated trajectory even after earlier G93 jobs.
    c.send(b"G94\n")
        .and_then(|()| c.ack(false))
        .map_err(|e| format!("feed mode before motion: {e}"))?;
    for (i, line) in lines.iter().enumerate() {
        let block = format!("{line}\n");
        c.send(block.as_bytes()).and_then(|()| c.ack(false)).map_err(|e|
            format!("{i} previous program lines acknowledged; line {}: {e}; execution incomplete, inspect before retry",i+1))?;
    }
    c.idle(false).map_err(|e| {
        format!(
            "all {} lines acknowledged; final motion state unverified: {e}",
            lines.len()
        )
    })?;
    Ok(lines.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[derive(Default)]
    struct Mock {
        writes: Vec<String>,
        replies: VecDeque<Vec<u8>>,
        commands: VecDeque<(String, Vec<Vec<u8>>)>,
    }
    impl Mock {
        fn expect(&mut self, command: &str, reply: &[&str]) {
            self.commands.push_back((
                command.into(),
                reply.iter().map(|s| s.as_bytes().to_vec()).collect(),
            ));
        }
        fn ready(&mut self) {
            self.expect("?", &["<Idle|MPos:0,0,0>\r\n"]);
            self.expect("$I\n", &["[VER:1.1h.2019:]\r\n[OPT:V,15,128]\r\nok\r\n"]);
            self.expect("G94\n", &["ok\n"]);
        }
    }
    impl Port for Mock {
        fn write(&mut self, bytes: &[u8], _: u32) -> Result<usize> {
            self.writes.push(String::from_utf8(bytes.to_vec()).unwrap());
            let (q, a) = self.commands.pop_front().expect("unexpected send");
            assert_eq!(q.as_bytes(), bytes);
            self.replies.extend(a);
            Ok(bytes.len())
        }
        fn read(&mut self, _: u32) -> Result<ReadResult> {
            let bytes = self.replies.pop_front().ok_or("test disconnect")?;
            Ok(ReadResult {
                bytes,
                timed_out: false,
            })
        }
    }
    #[test]
    fn normalized_probe_reads_reporting_units_and_derives_work_xyz_from_one_report() {
        let mut m = Mock::default();
        m.expect("?", &["<Idle|MPos:8,8,8>\n"]);
        m.expect("$I\n", &["[VER:1.1h.2019:]\nok\n"]);
        m.expect("$$\n", &["$13=1\nok\n"]);
        m.expect("?", &["<Idle|MPos:2,2,2|WCO:1,1,1>\n"]);
        let result = inspect_port(
            &mut m,
            deadline(500).unwrap(),
            &Cancellation::default(),
            true,
        )
        .unwrap();
        for field in [
            r#""units":"mm""#,
            r#""report_units":"inch""#,
            r#""work_position":[25.4, 25.4, 25.4]"#,
        ] {
            assert!(result.contains(field), "{result}");
        }
    }

    #[test]
    fn jog_converts_report_inches_and_waits_for_matching_work_offset() {
        let mut mock = Mock::default();
        mock.expect("?", &["<Idle|MPos:20,0,0>\n"]);
        mock.expect("$I\n", &["[VER:1.1h.2019:]\nok\n"]);
        mock.expect("$$\n", &["$10=1\n$13=1\nok\n"]);
        mock.expect("?", &["<Idle|MPos:21,0,0>\n"]);
        mock.expect("?", &["<Idle|MPos:3,0,0|WCO:2,0,0>\n"]);
        mock.expect("$J=G21G90X27.400000F100.000000\n", &["ok\n"]);
        mock.expect("?", &["<Jog|WPos:1.01,0,0>\n"]);
        mock.expect("?", &["<Idle|WPos:1.0787,0,0>\n"]);
        let receipt = jog_on_port(
            &mut mock,
            'X',
            2.,
            100.,
            deadline(3000).unwrap(),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(receipt.contains(r#""target_mm":27.4"#));
        assert!(mock.commands.is_empty());
    }

    #[test]
    fn missing_units_and_unknown_motion_ack_never_replay_a_jog() {
        for (units, fail_ack) in [("$0=10\nok\n", false), ("$13=0\nok\n", true)] {
            let mut mock = Mock::default();
            mock.expect("?", &["<Idle|WPos:1,0,0>\n"]);
            mock.expect("$I\n", &["[VER:1.1h.2019:]\nok\n"]);
            mock.expect("$$\n", &[units]);
            if fail_ack {
                mock.expect("?", &["<Idle|WPos:1,0,0>\n"]);
                mock.expect("$J=G21G90X2.000000F100.000000\n", &[]);
            }
            assert!(
                jog_on_port(
                    &mut mock,
                    'X',
                    1.,
                    100.,
                    deadline(500).unwrap(),
                    &Cancellation::default()
                )
                .is_err()
            );
            assert!(mock.commands.is_empty());
            assert_eq!(
                mock.writes
                    .iter()
                    .filter(|line| line.starts_with("$J"))
                    .count(),
                usize::from(fail_ack)
            );
        }
    }
    const WRITER_STATE: &str =
        "<Idle|MPos:0.000,0.000,0.000|FS:0,0|Pn:XY|WCO:0.000,0.000,0.000>\r\n";
    const WRITER_ID: &str = "[VER:1.3a.20221114:]\r\n[OPT:MPHB]\r\n[MSG:Mode=BT:Name=BtWriter(test-device):Status=Not connected]\r\nok\r\n";
    #[test]
    fn writer_probe_is_read_only_and_excludes_transport_address() {
        let mut m = Mock::default();
        m.expect("?", &[WRITER_STATE]);
        m.expect("$I\n", &[WRITER_ID]);
        let json = inspect_port(
            &mut m,
            deadline(500).unwrap(),
            &Cancellation::default(),
            false,
        )
        .unwrap();
        assert_eq!(
            json,
            r#"{"controller":"BtWriter","firmware":"1.3a.20221114","state":"Idle","units":"controller_report","machine_position":[0.0, 0.0, 0.0],"work_offset":[0.0, 0.0, 0.0],"pins":"XY"}"#
        );
        assert_eq!(m.writes, ["?", "$I\n"]);
    }
    #[test]
    fn strict_writer_identity_and_generated_program() {
        let program = "G21 G17 G90 G94\nG0 Z2\nG0 X0 Y0\nG1 Z0 F100\nG1 X2 Y3\nG0 Z2\nM2\n";
        let lines = plotter_lines(program).unwrap();
        assert_eq!(lines.len(), 7);
        for id in [
            "[VER:1.3a.20221114:]\nok\n",
            "[VER:1.3a.20221225:]\n[MSG:Mode=BT:Name=BtWriter(test):Status=Not connected]\nok\n",
        ] {
            let mut m = Mock::default();
            m.expect("?", &[WRITER_STATE]);
            m.expect("$I\n", &[id]);
            assert!(
                transmit(
                    &mut m,
                    &lines,
                    deadline(500).unwrap(),
                    &Cancellation::default()
                )
                .is_err()
            );
            assert_eq!(m.writes, ["?", "$I\n"]);
        }
        let mut m = Mock::default();
        m.expect("?", &[WRITER_STATE]);
        m.expect("$I\n", &[WRITER_ID]);
        m.expect("G94\n", &["ok\n"]);
        for line in &lines {
            m.expect(&format!("{line}\n"), &["ok\n"]);
        }
        m.expect("?", &[WRITER_STATE]);
        assert_eq!(
            transmit(
                &mut m,
                &lines,
                deadline(500).unwrap(),
                &Cancellation::default()
            )
            .unwrap(),
            7
        );
    }
    const CHECK: &str = "<Check|MPos:0,0,0>\r\n";
    const STARTUP: &str = "$N0=\n$N1=G21G90\nok\n";
    fn check_entry(m: &mut Mock, startup: &str) {
        m.expect("?", &[WRITER_STATE]);
        m.expect("$I\n", &[WRITER_ID]);
        m.expect("$N\n", &[startup]);
    }
    fn toggle(m: &mut Mock, ack: &str) {
        m.expect("?", &[WRITER_STATE]);
        m.expect("$C\n", &[ack]);
    }
    #[test]
    fn hardware_check_validates_each_block_and_observes_reset_to_idle() {
        let lines = plotter_lines("G21 G90\nG0 X2").unwrap();
        let mut m = Mock::default();
        check_entry(&mut m, STARTUP);
        toggle(&mut m, "[MSG:Enabled]\nok\n");
        for line in &lines {
            m.expect("?", &[CHECK]);
            m.expect(&format!("{line}\n"), &["ok\n"]);
        }
        m.expect("?", &[CHECK]);
        m.expect("$C\n", &["[MSG:Disabled]\nok\nGrbl 1.3a ['$' for help]\n"]);
        m.expect("?", &[WRITER_STATE]);
        assert_eq!(
            check_on_port(
                &mut m,
                &lines,
                deadline(1000).unwrap(),
                &Cancellation::default()
            )
            .unwrap(),
            2
        );
        assert!(m.commands.is_empty());
    }
    #[test]
    fn check_denial_and_startup_motion_never_send_geometry() {
        let lines = plotter_lines("G21 G90\nG0 X2").unwrap();
        for startup in ["$N0=G0X1\n$N1=\nok\n", "error:3\n", "$N0=\nok\n"] {
            let mut m = Mock::default();
            check_entry(&mut m, startup);
            assert!(
                check_on_port(
                    &mut m,
                    &lines,
                    deadline(1000).unwrap(),
                    &Cancellation::default()
                )
                .is_err()
            );
            assert_eq!(m.writes, ["?", "$I\n", "$N\n"]);
        }
        let mut m = Mock::default();
        check_entry(&mut m, STARTUP);
        toggle(&mut m, "error:3\n");
        assert!(
            check_on_port(
                &mut m,
                &lines,
                deadline(1000).unwrap(),
                &Cancellation::default()
            )
            .unwrap_err()
            .contains("no program blocks sent")
        );
        assert!(m.commands.is_empty());
        let mut m = Mock::default();
        check_entry(&mut m, STARTUP);
        toggle(&mut m, "ok\n");
        m.expect("?", &[WRITER_STATE]);
        m.expect("?", &[WRITER_STATE]);
        assert!(
            check_on_port(
                &mut m,
                &lines,
                deadline(1000).unwrap(),
                &Cancellation::default()
            )
            .unwrap_err()
            .contains("0 blocks accepted")
        );
        assert_eq!(m.writes.iter().filter(|s| s.as_str() == "$C\n").count(), 1);
    }
    #[test]
    fn failed_check_block_exits_only_from_a_fresh_check_state() {
        let lines = plotter_lines("G21 G90\nG0 X2").unwrap();
        for restored in [false, true] {
            let mut m = Mock::default();
            check_entry(&mut m, STARTUP);
            toggle(&mut m, "ok\n");
            m.expect("?", &[CHECK]);
            m.expect("G21G90\n", &["error:20\n"]);
            if restored {
                m.expect("?", &[CHECK]);
                m.expect("$C\n", &["Grbl 1.1h ['$' for help]\n"]);
                m.expect("?", &[WRITER_STATE]);
            } else {
                m.expect("?", &["<Alarm|MPos:0,0,0>\n"]);
            }
            let error = check_on_port(
                &mut m,
                &lines,
                deadline(1000).unwrap(),
                &Cancellation::default(),
            )
            .unwrap_err();
            assert!(error.contains("block 1 unconfirmed"));
            assert_eq!(error.contains("restored Idle"), restored);
            assert!(!m.writes.iter().any(|s| s == "G0X2\n"));
            assert!(m.commands.is_empty());
        }
    }
    #[test]
    fn fragmented_ack_waits_for_idle() {
        let mut m = Mock::default();
        m.ready();
        m.expect("G21G90\n", &["o", "k\r", "\n"]);
        m.expect("G0X2\n", &["ok\n"]);
        m.expect("?", &["<Idle|WPos:2,0,0>\n"]);
        let lines = plotter_lines("G21 G90 ; hi\nG0 X2 (ignored ! motion control)\n").unwrap();
        assert_eq!(
            transmit(
                &mut m,
                &lines,
                deadline(1000).unwrap(),
                &Cancellation::default()
            )
            .unwrap(),
            2
        );
        assert!(m.commands.is_empty());
    }
    #[test]
    fn alarm_reject_reset_disconnect_and_duplicate_never_resend() {
        for reply in [
            vec!["ALARM:1\n"],
            vec!["error:20\n"],
            vec!["Grbl 1.1h ['$' for help]\n"],
            vec!["ok\nok\n"],
            vec![],
        ] {
            let mut m = Mock::default();
            m.ready();
            m.expect("G21G90\n", &reply);
            let e = transmit(
                &mut m,
                &plotter_lines("G21 G90\nG1 X0 F1").unwrap(),
                deadline(1000).unwrap(),
                &Cancellation::default(),
            )
            .unwrap_err();
            assert!(e.contains("0 previous program lines"));
            assert_eq!(m.writes.len(), 4);
        }
    }
    #[test]
    fn preflight_and_cancellation() {
        for p in [
            "",
            "G21 G90\nG7 X3",
            "G21 G90\nG1 X0\n$X",
            "G0 X3",
            "G21 G90\nG1 X1!",
            "G21 G90\nM2\nG0 X1",
            "G21 G90\nG0 X1 X2",
            "G21 G90\nG0 G1 X1 F2",
            "G21 G90\nG1 Y1",
        ] {
            assert!(plotter_lines(p).is_err());
        }
        let c = Cancellation::default();
        c.cancel();
        let mut m = Mock::default();
        assert!(transmit(&mut m, &["G1X0".into()], deadline(1000).unwrap(), &c).is_err());
        assert!(m.writes.is_empty());
    }
}
