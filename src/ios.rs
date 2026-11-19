//! Native HTTP client for an explicitly selected local WebDriverAgent endpoint.
//!
//! Protocol reviewed against appium/appium-ios commit
//! 9a9092aa35e913a893f6dafddc2bfc13893d1daf, packages/webdriveragent.
//! WDA uses Apple's XCTest/XCUIAutomation on the device; zuse does not launch
//! Appium, install WDA, discover devices, or silently replace another session.
//! Coordinates are UIKit points. Captured PNG dimensions are image pixels.
//!
//! Each public operation uses one absolute 15-second network deadline.
//! Actions are never automatically replayed after an uncertain transport error.
use crate::json::{quote, text};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use zmcpio::json::{Document, Kind, Limits, Token, Value};

type Result<T> = std::result::Result<T, String>;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEADER: usize = 32 * 1024;
const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_SCREENSHOT_JSON: usize = 24 * 1024 * 1024;
const MAX_IMAGE: usize = 16 * 1024 * 1024;
const MAX_TEXT: usize = 16 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Endpoint {
    address: SocketAddr,
    authority: String,
    prefix: String,
    display: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ActiveSession {
    Unknown,
    None,
    Id(String),
}

struct Response {
    value: String,
    active: ActiveSession,
}

/// A local WDA endpoint with separate attached and owned session identities.
///
/// Attaching never grants ownership. Drop performs no network operation.
/// WDA has no atomic compare-and-create session request: applications sharing
/// an endpoint must coordinate changes between status checks and mutations.
pub struct Ios {
    endpoint: Endpoint,
    session: Option<String>,
    owned_session: Option<String>,
    timeout: Duration,
}

impl Ios {
    /// Check WDA status and optionally attach to an explicitly named session.
    /// No session is implicitly created, adopted, replaced or deleted.
    pub fn connect(endpoint: &str, session: Option<&str>) -> Result<Self> {
        Self::connect_with_status(endpoint, session).map(|(device, _)| device)
    }

    /// Connect and return the checked status without a second network operation.
    pub fn connect_with_status(endpoint: &str, session: Option<&str>) -> Result<(Self, String)> {
        let endpoint = parse_endpoint(endpoint)?;
        if let Some(session) = session {
            validate_session(session)?;
        }
        let mut device = Self {
            endpoint,
            session: None,
            owned_session: None,
            timeout: OPERATION_TIMEOUT,
        };
        let deadline = device.deadline();
        let status = device.status_response(deadline)?;
        if let Some(session) = session {
            if matches!(&status.active, ActiveSession::Id(active) if active != session) {
                return Err("iOS requested session is not WDA's active session".into());
            }
            let response = device.request(
                "GET",
                &format!("/session/{session}"),
                None,
                MAX_JSON,
                deadline,
            )?;
            let actual = session_from_value(&response.value)?;
            if actual != session {
                return Err("iOS WDA returned a different session than requested".into());
            }
            device.session = Some(actual);
        }
        let output = device.status_json(&status);
        Ok((device, output))
    }

    pub fn status(&self) -> Result<String> {
        let status = self.status_response(self.deadline())?;
        Ok(self.status_json(&status))
    }

    pub fn screen(&self) -> Result<String> {
        let response = self.request(
            "GET",
            &self.route("/wda/screen", false)?,
            None,
            MAX_JSON,
            self.deadline(),
        )?;
        require_object(&response.value, "WDA screen")?;
        Ok(format!(
            r#"{{"backend":"ios-wda","coordinateSpace":"points","screen":{}}}"#,
            response.value
        ))
    }

    /// Read the native accessibility tree and screen geometry within one budget.
    pub fn snapshot(&self) -> Result<String> {
        let deadline = self.deadline();
        let tree = self.request(
            "GET",
            &self.route("/source?format=json", false)?,
            None,
            MAX_JSON,
            deadline,
        )?;
        let tree = redact_tree(&tree.value)?;
        let screen = self.request(
            "GET",
            &self.route("/wda/screen", false)?,
            None,
            MAX_JSON,
            deadline,
        )?;
        require_object(&screen.value, "WDA screen")?;
        Ok(format!(
            r#"{{"backend":"ios-wda","coordinateSpace":"points","redaction":"secure-fields","tree":{},"screen":{}}}"#,
            tree, screen.value
        ))
    }

    /// Read structured source with secure text fields and their descendants redacted.
    /// XML and description require an explicit raw opt-in.
    pub fn source(&self, format: &str) -> Result<String> {
        self.source_with_options(format, false)
    }

    /// Raw source may include sensitive values supplied by the application.
    pub fn source_with_options(&self, format: &str, raw: bool) -> Result<String> {
        validate_source(format, raw)?;
        let response = self.request(
            "GET",
            &self.route(&format!("/source?format={format}"), false)?,
            None,
            MAX_JSON,
            self.deadline(),
        )?;
        let source = if format == "json" {
            if raw {
                require_object(&response.value, "WDA JSON source")?;
                response.value
            } else {
                redact_tree(&response.value)?
            }
        } else {
            with_json(&response.value, |value| text(value).map(|_| ()))?;
            response.value
        };
        Ok(format!(
            r#"{{"backend":"ios-wda","format":{},"raw":{raw},"source":{source}}}"#,
            quote(format),
        ))
    }

    /// Capture WDA's PNG output; preserve an existing destination on failure.
    pub fn screenshot(&self, path: &Path) -> Result<String> {
        validate_output_path(path)?;
        let deadline = self.deadline();
        let response = self.request(
            "GET",
            &self.route("/screenshot", false)?,
            None,
            MAX_SCREENSHOT_JSON,
            deadline,
        )?;
        let encoded = with_json(&response.value, text)?;
        let capacity = z64::decoded_capacity(encoded.len()).ok_or("iOS screenshot is too large")?;
        if capacity > MAX_IMAGE {
            return Err("iOS screenshot exceeds 16 MiB".into());
        }
        let mut bytes = vec![0; capacity];
        let length = z64::decode(encoded.as_bytes(), &mut bytes, z64::Mode::Standard)
            .map_err(|error| format!("iOS screenshot base64: {error}"))?;
        bytes.truncate(length);
        let (width, height) = png_dimensions(&bytes)?;
        remaining(deadline)?;
        write_image(path, &bytes, deadline)?;
        Ok(format!(
            r#"{{"backend":"ios-wda","path":{},"mimeType":"image/png","width":{width},"height":{height},"bytes":{length},"coordinateSpace":"pixels","inputCoordinateSpace":"points"}}"#,
            quote(&path.to_string_lossy())
        ))
    }

    /// Create a WDA session. WDA replaces its global active session on POST.
    ///
    /// An existing session owned by another client requires replace=true.
    /// Unknown status also requires replace=true. No retries are performed.
    pub fn session_start(&mut self, bundle: Option<&str>, replace: bool) -> Result<String> {
        if let Some(bundle) = bundle {
            validate_bundle(bundle)?;
        }
        let deadline = self.deadline();
        let status = self.status_response(deadline)?;
        let other_or_unknown = match &status.active {
            ActiveSession::None => false,
            ActiveSession::Id(id) => self.owned_session.as_ref() != Some(id),
            ActiveSession::Unknown => true,
        };
        if other_or_unknown && !replace {
            return Err("iOS WDA already has another session, or its ownership cannot be verified; session_start requires replace=true to replace it".into());
        }
        let body = match bundle {
            Some(bundle) => format!(
                r#"{{"capabilities":{{"alwaysMatch":{{"bundleId":{},"forceAppLaunch":false}}}}}}"#,
                quote(bundle)
            ),
            None => r#"{"capabilities":{"alwaysMatch":{}}}"#.into(),
        };
        // WDA may kill the old session even if creation fails. Never continue
        // input through the previous attached identity after this request.
        self.session = None;
        let response = self.request("POST", "/session", Some(&body), MAX_JSON, deadline)?;
        let created = session_from_value(&response.value)?;
        if matches!(&response.active, ActiveSession::None)
            || matches!(&response.active, ActiveSession::Id(id) if id != &created)
        {
            return Err("iOS WDA created-session identifiers disagree".into());
        }
        self.session = Some(created.clone());
        self.owned_session = Some(created);
        Ok(format!(
            r#"{{"backend":"ios-wda","owned":true,"session":{}}}"#,
            response.value
        ))
    }

    /// End only a session this instance created and that is still active.
    pub fn session_end(&mut self) -> Result<String> {
        let owned = self
            .owned_session
            .clone()
            .ok_or("iOS session_end cannot delete an attached external session")?;
        let deadline = self.deadline();
        let status = self.status_response(deadline)?;
        match &status.active {
            ActiveSession::Id(id) if id == &owned => {}
            ActiveSession::Unknown => {
                return Err(
                    "iOS current WDA session is unknown; refusing to delete a session".into(),
                );
            }
            _ => {
                self.session = None;
                self.owned_session = None;
                return Err(
                    "iOS owned session is no longer active; no other session was deleted".into(),
                );
            }
        }
        self.session = None;
        // Keep ownership on an uncertain error so an explicit later cleanup
        // can recheck the identity; never permit input through that old session.
        self.request(
            "DELETE",
            &format!("/session/{owned}"),
            None,
            MAX_JSON,
            deadline,
        )?;
        self.owned_session = None;
        Ok(format!(
            r#"{{"backend":"ios-wda","ended":true,"sessionId":{}}}"#,
            quote(&owned)
        ))
    }

    pub fn tap(&self, x: f64, y: f64) -> Result<String> {
        validate_point(x, y)?;
        let body = format!(r#"{{"x":{x},"y":{y}}}"#);
        self.action("/wda/tap", &body)
    }

    /// Move a touch pointer over duration_ms, rather than holding before a drag.
    pub fn swipe(&self, x1: f64, y1: f64, x2: f64, y2: f64, duration_ms: u64) -> Result<String> {
        validate_swipe(x1, y1, x2, y2, duration_ms)?;
        let body = format!(
            r#"{{"actions":[{{"type":"pointer","id":"zuse-touch","parameters":{{"pointerType":"touch"}},"actions":[{{"type":"pointerMove","duration":0,"origin":"viewport","x":{x1},"y":{y1}}},{{"type":"pointerDown","button":0}},{{"type":"pointerMove","duration":{duration_ms},"origin":"viewport","x":{x2},"y":{y2}}},{{"type":"pointerUp","button":0}}]}}]}}"#
        );
        self.action("/actions", &body)
    }

    pub fn type_text(&self, value: &str) -> Result<String> {
        validate_text(value)?;
        self.type_value(value)
    }

    pub fn key(&self, name: &str) -> Result<String> {
        match key_value(name)? {
            Key::Text(value) => self.type_value(value),
            Key::Button(name) => self.action(
                "/wda/pressButton",
                &format!(r#"{{"name":{}}}"#, quote(name)),
            ),
        }
    }

    pub fn launch(&self, bundle: &str) -> Result<String> {
        validate_bundle(bundle)?;
        self.action(
            "/wda/apps/launch",
            &format!(r#"{{"bundleId":{},"asAppUnderTest":true}}"#, quote(bundle)),
        )
    }

    /// Open a browser URL through WDA; this is native UI control, not a DOM API.
    pub fn open_url(&self, url: &str) -> Result<String> {
        validate_url(url)?;
        self.action("/url", &format!(r#"{{"url":{}}}"#, quote(url)))
    }

    fn type_value(&self, value: &str) -> Result<String> {
        self.action("/wda/keys", &format!(r#"{{"value":[{}]}}"#, quote(value)))
    }

    fn action(&self, suffix: &str, body: &str) -> Result<String> {
        let response = self.request(
            "POST",
            &self.route(suffix, true)?,
            Some(body),
            MAX_JSON,
            self.deadline(),
        )?;
        Ok(format!(
            r#"{{"backend":"ios-wda","ok":true,"value":{}}}"#,
            response.value
        ))
    }

    fn deadline(&self) -> Instant {
        Instant::now() + self.timeout
    }

    fn route(&self, suffix: &str, required: bool) -> Result<String> {
        match &self.session {
            Some(session) => Ok(format!("/session/{session}{suffix}")),
            None if required => {
                Err("iOS input requires an explicitly attached or owned WDA session".into())
            }
            None => Ok(suffix.into()),
        }
    }

    fn status_response(&self, deadline: Instant) -> Result<Response> {
        let response = self.request("GET", "/status", None, MAX_JSON, deadline)?;
        with_json(&response.value, |value| {
            if unique_field(value, "ready")?.and_then(Value::boolean) != Some(true) {
                return Err("iOS WebDriverAgent is not ready".into());
            }
            Ok(())
        })?;
        Ok(response)
    }

    fn status_json(&self, status: &Response) -> String {
        let (known, active) = match &status.active {
            ActiveSession::Unknown => (false, "null".into()),
            ActiveSession::None => (true, "null".into()),
            ActiveSession::Id(id) => (true, quote(id)),
        };
        let attached = self
            .session
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "null".into());
        let owned = self.session.is_some() && self.session == self.owned_session;
        format!(
            r#"{{"platform":"ios","backend":"ios-wda","endpoint":{},"attachedSessionId":{attached},"owned":{owned},"activeSessionKnown":{known},"activeSessionId":{active},"wda":{}}}"#,
            quote(&self.endpoint.display),
            status.value
        )
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        limit: usize,
        deadline: Instant,
    ) -> Result<Response> {
        let (status, body) = http_request(&self.endpoint, method, path, body, limit, deadline)?;
        parse_response(status, &body)
            .and_then(|response| {
                remaining(deadline)?;
                Ok(response)
            })
            .map_err(|error| {
                if method == "GET" {
                    error
                } else {
                    // A reported failure may follow a partial action. A malformed
                    // reply supplies no completion evidence. Do not replay it.
                    format!("{error}; iOS action outcome could not be confirmed, observe before retrying")
                }
            })
    }
}

pub(crate) fn validate_endpoint(endpoint: &str) -> Result<()> {
    parse_endpoint(endpoint).map(|_| ())
}

pub(crate) fn validate_session(session: &str) -> Result<()> {
    if session.is_empty()
        || session.len() > 128
        || !session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "iOS session must contain 1..128 ASCII letters, digits, hyphens or underscores".into(),
        );
    }
    Ok(())
}

pub(crate) fn validate_bundle(bundle: &str) -> Result<()> {
    if bundle.is_empty()
        || bundle.len() > 255
        || !bundle
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        || bundle.starts_with('.')
        || bundle.ends_with('.')
        || bundle.contains("..")
    {
        return Err(
            "iOS bundle identifier must contain 1..255 ASCII letters, digits, dots or hyphens"
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn validate_text(value: &str) -> Result<()> {
    if value.len() > MAX_TEXT {
        return Err("iOS text exceeds 16 KiB of UTF-8".into());
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err("iOS text contains an unsupported control character".into());
    }
    Ok(())
}

pub(crate) fn validate_point(x: f64, y: f64) -> Result<()> {
    if [x, y]
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=65535.0).contains(v))
    {
        return Err("iOS coordinates must be finite UIKit points in 0..65535".into());
    }
    Ok(())
}

pub(crate) fn validate_swipe(x1: f64, y1: f64, x2: f64, y2: f64, duration_ms: u64) -> Result<()> {
    validate_point(x1, y1)?;
    validate_point(x2, y2)?;
    if !(1..=10_000).contains(&duration_ms) {
        return Err("iOS swipe duration must be 1..10000 ms".into());
    }
    Ok(())
}

pub(crate) fn validate_format(format: &str) -> Result<()> {
    if !matches!(format, "xml" | "json" | "description") {
        return Err("iOS source format must be xml, json or description".into());
    }
    Ok(())
}

pub(crate) fn validate_source(format: &str, raw: bool) -> Result<()> {
    validate_format(format)?;
    if format != "json" && !raw {
        return Err("iOS XML and description source require raw=true because secure text cannot be reliably redacted from those formats".into());
    }
    Ok(())
}

pub(crate) fn validate_url(url: &str) -> Result<()> {
    if url.len() > 8192 || url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err("iOS browser URL is invalid or exceeds 8 KiB".into());
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("iOS browser URL must use http:// or https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err("iOS browser URL requires a host without embedded credentials".into());
    }
    let port = if let Some(bracketed) = authority.strip_prefix('[') {
        let (address, suffix) = bracketed
            .split_once(']')
            .ok_or("iOS browser IPv6 host requires a closing bracket")?;
        address
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| "iOS browser bracketed host must be a valid IPv6 address")?;
        if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or("iOS browser IPv6 closing bracket may only be followed by a port")?,
            )
        }
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        validate_url_host(host)?;
        port
    };
    // RFC 9110 permits an empty port component and uses the scheme's default.
    // Nonempty ports follow the browser URL range, including syntactic port 0.
    if let Some(port) = port
        && !port.is_empty()
        && (!port.bytes().all(|byte| byte.is_ascii_digit()) || port.parse::<u16>().is_err())
    {
        return Err("iOS browser URL port must be decimal in 0..65535".into());
    }
    Ok(())
}

fn validate_url_host(host: &str) -> Result<()> {
    if host.is_empty() {
        return Err("iOS browser URL requires a nonempty host".into());
    }
    // HTTP hosts are percent-decoded before interpretation. Preserve ordinary
    // Unicode/IDN names, but do not let escaped authority delimiters bypass the
    // structure checks. DNS lookup and IDNA mapping remain with the platform.
    let bytes = host.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err("iOS browser host has an incomplete percent escape".into());
            }
            let high = (bytes[index + 1] as char)
                .to_digit(16)
                .ok_or("iOS browser host has an invalid percent escape")?;
            let low = (bytes[index + 2] as char)
                .to_digit(16)
                .ok_or("iOS browser host has an invalid percent escape")?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded =
        std::str::from_utf8(&decoded).map_err(|_| "iOS browser host is not valid UTF-8")?;
    if decoded.chars().any(|ch| {
        ch.is_control()
            || ch.is_whitespace()
            || matches!(
                ch,
                '#' | '/' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|' | '%'
            )
    }) {
        return Err(
            "iOS browser host contains a forbidden character; IPv6 requires brackets".into(),
        );
    }
    Ok(())
}

pub(crate) fn validate_key(name: &str) -> Result<()> {
    key_value(name).map(|_| ())
}

enum Key {
    Text(&'static str),
    Button(&'static str),
}

fn key_value(name: &str) -> Result<Key> {
    match name.to_ascii_lowercase().as_str() {
        "home" => Ok(Key::Button("home")),
        "volumeup" => Ok(Key::Button("volumeUp")),
        "volumedown" => Ok(Key::Button("volumeDown")),
        "enter" | "return" => Ok(Key::Text("\n")),
        "backspace" => Ok(Key::Text("\u{8}")),
        _ => Err("iOS key supports Home, VolumeUp, VolumeDown, Enter and Backspace".into()),
    }
}

pub(crate) fn validate_output_path(path: &Path) -> Result<()> {
    if path.file_name().is_none()
        || path.as_os_str().is_empty()
        || path.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err("iOS screenshot needs an output filename".into());
    }
    Ok(())
}

fn parse_endpoint(input: &str) -> Result<Endpoint> {
    if input.len() > 2048
        || !input.is_ascii()
        || input.bytes().any(|b| b.is_ascii_control() || b == b' ')
        || input.contains(['@', '?', '#'])
    {
        return Err(
            "iOS WDA endpoint must be a bounded local HTTP URL without credentials or query".into(),
        );
    }
    let rest = input
        .strip_prefix("http://")
        .ok_or("iOS WDA endpoint must use http://")?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed
            .split_once("]:")
            .ok_or("iOS IPv6 WDA endpoint requires [address]:port")?
    } else {
        let pair = authority
            .rsplit_once(':')
            .ok_or("iOS WDA endpoint requires an explicit port")?;
        if pair.0.contains(':') {
            return Err("iOS IPv6 WDA address must use brackets".into());
        }
        pair
    };
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err("iOS WDA endpoint port is invalid".into());
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| "iOS WDA endpoint port is invalid")?;
    if port == 0 {
        return Err("iOS WDA endpoint port must be nonzero".into());
    }
    let ip = if host.eq_ignore_ascii_case("localhost") {
        IpAddr::from([127, 0, 0, 1])
    } else {
        host.parse::<IpAddr>()
            .map_err(|_| "iOS WDA endpoint must use localhost or a literal loopback address")?
    };
    if !ip.is_loopback() {
        return Err(
            "iOS WDA endpoint must be loopback; explicitly forward the selected device first"
                .into(),
        );
    }
    if path
        .bytes()
        .any(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'/' | b'_' | b'-' | b'.'))
        || path.split('/').any(|part| part == "." || part == "..")
    {
        return Err("iOS WDA endpoint path contains unsupported characters or traversal".into());
    }
    let path = path.trim_end_matches('/');
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    Ok(Endpoint {
        address: SocketAddr::new(ip, port),
        authority: authority.into(),
        prefix,
        display: input.trim_end_matches('/').into(),
    })
}

fn with_json<T>(input: &str, f: impl FnOnce(Value<'_>) -> Result<T>) -> Result<T> {
    let mut capacity = 128;
    loop {
        let mut tokens = vec![Token::EMPTY; capacity];
        match Document::parse(
            input.as_bytes(),
            &mut tokens,
            Limits {
                max_bytes: MAX_SCREENSHOT_JSON,
                max_depth: 64,
                max_members: 65536,
                max_string_bytes: MAX_SCREENSHOT_JSON,
            },
        ) {
            Ok(document) => return f(document.root()),
            Err(error) if error.kind == zmcpio::ErrorKind::Tokens && capacity < 262144 => {
                capacity *= 2
            }
            Err(error) => return Err(format!("iOS WDA JSON: {error}")),
        }
    }
}

fn unique_field<'a>(value: Value<'a>, name: &str) -> Result<Option<Value<'a>>> {
    let mut matches = value.members().filter(|(key, _)| key.matches(name));
    let first = matches.next().map(|(_, value)| value);
    if matches.next().is_some() {
        return Err(format!("iOS WDA response repeats field {name}"));
    }
    Ok(first)
}

fn require_object(value: &str, name: &str) -> Result<()> {
    with_json(value, |v| {
        if v.kind() != Kind::Object {
            return Err(format!("{name} must be a JSON object"));
        }
        Ok(())
    })
}

// WDA's JSON source has typed nodes and a children array. Do not infer secure
// text by its contents: trust the native secure role, and propagate it through
// descendants. Unknown attributes on protected nodes are omitted, so a new WDA
// content field cannot bypass a list of known value/text/label names.
fn redact_tree(input: &str) -> Result<String> {
    with_json(input, |root| {
        let mut output = String::with_capacity(input.len());
        redact_node(root, false, &mut output)?;
        Ok(output)
    })
}

fn redact_node(node: Value<'_>, inherited: bool, output: &mut String) -> Result<()> {
    if node.kind() != Kind::Object {
        return Err("iOS WDA JSON source node must be an object".into());
    }
    let role = text(unique_field(node, "type")?.ok_or("iOS WDA JSON source node has no type")?)?;
    let protected = inherited
        || [
            "SecureTextField",
            "XCUIElementTypeSecureTextField",
            "PasswordField",
            "Password",
        ]
        .iter()
        .any(|name| role.eq_ignore_ascii_case(name))
        || ["secure", "isSecure", "password", "isPassword", "protected"]
            .iter()
            .try_fold(false, |found, key| -> Result<bool> {
                Ok(found || unique_field(node, key)?.and_then(Value::boolean) == Some(true))
            })?;
    let children = unique_field(node, "children")?;
    output.push_str("{\"type\":");
    output.push_str(&quote(&role));
    if protected {
        output.push_str(",\"redacted\":true");
    }
    for (key, value) in node.members() {
        if key.matches("type") || key.matches("children") {
            continue;
        }
        let name: String = key.chars().collect();
        if protected {
            // Preserve only numeric geometry and boolean accessibility state.
            // Content, identifiers and future string attributes are omitted.
            if name == "rect" {
                if value.kind() != Kind::Object {
                    return Err("iOS WDA source rectangle must be an object".into());
                }
                output.push_str(",\"rect\":{");
                let mut first = true;
                for coordinate in ["x", "y", "width", "height"] {
                    if let Some(number) = unique_field(value, coordinate)? {
                        if number.kind() != Kind::Number {
                            return Err("iOS WDA source rectangle must contain numbers".into());
                        }
                        if !first {
                            output.push(',');
                        }
                        first = false;
                        output.push_str(&quote(coordinate));
                        output.push(':');
                        output.push_str(number.raw());
                    }
                }
                output.push('}');
            } else if matches!(
                name.as_str(),
                "enabled"
                    | "visible"
                    | "focused"
                    | "selected"
                    | "accessible"
                    | "isEnabled"
                    | "isVisible"
                    | "isFocused"
                    | "isSelected"
                    | "isAccessible"
                    | "isAccessibilityContainer"
            ) && value.boolean().is_some()
            {
                output.push(',');
                output.push_str(&quote(&name));
                output.push(':');
                output.push_str(value.raw());
            }
            continue;
        }
        output.push(',');
        output.push_str(&quote(&name));
        output.push(':');
        output.push_str(value.raw());
    }
    if let Some(children) = children {
        if children.kind() != Kind::Array {
            return Err("iOS WDA source children must be an array".into());
        }
        output.push_str(",\"children\":[");
        for (index, child) in children.elements().enumerate() {
            if index != 0 {
                output.push(',');
            }
            redact_node(child, protected, output)?;
        }
        output.push(']');
    }
    output.push('}');
    Ok(())
}

fn session_from_value(value: &str) -> Result<String> {
    with_json(value, |v| {
        let id = text(unique_field(v, "sessionId")?.ok_or("iOS WDA response has no sessionId")?)?;
        validate_session(&id)?;
        Ok(id)
    })
}

fn parse_response(status: u16, body: &str) -> Result<Response> {
    with_json(body, |root| {
        if root.kind() != Kind::Object {
            return Err("iOS WDA response must be a JSON object".into());
        }
        let value = unique_field(root, "value")?.ok_or("iOS WDA response has no value")?;
        if let Some(error) = unique_field(value, "error")? {
            let code = text(error)?;
            let message = unique_field(value, "message")?
                .map(text)
                .transpose()?
                .unwrap_or_default();
            return Err(format!("iOS WDA {code}: {}", bounded(&message)));
        }
        if let Some(legacy) = unique_field(root, "status")?
            && legacy
                .integer()
                .map_err(|_| "iOS WDA status must be an integer")?
                != 0
        {
            return Err(format!(
                "iOS WDA legacy error status {}: {}",
                legacy.raw(),
                bounded(value.raw())
            ));
        }
        if !(200..300).contains(&status) {
            return Err(format!("iOS WDA HTTP {status}: {}", bounded(value.raw())));
        }
        let active = match unique_field(root, "sessionId")? {
            None => ActiveSession::Unknown,
            Some(value) if value.kind() == Kind::Null => ActiveSession::None,
            Some(value) => {
                let id = text(value)?;
                validate_session(&id)?;
                ActiveSession::Id(id)
            }
        };
        Ok(Response {
            value: value.raw().into(),
            active,
        })
    })
}

fn bounded(value: &str) -> String {
    value.chars().take(1024).collect()
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| "iOS WDA operation deadline exceeded".into())
}

fn write_all_until(stream: &mut TcpStream, bytes: &[u8], deadline: Instant) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|e| e.to_string())?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err("iOS WDA stopped accepting request bytes".into()),
            Ok(size) => offset += size,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Err("iOS WDA operation deadline exceeded".into());
            }
            Err(error) => return Err(format!("iOS WDA write: {error}")),
        }
    }
    Ok(())
}

struct Input {
    stream: TcpStream,
    buffer: Vec<u8>,
    offset: usize,
    deadline: Instant,
}

impl Input {
    fn fill(&mut self) -> Result<bool> {
        self.buffer.resize(4096, 0);
        self.offset = 0;
        loop {
            self.stream
                .set_read_timeout(Some(remaining(self.deadline)?))
                .map_err(|e| e.to_string())?;
            match self.stream.read(&mut self.buffer) {
                Ok(size) => {
                    self.buffer.truncate(size);
                    return Ok(size != 0);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err("iOS WDA operation deadline exceeded".into());
                }
                Err(error) => return Err(format!("iOS WDA read: {error}")),
            }
        }
    }

    fn byte(&mut self) -> Result<u8> {
        if self.offset == self.buffer.len() && !self.fill()? {
            return Err("iOS WDA closed an incomplete HTTP response".into());
        }
        let byte = self.buffer[self.offset];
        self.offset += 1;
        Ok(byte)
    }

    fn line(&mut self, limit: usize) -> Result<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            line.push(self.byte()?);
            if line.ends_with(b"\r\n") {
                if line.len() - 2 > limit {
                    return Err("iOS WDA HTTP line exceeds its bound".into());
                }
                line.truncate(line.len() - 2);
                return Ok(line);
            }
            if line.len() > limit + 2 {
                return Err("iOS WDA HTTP line exceeds its bound".into());
            }
        }
    }

    fn append(&mut self, output: &mut Vec<u8>, count: usize, limit: usize) -> Result<()> {
        if count > limit.saturating_sub(output.len()) {
            return Err("iOS WDA HTTP body exceeds its bound".into());
        }
        let mut left = count;
        while left != 0 {
            if self.offset == self.buffer.len() && !self.fill()? {
                return Err("iOS WDA closed an incomplete HTTP body".into());
            }
            let length = left.min(self.buffer.len() - self.offset);
            output.extend_from_slice(&self.buffer[self.offset..self.offset + length]);
            self.offset += length;
            left -= length;
        }
        Ok(())
    }

    fn read_remaining(&mut self, output: &mut Vec<u8>, limit: usize) -> Result<()> {
        loop {
            let available = self.buffer.len() - self.offset;
            self.append(output, available, limit)?;
            if !self.fill()? {
                return Ok(());
            }
        }
    }
}

fn header_field(line: &str) -> Result<(&str, &str)> {
    let (name, value) = line
        .split_once(':')
        .ok_or("iOS malformed WDA HTTP header")?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_\x60|~".contains(&byte))
        || value
            .bytes()
            .any(|byte| (byte < 32 && byte != b'\t') || byte == 127)
    {
        return Err("iOS invalid WDA HTTP header name or value".into());
    }
    Ok((name, value.trim()))
}

fn http_request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&str>,
    limit: usize,
    deadline: Instant,
) -> Result<(u16, String)> {
    let body = body.unwrap_or("");
    if body.len() > 64 * 1024 {
        return Err("iOS WDA request body exceeds 64 KiB".into());
    }
    let mut sent = false;
    let result = (|| {
        let mut stream = TcpStream::connect_timeout(
            &endpoint.address,
            remaining(deadline)?.min(Duration::from_secs(5)),
        )
        .map_err(|e| format!("iOS WDA connect: {e}"))?;
        let request = format!(
            "{method} {}{path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nAccept-Encoding: identity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            endpoint.prefix,
            endpoint.authority,
            body.len()
        );
        sent = true;
        write_all_until(&mut stream, request.as_bytes(), deadline)?;
        let mut input = Input {
            stream,
            buffer: Vec::new(),
            offset: 0,
            deadline,
        };
        let status_line = input.line(MAX_HEADER)?;
        let status_line =
            std::str::from_utf8(&status_line).map_err(|_| "iOS invalid HTTP status line")?;
        let mut parts = status_line.split_whitespace();
        if !matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
            return Err("iOS WDA requires an HTTP/1.0 or HTTP/1.1 response".into());
        }
        let status = parts
            .next()
            .filter(|p| p.len() == 3)
            .ok_or("iOS WDA has no HTTP status")?
            .parse::<u16>()
            .map_err(|_| "iOS invalid HTTP status")?;
        if !(200..600).contains(&status) {
            return Err("iOS WDA returned an unsupported informational HTTP status".into());
        }
        let mut used = status_line.len() + 2;
        let mut length = None;
        let mut chunked = false;
        let mut transfer_seen = false;
        loop {
            let line = input.line(MAX_HEADER)?;
            used += line.len() + 2;
            if used > MAX_HEADER {
                return Err("iOS WDA HTTP headers exceed 32 KiB".into());
            }
            if line.is_empty() {
                break;
            }
            let line =
                std::str::from_utf8(&line).map_err(|_| "iOS WDA HTTP header is not UTF-8")?;
            if line.starts_with([' ', '\t']) {
                return Err("iOS WDA folded HTTP headers are unsupported".into());
            }
            let (name, value) = header_field(line)?;
            match name.to_ascii_lowercase().as_str() {
                "content-length" => {
                    if length.is_some()
                        || value.is_empty()
                        || !value.bytes().all(|b| b.is_ascii_digit())
                    {
                        return Err("iOS ambiguous WDA Content-Length".into());
                    }
                    let count = value
                        .parse::<usize>()
                        .map_err(|_| "iOS invalid WDA Content-Length")?;
                    if count > limit {
                        return Err("iOS WDA HTTP body exceeds its bound".into());
                    }
                    length = Some(count);
                }
                "transfer-encoding" => {
                    if transfer_seen || !value.eq_ignore_ascii_case("chunked") {
                        return Err("iOS unsupported WDA Transfer-Encoding".into());
                    }
                    transfer_seen = true;
                    chunked = true;
                }
                "content-encoding" if !value.eq_ignore_ascii_case("identity") => {
                    return Err("iOS compressed WDA responses are unsupported".into());
                }
                _ => {}
            }
        }
        if chunked && length.is_some() {
            return Err("iOS WDA response has both chunked framing and Content-Length".into());
        }
        let mut output = Vec::new();
        if chunked {
            loop {
                let line = input.line(1024)?;
                let line = std::str::from_utf8(&line).map_err(|_| "iOS invalid HTTP chunk size")?;
                let size = line.split(';').next().unwrap_or("");
                if size.is_empty() || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("iOS invalid HTTP chunk size".into());
                }
                let size =
                    usize::from_str_radix(size, 16).map_err(|_| "iOS HTTP chunk size overflow")?;
                if size == 0 {
                    let mut trailers = 0;
                    loop {
                        let trailer = input.line(MAX_HEADER)?;
                        trailers += trailer.len() + 2;
                        if trailers > MAX_HEADER {
                            return Err("iOS WDA HTTP trailers exceed 32 KiB".into());
                        }
                        if trailer.is_empty() {
                            break;
                        }
                        let line = std::str::from_utf8(&trailer)
                            .map_err(|_| "iOS invalid HTTP trailer")?;
                        let (name, _) = header_field(line)?;
                        if name.eq_ignore_ascii_case("content-length")
                            || name.eq_ignore_ascii_case("transfer-encoding")
                        {
                            return Err("iOS WDA trailer attempts to change body framing".into());
                        }
                    }
                    break;
                }
                input.append(&mut output, size, limit)?;
                if input.byte()? != b'\r' || input.byte()? != b'\n' {
                    return Err("iOS WDA HTTP chunk has no CRLF terminator".into());
                }
            }
        } else if let Some(length) = length {
            input.append(&mut output, length, limit)?;
        } else {
            input.read_remaining(&mut output, limit)?;
        }
        let text = String::from_utf8(output).map_err(|_| "iOS WDA body is not UTF-8")?;
        Ok((status, text))
    })();
    result.map_err(|error: String| {
        if sent && method != "GET" {
            format!("{error}; iOS action outcome is unknown, observe before retrying")
        } else {
            error
        }
    })
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    if bytes.len() < 45
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes[8..12] != 13u32.to_be_bytes()
        || &bytes[12..16] != b"IHDR"
    {
        return Err("iOS WDA screenshot is not a complete PNG".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        return Err("iOS WDA screenshot dimensions are invalid".into());
    }
    let mut offset = 8;
    let mut image_data = false;
    while offset + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(length)
            .and_then(|v| v.checked_add(12))
            .filter(|&end| end <= bytes.len())
            .ok_or("iOS WDA PNG chunk is truncated")?;
        let kind = &bytes[offset + 4..offset + 8];
        if kind == b"IDAT" {
            image_data = true;
        }
        if kind == b"IEND" {
            if length != 0 || end != bytes.len() || !image_data {
                return Err("iOS WDA PNG terminator is invalid".into());
            }
            return Ok((width, height));
        }
        offset = end;
    }
    Err("iOS WDA PNG has no complete image terminator".into())
}

fn write_image(path: &Path, bytes: &[u8], deadline: Instant) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(
        ".zuse-ios-{}-{}.png",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|e| format!("iOS screenshot output: {e}"))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|e| format!("iOS screenshot write: {e}"))?;
        file.flush()
            .map_err(|e| format!("iOS screenshot flush: {e}"))?;
        remaining(deadline)?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| format!("iOS screenshot replace: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
#[path = "../tests/ios/protocol.rs"]
mod tests;
