use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::Path;
use zclap::{Arg, Command, Event};
use zuse::{
    human, json,
    language::MAX_SCRIPT_BYTES,
    mcp,
    runtime::{self, Options, Runtime},
};

const CLI: Command = Command::new("zuse").args(&[
    Arg::flag("help").short('h'),
    Arg::flag("version").short('V'),
    Arg::flag("read-only"),
    Arg::flag("dry-run"),
    Arg::value("max-steps"),
    Arg::value("max-wait-ms"),
    Arg::flag("stdio").conflicts_with(&["host", "port", "socket"]),
    Arg::value("host").conflicts("socket"),
    Arg::value("port").conflicts("socket"),
    Arg::value("socket"),
    Arg::positional("words").repeat(),
]);

const HELP: &str = r#"zuse — an agent-native computer-use shell

USAGE
  zuse eval 'mouse.position(); window.list()'
  zuse run FILE                 Run a UTF-8 .zuse program; '-' reads stdin
  zuse check FILE               Validate every statement without execution
  zuse shell                    Persistent line-oriented interactive shell
  zuse doctor                   Inspect available backends and integrations
  zuse schema                   Print the supported commands and arguments
  zuse resume REQUEST_ID        Continue after completed human assistance
  zuse human list
  zuse human assist REQUEST_ID   Open a trusted local approval prompt
  zuse human approve REQUEST_ID  Record the local operator's approval
  zuse human deny REQUEST_ID     Record the local operator's denial
  zuse mcp --stdio
  zuse mcp --host 127.0.0.1 --port 8377
  zuse mcp --socket /path/to/zuse.sock

OPTIONS
  --dry-run                     Validate and return an execution plan
  --read-only                   Permit observation commands only
  --max-steps NUMBER             Bound statements per execution
  --max-wait-ms NUMBER           Bound an execution's cumulative wait budget
  -h, --help                    Show help
  -V, --version                 Show version

Programs use named calls, for example:
  mouse.move_to(text="Save", role="button", duration=450, jitter=0.8, seed=42)
  mouse.move(x=400, y=240, duration=300, curve="bezier", jitter=1, seed=42)
  keyboard.type(text="Hello")
  human.request(kind="login", message="Please sign in in the local browser")

Output is JSON. Errors and interactive prompts go to stderr.
The shell retains browser/mobile sessions, device handles and harnesses until it exits.
"#;

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

fn repl(options: Options) -> Result<i32, String> {
    let interactive = io::stdin().is_terminal();
    let mut machine = Runtime::new();
    let mut input = io::stdin().lock();
    let mut failed = false;
    loop {
        if interactive {
            eprint!("zuse> ");
            io::stderr().flush().map_err(|e| e.to_string())?;
        }
        let mut line = String::new();
        let count = input
            .by_ref()
            .take(MAX_SCRIPT_BYTES as u64 + 1)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if line.len() > MAX_SCRIPT_BYTES {
            return Err("shell line exceeds the 1 MiB limit".into());
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
        match machine.execute(&line, options) {
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
    let mut parser = CLI.parse(&argv);
    let mut words = Vec::new();
    let mut options = Options::default();
    let mut stdio = false;
    let mut host = None;
    let mut port = None;
    let mut socket = None;
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
            Event::Flag { arg: "stdio" } => stdio = true,
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
    let command = words.first().copied().unwrap_or("help");
    if command != "mcp" && (stdio || host.is_some() || port.is_some() || socket.is_some()) {
        return Err("transport options are only valid for zuse mcp".into());
    }
    match command {
        "help" => {
            print!("{HELP}");
            Ok(0)
        }
        "eval" => {
            exactly(&words, 2, "eval CODE")?;
            output(Runtime::new().execute(words[1], options)?)
        }
        "run" => {
            exactly(&words, 2, "run FILE")?;
            output(Runtime::new().execute(&read_source(words[1])?, options)?)
        }
        "check" => {
            exactly(&words, 2, "check FILE")?;
            output(runtime::check(&read_source(words[1])?, options)?)
        }
        "shell" => {
            exactly(&words, 1, "shell")?;
            repl(options)
        }
        "doctor" => {
            exactly(&words, 1, "doctor")?;
            output(Runtime::new().capabilities())
        }
        "schema" => {
            exactly(&words, 1, "schema")?;
            output(runtime::schema())
        }
        "resume" => {
            exactly(&words, 2, "resume REQUEST_ID")?;
            output(Runtime::new().resume(words[1], options)?)
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
            exactly(
                &words,
                1,
                "mcp --stdio|--host HOST --port PORT|--socket PATH",
            )?;
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
