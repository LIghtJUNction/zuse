use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use zclap::{Arg, Command, Event};
use zuse::{
    human, json,
    language::MAX_SCRIPT_BYTES,
    mcp,
    runtime::{self, Options, Runtime},
};

mod machine_cli;
mod update;

const DEFAULT_SHELL_IDLE_TIMEOUT_MS: u64 = 900_000;

const CLI: Command = Command::new("zuse").args(&[
    Arg::flag("help").short('h'),
    Arg::flag("version").short('V'),
    Arg::value("code").short('c'),
    Arg::flag("read-only"),
    Arg::flag("dry-run"),
    Arg::flag("check").conflicts("dry-run"),
    Arg::value("max-steps"),
    Arg::value("max-wait-ms"),
    Arg::value("idle-timeout-ms"),
    Arg::flag("stdio").conflicts_with(&["host", "port", "socket"]),
    Arg::value("host").conflicts("socket"),
    Arg::value("port").conflicts("socket"),
    Arg::value("socket"),
    Arg::positional("words").repeat(),
]);

const HELP: &str = r##"zuse — an agent-native computer-use shell

USAGE
  zuse -c 'window.list()'        Run a program directly from the command line
  zuse eval 'window.list()'
  zuse run FILE                 Run a UTF-8 .zuse program; '-' reads stdin
  zuse script FILE              Run functions, imports and scoped mouse tracks
  zuse check FILE               Validate every statement without execution
  zuse shell                    Persistent line-oriented interactive shell
  zuse session help             Persistent externally controlled Runtime sessions
  zuse web                      Serve the local touch workbench on 127.0.0.1:43497
  zuse font list|add|remove      Manage single-line handwriting fonts
  zuse machine help             Preview or run a controller-specific G-code program
  zuse doctor                   Inspect available backends and integrations
  zuse update                   Update a tracked user Cargo installation
  zuse update --check           Check the latest stable version without installing
  zuse schema                   Print the supported commands and arguments
  zuse pkg help                 Manage locked Git packages and publish version tags
  zuse resume REQUEST_ID        Continue after completed human assistance
  zuse human list
  zuse human assist REQUEST_ID   Open a trusted local approval prompt
  zuse human approve REQUEST_ID  Record the local operator's approval
  zuse human deny REQUEST_ID     Record the local operator's denial
  zuse mcp serve --stdio        Run the native MCP server (or use zuse mcp --stdio)
  zuse mcp install codex|claude|ssmcp [--dry-run]
  zuse mcp --host 127.0.0.1 --port 8377
  zuse mcp --socket /path/to/zuse.sock

OPTIONS
  -c, --code CODE               Execute CODE with the same options as eval
  --dry-run                     Validate and return an execution plan
  --check                       Only check for an update (zuse update)
  --read-only                   Permit observation commands only
  --max-steps NUMBER             Bound statements per execution
  --max-wait-ms NUMBER           Bound an execution's cumulative wait budget
  --idle-timeout-ms NUMBER       Close an idle shell (default 900000; must be positive)
  -h, --help                    Show help
  -V, --version                 Show version

Programs use named calls, for example:
  mouse.use(kind="user", appearance="system")
  mouse.move_to(text="Save", role="button", duration=450, jitter=0.8, seed=42)
  mouse.move(x=400, y=240, duration=300, curve="bezier", jitter=1, seed=42)
  keyboard.type(text="Hello")
  human.request(kind="login", message="Please sign in in the local browser")

Run multiple calls without a script file:
  zuse -c 'mouse.use(kind="user",appearance="system"); mouse.move(x=400,y=240)'

Reusable functions can be loaded once in the persistent shell:
  import(path="wechat.zuse", as="wechat")
  functions(namespace="wechat")
  wechat.read_chat(scope="0/1")
  eval(source="fn pause(ms) { wait(ms=ms) }", as="local")
  local.pause(ms=10)
Imports register declarations only. Explicit eval may also execute actions.
File imports resolve relative to their containing .zuse file.

Scoped programs use: let p = mouse(); p.move(x=400,y=240);
Use parallel { ... } or sequence { ... } to compose tracks.
The default mouse is independent and currently requires Linux X11 MPX.
Explicit user_mouse(appearance="system") borrows the supported system pointer.

Output is JSON. Errors and interactive prompts go to stderr.
The shell retains functions, browser/mobile sessions, device handles and harnesses until it exits.
Idle shells close after 15 minutes by default. Command execution does not count toward idle time.
"##;

fn read_source(path: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    if path == "-" {
        io::stdin()
            .lock()
            .take(MAX_SCRIPT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    } else {
        std::fs::File::open(path)
            .map_err(|e| format!("{path}: {e}"))?
            .take(MAX_SCRIPT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    }
    if bytes.len() > MAX_SCRIPT_BYTES {
        return Err("script exceeds the 1 MiB limit".into());
    }
    String::from_utf8(bytes).map_err(|e| format!("script must be UTF-8: {e}"))
}

fn output(value: String) -> Result<i32, String> {
    let failed = runtime::failed(&value);
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{value}").map_err(|e| e.to_string())?;
    Ok(if failed { 1 } else { 0 })
}

fn exactly(words: &[&str], count: usize, usage: &str) -> Result<(), String> {
    if words.len() == count {
        Ok(())
    } else {
        Err(format!("usage: zuse {usage}"))
    }
}

/// Bridge the CLI-owned signal flag to local waits without changing library handlers.
fn with_cancellation(
    machine: &mut Runtime,
    action: impl FnOnce(&mut Runtime) -> Result<String, String>,
) -> Result<String, String> {
    let interrupt = zuse::script::interrupt::InterruptGuard::install()?;
    with_interrupt(machine, &interrupt, action)
}

fn with_interrupt(
    machine: &mut Runtime,
    interrupt: &zuse::script::interrupt::InterruptGuard,
    action: impl FnOnce(&mut Runtime) -> Result<String, String>,
) -> Result<String, String> {
    let cancellation = zuse::observation::Cancellation::default();
    if interrupt.cancelled() {
        cancellation.cancel();
    }
    machine.set_cancellation(cancellation.clone());
    std::thread::scope(|scope| {
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let monitor = scope.spawn(move || {
            loop {
                match stopped.recv_timeout(std::time::Duration::from_millis(10)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if interrupt.cancelled() {
                            cancellation.cancel();
                            break;
                        }
                    }
                }
            }
        });
        let result = action(machine);
        let _ = stop.send(());
        monitor
            .join()
            .map_err(|_| "CLI cancellation monitor failed".to_string())?;
        result
    })
}

fn execute_file(
    path: &str,
    mut options: Options,
    check_only: bool,
    prefer_scoped: bool,
) -> Result<String, String> {
    let source = read_source(path)?;
    options.dry_run |= check_only;
    let modules = zuse::script::ModuleSession::default();
    let mut prepared = if path == "-" {
        modules.prepare(&source, options)?
    } else {
        let absolute = std::fs::canonicalize(path).map_err(|error| format!("{path}: {error}"))?;
        modules.prepare_with_base(
            &source,
            options,
            absolute.parent().ok_or("script has no parent directory")?,
        )?
    };
    // Preserve the original script engine's bounded event receipts for programs
    // it already accepts, including repeat/wait/capability-only scripts. New
    // Runtime commands still use the flat executor. This is a pure preflight of
    // already expanded source; imports are never reopened to choose the engine.
    if prefer_scoped && prepared.mode == zuse::script::ProgramMode::Flat {
        prepared.mode = zuse::script::ProgramMode::Scoped;
        if prepared.compile_scoped(options).is_err() {
            prepared.mode = zuse::script::ProgramMode::Flat;
        }
    }
    if options.dry_run {
        return runtime::check_prepared(&prepared, options);
    }
    match prepared.mode {
        zuse::script::ProgramMode::Flat => with_cancellation(&mut Runtime::new(), |machine| {
            machine.execute_prepared(prepared, options)
        }),
        zuse::script::ProgramMode::Scoped => {
            let interrupt = zuse::script::interrupt::InterruptGuard::install()?;
            zuse::script::execute_prepared_with_cancel(&prepared, options, || interrupt.cancelled())
        }
    }
}

fn shell_input() -> Result<Receiver<Result<String, String>>, String> {
    let (lines, input) = mpsc::sync_channel(1);
    // Reading a partial line may block indefinitely. The shell owns the Runtime
    // on its main thread and can drop it without waiting for this reader.
    let _reader = std::thread::Builder::new()
        .name("zuse-stdin".into())
        .spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                let mut line = String::new();
                let result = match input
                    .by_ref()
                    .take(MAX_SCRIPT_BYTES as u64 + 1)
                    .read_line(&mut line)
                {
                    Ok(0) => break,
                    Ok(_) if line.len() > MAX_SCRIPT_BYTES => {
                        Err("shell line exceeds the 1 MiB limit".into())
                    }
                    Ok(_) => Ok(line),
                    Err(error) => Err(error.to_string()),
                };
                let failed = result.is_err();
                if lines.send(result).is_err() || failed {
                    break;
                }
            }
        })
        .map_err(|error| format!("cannot start shell input reader: {error}"))?;
    Ok(input)
}

fn repl(options: Options, idle_timeout: Duration) -> Result<i32, String> {
    // Keep signal ownership until after the retained Runtime has been dropped.
    let interrupt = zuse::script::interrupt::InterruptGuard::install()?;
    let interactive = io::stdin().is_terminal();
    let mut machine = Runtime::new();
    let input = shell_input()?;
    let mut failed = false;
    'shell: loop {
        if interactive {
            eprint!("zuse> ");
            io::stderr().flush().map_err(|e| e.to_string())?;
        }
        let idle_started = Instant::now();
        let line = loop {
            if interrupt.cancelled() {
                return Err("shell interrupted".into());
            }
            let remaining = idle_timeout.saturating_sub(idle_started.elapsed());
            match input.recv_timeout(remaining.min(Duration::from_millis(25))) {
                Ok(line) => break line?,
                Err(RecvTimeoutError::Disconnected) => break 'shell,
                Err(RecvTimeoutError::Timeout) if idle_started.elapsed() >= idle_timeout => {
                    if interactive {
                        eprintln!();
                    }
                    eprintln!(
                        "zuse shell: idle timeout after {} ms",
                        idle_timeout.as_millis()
                    );
                    break 'shell;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        };
        // A queued line must not start another command after an interrupt.
        if interrupt.cancelled() {
            return Err("shell interrupted".into());
        }
        match line.trim() {
            "" => continue,
            ":quit" | ":exit" => break,
            ":help" => {
                eprintln!("{HELP}");
                continue;
            }
            ":schema" => {
                output(runtime::schema())?;
                continue;
            }
            _ => {}
        }
        match with_interrupt(&mut machine, &interrupt, |machine| {
            machine.execute(&line, options)
        }) {
            Ok(result) => {
                failed |= output(result)? != 0;
            }
            Err(error) => {
                eprintln!("{}", json::error(&error));
                failed = true;
            }
        }
    }
    Ok(if failed { 1 } else { 0 })
}

fn run() -> Result<i32, String> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "command-line arguments must be UTF-8".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let argv = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    if argv.first() == Some(&"pkg") {
        return output(zuse::packages::run(&argv[1..])?);
    }
    if argv.first() == Some(&"session") {
        // Reading a failed job receipt is a successful controller request.
        return output(zuse::session::run(&argv[1..])?).map(|_| 0);
    }
    let mut parser = CLI.parse(&argv);
    let mut words = Vec::new();
    let mut code = None;
    let mut options = Options::default();
    let mut stdio = false;
    let mut host = None;
    let mut port = None;
    let mut socket = None;
    let mut idle_timeout_ms = None;
    let mut update_check = false;
    while let Some(event) = parser.next().map_err(|e| e.to_string())? {
        match event {
            Event::Flag { arg: "help" } => {
                print!("{HELP}");
                return Ok(0);
            }
            Event::Flag { arg: "version" } => {
                println!("zuse {}", zuse::VERSION);
                return Ok(0);
            }
            Event::Flag { arg: "read-only" } => options.read_only = true,
            Event::Flag { arg: "dry-run" } => options.dry_run = true,
            Event::Flag { arg: "check" } => update_check = true,
            Event::Flag { arg: "stdio" } => stdio = true,
            Event::Value { arg: "code", value } => {
                if code.replace(value).is_some() {
                    return Err("-c/--code may only be provided once".into());
                }
            }
            Event::Value {
                arg: "max-steps",
                value,
            } => {
                options.max_steps = value
                    .parse()
                    .map_err(|_| "max-steps must be an integer".to_string())?
            }
            Event::Value {
                arg: "max-wait-ms",
                value,
            } => {
                options.max_wait_ms = value
                    .parse()
                    .map_err(|_| "max-wait-ms must be an integer".to_string())?
            }
            Event::Value { arg: "host", value } => host = Some(value),
            Event::Value {
                arg: "idle-timeout-ms",
                value,
            } => {
                let timeout = value
                    .parse::<u64>()
                    .map_err(|_| "idle-timeout-ms must be a positive integer".to_string())?;
                if timeout == 0 {
                    return Err("idle-timeout-ms must be a positive integer".into());
                }
                idle_timeout_ms = Some(timeout);
            }
            Event::Value { arg: "port", value } => {
                port = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| "port must be 0..65535".to_string())?,
                )
            }
            Event::Value {
                arg: "socket",
                value,
            } => socket = Some(value),
            Event::Value {
                arg: "words",
                value,
            } => words.push(value),
            _ => return Err("unsupported command-line option".into()),
        }
    }
    parser.finish().map_err(|e| e.to_string())?;
    if let Some(source) = code {
        if !words.is_empty() {
            return Err(
                "-c/--code cannot be combined with a command or positional arguments".into(),
            );
        }
        words.extend(["eval", source]);
    }
    let command = words.first().copied().unwrap_or("help");
    if command != "mcp" && (stdio || host.is_some() || port.is_some() || socket.is_some()) {
        return Err("transport options are only valid for zuse mcp".into());
    }
    if command != "update" && update_check {
        return Err("--check is only valid for zuse update".into());
    }
    if command != "shell" && idle_timeout_ms.is_some() {
        return Err("--idle-timeout-ms is only valid for zuse shell".into());
    }
    match command {
        "help" => {
            print!("{HELP}");
            Ok(0)
        }
        "eval" => {
            exactly(&words, 2, "eval CODE")?;
            output(with_cancellation(&mut Runtime::new(), |machine| {
                machine.execute(words[1], options)
            })?)
        }
        "run" => {
            exactly(&words, 2, "run FILE")?;
            output(execute_file(words[1], options, false, false)?)
        }
        "script" => {
            exactly(&words, 2, "script FILE")?;
            output(execute_file(words[1], options, false, true)?)
        }
        "check" => {
            exactly(&words, 2, "check FILE")?;
            output(execute_file(words[1], options, true, false)?)
        }
        "shell" => {
            exactly(&words, 1, "shell")?;
            repl(
                options,
                Duration::from_millis(idle_timeout_ms.unwrap_or(DEFAULT_SHELL_IDLE_TIMEOUT_MS)),
            )
        }
        "web" => {
            exactly(&words, 1, "web")?;
            zuse::browser::workbench::serve().map(|()| 0)
        }
        "font" => {
            if matches!(words.get(1).copied(), Some("add" | "remove"))
                && (options.read_only || options.dry_run)
            {
                return Err("font changes require normal execution mode".into());
            }
            output(zuse::font_store::cli(&words[1..])?)
        }
        "machine" => output(machine_cli::run(&words[1..], options)?),
        "doctor" => {
            exactly(&words, 1, "doctor")?;
            output(Runtime::new().capabilities())
        }
        "update" => {
            exactly(&words, 1, "update [--check | --dry-run]")?;
            if options.read_only && !update_check && !options.dry_run {
                return Err(
                    "updates are blocked in read-only mode; use zuse update --check".into(),
                );
            }
            output(update::run(update_check, options.dry_run)?)
        }
        "schema" => {
            exactly(&words, 1, "schema")?;
            output(runtime::schema())
        }
        "resume" => {
            exactly(&words, 2, "resume REQUEST_ID")?;
            output(with_cancellation(&mut Runtime::new(), |machine| {
                machine.resume(words[1], options)
            })?)
        }
        "human" => {
            let queue = human::Queue::new()?;
            match words.get(1).copied() {
                Some("list") => {
                    exactly(&words, 2, "human list")?;
                    output(queue.list()?)
                }
                Some(action @ ("approve" | "deny" | "assist")) => {
                    if options.read_only || options.dry_run {
                        return Err("human operator actions require normal execution mode".into());
                    }
                    exactly(&words, 3, "human approve|deny|assist REQUEST_ID")?;
                    if action == "assist" {
                        queue.assist(words[2])?;
                    } else {
                        queue.respond(words[2], action == "approve")?;
                    }
                    output(format!(
                        "{{\"id\":{},\"action\":{}}}",
                        json::quote(words[2]),
                        json::quote(action)
                    ))
                }
                _ => Err("usage: zuse human list|assist|approve|deny [REQUEST_ID]".into()),
            }
        }
        "mcp" => {
            match words.as_slice() {
                ["mcp", "install", client] => {
                    if stdio || host.is_some() || port.is_some() || socket.is_some() {
                        return Err("MCP transport options apply to serve, not install".into());
                    }
                    return output(mcp::install_client(client, options)?);
                }
                ["mcp"] | ["mcp", "serve"] => {}
                _ => return Err("usage: zuse mcp serve [--stdio|--host HOST --port PORT|--socket PATH] or zuse mcp install codex|claude|ssmcp [--dry-run]".into()),
            }
            if let Some(socket) = socket {
                mcp::unix(Path::new(socket), options)?;
            } else if host.is_some() || port.is_some() {
                mcp::http(host.unwrap_or("127.0.0.1"), port.unwrap_or(8377), options)?;
            } else {
                mcp::stdio(options)?;
            }
            Ok(0)
        }
        _ => Err(format!("unknown command {command}; run zuse --help")),
    }
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(0) => std::process::ExitCode::SUCCESS,
        Ok(_) => std::process::ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{}", json::error(&error));
            std::process::ExitCode::FAILURE
        }
    }
}
