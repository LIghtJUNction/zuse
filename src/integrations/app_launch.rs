//! Explicitly launch a user application outside the Runtime's owned processes.
//! Its standard streams are disconnected, and Unix gives it a separate group.
use super::Result;
use crate::observation::Cancellation;

/// Launch an independent user application. It is not registered with
/// ProcessManager and cannot be stopped by dropping the originating Runtime.
/// The returned PID identifies this spawn, not application-level readiness.
pub fn launch(program: &str, args: &str, cancellation: &Cancellation) -> Result<String> {
    super::validate_start(program, args)?;
    cancellation.check()?;
    #[cfg(unix)]
    {
        use std::{
            os::unix::process::CommandExt,
            process::{Child, Command, Stdio},
            sync::mpsc,
            thread,
        };

        let arguments = super::parse_arguments(args)?;
        let mut command = Command::new(program);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);

        // Reserve the reaper before launching. A thread allocation failure
        // therefore cannot leave a successfully started app without a receipt.
        let (sender, receiver) = mpsc::sync_channel::<Child>(1);
        let reaper = thread::Builder::new()
            .name("zuse-app-reaper".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                if let Ok(mut child) = receiver.recv() {
                    let _ = child.wait();
                }
            })
            .map_err(|error| format!("prepare independent application reaper: {error}"))?;

        // This is the only effectful boundary. A denied or cancelled launch
        // drops the sender, allowing the empty reaper to exit without a child.
        let child = cancellation.dispatch(|| {
            command
                .spawn()
                .map_err(|error| format!("launch {program}: {error}"))
        })?;
        let pid = child.id();
        if let Err(error) = sender.send(child) {
            let mut child = error.0;
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // SAFETY: this unreaped child was just put in its own group. No
            // remembered or caller-supplied PID is ever signalled here.
            unsafe {
                kill(-(pid as i32), 9);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "independent application reaper was unavailable; new process stopped".into(),
            );
        }
        // Detaching this waiter does not detach/forget the Child itself: it
        // reaps normal exits while Zuse lives. Zuse exit or Runtime drop never
        // sends a signal to this application's group.
        drop(reaper);
        Ok(format!(
            r#"{{"pid":{pid},"program":{},"state":"launched","lifecycle":"independent","process_group":{pid},"stdio":"null"}}"#,
            crate::json::quote(program)
        ))
    }
    #[cfg(not(unix))]
    {
        Err("unsupported_state: independent application launch currently requires Unix process groups".into())
    }
}
