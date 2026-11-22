//! IDE, managed-process and Omarchy adapters using literal argument vectors.
//! Programs are never wrapped in a shell. The application owns its allowlist.
mod app_launch;
use crate::json::{quote, with_value};
pub use app_launch::launch;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
const STREAM_LIMIT: usize = 128 * 1024;
const INPUT_LIMIT: usize = 64 * 1024;
const MAX_PROCESSES: usize = 8;
const MAX_HISTORY: usize = 32;
const THREAD_STACK: usize = 64 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
type Result<T> = std::result::Result<T, String>;

#[derive(Default)]
struct Tail {
    bytes: VecDeque<u8>,
    dropped: u64,
    total: u64,
    closed: bool,
    error: Option<String>,
}
impl Tail {
    fn append(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        let excess = (self.bytes.len() + bytes.len()).saturating_sub(STREAM_LIMIT);
        let discard_old = excess.min(self.bytes.len());
        self.bytes.drain(..discard_old);
        self.dropped = self.dropped.saturating_add(excess as u64);
        self.bytes
            .extend(bytes[excess - discard_old..].iter().copied());
    }
    fn take(&mut self) -> String {
        let bytes = self.bytes.make_contiguous();
        let mut boundary = bytes.len();
        if !self.closed && boundary > 0 {
            let mut start = boundary - 1;
            while start > 0 && bytes[start] & 0xc0 == 0x80 && boundary - start < 4 {
                start -= 1;
            }
            let expected = match bytes[start] {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 1,
            };
            if expected > boundary - start {
                boundary = start;
            }
        }
        String::from_utf8_lossy(&self.bytes.drain(..boundary).collect::<Vec<_>>()).into_owned()
    }
}
trait PollPipe: Read + Send + 'static {
    fn prepare(&self) -> io::Result<()>;
    fn available(&self) -> io::Result<Option<usize>>;
}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd + Send + 'static> PollPipe for T {
    fn prepare(&self) -> io::Result<()> {
        unsafe extern "C" {
            fn fcntl(fd: i32, command: i32, ...) -> i32;
        }
        #[cfg(target_os = "macos")]
        const NONBLOCK: i32 = 0x4;
        #[cfg(not(target_os = "macos"))]
        const NONBLOCK: i32 = 0x800;
        // SAFETY: this descriptor belongs to the live owned pipe. Preserve all
        // current flags, adding nonblocking behavior to its read endpoint.
        let flags = unsafe { fcntl(self.as_raw_fd(), 3) };
        if flags < 0 || unsafe { fcntl(self.as_raw_fd(), 4, flags | NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn available(&self) -> io::Result<Option<usize>> {
        Ok(Some(8192))
    }
}
#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle + Send + 'static> PollPipe for T {
    fn prepare(&self) -> io::Result<()> {
        Ok(())
    }
    fn available(&self) -> io::Result<Option<usize>> {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn PeekNamedPipe(
                handle: *mut core::ffi::c_void,
                buffer: *mut core::ffi::c_void,
                size: u32,
                read: *mut u32,
                available: *mut u32,
                left: *mut u32,
            ) -> i32;
        }
        let mut available = 0;
        // SAFETY: the owned pipe handle is live, and available is writable.
        if unsafe {
            PeekNamedPipe(
                self.as_raw_handle(),
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
                &mut available,
                core::ptr::null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(109) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        Ok(Some(available as usize))
    }
}
#[cfg(not(any(unix, windows)))]
impl<T: Read + Send + 'static> PollPipe for T {
    fn prepare(&self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process pipes require Unix or Windows",
        ))
    }
    fn available(&self) -> io::Result<Option<usize>> {
        Ok(None)
    }
}
struct Reader {
    tail: Arc<Mutex<Tail>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Reader {
    fn start(mut pipe: impl PollPipe, name: &str) -> Result<Self> {
        pipe.prepare()
            .map_err(|e| format!("prepare process pipe: {e}"))?;
        let tail = Arc::new(Mutex::new(Tail::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let output = Arc::clone(&tail);
        let stopping = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name(name.into())
            .stack_size(THREAD_STACK)
            .spawn(move || {
                let mut buffer = [0; 8192];
                let mut failure = None;
                while !stopping.load(Ordering::Relaxed) {
                    let available = match pipe.available() {
                        Ok(None) => break,
                        Ok(Some(0)) => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Ok(Some(n)) => n.min(buffer.len()),
                        Err(e) => {
                            failure = Some(e.to_string());
                            break;
                        }
                    };
                    match pipe.read(&mut buffer[..available]) {
                        Ok(0) => break,
                        Ok(n) => output
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .append(&buffer[..n]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            failure = Some(e.to_string());
                            break;
                        }
                    }
                }
                let mut tail = output.lock().unwrap_or_else(|e| e.into_inner());
                tail.closed = true;
                tail.error = failure;
            })
            .map_err(|e| format!("start process reader: {e}"))?;
        Ok(Self {
            tail,
            stop,
            handle: Some(handle),
        })
    }
    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
    fn closed(&self) -> bool {
        self.tail.lock().unwrap_or_else(|e| e.into_inner()).closed
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.finish();
    }
}
struct Input {
    bytes: Vec<u8>,
    result: SyncSender<(usize, Option<String>)>,
}
struct Managed {
    child: Child,
    group: Option<ProcessGroup>,
    stdout: Reader,
    stderr: Reader,
    input: Option<SyncSender<Input>>,
    writer: Option<JoinHandle<()>>,
    status: Option<ExitStatus>,
    stopped: bool,
    program: String,
    owned_descendants: bool,
}
impl Managed {
    fn spawn(program: &OsStr, args: &[OsString], interactive: bool) -> Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(if interactive {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        if std::env::var_os("OMARCHY_PATH").is_none()
            && let Some(root) = omarchy_root()
        {
            command.env("OMARCHY_PATH", root);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("start {}: {e}", program.to_string_lossy()))?;
        let group = match ProcessGroup::new(&child) {
            Ok(group) => group,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let stdout = match Reader::start(
            child.stdout.take().ok_or("stdout pipe missing")?,
            "zuse-stdout",
        ) {
            Ok(reader) => reader,
            Err(error) => {
                group.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let stderr = match Reader::start(
            child.stderr.take().ok_or("stderr pipe missing")?,
            "zuse-stderr",
        ) {
            Ok(reader) => reader,
            Err(error) => {
                group.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let (input, writer) = if let Some(mut pipe) = child.stdin.take() {
            let (sender, receiver) = mpsc::sync_channel::<Input>(1);
            let writer = thread::Builder::new()
                .name("zuse-stdin".into())
                .stack_size(THREAD_STACK)
                .spawn(move || {
                    while let Ok(request) = receiver.recv() {
                        let mut written = 0;
                        let mut error = None;
                        while written < request.bytes.len() {
                            match pipe.write(&request.bytes[written..]) {
                                Ok(0) => {
                                    error = Some("stdin closed".into());
                                    break;
                                }
                                Ok(n) => written += n,
                                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                                Err(e) => {
                                    error = Some(e.to_string());
                                    break;
                                }
                            }
                        }
                        if error.is_none()
                            && let Err(e) = pipe.flush()
                        {
                            error = Some(e.to_string());
                        }
                        let _ = request.result.send((written, error));
                    }
                });
            match writer {
                Ok(writer) => (Some(sender), Some(writer)),
                Err(error) => {
                    group.terminate();
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("start stdin writer: {error}"));
                }
            }
        } else {
            (None, None)
        };
        Ok(Self {
            child,
            group: Some(group),
            stdout,
            stderr,
            input,
            writer,
            status: None,
            stopped: false,
            program: program.to_string_lossy().into_owned(),
            owned_descendants: interactive,
        })
    }
    fn refresh(&mut self) -> Result<()> {
        if self.status.is_none() {
            self.status = self
                .child
                .try_wait()
                .map_err(|e| format!("read process status: {e}"))?;
            if self.status.is_some() && self.owned_descendants {
                // Dispose the group immediately after reaping, preventing a stale
                // remembered PID from later addressing a reused process group.
                if let Some(group) = self.group.take() {
                    group.terminate();
                }
                self.input.take();
            }
        }
        Ok(())
    }
    fn finish(&mut self, terminate: bool) {
        self.input.take();
        if terminate {
            if let Some(group) = &self.group {
                group.terminate();
            }
            if self.status.is_none() {
                let _ = self.child.kill();
                self.status = self.child.wait().ok();
            }
            self.stopped = true;
        }
        let deadline = Instant::now() + Duration::from_millis(100);
        while !(self.stdout.closed() && self.stderr.closed()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        self.stdout.finish();
        self.stderr.finish();
        if let Some(writer) = self.writer.take()
            && (writer.is_finished() || terminate)
        {
            let _ = writer.join();
        }
        if let Some(mut group) = self.group.take()
            && !terminate
        {
            group.release();
        }
    }
    fn summary(&self, id: u64) -> String {
        let status = self.status.map_or(
            "running",
            |_| if self.stopped { "stopped" } else { "exited" },
        );
        let code = self
            .status
            .and_then(|s| s.code())
            .map_or("null".into(), |n| n.to_string());
        format!(
            "{{\"id\":{id},\"pid\":{},\"program\":{},\"state\":{},\"exit_code\":{code}}}",
            self.child.id(),
            quote(&self.program),
            quote(status)
        )
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        self.finish(true);
    }
}

/// Owns bounded interactive processes. An application controls which executable
/// it passes to start. Children have pipes, not a pseudo-terminal.
pub struct ProcessManager {
    processes: BTreeMap<u64, Managed>,
    next_id: u64,
}
impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}
impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: BTreeMap::new(),
            next_id: 1,
        }
    }
    pub fn start(&mut self, program: &str, args: &str) -> Result<String> {
        validate_program(program)?;
        for process in self.processes.values_mut() {
            process.refresh()?;
        }
        if self
            .processes
            .values()
            .filter(|p| p.status.is_none())
            .count()
            >= MAX_PROCESSES
        {
            return Err(format!(
                "at most {MAX_PROCESSES} managed processes may run at once"
            ));
        }
        while self.processes.len() >= MAX_HISTORY {
            let finished = self
                .processes
                .iter()
                .find(|(_, p)| p.status.is_some())
                .map(|(id, _)| *id)
                .ok_or("process history is full")?;
            self.processes.remove(&finished);
        }
        let arguments = parse_arguments(args)?
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or("process ID space exhausted")?;
        let managed = Managed::spawn(OsStr::new(program), &arguments, true)?;
        let summary = managed.summary(id);
        self.processes.insert(id, managed);
        Ok(summary)
    }
    pub fn write(&mut self, id: u64, text: &str) -> Result<String> {
        if text.len() > INPUT_LIMIT {
            return Err("process input exceeds 64 KiB".into());
        }
        let process = self
            .processes
            .get_mut(&id)
            .ok_or_else(|| format!("unknown process {id}"))?;
        process.refresh()?;
        if process.status.is_some() {
            return Err(format!("process {id} has exited"));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        process
            .input
            .as_ref()
            .ok_or("process stdin is closed")?
            .try_send(Input {
                bytes: text.as_bytes().to_vec(),
                result: sender,
            })
            .map_err(|e| format!("queue process input: {e}"))?;
        match receiver.recv_timeout(WRITE_TIMEOUT) {
            Ok((written, None)) => Ok(format!("{{\"id\":{id},\"written\":{written}}}")),
            Ok((written, Some(error))) => Err(format!(
                "process stdin failed after {written} bytes: {error}"
            )),
            Err(error) => {
                process.finish(true);
                Err(format!(
                    "process stdin did not complete within 2 seconds; process {id} stopped: {error}"
                ))
            }
        }
    }
    /// Drain buffered stdout/stderr. Cumulative drop counts report output loss.
    pub fn read(&mut self, id: u64) -> Result<String> {
        let process = self
            .processes
            .get_mut(&id)
            .ok_or_else(|| format!("unknown process {id}"))?;
        process.refresh()?;
        if process.status.is_some() {
            let deadline = Instant::now() + Duration::from_millis(50);
            while !(process.stdout.closed() && process.stderr.closed()) && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(5));
            }
        }
        let mut out = process
            .stdout
            .tail
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut err = process
            .stderr
            .tail
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let stdout = out.take();
        let stderr = err.take();
        let summary = process.summary(id);
        Ok(format!(
            "{},\"stdout\":{},\"stderr\":{},\"stdout_dropped\":{},\"stderr_dropped\":{},\"stdout_total\":{},\"stderr_total\":{},\"stdout_closed\":{},\"stderr_closed\":{},\"stdout_error\":{},\"stderr_error\":{}}}",
            &summary[..summary.len() - 1],
            quote(&stdout),
            quote(&stderr),
            out.dropped,
            err.dropped,
            out.total,
            err.total,
            out.closed,
            err.closed,
            out.error.as_deref().map_or("null".into(), quote),
            err.error.as_deref().map_or("null".into(), quote)
        ))
    }
    pub fn stop(&mut self, id: u64) -> Result<String> {
        let process = self
            .processes
            .get_mut(&id)
            .ok_or_else(|| format!("unknown process {id}"))?;
        process.refresh()?;
        process.finish(true);
        Ok(process.summary(id))
    }
    pub fn list(&mut self) -> String {
        let values = self
            .processes
            .iter_mut()
            .map(|(id, process)| {
                if let Err(error) = process.refresh() {
                    return format!("{{\"id\":{id},\"error\":{}}}", quote(&error));
                }
                process.summary(*id)
            })
            .collect::<Vec<_>>();
        format!("[{}]", values.join(","))
    }
}
pub(crate) fn validate_start(program: &str, args: &str) -> Result<()> {
    validate_program(program)?;
    parse_arguments(args)?;
    Ok(())
}

fn validate_program(program: &str) -> Result<()> {
    if program.is_empty() || program.len() > 4096 || program.contains('\0') {
        return Err("program must contain 1..4096 non-NUL bytes".into());
    }
    #[cfg(windows)]
    if program.to_ascii_lowercase().ends_with(".cmd")
        || program.to_ascii_lowercase().ends_with(".bat")
    {
        return Err("use a native executable instead of a Windows batch launcher".into());
    }
    Ok(())
}
fn parse_arguments(input: &str) -> Result<Vec<String>> {
    if input.len() > INPUT_LIMIT || input.contains('\0') {
        return Err("arguments exceed 64 KiB or contain NUL".into());
    }
    let mut args = Vec::new();
    let mut current = String::new();
    let mut delimiter = None;
    let mut escaped = false;
    let mut started = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        if ch == '\\' && delimiter != Some('\'') {
            escaped = true;
            started = true;
            continue;
        }
        if let Some(quote_char) = delimiter {
            if ch == quote_char {
                delimiter = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                delimiter = Some(ch);
                started = true;
            }
            c if c.is_whitespace() => {
                if started {
                    args.push(core::mem::take(&mut current));
                    started = false;
                }
                if args.len() > 128 {
                    return Err("at most 128 process arguments are accepted".into());
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if escaped || delimiter.is_some() {
        return Err("unterminated quote or backslash in process arguments".into());
    }
    if started {
        args.push(current);
    }
    if args.len() > 128 {
        return Err("at most 128 process arguments are accepted".into());
    }
    Ok(args)
}
fn executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return is_executable(path).then(|| path.to_owned());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|directory| {
            let candidate = directory.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
            #[cfg(windows)]
            if !name.to_ascii_lowercase().ends_with(".exe") {
                let candidate = directory.join(format!("{name}.exe"));
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
            None
        })
    })
}
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        path.extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}
pub fn installed_harnesses() -> String {
    let entries = ["codex", "claude", "pi", "opencode", "aider", "ssmcp"]
        .iter()
        .map(|name| {
            let path = executable(name);
            format!(
                "{{\"name\":{},\"available\":{},\"path\":{}}}",
                quote(name),
                path.is_some(),
                path.as_ref()
                    .map_or("null".into(), |p| quote(&p.to_string_lossy()))
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(","))
}
fn utility(program: &Path, args: &[OsString], timeout: Duration) -> Result<String> {
    let mut managed = Managed::spawn(program.as_os_str(), args, false)?;
    let deadline = Instant::now() + timeout;
    while managed.status.is_none() {
        managed.refresh()?;
        if managed.status.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            managed.finish(true);
            return Err(format!(
                "{} exceeded {} milliseconds",
                program.display(),
                timeout.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    // Successful IDE/desktop commands may intentionally launch an application.
    // Release those descendants, while stopping our nonblocking reader threads.
    managed.finish(false);
    let stdout = managed
        .stdout
        .tail
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let stderr = managed
        .stderr
        .tail
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let out =
        String::from_utf8_lossy(&stdout.bytes.iter().copied().collect::<Vec<_>>()).into_owned();
    let err =
        String::from_utf8_lossy(&stderr.bytes.iter().copied().collect::<Vec<_>>()).into_owned();
    if stdout.dropped > 0 || stderr.dropped > 0 {
        return Err(format!(
            "{} exceeded the 128 KiB output limit",
            program.display()
        ));
    }
    if let Some(error) = stdout.error.as_ref().or(stderr.error.as_ref()) {
        return Err(format!("{} output: {error}", program.display()));
    }
    if !managed.status.is_some_and(|status| status.success()) {
        return Err(format!("{} failed: {}", program.display(), err.trim()));
    }
    Ok(out)
}
fn run_named(name: &str, args: &[&str]) -> Result<String> {
    let path = executable(name).ok_or_else(|| {
        format!("{name} is unavailable; install it and add its executable to PATH")
    })?;
    utility(
        &path,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        COMMAND_TIMEOUT,
    )
}
fn editor() -> Result<PathBuf> {
    for name in ["code", "codium", "code-insiders"] {
        if let Some(path) = executable(name) {
            return Ok(path);
        }
    }
    #[cfg(windows)]
    for (variable, relative) in [
        ("LOCALAPPDATA", "Programs/Microsoft VS Code/Code.exe"),
        ("ProgramFiles", "Microsoft VS Code/Code.exe"),
    ] {
        if let Some(root) = std::env::var_os(variable) {
            let path = PathBuf::from(root).join(relative);
            if is_executable(&path) {
                return Ok(path);
            }
        }
    }
    Err("IDE control requires VS Code, VSCodium or Code Insiders in PATH".into())
}
fn absolute_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        return Err("path must contain 1..4096 non-NUL bytes".into());
    }
    let path = Path::new(path);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    })
}
pub fn ide_open(path: &str, line: u32, column: u32) -> Result<String> {
    if line == 0 || column == 0 {
        return Err("IDE line and column start at one".into());
    }
    let editor = editor()?;
    let path = absolute_path(path)?;
    utility(
        &editor,
        &[
            "--goto".into(),
            format!("{}:{line}:{column}", path.display()).into(),
        ],
        COMMAND_TIMEOUT,
    )?;
    Ok(format!(
        "{{\"opened\":{},\"line\":{line},\"column\":{column},\"adapter\":\"vscode-cli\"}}",
        quote(&path.to_string_lossy())
    ))
}
pub fn ide_diff(left: &str, right: &str) -> Result<String> {
    let editor = editor()?;
    let left = absolute_path(left)?;
    let right = absolute_path(right)?;
    utility(
        &editor,
        &[
            "--diff".into(),
            left.as_os_str().to_owned(),
            right.as_os_str().to_owned(),
        ],
        COMMAND_TIMEOUT,
    )?;
    Ok(format!(
        "{{\"left\":{},\"right\":{},\"adapter\":\"vscode-cli\"}}",
        quote(&left.to_string_lossy()),
        quote(&right.to_string_lossy())
    ))
}
pub(crate) fn ocr(path: &Path) -> Result<String> {
    let program = executable("tesseract").ok_or("OCR requires tesseract in PATH")?;
    utility(
        &program,
        &[path.as_os_str().to_owned(), "stdout".into(), "quiet".into()],
        Duration::from_secs(15),
    )
}

pub fn ide_extensions() -> Result<String> {
    let editor = editor()?;
    let output = utility(
        &editor,
        &["--list-extensions".into(), "--show-versions".into()],
        COMMAND_TIMEOUT,
    )?;
    let extensions = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(quote)
        .collect::<Vec<_>>();
    Ok(format!(
        "{{\"adapter\":\"vscode-cli\",\"extensions\":[{}]}}",
        extensions.join(",")
    ))
}
fn omarchy_root() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/usr/share/omarchy")];
    if let Some(root) = std::env::var_os("OMARCHY_PATH") {
        candidates.insert(0, PathBuf::from(root));
    }
    if let Some(user_dir) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(user_dir).join(".local/share/omarchy"));
    }
    candidates
        .into_iter()
        .find(|path| path.join("bin/omarchy-theme-list").is_file())
}
fn omarchy_command(name: &str, args: &[&str]) -> Result<String> {
    if !cfg!(target_os = "linux") {
        return Err("Omarchy control requires Linux".into());
    }
    let path = executable(name)
        .or_else(|| {
            omarchy_root()
                .map(|root| root.join("bin").join(name))
                .filter(|path| is_executable(path))
        })
        .ok_or("Omarchy is unavailable on this system")?;
    utility(
        &path,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        COMMAND_TIMEOUT,
    )
}
pub fn omarchy_status() -> Result<String> {
    let theme = omarchy_command("omarchy-theme-current", &[])?;
    let version = run_named("hyprctl", &["-j", "version"])?;
    with_value(&version, |_| Ok(()))?;
    Ok(format!(
        "{{\"available\":true,\"theme\":{},\"hyprland\":{}}}",
        quote(theme.trim()),
        version.trim()
    ))
}
pub fn omarchy_workspaces() -> Result<String> {
    if !cfg!(target_os = "linux") {
        return Err("Omarchy workspaces require Linux and Hyprland".into());
    }
    let output = run_named("hyprctl", &["-j", "workspaces"])?;
    with_value(&output, |_| Ok(()))?;
    Ok(output)
}
pub fn omarchy_workspace(id: &str) -> Result<String> {
    if !cfg!(target_os = "linux") {
        return Err("Omarchy workspaces require Linux and Hyprland".into());
    }
    let numeric = id.parse::<u32>().is_ok_and(|n| (1..=10_000).contains(&n));
    let named = id.strip_prefix("name:").is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_- ".contains(&b))
    });
    if !numeric && !named {
        return Err("workspace must be 1..10000 or name:<letters, numbers, spaces, _ or ->".into());
    }
    run_named("hyprctl", &["dispatch", "workspace", id])?;
    Ok(format!("{{\"workspace\":{}}}", quote(id)))
}
pub fn omarchy_themes() -> Result<String> {
    let output = omarchy_command("omarchy-theme-list", &[])?;
    let themes = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(quote)
        .collect::<Vec<_>>();
    Ok(format!("[{}]", themes.join(",")))
}
pub fn omarchy_theme(name: &str) -> Result<String> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("theme name must contain 1..128 bytes without control characters".into());
    }
    let known = omarchy_command("omarchy-theme-list", &[])?;
    let theme = known
        .lines()
        .find(|item| item.trim().eq_ignore_ascii_case(name))
        .map(str::trim)
        .ok_or("theme is not in the installed Omarchy theme list")?;
    omarchy_command("omarchy-theme-set", &[theme])?;
    Ok(format!("{{\"theme\":{}}}", quote(theme)))
}

#[cfg(unix)]
struct ProcessGroup {
    id: i32,
}
#[cfg(unix)]
impl ProcessGroup {
    fn new(child: &Child) -> Result<Self> {
        Ok(Self {
            id: child.id() as i32,
        })
    }
    fn terminate(&self) {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        if self.id > 0 {
            // SAFETY: process_group(0) creates a distinct group for this child.
            unsafe {
                kill(-self.id, 9);
            }
        }
    }
    fn release(&mut self) {
        self.id = 0;
    }
}
#[cfg(windows)]
struct ProcessGroup {
    handle: *mut core::ffi::c_void,
}
// SAFETY: exclusive ownership of a Windows kernel handle may move across threads.
#[cfg(windows)]
unsafe impl Send for ProcessGroup {}
#[cfg(windows)]
mod windows_job {
    use core::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    pub struct Basic {
        pub process_time: i64,
        pub job_time: i64,
        pub flags: u32,
        pub min_working: usize,
        pub max_working: usize,
        pub active_processes: u32,
        pub affinity: usize,
        pub priority: u32,
        pub scheduling: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    pub struct Extended {
        pub basic: Basic,
        pub io: [u64; 6],
        pub process_memory: usize,
        pub job_memory: usize,
        pub peak_process: usize,
        pub peak_job: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
        pub fn SetInformationJobObject(
            job: *mut c_void,
            class: i32,
            info: *const c_void,
            size: u32,
        ) -> i32;
        pub fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        pub fn TerminateJobObject(job: *mut c_void, code: u32) -> i32;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
    }
}
#[cfg(windows)]
impl ProcessGroup {
    fn new(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: Win32 structures use their documented C layout. Every failure
        // closes the job; successful construction transfers ownership to Self.
        unsafe {
            let handle = windows_job::CreateJobObjectW(core::ptr::null_mut(), core::ptr::null());
            if handle.is_null() {
                return Err(format!(
                    "create process job: {}",
                    io::Error::last_os_error()
                ));
            }
            let mut limits = windows_job::Extended::default();
            limits.basic.flags = 0x2000;
            if windows_job::SetInformationJobObject(
                handle,
                9,
                &limits as *const _ as *const _,
                size_of::<windows_job::Extended>() as u32,
            ) == 0
                || windows_job::AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
            {
                let error = io::Error::last_os_error();
                windows_job::CloseHandle(handle);
                return Err(format!("assign process job: {error}"));
            }
            Ok(Self { handle })
        }
    }
    fn terminate(&self) {
        unsafe {
            windows_job::TerminateJobObject(self.handle, 1);
        }
    }
    fn release(&mut self) {
        let limits = windows_job::Extended::default();
        unsafe {
            windows_job::SetInformationJobObject(
                self.handle,
                9,
                &limits as *const _ as *const _,
                size_of::<windows_job::Extended>() as u32,
            );
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            windows_job::CloseHandle(self.handle);
        }
    }
}
#[cfg(not(any(unix, windows)))]
struct ProcessGroup;
#[cfg(not(any(unix, windows)))]
impl ProcessGroup {
    fn new(_: &Child) -> Result<Self> {
        Err("managed processes require Unix or Windows".into())
    }
    fn terminate(&self) {}
    fn release(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_are_literal_and_quotes_are_bounded() {
        assert_eq!(
            parse_arguments(r#"--name "hello world" '' '$HOME' '$(id)' a\ b"#).unwrap(),
            vec!["--name", "hello world", "", "$HOME", "$(id)", "a b"]
        );
        assert!(parse_arguments("'unterminated").is_err());
        assert!(parse_arguments("a\\").is_err());
        assert!(parse_arguments("a\0b").is_err());
        assert!(parse_arguments(&"x ".repeat(129)).is_err());
    }
    #[test]
    fn incremental_output_preserves_split_utf8() {
        let mut tail = Tail::default();
        tail.append(&[0xe4]);
        assert_eq!(tail.take(), "");
        tail.append(&[0xbd, 0xa0]);
        assert_eq!(tail.take(), "你");
        tail.append(&[0xe4]);
        tail.closed = true;
        assert_eq!(tail.take(), "�");
    }
    #[test]
    fn tail_is_bounded_and_reports_loss() {
        let mut tail = Tail::default();
        for _ in 0..32 {
            tail.append(&[b'x'; 8192]);
        }
        assert_eq!(tail.bytes.len(), STREAM_LIMIT);
        assert_eq!(tail.dropped, STREAM_LIMIT as u64);
        assert_eq!(tail.total, (STREAM_LIMIT * 2) as u64);
        assert_eq!(tail.take().len(), STREAM_LIMIT);
        assert!(tail.take().is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn managed_cat_round_trip_and_stop() {
        let Some(cat) = executable("cat") else {
            return;
        };
        let mut manager = ProcessManager::new();
        assert!(
            manager
                .start(cat.to_str().unwrap(), "")
                .unwrap()
                .contains("\"id\":1")
        );
        manager.write(1, "hello 世界\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = String::new();
        while Instant::now() < deadline {
            let read = manager.read(1).unwrap();
            received.push_str(&crate::json::field(&read, "stdout").unwrap());
            if received.contains("hello 世界\n") {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(received, "hello 世界\n");
        assert!(manager.stop(1).unwrap().contains("\"state\":\"stopped\""));
        assert!(manager.write(1, "no").is_err());
        assert!(manager.read(999).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn utility_deadline_cancels_owned_process() {
        let Some(sleep) = executable("sleep") else {
            return;
        };
        let begin = Instant::now();
        assert!(utility(&sleep, &["30".into()], Duration::from_millis(50)).is_err());
        assert!(begin.elapsed() < Duration::from_secs(2));
    }
    #[cfg(unix)]
    #[test]
    fn manager_drop_reaps_its_child() {
        let Some(cat) = executable("cat") else {
            return;
        };
        let mut manager = ProcessManager::new();
        manager.start(cat.to_str().unwrap(), "").unwrap();
        let pid = manager.processes.get(&1).unwrap().child.id();
        drop(manager);
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        assert_eq!(unsafe { kill(pid as i32, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(3));
    }
    #[test]
    fn configuration_values_reject_invalid_input_before_execution() {
        assert!(ide_open("file", 0, 1).is_err());
        assert!(absolute_path("\0").is_err());
        assert!(omarchy_workspace("1;exec anything").is_err());
        assert!(omarchy_theme("\n").is_err());
    }
}
