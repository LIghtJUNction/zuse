//! Load and run the same validated XZ program on a specifically selected LinuxCNC installation.
use crate::observation::Cancellation;
type Result<T> = std::result::Result<T, String>;

/// The operator selects the active INI and prepares the lathe in AUTO mode.
/// Cancellation stops this host's dispatch/observation. A submitted AUTO_RUN is never replayed.
pub fn run_linuxcnc(
    ini: &str,
    program: &str,
    timeout_ms: u32,
    cancel: &Cancellation,
) -> Result<()> {
    let end = crate::machine_stream::deadline(timeout_ms)?;
    cancel.check()?;
    crate::plotter::lathe_preview(program)?;
    #[cfg(target_os = "linux")]
    {
        native::run(ini, program, end, cancel)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (ini, end);
        Err("LinuxCNC requires its Linux host and native Python module".into())
    }
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::fs::DirBuilderExt,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    // program_open uses the filesystem. Stage the already-checked bytes in a private directory.
    struct Staged(PathBuf);
    impl Drop for Staged {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("program.ngc"));
            let _ = fs::remove_dir(&self.0);
        }
    }
    fn stage(text: &str) -> Result<Staged> {
        if text.len() > 1_048_576 {
            return Err("lathe program exceeds 1 MiB".into());
        }
        // LinuxCNC comments can be executable (e.g. LOGOPEN and PROBEOPEN). Only transmit
        // data words, never the contents of comments a geometric preview discarded.
        let mut body = String::new();
        for line in text.lines() {
            let mut inside = false;
            let mut clean = String::new();
            for ch in line.chars() {
                match ch {
                    ';' if !inside => break,
                    '(' if !inside => inside = true,
                    ')' if inside => inside = false,
                    '(' | ')' => return Err("malformed lathe comment".into()),
                    _ if inside => {}
                    c if c.is_ascii_alphanumeric() || " \t+-.".contains(c) => clean.push(c),
                    '%' if line.trim() == "%" => clean.push('%'),
                    _ => return Err("unexpected LinuxCNC program character".into()),
                }
            }
            if inside {
                return Err("unterminated lathe comment".into());
            }
            if !clean.trim().is_empty() {
                body.push_str(clean.trim());
                body.push('\n');
            }
        }
        if body.len() >= 4 && body.starts_with("%\n") && body.ends_with("%\n") {
            body = body[2..body.len().saturating_sub(2)].to_owned();
        }
        if body.is_empty() || body.contains('%') {
            return Err("invalid LinuxCNC program framing".into());
        }
        // A pair of percent delimiters provides a file end without inserting machine commands.
        let wrapped = format!("%\n{body}%\n");
        static ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "zuse-lathe-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| format!("create private LinuxCNC staging directory: {e}"))?;
        let staged = Staged(path);
        fs::write(staged.0.join("program.ngc"), wrapped)
            .map_err(|e| format!("write LinuxCNC program: {e}"))?;
        Ok(staged)
    }

    pub(super) fn run(ini: &str, program: &str, end: Instant, cancel: &Cancellation) -> Result<()> {
        let ini = Path::new(ini)
            .canonicalize()
            .map_err(|e| format!("selected LinuxCNC INI: {e}"))?;
        let staged = stage(program)?;
        let mut child = Command::new("/usr/bin/python3")
            .args(["-I", "-u", "-c", DRIVER])
            .arg(ini)
            .arg(staged.0.join("program.ngc"))
            .arg(
                end.saturating_duration_since(Instant::now())
                    .as_secs_f64()
                    .to_string(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("start LinuxCNC host Python: {e}"))?;
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(4);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut authorized = false;
        let result = (|| loop {
            cancel.check()?;
            let budget = end.saturating_duration_since(Instant::now());
            if budget.is_zero() {
                return Err("total LinuxCNC deadline expired".into());
            }
            match rx.recv_timeout(budget.min(Duration::from_millis(30))) {
                Ok(msg) if msg == "READY" && !authorized => {
                    cancel.dispatch(|| {
                        child
                            .stdin
                            .take()
                            .ok_or("missing LinuxCNC authorization pipe")?
                            .write_all(b"RUN\n")
                            .map_err(|e| e.to_string())?;
                        authorized = true;
                        Ok(())
                    })?;
                }
                Ok(msg) if authorized && msg == "DONE" => return Ok(()),
                Ok(msg) => return Err(msg.strip_prefix("ERROR ").unwrap_or(&msg).to_owned()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("LinuxCNC helper closed without completion".into());
                }
            }
        })();
        if result.is_err() {
            let _ = child.kill();
        }
        drop(rx);
        let _ = reader.join();
        let exit = child.wait().map_err(|e| e.to_string())?;
        match result {
            Err(e) if authorized => Err(format!(
                "LinuxCNC run authorized; final state unverified; inspect controller before retry: {e}"
            )),
            Err(e) => Err(format!("LinuxCNC AUTO_RUN not authorized: {e}")),
            Ok(()) if exit.success() => Ok(()),
            Ok(()) => Err(format!(
                "LinuxCNC reported completion; helper exited {exit}"
            )),
        }
    }

    const DRIVER: &str = r#"
import os, sys, time

def run(cnc, ini, path, seconds):
    end = time.monotonic() + seconds
    state = cnc.stat()
    command = cnc.command()

    def poll(expect_idle):
        state.poll()
        if not os.path.samefile(ini, state.ini_filename):
            raise ValueError("active controller uses a different INI")
        if state.axis_mask != 5:
            raise ValueError("expected dedicated XZ axes")
        if state.task_mode != cnc.MODE_AUTO or state.estop or not state.enabled or state.task_state != cnc.STATE_ON:
            raise ValueError("lathe must be enabled, clear of E-stop and in AUTO")
        if not state.joints or not all(state.homed[:state.joints]):
            raise ValueError("lathe must already be homed")
        if expect_idle:
            if state.interp_state != cnc.INTERP_IDLE or not state.inpos or state.queue != 0:
                raise ValueError("lathe must be idle and in position")
            if 400 not in state.gcodes or 940 not in state.gcodes or 970 not in state.gcodes:
                raise ValueError("set G40 (no cutter comp), G94 (feed/min) and G97 (fixed rpm) before running")

    def wait_command():
        remaining = end - time.monotonic()
        if remaining <= 0 or command.wait_complete(remaining) != cnc.RCS_DONE:
            raise ValueError("controller did not confirm command completion")
        state.poll()
        if state.echo_serial_number != command.serial:
            raise ValueError("command receipt superseded by another client")

    def check_program():
        if not os.path.samefile(state.file, path):
            raise ValueError("controller loaded a different program")

    poll(True)
    config = cnc.ini(ini)
    if str(config.find("DISPLAY", "LATHE")).strip().lower() not in ("1", "yes", "true", "on"):
        raise ValueError("selected configuration does not declare a lathe")
    command.program_open(path)
    wait_command()
    poll(True)
    check_program()
    print("READY", flush=True)
    if sys.stdin.readline() != "RUN\n" or time.monotonic() >= end:
        raise ValueError("run authorization missing or expired")
    poll(True)
    check_program()
    command.auto(cnc.AUTO_RUN, 0)
    wait_command()
    while time.monotonic() < end:
        poll(False)
        check_program()
        if state.echo_serial_number != command.serial:
            raise ValueError("run receipt superseded by another command")
        if state.exec_state == cnc.EXEC_ERROR or not 0 <= state.interpreter_errcode <= 3:
            raise ValueError("LinuxCNC reported an execution/interpreter error")
        if state.interp_state == cnc.INTERP_IDLE and state.exec_state == cnc.EXEC_DONE and state.inpos and state.queue == 0:
            print("DONE", flush=True)
            return
        time.sleep(0.025)
    raise ValueError("timed out observing lathe motion completion")

try:
    import linuxcnc
    run(linuxcnc, sys.argv[1], sys.argv[2], float(sys.argv[3]))
except Exception as exc:
    print("ERROR " + str(exc).replace("\n", " ")[:512], flush=True)
    sys.exit(1)
"#;

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn simulated_controller_requires_matching_receipts_and_ready_state() {
            // Exercise the very same Python control flow without importing the native module.
            let (bindings, _) = DRIVER.split_once("\ntry:\n    import linuxcnc").unwrap();
            let mock = r#"
import io, tempfile, types
with tempfile.NamedTemporaryFile() as ini, tempfile.NamedTemporaryFile() as program:
    def scenario(case):
        events = []
        state = types.SimpleNamespace(ini_filename=ini.name, task_state=4, axis_mask=5,
            task_mode=2, estop=(case == 'estop'), enabled=True,
            joints=2, homed=[True, True], interp_state=1, exec_state=2, interpreter_errcode=0,
            gcodes=[400,940,970], inpos=True, queue=0, echo_serial_number=0, file='')
        state.poll = lambda: None
        class Command:
            serial=0
            def program_open(self,path):
                events.append('open')
                state.file=ini.name if case == 'wrong-program' else path
                self.serial=1
            def auto(self,action,line):
                assert (action,line) == (0,0)
                events.append('run')
                self.serial=2
                if case == 'interp-error': state.interpreter_errcode=5
            def wait_complete(self,seconds):
                assert seconds > 0
                state.echo_serial_number=7 if case == 'wrong-receipt' else self.serial
                return 1
        cnc = types.SimpleNamespace(stat=lambda:state, command=Command, MODE_AUTO=2,
            STATE_ON=4, INTERP_IDLE=1, EXEC_DONE=2, EXEC_ERROR=1, RCS_DONE=1, AUTO_RUN=0,
            ini=lambda _:types.SimpleNamespace(find=lambda a,b:'1'))
        sys.stdin=io.StringIO('' if case == 'cancel' else 'RUN\n')
        try:
            run(cnc,ini.name,program.name,1)
        except ValueError:
            assert case != 'ok'
        else:
            assert case == 'ok'
        return events
    assert scenario('ok') == ['open','run']
    assert scenario('estop') == []
    assert scenario('wrong-program') == ['open']
    assert scenario('wrong-receipt') == ['open']
    assert scenario('cancel') == ['open']
    assert scenario('interp-error') == ['open','run']
"#;
            let out = Command::new("/usr/bin/python3")
                .args(["-I", "-c", &format!("{bindings}{mock}")])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        #[test]
        fn strips_interpreter_comments_and_frames_without_machine_commands() {
            let s = stage("G21 G90 G7\nG1 X2 Z3 F5 (LOGOPEN,/tmp/x) ; comment").unwrap();
            assert_eq!(
                fs::read_to_string(s.0.join("program.ngc")).unwrap(),
                "%\nG21 G90 G7\nG1 X2 Z3 F5\n%\n"
            );
            assert!(stage("G21\n%\nG0 X2").is_err());
        }
    }
}
