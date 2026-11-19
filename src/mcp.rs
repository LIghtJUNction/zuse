//! MCP stdio, Unix sockets, and current Streamable HTTP via Project Z zmcpio.
use crate::{
    human::Queue,
    json::{self, quote},
    runtime::{self, Options, Runtime},
};
use std::{
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::Path,
    time::Duration,
};
use zmcpio::{
    json::{Document, Limits, Token, Value},
    mcp::{Action, Era, Server},
    rpc,
    tools::{Content, Tool, ToolResult},
};
const MAX_FRAME: usize = 1024 * 1024;
fn limits() -> Limits {
    Limits {
        max_bytes: MAX_FRAME,
        max_depth: 32,
        max_members: 8192,
        max_string_bytes: MAX_FRAME,
    }
}
type Result<T> = std::result::Result<T, String>;
struct ToolDef {
    name: &'static str,
    description: &'static str,
    schema: &'static str,
}
const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "zuse.execute",
        description: "Validate the full computer-use program before executing it. Returns ordered results or a human handoff. Typed text is omitted from execution logs.",
        schema: r#"{"type":"object","properties":{"code":{"type":"string"},"dry_run":{"type":"boolean"},"read_only":{"type":"boolean"}},"required":["code"],"additionalProperties":false}"#,
    },
    ToolDef {
        name: "zuse.check",
        description: "Check a program and inspect its operation plan without executing it.",
        schema: r#"{"type":"object","properties":{"code":{"type":"string"}},"required":["code"],"additionalProperties":false}"#,
    },
    ToolDef {
        name: "zuse.capabilities",
        description: "Discover this host's actual desktop backend and installed harnesses.",
        schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    ToolDef {
        name: "zuse.schema",
        description: "Get the complete executable language command and argument schema.",
        schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    ToolDef {
        name: "zuse.observe",
        description: "Observe windows, pointer, accessibility tree or screen text without input effects.",
        schema: r#"{"type":"object","properties":{"mode":{"type":"string","enum":["windows","pointer","accessibility","text"]}},"additionalProperties":false}"#,
    },
    ToolDef {
        name: "zuse.screenshot",
        description: "Return a real PNG image from the desktop, a window, or the connected browser. Optional path preserves the screenshot.",
        schema: r#"{"type":"object","properties":{"source":{"type":"string","enum":["desktop","browser"]},"window":{"type":"string"},"path":{"type":"string"}},"additionalProperties":false}"#,
    },
    ToolDef {
        name: "zuse.human.pending",
        description: "List local human requests with their pending, approved, denied or expired status.",
        schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    ToolDef {
        name: "zuse.resume",
        description: "Resume a single-use continuation after local human approval. Does not grant its own approval.",
        schema: r#"{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}"#,
    },
];
fn arg(args: Option<Value<'_>>, name: &str, default: &str) -> Result<String> {
    match args.and_then(|v| v.get(name)) {
        Some(v) => json::text(v),
        None => Ok(default.to_owned()),
    }
}
fn flag(args: Option<Value<'_>>, name: &str) -> Result<bool> {
    match args.and_then(|v| v.get(name)) {
        Some(v) => v.boolean().ok_or_else(|| format!("{name} must be boolean")),
        None => Ok(false),
    }
}
pub struct Engine {
    protocol: Server<'static>,
    pub runtime: Runtime,
    options: Options,
}
impl Engine {
    pub fn new(options: Options) -> Self {
        Self {
            protocol: Server::new("zuse", crate::VERSION),
            runtime: Runtime::new(),
            options,
        }
    }
    pub fn handle(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>> {
        if frame.len() > MAX_FRAME {
            return Err("MCP frame exceeds 1 MiB".into());
        }
        let mut tokens = vec![Token::EMPTY; 8192];
        let mut out = vec![0u8; 65536];
        let action = match self.protocol.handle(frame, &mut tokens, limits(), &mut out) {
            Ok(a) => a,
            Err(e) => {
                let n = rpc::failure(None, -32700, &e.to_string(), None, &mut out)
                    .map_err(|e| e.to_string())?;
                out.truncate(n);
                return Ok(Some(out));
            }
        };
        let count = match action {
            Action::Reply(n) => n,
            Action::ListTools { context, cursor } => {
                if cursor.is_some() {
                    self.protocol
                        .error(context, -32602, "Invalid cursor", &mut out)
                        .map_err(|e| e.to_string())?
                } else {
                    let mut storage = vec![[Token::EMPTY; 128]; TOOLS.len()];
                    let mut registry = Vec::new();
                    for (definition, tokens) in TOOLS.iter().zip(storage.iter_mut()) {
                        let schema = Document::parse(
                            definition.schema.as_bytes(),
                            tokens,
                            Limits::default(),
                        )
                        .map_err(|e| e.to_string())?;
                        registry.push(
                            Tool::new(definition.name, definition.description, schema.root())
                                .map_err(|e| e.to_string())?,
                        );
                    }
                    self.protocol
                        .list_tools(context, &registry, None, &mut out)
                        .map_err(|e| e.to_string())?
                }
            }
            Action::CallTool(call) => {
                let name: String = call.name.chars().collect();
                if !TOOLS.iter().any(|d| d.name == name) {
                    return self.error_response(call.context, -32602, "Unknown tool", out);
                }
                let args = call.arguments;
                let is_error;
                let mut picture: Option<String> = None;
                // Modern MCP retries carry the previous request state and human response.
                let state = call
                    .context
                    .request
                    .params
                    .and_then(|p| p.get("requestState"))
                    .map(json::text)
                    .transpose()?;
                let outcome = if let Some(id) = state {
                    (|| {
                        if name != "zuse.execute" {
                            return Err("requestState is valid only for zuse.execute".into());
                        }
                        let queue = Queue::new()?;
                        let record = queue.record(&id)?;
                        if json::field(&record, "kind")? == "password" {
                            return Err("password input must be completed locally".into());
                        }
                        let response = call
                            .context
                            .request
                            .params
                            .and_then(|p| p.get("inputResponses"))
                            .and_then(|p| p.get("confirm"))
                            .ok_or("missing human input response")?;
                        let accepted = response
                            .get("action")
                            .and_then(|s| s.string())
                            .is_some_and(|s| s.matches("accept"))
                            && response
                                .get("content")
                                .and_then(|v| v.get("completed"))
                                .and_then(|v| v.boolean())
                                == Some(true);
                        queue.respond(&id, accepted)?;
                        if accepted {
                            self.runtime.resume(&id, self.options)
                        } else {
                            Ok("{\"status\":\"denied\"}".into())
                        }
                    })()
                } else {
                    (|| match name.as_str() {
                        "zuse.execute" => {
                            let code = arg(args, "code", "")?;
                            if args.and_then(|v| v.get("code")).is_none() {
                                return Err("code is required".into());
                            }
                            let mut options = self.options;
                            options.dry_run |= flag(args, "dry_run")?;
                            options.read_only |= flag(args, "read_only")?;
                            self.runtime.execute(&code, options)
                        }
                        "zuse.check" => runtime::check(&arg(args, "code", "")?, self.options),
                        "zuse.capabilities" => Ok(self.runtime.capabilities()),
                        "zuse.schema" => Ok(runtime::schema()),
                        "zuse.observe" => {
                            let mode = arg(args, "mode", "windows")?;
                            let code = match mode.as_str() {
                                "windows" => "window.list()",
                                "pointer" => "mouse.position()",
                                "accessibility" => "accessibility.tree()",
                                "text" => "screen.text()",
                                _ => return Err("unknown observation mode".into()),
                            };
                            let mut options = self.options;
                            options.read_only = true;
                            self.runtime.execute(code, options)
                        }
                        "zuse.human.pending" => Queue::new()?.list(),
                        "zuse.resume" => self.runtime.resume(&arg(args, "id", "")?, self.options),
                        "zuse.screenshot" => {
                            let mode = arg(args, "source", "desktop")?;
                            let given = arg(args, "path", "")?;
                            let temporary = given.is_empty();
                            let file = if temporary {
                                std::env::temp_dir().join(format!(
                                    "zuse-mcp-{}-{}.png",
                                    std::process::id(),
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_nanos()
                                ))
                            } else {
                                given.into()
                            };
                            let path = file.to_str().ok_or("screenshot path must be UTF-8")?;
                            let code = match mode.as_str() {
                                "desktop" => format!(
                                    "screen.capture(path={},window={})",
                                    quote(path),
                                    quote(&arg(args, "window", "")?)
                                ),
                                "browser" => format!("browser.capture(path={})", quote(path)),
                                _ => return Err("unknown screenshot source".into()),
                            };
                            let result = self.runtime.execute(&code, self.options)?;
                            if runtime::failed(&result) || self.options.dry_run {
                                return Ok(result);
                            }
                            let read = (|| {
                                let size =
                                    std::fs::metadata(&file).map_err(|e| e.to_string())?.len();
                                if size > 16 * 1024 * 1024 {
                                    return Err("screenshot exceeds 16 MiB; use a smaller window or browser viewport".into());
                                }
                                std::fs::read(&file).map_err(|e| e.to_string())
                            })();
                            if temporary {
                                let _ = std::fs::remove_file(&file);
                            }
                            let data = read?;
                            let mut encoded = vec![
                                0;
                                z64::encoded_len(data.len(), z64::Mode::Standard)
                                    .ok_or("image length overflow")?
                            ];
                            let n = z64::encode(&data, &mut encoded, z64::Mode::Standard)
                                .map_err(|e| format!("{e:?}"))?;
                            encoded.truncate(n);
                            picture = Some(String::from_utf8(encoded).map_err(|e| e.to_string())?);
                            Ok(result)
                        }
                        _ => Err("unknown tool".into()),
                    })()
                };
                let result = match outcome {
                    Ok(s) => {
                        is_error = runtime::failed(&s)
                            && json::field(&s, "status").ok().as_deref() != Some("valid");
                        s
                    }
                    Err(e) => {
                        is_error = true;
                        json::error(&e)
                    }
                };
                // Native modern elicitation asks for completion only; secrets stay on the host.
                if call.context.era == Era::Modern
                    && name == "zuse.execute"
                    && call
                        .context
                        .client_capabilities
                        .and_then(|v| v.get("elicitation"))
                        .is_some()
                {
                    let handoff = json::with_value(&result, |v| {
                        if v.get("status")
                            .and_then(|s| s.string())
                            .is_some_and(|s| s.matches("waiting_for_human"))
                        {
                            let request = v
                                .get("handoff")
                                .and_then(|x| x.get("request"))
                                .ok_or("missing handoff")?;
                            Ok(Some((
                                json::text(request.get("id").ok_or("missing id")?)?,
                                json::text(request.get("kind").ok_or("missing kind")?)?,
                                json::text(request.get("message").ok_or("missing message")?)?,
                            )))
                        } else {
                            Ok(None)
                        }
                    })?;
                    if let Some((id, kind, message)) = handoff
                        && kind != "password"
                    {
                        let request = format!(
                            "{{\"confirm\":{{\"method\":\"elicitation/create\",\"params\":{{\"mode\":\"form\",\"message\":{},\"requestedSchema\":{{\"type\":\"object\",\"properties\":{{\"completed\":{{\"type\":\"boolean\",\"title\":\"I completed the requested action\"}}}},\"required\":[\"completed\"]}}}}}}}}",
                            quote(&message)
                        );
                        let mut request_tokens = [Token::EMPTY; 128];
                        let doc = Document::parse(
                            request.as_bytes(),
                            &mut request_tokens,
                            Limits::default(),
                        )
                        .map_err(|e| e.to_string())?;
                        let n = self
                            .protocol
                            .input_required(call.context, Some(doc.root()), Some(&id), &mut out)
                            .map_err(|e| e.to_string())?;
                        out.truncate(n);
                        return Ok(Some(out));
                    }
                }
                let capacity = result
                    .len()
                    .saturating_mul(2)
                    .saturating_add(picture.as_ref().map_or(0, String::len))
                    .saturating_add(65536);
                if capacity > 32 * 1024 * 1024 {
                    return Err("MCP result exceeds 32 MiB".into());
                }
                out.resize(capacity, 0);
                json::with_value(&result, |structured| {
                    let mut content = vec![Content::Text(&result)];
                    if let Some(data) = picture.as_ref() {
                        content.push(Content::Image {
                            data,
                            mime_type: "image/png",
                        })
                    }
                    self.protocol
                        .tool_result(
                            call.context,
                            ToolResult {
                                content: &content,
                                structured_content: Some(structured),
                                is_error,
                            },
                            &mut out,
                        )
                        .map_err(|e| e.to_string())
                })?
            }
            Action::Extension { context, .. } => self
                .protocol
                .error(context, -32601, "Method not found", &mut out)
                .map_err(|e| e.to_string())?,
            Action::Notification(_) | Action::IncomingResponse(_) => return Ok(None),
        };
        out.truncate(count);
        Ok(Some(out))
    }
    fn error_response(
        &self,
        context: zmcpio::mcp::Context<'_>,
        code: i32,
        message: &str,
        mut out: Vec<u8>,
    ) -> Result<Option<Vec<u8>>> {
        let n = self
            .protocol
            .error(context, code, message, &mut out)
            .map_err(|e| e.to_string())?;
        out.truncate(n);
        Ok(Some(out))
    }
}
fn serve_lines(mut reader: impl BufRead, mut writer: impl Write, options: Options) -> Result<()> {
    let mut engine = Engine::new(options);
    let mut line = Vec::with_capacity(4096);
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take((MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        if read > MAX_FRAME {
            return Err("MCP frame exceeds 1 MiB".into());
        }
        if !line.ends_with(b"\n") {
            return Err("unterminated MCP frame".into());
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if line.is_empty() {
            continue;
        }
        if let Some(out) = engine.handle(&line)? {
            writer
                .write_all(&out)
                .and_then(|()| writer.write_all(b"\n"))
                .and_then(|()| writer.flush())
                .map_err(|e| e.to_string())?
        }
    }
    Ok(())
}
pub fn stdio(options: Options) -> Result<()> {
    serve_lines(io::stdin().lock(), io::stdout().lock(), options)
}
#[cfg(unix)]
pub fn unix(path: &Path, options: Options) -> Result<()> {
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    if path.exists() {
        return Err(format!("socket path exists: {}", path.display()));
    }
    let listener = UnixListener::bind(path).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    eprintln!("zuse MCP unix {}", path.display());
    for stream in listener.incoming() {
        let stream = stream.map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(300)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        if let Err(e) = serve_lines(reader, BufWriter::new(stream), options) {
            eprintln!("zuse connection: {e}")
        }
    }
    Ok(())
}
#[cfg(not(unix))]
pub fn unix(_path: &Path, _options: Options) -> Result<()> {
    Err("Unix sockets require a Unix platform; use stdio or HTTP".into())
}
pub fn http(host: &str, port: u16, options: Options) -> Result<()> {
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    let token = std::env::var("ZUSE_TOKEN").ok().filter(|s| !s.is_empty());
    if addresses.iter().any(|a| !a.ip().is_loopback())
        && token.as_ref().is_none_or(|t| t.len() < 32)
    {
        return Err("non-loopback HTTP requires ZUSE_TOKEN with at least 32 bytes".into());
    }
    let listener = TcpListener::bind(addresses.as_slice()).map_err(|e| e.to_string())?;
    eprintln!(
        "zuse MCP http://{}/mcp",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let mut engine = Engine::new(options);
    for connection in listener.incoming() {
        let connection = connection.map_err(|e| e.to_string())?;
        if let Err(e) = http_request(connection, &mut engine, token.as_deref()) {
            eprintln!("zuse HTTP: {e}")
        }
    }
    Ok(())
}
fn http_request(socket: TcpStream, engine: &mut Engine, token: Option<&str>) -> Result<()> {
    use zmcpio::{
        http::{self, Header, Incoming, Method, SecurityPolicy, ServerConfig, Version},
        http_io::{self, BodyReader},
    };
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let local = socket.local_addr().map_err(|e| e.to_string())?;
    let authority = local.to_string();
    let localhost = format!("localhost:{}", local.port());
    struct Policy<'a> {
        authority: &'a str,
        localhost: &'a str,
        token: Option<&'a str>,
        loopback: bool,
    }
    impl SecurityPolicy for Policy<'_> {
        fn origin_allowed(&self, origin: &str) -> bool {
            origin == format!("http://{}", self.authority)
                || (self.loopback && origin == format!("http://{}", self.localhost))
        }
        fn authorized(&self, headers: &[Header<'_>]) -> bool {
            let host = http::header(headers, "host").ok().flatten();
            let valid =
                host == Some(self.authority) || (self.loopback && host == Some(self.localhost));
            valid
                && match self.token {
                    Some(token) => {
                        http::header(headers, "authorization").ok().flatten()
                            == Some(format!("Bearer {token}").as_str())
                    }
                    None => true,
                }
        }
        fn session_valid(&self, _: &str, _: &[Header<'_>]) -> bool {
            false
        }
    }
    let policy = Policy {
        authority: &authority,
        localhost: &localhost,
        token,
        loopback: local.ip().is_loopback(),
    };
    let mut reader = BufReader::new(socket.try_clone().map_err(|e| e.to_string())?);
    let mut writer = BufWriter::new(socket);
    let mut head_storage = [0u8; 16384];
    let head = http_io::read_head(&mut reader, &mut head_storage).map_err(|e| e.to_string())?;
    let mut headers = [Header {
        name: "",
        value: "",
    }; 64];
    let count = head
        .copy_headers(&mut headers)
        .map_err(|e| format!("{e:?}"))?;
    let headers = &headers[..count];
    let reply = |writer: &mut BufWriter<TcpStream>, status, body: &[u8]| {
        http_io::write_response(
            writer,
            status,
            &[
                Header {
                    name: "Content-Type",
                    value: "application/json",
                },
                Header {
                    name: "Connection",
                    value: "close",
                },
            ],
            body,
        )
        .map_err(|e| e.to_string())
    };
    if head
        .header("origin")
        .is_some_and(|o| !policy.origin_allowed(o))
    {
        return reply(&mut writer, 403, b"{\"error\":\"origin rejected\"}");
    }
    if !policy.authorized(headers) {
        return reply(
            &mut writer,
            401,
            b"{\"error\":\"authorization or host rejected\"}",
        );
    }
    if head.target() != Some("/mcp") {
        return reply(&mut writer, 404, b"{\"error\":\"use /mcp\"}");
    }
    let method = match head.method() {
        Some("POST") => Method::Post,
        _ => return reply(&mut writer, 405, b"{\"error\":\"POST required\"}"),
    };
    if head.header("expect").is_some() {
        return reply(&mut writer, 417, b"{\"error\":\"Expect is unsupported\"}");
    }
    let mut request = vec![0u8; MAX_FRAME];
    let mut body = match BodyReader::new(&mut reader, head, MAX_FRAME) {
        Ok(b) => b,
        Err(_) => return reply(&mut writer, 400, b"{\"error\":\"invalid HTTP framing\"}"),
    };
    let length = match http_io::read_body(&mut body, &mut request) {
        Ok(n) => n,
        Err(_) => {
            return reply(
                &mut writer,
                413,
                b"{\"error\":\"invalid or oversized body\"}",
            );
        }
    };
    request.truncate(length);
    let mut tokens = vec![Token::EMPTY; 8192];
    let document=match Document::parse(&request,&mut tokens,limits()){Ok(d)=>d,Err(_)=>return reply(&mut writer,400,b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"Parse error\"}}")};
    let message=match rpc::parse(document.root()){Ok(m)=>m,Err(_)=>return reply(&mut writer,400,b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32600,\"message\":\"Invalid Request\"}}")};
    let config = ServerConfig::stateless(Version::V2026_07_28);
    let mut scratch = [0; 1024];
    if let Err(error) = config.validate(
        Incoming {
            method,
            headers,
            message: Some(message),
        },
        &policy,
        &mut scratch,
    ) {
        let mut output = [0; 8192];
        let id = match message {
            rpc::Message::Request(r) => Some(r.id),
            _ => None,
        };
        let n = error
            .write_rpc(
                id,
                config.version,
                head.header("mcp-protocol-version").unwrap_or(""),
                &mut output,
            )
            .map_err(|e| e.to_string())?;
        return reply(&mut writer, error.status(), &output[..n]);
    }
    match engine.handle(&request)? {
        Some(output) => {
            let status = json::with_value(
                std::str::from_utf8(&output).map_err(|e| e.to_string())?,
                |v| {
                    Ok(v.get("error")
                        .and_then(|e| e.get("code"))
                        .and_then(|e| e.integer().ok())
                        .map_or(200, |code| {
                            http::rpc_error_status(
                                config.version,
                                i32::try_from(code).unwrap_or(-32603),
                            )
                        }))
                },
            )
            .unwrap_or(200);
            reply(&mut writer, status, &output)
        }
        None => reply(&mut writer, 202, b""),
    }
}
