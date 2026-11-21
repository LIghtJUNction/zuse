//! Persistent local controllers for a single, retained Runtime.
//!
//! Protocol v1 is bounded JSON-lines over an owner-only Unix socket. Each
//! execution has a fresh cancellation token; neither retries nor observations
//! implicitly replay or interrupt an active program.

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 900_000;
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESULT_BYTES: usize = 512 * 1024;
pub const MAX_RESULTS: usize = 32;
pub const MAX_REQUESTS: usize = 64;
pub const MAX_REQUEST_CACHE_BYTES: usize = 8 * 1024 * 1024;

pub const HELP: &str = "zuse session start NAME [--idle-timeout-ms N]\nzuse session serve NAME [--idle-timeout-ms N]\nzuse session status NAME\nzuse session script NAME - [--read-only] [--dry-run] [--max-steps N] [--max-wait-ms N]\nzuse session observe NAME - [--dry-run] [--max-steps N] [--max-wait-ms N]\nzuse session result NAME [JOB_ID]\nzuse session cancel NAME\nzuse session close NAME\nscript/observe/cancel/close also accept --request-id ID and --expect-revision N.\n";

pub fn run(args: &[&str]) -> Result<String, String> {
    if matches!(args, [] | ["help"] | ["--help"] | ["-h"]) {
        return Ok(format!(
            "{{\"status\":\"help\",\"usage\":{}}}",
            crate::json::quote(HELP)
        ));
    }
    #[cfg(unix)]
    {
        unix::run(args)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        Err("unsupported_state: persistent sessions currently require Unix local sockets".into())
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::{
        json,
        observation::Cancellation,
        runtime::{self, Options, Runtime},
        script::interrupt::InterruptGuard,
    };
    use std::{
        collections::{BTreeMap, BTreeSet, VecDeque},
        fs::{self, DirBuilder},
        io::{self, BufRead, BufReader, Read, Write},
        os::unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{
            Arc, Condvar, Mutex, MutexGuard,
            mpsc::{self, Receiver, SyncSender},
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use zmcpio::json::{Kind, Value};

    const MAX_CONNECTIONS: usize = 16;
    const IO_TIMEOUT: Duration = Duration::from_millis(100);
    const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
    const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

    unsafe extern "C" {
        fn geteuid() -> u32;
        fn setsid() -> i32;
        fn kill(pid: i32, signal: i32) -> i32;
    }

    fn uid() -> u32 {
        unsafe { geteuid() }
    }

    fn single_line_json(input: &str) -> Result<String, String> {
        json::with_value(input, |_| Ok(()))?;
        let mut output = String::with_capacity(input.len());
        let mut quoted = false;
        let mut escaped = false;
        for character in input.chars() {
            if quoted {
                output.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quoted = false;
                }
            } else if !matches!(character, ' ' | '\t' | '\r' | '\n') {
                output.push(character);
                quoted = character == '"';
            }
        }
        Ok(output)
    }

    fn valid_name(name: &str) -> Result<(), String> {
        if name.is_empty()
            || name.len() > 40
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("session name must contain 1..40 ASCII letters, digits, '_' or '-'".into());
        }
        Ok(())
    }

    fn valid_request_id(id: &str) -> Result<(), String> {
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            Err("request_id must contain 1..128 bytes without control characters".into())
        } else {
            Ok(())
        }
    }

    fn directory() -> Result<PathBuf, String> {
        let path = if let Some(path) = std::env::var_os("ZUSE_SESSION_DIR") {
            PathBuf::from(path)
        } else if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR") {
            PathBuf::from(path).join("zuse-sessions")
        } else {
            std::env::temp_dir().join(format!("zuse-sessions-{}", uid()))
        };
        if !path.is_absolute() {
            return Err("session directory must be absolute".into());
        }
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create session directory: {e}")),
        }
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.uid() != uid()
            || meta.mode() & 0o777 != 0o700
        {
            return Err(
                "session directory must be owned by the current user, mode 0700, and not a symlink"
                    .into(),
            );
        }
        path.canonicalize().map_err(|e| e.to_string())
    }

    fn socket_path(name: &str) -> Result<PathBuf, String> {
        valid_name(name)?;
        let path = directory()?.join(format!("{name}.sock"));
        if path.as_os_str().as_bytes().len() > 100 {
            return Err(
                "session socket path exceeds 100 bytes; choose a shorter ZUSE_SESSION_DIR".into(),
            );
        }
        Ok(path)
    }

    fn verify_socket(path: &Path) -> Result<(), String> {
        let meta = fs::symlink_metadata(path)
            .map_err(|e| format!("session socket {}: {e}", path.display()))?;
        if !meta.file_type().is_socket() || meta.uid() != uid() || meta.mode() & 0o777 != 0o600 {
            return Err("session endpoint must be an owned mode-0600 Unix socket".into());
        }
        Ok(())
    }

    struct SocketOwner {
        path: PathBuf,
        device: u64,
        inode: u64,
    }
    impl Drop for SocketOwner {
        fn drop(&mut self) {
            if let Ok(meta) = fs::symlink_metadata(&self.path)
                && meta.file_type().is_socket()
                && meta.uid() == uid()
                && meta.dev() == self.device
                && meta.ino() == self.inode
            {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    struct Job {
        id: u64,
        source: String,
        base: PathBuf,
        options: Options,
        cancellation: Cancellation,
    }
    enum Work {
        Run(Job),
        Close,
    }
    struct Active {
        id: u64,
        cancellation: Cancellation,
        cancelling: bool,
    }
    struct Completed {
        id: u64,
        status: &'static str,
        result: String,
        bytes: usize,
        truncated: bool,
        elapsed_ms: u128,
    }
    struct Cached {
        request: String,
        response: Option<String>,
    }
    struct State {
        revision: u64,
        next_job: u64,
        active: Option<Active>,
        completed: VecDeque<Completed>,
        closing: bool,
        worker_done: bool,
        endpoint_closed: bool,
        last_activity: Instant,
        cache: BTreeMap<String, Cached>,
        cache_order: VecDeque<String>,
        cache_bytes: usize,
    }
    impl State {
        fn phase(&self) -> &'static str {
            if self.closing {
                "closing"
            } else if self.active.as_ref().is_some_and(|a| a.cancelling) {
                "cancelling"
            } else if self.active.is_some() {
                "running"
            } else {
                "idle"
            }
        }
        fn advance(&mut self) {
            self.revision = self.revision.saturating_add(1);
        }
    }
    struct Control {
        name: String,
        socket: PathBuf,
        base: PathBuf,
        state: Mutex<State>,
        changed: Condvar,
        work: SyncSender<Work>,
    }
    impl Control {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(|e| e.into_inner())
        }
        fn response(&self, state: &State, id: Option<&str>, fields: &str) -> String {
            let prefix = format!(
                "{{\"version\":1,\"request_id\":{},\"session\":{},\"state\":{},\"state_revision\":{}",
                id.map_or_else(|| "null".into(), json::quote),
                json::quote(&self.name),
                json::quote(state.phase()),
                state.revision
            );
            // Native observations can contain pretty-printed nested JSON. The
            // transport delimiter must never be confused with its whitespace.
            single_line_json(&format!("{prefix},{fields}}}")).unwrap_or_else(|_| {
                format!("{prefix},\"ok\":false,\"error\":{{\"code\":\"invalid_result\",\"message\":\"runtime result cannot be encoded as a valid protocol response\"}}}}")
            })
        }
        fn error(&self, state: &State, id: Option<&str>, code: &str, message: &str) -> String {
            self.response(
                state,
                id,
                &format!(
                    "\"ok\":false,\"error\":{{\"code\":{},\"message\":{}}}",
                    json::quote(code),
                    json::quote(message)
                ),
            )
        }
        fn request_close(&self) {
            let cancellation = {
                let mut state = self.lock();
                if !state.closing {
                    state.closing = true;
                    state.advance();
                }
                state.active.as_ref().map(|a| a.cancellation.clone())
            };
            if let Some(cancellation) = cancellation {
                cancellation.request_cancel();
            }
            let _ = self.work.try_send(Work::Close);
        }
        fn expire_if_idle(&self, timeout: Duration) {
            // Decide and close under the same lock used to accept jobs. An
            // expired snapshot must never cancel a newly accepted controller.
            let mut state = self.lock();
            if state.closing || state.active.is_some() || state.last_activity.elapsed() < timeout {
                return;
            }
            state.closing = true;
            state.advance();
            let _ = self.work.try_send(Work::Close);
        }
    }

    fn worker(control: &Control, input: Receiver<Work>) {
        // Native handles need not be Send: create, use and drop them here.
        let mut machine = Runtime::new();
        while let Ok(work) = input.recv() {
            let Work::Run(job) = work else {
                break;
            };
            machine.set_cancellation(job.cancellation.clone());
            let start = Instant::now();
            let output = match job
                .cancellation
                .check()
                .and_then(|_| machine.execute_with_base(&job.source, job.options, &job.base))
            {
                Ok(output) => output,
                Err(error) => format!(
                    "{{\"status\":\"failed\",\"stage\":\"prepare\",\"error\":{},\"error_code\":{},\"completed\":0,\"results\":[]}}",
                    json::quote(&error),
                    json::quote(if error.starts_with("cancelled:") {
                        "cancelled"
                    } else {
                        "preflight_error"
                    })
                ),
            };
            let bytes = output.len();
            let truncated = bytes > MAX_RESULT_BYTES;
            let status = if json::field(&output, "error_code").as_deref() == Ok("cancelled") {
                "cancelled"
            } else if truncated || runtime::failed(&output) {
                "failed"
            } else {
                "completed"
            };
            let result = if truncated { "null".into() } else { output };
            let mut state = control.lock();
            state.completed.push_back(Completed {
                id: job.id,
                status,
                result,
                bytes,
                truncated,
                elapsed_ms: start.elapsed().as_millis(),
            });
            while state.completed.len() > MAX_RESULTS {
                state.completed.pop_front();
            }
            state.active = None;
            state.last_activity = Instant::now();
            state.advance();
            control.changed.notify_all();
            if state.closing {
                break;
            }
        }
        // Runtime's owned mouse/browser/device/process fields drop before done.
        drop(machine);
    }

    fn object(value: Value<'_>, allowed: &[&str]) -> Result<(), String> {
        if value.kind() != Kind::Object {
            return Err("expected an object".into());
        }
        let mut keys = BTreeSet::new();
        for (key, _) in value.members() {
            let key = key.chars().collect::<String>();
            if !keys.insert(key.clone()) || !allowed.contains(&key.as_str()) {
                return Err(format!("duplicate or unknown field {key:?}"));
            }
        }
        Ok(())
    }
    fn unsigned(value: Value<'_>) -> Result<u64, String> {
        let number = value
            .integer()
            .map_err(|_| "expected a nonnegative integer")?;
        u64::try_from(number).map_err(|_| "expected a nonnegative integer".into())
    }
    struct Request {
        id: String,
        op: String,
        source: Option<String>,
        base: PathBuf,
        options: Options,
        job_id: Option<u64>,
        expected: Option<u64>,
    }
    impl Request {
        fn action(&self) -> bool {
            matches!(self.op.as_str(), "script" | "observe" | "cancel" | "close")
        }
    }
    fn parse_request(frame: &str, base: &Path) -> Result<Request, String> {
        json::with_value(frame, |value| {
            object(
                value,
                &[
                    "version",
                    "request_id",
                    "op",
                    "source",
                    "base",
                    "options",
                    "job_id",
                    "expected_revision",
                ],
            )?;
            if value.get("version").and_then(|v| v.integer().ok()) != Some(1) {
                return Err("unsupported_version: expected protocol version 1".into());
            }
            let id = json::text(value.get("request_id").ok_or("missing request_id")?)?;
            valid_request_id(&id)?;
            let op = json::text(value.get("op").ok_or("missing op")?)?;
            if !matches!(
                op.as_str(),
                "status" | "script" | "observe" | "result" | "cancel" | "close"
            ) {
                return Err("unknown session operation".into());
            }
            let action = matches!(op.as_str(), "script" | "observe" | "cancel" | "close");
            let expected = value.get("expected_revision").map(unsigned).transpose()?;
            if expected.is_some() && !action {
                return Err(
                    "expected_revision is valid only for script, observe, cancel or close".into(),
                );
            }
            let job_id = value.get("job_id").map(unsigned).transpose()?;
            if job_id.is_some() && (op != "result" || job_id == Some(0)) {
                return Err("job_id is a positive integer used only by result".into());
            }
            let mut options = Options::default();
            let mut source = None;
            let mut base = base.to_owned();
            if matches!(op.as_str(), "script" | "observe") {
                let text = json::text(value.get("source").ok_or("missing source")?)?;
                if text.len() > MAX_SOURCE_BYTES {
                    return Err("source exceeds 256 KiB".into());
                }
                source = Some(text);
                if let Some(path) = value.get("base") {
                    base = PathBuf::from(json::text(path)?);
                    if !base.is_absolute() || !base.is_dir() {
                        return Err("base must be an existing absolute directory".into());
                    }
                }
                if let Some(value) = value.get("options") {
                    object(value, &["read_only", "dry_run", "max_steps", "max_wait_ms"])?;
                    if let Some(v) = value.get("read_only") {
                        options.read_only = v.boolean().ok_or("read_only must be boolean")?;
                    }
                    if let Some(v) = value.get("dry_run") {
                        options.dry_run = v.boolean().ok_or("dry_run must be boolean")?;
                    }
                    if let Some(v) = value.get("max_steps") {
                        options.max_steps =
                            usize::try_from(unsigned(v)?).map_err(|_| "max_steps is too large")?;
                    }
                    if let Some(v) = value.get("max_wait_ms") {
                        options.max_wait_ms = unsigned(v)?;
                    }
                }
                if options.max_steps == 0
                    || options.max_steps > 65_536
                    || options.max_wait_ms > 86_400_000
                {
                    return Err(
                        "session limits require max_steps=1..65536 and max_wait_ms=0..86400000"
                            .into(),
                    );
                }
                options.read_only |= op == "observe";
            } else if ["source", "base", "options"]
                .iter()
                .any(|key| value.get(key).is_some())
            {
                return Err("source, base and options are only valid for script or observe".into());
            }
            Ok(Request {
                id,
                op,
                source,
                base,
                options,
                job_id,
                expected,
            })
        })
    }

    fn remember(control: &Control, state: &mut State, id: &str, response: &str) {
        if let Some(cached) = state.cache.get_mut(id) {
            state.cache_bytes += response.len();
            cached.response = Some(response.into());
        }
        control.changed.notify_all();
    }

    fn handle(control: &Control, frame: &str) -> String {
        let request = match parse_request(frame, &control.base) {
            Ok(request) => request,
            Err(error) => {
                let id = json::field(frame, "request_id")
                    .ok()
                    .filter(|id| valid_request_id(id).is_ok());
                let code = if error.starts_with("unsupported_version:") {
                    "unsupported_version"
                } else {
                    "invalid_request"
                };
                return control.error(&control.lock(), id.as_deref(), code, &error);
            }
        };
        let action = request.action();
        let mut state = control.lock();
        if action {
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            while let Some(cached) = state.cache.get(&request.id) {
                if cached.request != frame {
                    return control.error(
                        &state,
                        Some(&request.id),
                        "request_id_conflict",
                        "request_id was already used with a different frame",
                    );
                }
                if let Some(response) = &cached.response {
                    let response =
                        format!("{},\"replayed\":true}}", &response[..response.len() - 1]);
                    state.last_activity = Instant::now();
                    return response;
                }
                if Instant::now() >= deadline {
                    return control.error(
                        &state,
                        Some(&request.id),
                        "request_pending",
                        "original request is still completing; retry the same frame",
                    );
                }
                state = control
                    .changed
                    .wait_timeout(state, IO_TIMEOUT)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            while state.cache.len() >= MAX_REQUESTS
                || state.cache_bytes + frame.len() + 4096 > MAX_REQUEST_CACHE_BYTES
            {
                let removable = state.cache_order.iter().position(|id| {
                    state
                        .cache
                        .get(id)
                        .is_some_and(|entry| entry.response.is_some())
                });
                let Some(index) = removable else {
                    return control.error(
                        &state,
                        Some(&request.id),
                        "busy",
                        "request cache is full of pending operations",
                    );
                };
                let id = state.cache_order.remove(index).unwrap();
                let entry = state.cache.remove(&id).unwrap();
                state.cache_bytes -=
                    entry.request.len() + entry.response.as_ref().map_or(0, String::len);
            }
            state.cache_bytes += frame.len();
            state.cache_order.push_back(request.id.clone());
            state.cache.insert(
                request.id.clone(),
                Cached {
                    request: frame.into(),
                    response: None,
                },
            );
        }
        state.last_activity = Instant::now();
        if request
            .expected
            .is_some_and(|revision| revision != state.revision)
        {
            let response = control.error(
                &state,
                Some(&request.id),
                "stale_revision",
                "expected_revision does not match the current Zuse session state",
            );
            remember(control, &mut state, &request.id, &response);
            return response;
        }
        let mut cancel = None;
        let mut wait_close = false;
        let response = match request.op.as_str() {
            "status" => {
                let last = state.completed.back();
                control.response(&state, Some(&request.id), &format!("\"ok\":true,\"status\":\"ready\",\"pid\":{},\"socket\":{},\"active_job_id\":{},\"last_job_id\":{},\"last_status\":{}", std::process::id(), json::quote(&control.socket.to_string_lossy()), state.active.as_ref().map_or_else(|| "null".into(), |a| a.id.to_string()), last.map_or_else(|| "null".into(), |r| r.id.to_string()), last.map_or_else(|| "null".into(), |r| json::quote(r.status))))
            }
            "result" => {
                let id = request
                    .job_id
                    .or_else(|| state.active.as_ref().map(|a| a.id))
                    .or_else(|| state.completed.back().map(|r| r.id));
                if let Some(id) = id {
                    if state.active.as_ref().is_some_and(|a| a.id == id) {
                        control.response(
                            &state,
                            Some(&request.id),
                            &format!(
                                "\"ok\":true,\"status\":\"running\",\"job_id\":{id},\"result\":null"
                            ),
                        )
                    } else if let Some(result) = state.completed.iter().find(|r| r.id == id) {
                        control.response(&state, Some(&request.id), &format!("\"ok\":true,\"status\":{},\"job_id\":{id},\"elapsed_ms\":{},\"result_bytes\":{},\"result_truncated\":{},\"result\":{}", json::quote(result.status), result.elapsed_ms, result.bytes, result.truncated, result.result))
                    } else {
                        control.error(
                            &state,
                            Some(&request.id),
                            "job_not_found",
                            "job is unknown or no longer retained",
                        )
                    }
                } else {
                    control.response(
                        &state,
                        Some(&request.id),
                        "\"ok\":true,\"status\":\"empty\",\"result\":null",
                    )
                }
            }
            "script" | "observe" => {
                if state.closing {
                    control.error(
                        &state,
                        Some(&request.id),
                        "session_closing",
                        "session is closing",
                    )
                } else if state.active.is_some() {
                    control.error(&state, Some(&request.id), "busy", "a program is already running; poll result or cancel it before submitting another")
                } else {
                    let id = state.next_job;
                    state.next_job += 1;
                    let cancellation = Cancellation::default();
                    state.active = Some(Active {
                        id,
                        cancellation: cancellation.clone(),
                        cancelling: false,
                    });
                    let job = Job {
                        id,
                        source: request.source.unwrap(),
                        base: request.base,
                        options: request.options,
                        cancellation,
                    };
                    if control.work.try_send(Work::Run(job)).is_err() {
                        state.active = None;
                        state.closing = true;
                        state.advance();
                        control.error(
                            &state,
                            Some(&request.id),
                            "worker_unavailable",
                            "session worker is unavailable",
                        )
                    } else {
                        state.advance();
                        control.response(
                            &state,
                            Some(&request.id),
                            &format!("\"ok\":true,\"status\":\"accepted\",\"job_id\":{id}"),
                        )
                    }
                }
            }
            "cancel" => {
                if let Some(active) = &mut state.active {
                    cancel = Some(active.cancellation.clone());
                    if !active.cancelling {
                        active.cancelling = true;
                        state.advance();
                    }
                    control.response(
                        &state,
                        Some(&request.id),
                        "\"ok\":true,\"status\":\"cancelling\",\"cleanup_complete\":false",
                    )
                } else {
                    control.response(
                        &state,
                        Some(&request.id),
                        "\"ok\":true,\"status\":\"idle\",\"cleanup_complete\":true",
                    )
                }
            }
            "close" => {
                if !state.closing {
                    state.closing = true;
                    state.advance();
                }
                cancel = state.active.as_ref().map(|a| a.cancellation.clone());
                wait_close = true;
                let _ = control.work.try_send(Work::Close);
                String::new()
            }
            _ => unreachable!(),
        };
        if action && !wait_close {
            remember(control, &mut state, &request.id, &response);
        }
        drop(state);
        if let Some(cancellation) = cancel {
            cancellation.request_cancel();
        }
        if wait_close {
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            let mut state = control.lock();
            while !state.endpoint_closed && Instant::now() < deadline {
                state = control
                    .changed
                    .wait_timeout(state, IO_TIMEOUT)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            let response = control.response(
                &state,
                Some(&request.id),
                &format!(
                    "\"ok\":true,\"status\":{},\"resources_released\":{}",
                    json::quote(if state.endpoint_closed {
                        "closed"
                    } else {
                        "closing"
                    }),
                    state.endpoint_closed
                ),
            );
            remember(control, &mut state, &request.id, &response);
            response
        } else {
            response
        }
    }

    fn read_frame(
        reader: &mut BufReader<UnixStream>,
        stopped: impl Fn() -> bool,
    ) -> Result<Option<String>, String> {
        let deadline = Instant::now() + FRAME_TIMEOUT;
        let mut bytes = Vec::new();
        loop {
            if stopped() {
                return Ok(None);
            }
            if Instant::now() >= deadline {
                return Err("frame_timeout: incomplete or idle connection".into());
            }
            match reader.fill_buf() {
                Ok([]) if bytes.is_empty() => return Ok(None),
                Ok([]) => return Err("invalid_request: request frame requires a newline".into()),
                Ok(buffer) => {
                    let newline = buffer.iter().position(|b| *b == b'\n');
                    let length = newline.unwrap_or(buffer.len());
                    if bytes.len() + length > MAX_FRAME_BYTES {
                        return Err("frame_too_large: frame exceeds 2 MiB".into());
                    }
                    bytes.extend_from_slice(&buffer[..length]);
                    reader.consume(length + usize::from(newline.is_some()));
                    if newline.is_some() {
                        return String::from_utf8(bytes)
                            .map(Some)
                            .map_err(|_| "invalid_request: frame is not UTF-8".into());
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn connection(stream: UnixStream, control: Arc<Control>) {
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream);
        loop {
            let response = match read_frame(&mut reader, || control.lock().worker_done) {
                Ok(Some(frame)) => handle(&control, &frame),
                Ok(None) => break,
                Err(error) => {
                    let code = if error.starts_with("frame_too_large:") {
                        "frame_too_large"
                    } else {
                        "invalid_request"
                    };
                    let response = control.error(&control.lock(), None, code, &error);
                    let _ = writeln!(reader.get_mut(), "{response}");
                    break;
                }
            };
            if writeln!(reader.get_mut(), "{response}").is_err() {
                break;
            }
        }
    }

    fn serve(name: &str, idle_timeout_ms: u64) -> Result<String, String> {
        let socket = socket_path(name)?;
        if fs::symlink_metadata(&socket).is_ok() {
            return Err(
                "session endpoint already exists; inspect it before removing a stale socket".into(),
            );
        }
        let interrupt = InterruptGuard::install()?;
        let listener = UnixListener::bind(&socket).map_err(|e| format!("bind session: {e}"))?;
        let meta = fs::symlink_metadata(&socket).map_err(|e| e.to_string())?;
        let owner = SocketOwner {
            path: socket.clone(),
            device: meta.dev(),
            inode: meta.ino(),
        };
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (work, input) = mpsc::sync_channel(1);
        let control = Arc::new(Control {
            name: name.into(),
            socket,
            base: std::env::current_dir().map_err(|e| e.to_string())?,
            work,
            state: Mutex::new(State {
                revision: 1,
                next_job: 1,
                active: None,
                completed: VecDeque::new(),
                closing: false,
                worker_done: false,
                endpoint_closed: false,
                last_activity: Instant::now(),
                cache: BTreeMap::new(),
                cache_order: VecDeque::new(),
                cache_bytes: 0,
            }),
            changed: Condvar::new(),
        });
        let worker_control = control.clone();
        let worker = thread::Builder::new()
            .name("zuse-session-runtime".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker(&worker_control, input)
                }));
                let mut state = worker_control.lock();
                if outcome.is_err()
                    && let Some(active) = state.active.take()
                {
                    state.completed.push_back(Completed {
                        id: active.id,
                        status: "failed",
                        result: "{\"status\":\"failed\",\"error\":\"session worker panicked\"}"
                            .into(),
                        bytes: 0,
                        truncated: false,
                        elapsed_ms: 0,
                    });
                }
                state.closing = true;
                state.worker_done = true;
                state.advance();
                worker_control.changed.notify_all();
            })
            .map_err(|e| format!("start session worker: {e}"))?;
        let ready = control.response(&control.lock(), None, &format!("\"ok\":true,\"status\":\"started\",\"pid\":{},\"socket\":{},\"idle_timeout_ms\":{idle_timeout_ms}", std::process::id(), json::quote(&control.socket.to_string_lossy())));
        println!("{ready}");
        let _ = io::stdout().flush();
        let mut connections: Vec<thread::JoinHandle<()>> = Vec::new();
        loop {
            if control.lock().worker_done {
                break;
            }
            control.expire_if_idle(Duration::from_millis(idle_timeout_ms));
            if interrupt.cancelled() {
                control.request_close();
            }
            let mut index = 0;
            while index < connections.len() {
                if connections[index].is_finished() {
                    let _ = connections.swap_remove(index).join();
                } else {
                    index += 1;
                }
            }
            match listener.accept() {
                Ok((stream, _)) if connections.len() < MAX_CONNECTIONS => {
                    let next = control.clone();
                    if let Ok(thread) = thread::Builder::new()
                        .name("zuse-session-control".into())
                        .spawn(move || connection(stream, next))
                    {
                        connections.push(thread);
                    }
                }
                Ok(_) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::Interrupted =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(_) => {
                    control.request_close();
                    thread::sleep(IO_TIMEOUT);
                }
            }
        }
        drop(listener);
        // Close receipts wait for this marker, not for their own connection
        // thread to exit. Partial peers cannot delay endpoint cleanup either.
        drop(owner);
        {
            let mut state = control.lock();
            state.endpoint_closed = true;
            control.changed.notify_all();
        }
        let _ = worker.join();
        for connection in connections {
            let _ = connection.join();
        }
        Ok(control.response(
            &control.lock(),
            None,
            "\"ok\":true,\"status\":\"closed\",\"resources_released\":true",
        ))
    }

    fn request(path: &Path, frame: &str) -> Result<String, String> {
        verify_socket(path)?;
        if frame.len() > MAX_FRAME_BYTES {
            return Err("request frame exceeds 2 MiB".into());
        }
        let mut stream = UnixStream::connect(path).map_err(|e| format!("connect session: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(12)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        writeln!(stream, "{frame}").map_err(|e| format!("send session request: {e}"))?;
        let mut reader = BufReader::new(stream);
        let result =
            read_frame(&mut reader, || false)?.ok_or("session closed without a response")?;
        let id = json::field(frame, "request_id")?;
        let accepted = json::with_value(&result, |value| {
            if value.get("version").and_then(|v| v.integer().ok())
                != Some(i64::from(PROTOCOL_VERSION))
            {
                return Err("session response has an unsupported protocol version".into());
            }
            if json::text(
                value
                    .get("request_id")
                    .ok_or("session response is missing request_id")?,
            )? != id
            {
                return Err("session response request_id does not match the request".into());
            }
            value
                .get("ok")
                .and_then(|v| v.boolean())
                .ok_or_else(|| "session response is missing a boolean ok field".into())
        })?;
        if accepted { Ok(result) } else { Err(result) }
    }

    fn request_id() -> String {
        format!(
            "cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
    }

    fn stop_child(child: &mut Child) {
        // Only the child just spawned by this start command is signalled.
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            kill(child.id() as i32, 15);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    fn start(name: &str, idle_timeout_ms: u64) -> Result<String, String> {
        let path = socket_path(name)?;
        if fs::symlink_metadata(&path).is_ok() {
            return Err(
                "session endpoint already exists; use status or inspect the stale socket".into(),
            );
        }
        let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command
            .args([
                "session",
                "serve",
                name,
                "--idle-timeout-ms",
                &idle_timeout_ms.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if setsid() < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = command.spawn().map_err(|e| format!("start session: {e}"))?;
        let frame = format!(
            "{{\"version\":1,\"request_id\":{},\"op\":\"status\"}}",
            json::quote(&request_id())
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("session server exited before readiness: {status}"));
            }
            if let Ok(response) = request(&path, &frame) {
                let pid = json::with_value(&response, |v| {
                    unsigned(v.get("pid").ok_or("missing server pid")?)
                })?;
                if pid != u64::from(child.id()) {
                    stop_child(&mut child);
                    return Err("another process owns this session endpoint".into());
                }
                return Ok(response.replacen("\"status\":\"ready\"", "\"status\":\"started\"", 1));
            }
            thread::sleep(Duration::from_millis(10));
        }
        stop_child(&mut child);
        Err("session server did not become ready within five seconds".into())
    }

    pub(super) fn run(args: &[&str]) -> Result<String, String> {
        let (op, tail) = args.split_first().ok_or(HELP)?;
        let (name, tail) = tail.split_first().ok_or(HELP)?;
        valid_name(name)?;
        if matches!(*op, "start" | "serve") {
            let timeout = match tail {
                [] => DEFAULT_IDLE_TIMEOUT_MS,
                ["--idle-timeout-ms", value] => value
                    .parse::<u64>()
                    .map_err(|_| "idle timeout must be a positive integer")?,
                _ => return Err(HELP.into()),
            };
            if timeout == 0 || timeout > 86_400_000 {
                return Err("idle timeout must be 1..86400000 milliseconds".into());
            }
            return if *op == "start" {
                start(name, timeout)
            } else {
                serve(name, timeout)
            };
        }
        if !matches!(
            *op,
            "status" | "script" | "observe" | "result" | "cancel" | "close"
        ) {
            return Err(HELP.into());
        }
        let mut tail = tail;
        let mut job_id = None;
        if *op == "result" && !tail.is_empty() {
            if tail.len() != 1 {
                return Err(HELP.into());
            }
            let value = tail[0]
                .parse::<u64>()
                .map_err(|_| "JOB_ID must be a positive integer")?;
            if value == 0 || value > i64::MAX as u64 {
                return Err("JOB_ID must be a positive integer".into());
            }
            job_id = Some(value);
            tail = &[];
        }
        let script = matches!(*op, "script" | "observe");
        if script {
            if tail.first() != Some(&"-") {
                return Err("session script/observe requires '-' and source on stdin".into());
            }
            tail = &tail[1..];
        }
        let action = script || matches!(*op, "cancel" | "close");
        let mut id = request_id();
        let mut expected = None;
        let mut options = Options {
            read_only: *op == "observe",
            ..Options::default()
        };
        let mut seen = BTreeSet::new();
        while let Some((flag, rest)) = tail.split_first() {
            if !seen.insert(*flag) {
                return Err(format!("duplicate session option {flag}"));
            }
            tail = rest;
            match *flag {
                "--read-only" if script => options.read_only = true,
                "--dry-run" if script => options.dry_run = true,
                "--max-steps" | "--max-wait-ms" | "--expect-revision" | "--request-id" => {
                    let (value, rest) = tail.split_first().ok_or("session option needs a value")?;
                    tail = rest;
                    match *flag {
                        "--request-id" if action => {
                            valid_request_id(value)?;
                            id = (*value).into();
                        }
                        "--expect-revision" if action => {
                            expected = Some(
                                value
                                    .parse::<u64>()
                                    .map_err(|_| "revision must be a nonnegative integer")?,
                            )
                        }
                        "--max-steps" if script => {
                            options.max_steps =
                                value.parse().map_err(|_| "max-steps must be an integer")?
                        }
                        "--max-wait-ms" if script => {
                            options.max_wait_ms = value
                                .parse()
                                .map_err(|_| "max-wait-ms must be an integer")?
                        }
                        _ => return Err(HELP.into()),
                    }
                }
                _ => return Err(HELP.into()),
            }
        }
        let mut frame = format!(
            "{{\"version\":1,\"request_id\":{},\"op\":{}",
            json::quote(&id),
            json::quote(op)
        );
        if let Some(revision) = expected {
            frame.push_str(&format!(",\"expected_revision\":{revision}"));
        }
        if let Some(job_id) = job_id {
            frame.push_str(&format!(",\"job_id\":{job_id}"));
        }
        if script {
            let mut bytes = Vec::new();
            io::stdin()
                .lock()
                .take(MAX_SOURCE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_SOURCE_BYTES {
                return Err("source exceeds 256 KiB".into());
            }
            let source = String::from_utf8(bytes).map_err(|_| "source must be UTF-8")?;
            let base = std::env::current_dir().map_err(|e| e.to_string())?;
            let base = base.to_str().ok_or("script base directory must be UTF-8")?;
            frame.push_str(&format!(",\"source\":{},\"base\":{},\"options\":{{\"read_only\":{},\"dry_run\":{},\"max_steps\":{},\"max_wait_ms\":{}}}", json::quote(&source), json::quote(base), options.read_only, options.dry_run, options.max_steps, options.max_wait_ms));
        }
        frame.push('}');
        request(&socket_path(name)?, &frame)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pretty_native_json_stays_one_frame_on_a_persistent_socket() {
            let result = r#"{
              "status": "completed",
              "results": [{"value": {
                "workspace": { "id": 1, "name": "work space" },
                "text": "line1\nline2\t\"quoted\" \\ path"
              }}]
            }"#;
            let (work, _input) = mpsc::sync_channel(1);
            let control = Arc::new(Control {
                name: "wire-test".into(),
                socket: PathBuf::from("/unused"),
                base: std::env::current_dir().unwrap(),
                work,
                state: Mutex::new(State {
                    revision: 3,
                    next_job: 2,
                    active: None,
                    completed: VecDeque::from([Completed {
                        id: 1,
                        status: "completed",
                        result: result.into(),
                        bytes: result.len(),
                        truncated: false,
                        elapsed_ms: 1,
                    }]),
                    closing: false,
                    worker_done: false,
                    endpoint_closed: false,
                    last_activity: Instant::now(),
                    cache: BTreeMap::new(),
                    cache_order: VecDeque::new(),
                    cache_bytes: 0,
                }),
                changed: Condvar::new(),
            });
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let worker = thread::spawn(move || connection(server, control));
            let mut read = BufReader::new(client.try_clone().unwrap());
            for (id, op) in [("result-one", "result"), ("status-two", "status")] {
                writeln!(
                    client,
                    "{{\"version\":1,\"request_id\":\"{id}\",\"op\":\"{op}\"}}"
                )
                .unwrap();
                let mut response = String::new();
                assert!(read.read_line(&mut response).unwrap() > 0);
                assert_eq!(json::field(&response, "request_id").unwrap(), id);
                json::with_value(&response, |root| {
                    assert_eq!(root.get("ok").and_then(|value| value.boolean()), Some(true));
                    if op == "result" {
                        let value = root
                            .get("result")
                            .unwrap()
                            .get("results")
                            .unwrap()
                            .elements()
                            .next()
                            .unwrap()
                            .get("value")
                            .unwrap();
                        assert_eq!(
                            json::text(value.get("text").unwrap())?,
                            "line1\nline2\t\"quoted\" \\ path"
                        );
                        assert_eq!(
                            json::text(value.get("workspace").unwrap().get("name").unwrap())?,
                            "work space"
                        );
                    }
                    Ok(())
                })
                .unwrap();
            }
            drop(read);
            drop(client);
            worker.join().unwrap();
        }
    }
}
