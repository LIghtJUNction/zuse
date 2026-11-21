//! Native Chrome DevTools Protocol client over bounded loopback WebSockets.
//!
//! Launches Chromium with a private temporary profile, or connects to an explicitly
//! supplied local debugging endpoint. No browser driver or scripting runtime is used.
use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zmcpio::json::{Document, Limits, Token, Value};

mod click_watch;
mod key_hold;

type Result<T> = std::result::Result<T, String>;
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
static PROFILE_ID: AtomicU64 = AtomicU64::new(0);

/// One persistent browser connection and its selected page.
pub struct Browser {
    socket: WebSocket,
    child: Option<Child>,
    profile: Option<PathBuf>,
    session: String,
    target: String,
    next_id: u64,
    events: VecDeque<String>,
    owned_target: bool,
    mobile: bool,
    closed: bool,
    close_error: Option<String>,
    observation_binding: String,
    observation_ids: BTreeSet<String>,
}

impl Browser {
    /// Start a separate Chromium profile. `ZUSE_BROWSER` selects the executable.
    /// Chromium's sandbox stays enabled.
    pub fn launch(headless: bool) -> Result<Self> {
        let executable = browser_executable()?;
        let profile = new_profile()?;
        let mut command = Command::new(executable);
        command
            .args([
                "--remote-debugging-port=0",
                "--remote-debugging-address=127.0.0.1",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
            ])
            .arg(format!("--user-data-dir={}", profile.display()));
        if headless {
            command.arg("--headless=new");
        }
        command
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&profile);
                return Err(format!("launch browser: {error}"));
            }
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let connection = loop {
            if let Ok(text) = fs::read_to_string(profile.join("DevToolsActivePort")) {
                let mut lines = text.lines();
                if let (Some(port), Some(path)) = (lines.next(), lines.next())
                    && port.parse::<u16>().is_ok()
                    && path.starts_with("/devtools/browser/")
                {
                    break Self::connect(&format!("ws://127.0.0.1:{port}{path}"));
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    break Err(format!(
                        "browser exited before its debugging endpoint became ready: {status}"
                    ));
                }
                Err(error) => break Err(format!("observe browser launch: {error}")),
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                break Err("browser launch timed out".into());
            }
            thread::sleep(Duration::from_millis(40));
        };
        match connection {
            Ok(mut browser) => {
                browser.child = Some(child);
                browser.profile = Some(profile);
                Ok(browser)
            }
            Err(error) => {
                stop_owned_browser(&mut child, Duration::ZERO);
                let _ = fs::remove_dir_all(profile);
                Err(error)
            }
        }
    }

    /// Connect only to a literal loopback IP or `localhost`. An HTTP URL is
    /// resolved through `/json/version`; a WebSocket URL is used directly.
    /// A fresh page is created; existing pages can be selected with `select`.
    pub fn connect(endpoint: &str) -> Result<Self> {
        let endpoint = if endpoint.starts_with("http://") {
            discover_endpoint(endpoint)?
        } else {
            endpoint.to_owned()
        };
        let socket = WebSocket::connect(&endpoint)?;
        let page_endpoint = endpoint.contains("/devtools/page/");
        Self::from_socket(socket, page_endpoint, false)
    }

    /// Attach to one explicitly named, existing page. This does not create,
    /// activate, navigate or take ownership of a tab. Closing the connection
    /// leaves the selected page open.
    pub fn connect_existing(endpoint: &str, target: &str) -> Result<Self> {
        Self::connect_existing_cancellable(
            endpoint,
            target,
            &crate::observation::Cancellation::default(),
        )
    }

    /// Existing-page attachment whose discovery, handshake and CDP reads wake
    /// when the embedding host cancels. No input or page activation is sent.
    pub fn connect_existing_cancellable(
        endpoint: &str,
        target: &str,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<Self> {
        if target.is_empty() || target.len() > 1024 || target.contains('\0') {
            return Err("an explicit existing browser target ID is required".into());
        }
        cancellation.check()?;
        let address = if endpoint.starts_with("http://") {
            let address = self::endpoint(endpoint, "http://")?;
            let (mut stream, _wake) = existing_stream(&address.address, cancellation)?;
            let discovered = discover_stream(&mut stream, &address.host);
            cancellation.check()?;
            discovered?
        } else {
            endpoint.to_owned()
        };
        if address.contains("/devtools/page/") {
            return Err("connect_existing requires the browser debugging endpoint and an explicit target ID".into());
        }
        let address = self::endpoint(&address, "ws://")?;
        let (stream, _wake) = existing_stream(&address.address, cancellation)?;
        let socket = WebSocket::connect_stream(stream, &address.host, &address.path);
        cancellation.check()?;
        let mut browser = Self::unattached_socket(socket?, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let info = browser.request_until(
            "Target.getTargetInfo",
            &format!(r#"{{"targetId":{}}}"#, quote(target)),
            false,
            deadline,
            Some(cancellation),
        )?;
        if field_string(&info, &["targetInfo", "targetId"])? != target
            || field_string(&info, &["targetInfo", "type"])? != "page"
        {
            return Err("requested existing target is not the named browser page".into());
        }
        let attached = browser.request_until(
            "Target.attachToTarget",
            &format!(r#"{{"targetId":{},"flatten":true}}"#, quote(target)),
            false,
            deadline,
            Some(cancellation),
        )?;
        browser.session = field_string(&attached, &["sessionId"])?;
        browser.target = target.to_owned();
        browser.request_until("Page.enable", "{}", true, deadline, Some(cancellation))?;
        browser.request_until(
            "Page.setLifecycleEventsEnabled",
            r#"{"enabled":true}"#,
            true,
            deadline,
            Some(cancellation),
        )?;
        cancellation.check()?;
        Ok(browser)
    }

    /// Connect through an authorized local ADB server to one observed Chrome socket.
    /// A fresh owned tab is requested with background=true. Existing tabs are never
    /// enumerated or selected by this session, and Chrome is not started for the user.
    pub fn connect_android(serial: &str, socket: Option<&str>) -> Result<Self> {
        let tunnel = crate::android::DevToolsTunnel::connect(serial, socket)
            .map_err(|error| format!("Android Chrome socket discovery: {error}"))?;
        Self::connect_android_tunnel(tunnel)
    }

    /// Explicit-server variant for local tunnels and deterministic mock testing.
    pub fn connect_android_at(
        address: SocketAddr,
        serial: &str,
        socket: Option<&str>,
    ) -> Result<Self> {
        let tunnel = crate::android::DevToolsTunnel::connect_at(address, serial, socket)
            .map_err(|error| format!("Android Chrome socket discovery: {error}"))?;
        Self::connect_android_tunnel(tunnel)
    }

    fn connect_android_tunnel(tunnel: crate::android::DevToolsTunnel) -> Result<Self> {
        let mut discovery = tunnel
            .open()
            .map_err(|error| format!("Android Chrome HTTP tunnel: {error}"))?;
        let advertised = discover_stream(&mut discovery, "localhost")
            .map_err(|error| format!("Android Chrome HTTP /json/version: {error}"))?;
        drop(discovery);
        let path = android_debugging_path(&advertised)
            .map_err(|error| format!("Android Chrome discovery endpoint: {error}"))?;
        let stream = tunnel
            .open()
            .map_err(|error| format!("Android Chrome WebSocket tunnel: {error}"))?;
        let socket = WebSocket::connect_stream(stream, "localhost", &path)
            .map_err(|error| format!("Android Chrome WebSocket handshake: {error}"))?;
        Self::from_socket(socket, false, true)
            .map_err(|error| format!("Android Chrome session initialization: {error}"))
    }

    fn unattached_socket(socket: WebSocket, mobile: bool) -> Self {
        Self {
            socket,
            child: None,
            profile: None,
            session: String::new(),
            target: String::new(),
            next_id: 1,
            events: VecDeque::new(),
            owned_target: false,
            mobile,
            closed: false,
            close_error: None,
            observation_binding: format!(
                "__zuse_observation_{}_{}",
                std::process::id(),
                PROFILE_ID.fetch_add(1, Ordering::Relaxed)
            ),
            observation_ids: BTreeSet::new(),
        }
    }

    fn from_socket(socket: WebSocket, page_endpoint: bool, mobile: bool) -> Result<Self> {
        let mut browser = Self::unattached_socket(socket, mobile);
        if !page_endpoint {
            let params = if mobile {
                r#"{"url":"about:blank","background":true}"#
            } else {
                r#"{"url":"about:blank"}"#
            };
            let result = browser.request("Target.createTarget", params, false)?;
            let target = field_string(&result, &["targetId"])?;
            browser.target = target.clone();
            browser.owned_target = true;
            browser.attach(&target)?;
        }
        browser.enable_page()?;
        Ok(browser)
    }

    fn enable_page(&mut self) -> Result<()> {
        self.request("Page.enable", "{}", true)?;
        self.request(
            "Page.setLifecycleEventsEnabled",
            r#"{"enabled":true}"#,
            true,
        )?;
        Ok(())
    }

    fn attach_session(&mut self, target: &str) -> Result<String> {
        let result = self.request(
            "Target.attachToTarget",
            &format!(r#"{{"targetId":{},"flatten":true}}"#, quote(target)),
            false,
        )?;
        field_string(&result, &["sessionId"])
    }

    fn attach(&mut self, target: &str) -> Result<()> {
        self.session = self.attach_session(target)?;
        self.target = target.to_owned();
        Ok(())
    }

    fn detach_session(&mut self, session: &str) -> Result<()> {
        self.request(
            "Target.detachFromTarget",
            &format!(r#"{{"sessionId":{}}}"#, quote(session)),
            false,
        )?;
        Ok(())
    }

    fn close_target(&mut self, target: &str) -> Result<()> {
        let response = self.request(
            "Target.closeTarget",
            &format!(r#"{{"targetId":{}}}"#, quote(target)),
            false,
        )?;
        with_json(&response, |root| match root.get("success") {
            Some(value) if value.raw() == "true" => Ok(()),
            Some(value) if value.raw() == "false" => Err(
                "CDP Target.closeTarget returned success=false; the owned tab may remain open"
                    .into(),
            ),
            _ => Err("CDP Target.closeTarget did not confirm closing the owned tab".into()),
        })
    }

    /// List all inspectable targets as CDP JSON.
    pub fn tabs(&mut self) -> Result<String> {
        if self.mobile {
            let response = self.request(
                "Target.getTargetInfo",
                &format!(r#"{{"targetId":{}}}"#, quote(&self.target)),
                false,
            )?;
            if field_string(&response, &["targetInfo", "targetId"])? != self.target {
                return Err("Chrome returned a different target for the owned mobile tab".into());
            }
            return with_json(&response, |root| {
                let info = root
                    .get("targetInfo")
                    .ok_or("missing owned target information")?;
                Ok(format!(
                    r#"{{"targetInfos":[{}],"scope":"owned"}}"#,
                    info.raw()
                ))
            });
        }
        self.request("Target.getTargets", "{}", false)
    }

    /// Select an existing page by its exact target ID. Closing zuse does not
    /// close a selected user-owned target. Re-selecting the current page preserves
    /// its session; a new page is initialized before releasing the current one.
    pub fn select(&mut self, target: &str) -> Result<String> {
        if self.closed {
            return Err("browser connection is closed".into());
        }
        if self.mobile {
            if target != self.target {
                return Err("mobile browser sessions only select their own created tab".into());
            }
            return Ok(format!(
                r#"{{"targetId":{},"scope":"owned"}}"#,
                quote(target)
            ));
        }
        if !target.is_empty() && target == self.target {
            return Ok(format!(r#"{{"targetId":{}}}"#, quote(target)));
        }
        let result = self.request(
            "Target.getTargetInfo",
            &format!(r#"{{"targetId":{}}}"#, quote(target)),
            false,
        )?;
        if field_string(&result, &["targetInfo", "type"])? != "page" {
            return Err("target is not a page".into());
        }
        let candidate = self.attach_session(target)?;
        let previous_session = std::mem::replace(&mut self.session, candidate);
        let prepared = self.enable_page().and_then(|()| {
            if !self.observation_ids.is_empty() {
                let candidate_session =
                    std::mem::replace(&mut self.session, previous_session.clone());
                let cleanup = self.clear_observations();
                self.session = candidate_session;
                cleanup?;
            }
            if self.owned_target {
                // Closing the old owned target also releases its session.
                self.close_target(&self.target.clone())
            } else if !previous_session.is_empty() {
                self.detach_session(&previous_session)
            } else {
                Ok(())
            }
        });
        if let Err(error) = prepared {
            let candidate = std::mem::replace(&mut self.session, previous_session);
            return match self.detach_session(&candidate) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!(
                    "{error}; candidate session cleanup failed: {cleanup}"
                )),
            };
        }
        self.target = target.to_owned();
        self.owned_target = false;
        Ok(format!(r#"{{"targetId":{}}}"#, quote(target)))
    }

    /// Navigate and wait for the resulting loader's load event (30 s limit).
    pub fn navigate(&mut self, url: &str) -> Result<String> {
        if url.len() > 65536 || url.chars().any(|c| c == '\0') {
            return Err("invalid URL".into());
        }
        self.clear_observations()?;
        self.events.clear();
        let result = self.request(
            "Page.navigate",
            &format!(r#"{{"url":{}}}"#, quote(url)),
            true,
        )?;
        let error = optional_string(&result, &["errorText"])?;
        if let Some(error) = error {
            return Err(format!("navigation failed: {error}"));
        }
        if let Some(loader) = optional_string(&result, &["loaderId"])? {
            let deadline = Instant::now() + TIMEOUT;
            loop {
                if let Some(event) = self.events.pop_front() {
                    if lifecycle_loaded(&event, &loader, &self.session)? {
                        break;
                    }
                } else {
                    let event = self.socket.read_text(deadline)?;
                    if lifecycle_loaded(&event, &loader, &self.session)? {
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    return Err("navigation load timed out".into());
                }
            }
        }
        Ok(result)
    }

    /// Evaluate JavaScript in the selected page and return its JSON value.
    pub fn evaluate(&mut self, expression: &str) -> Result<String> {
        if expression.len() > 1 << 20 {
            return Err("expression exceeds 1 MiB".into());
        }
        let result = self.request("Runtime.evaluate", &format!(r#"{{"expression":{},"returnByValue":true,"awaitPromise":true,"userGesture":true,"timeout":30000}}"#, quote(expression)), true)?;
        with_json(&result, |root| {
            if let Some(exception) = root.get("exceptionDetails") {
                return Err(format!("page JavaScript failed: {}", exception.raw()));
            }
            let value = root.get("result").ok_or("missing evaluation result")?;
            Ok(value
                .get("value")
                .map(|v| v.raw().to_owned())
                .unwrap_or_else(|| "null".into()))
        })
    }

    /// A bounded accessible view. Input values are omitted; text and roles are
    /// obtained from Chromium's native accessibility tree.
    pub fn snapshot(&mut self) -> Result<String> {
        let metadata = self.evaluate("({url:location.href,title:document.title})")?;
        let tree = self.request("Accessibility.getFullAXTree", "{}", true)?;
        with_json(&tree, |root| {
            let nodes = root.get("nodes").ok_or("missing accessibility nodes")?;
            let mut output = format!(r#"{{"page":{metadata},"nodes":["#);
            let mut count = 0;
            let mut truncated = false;
            for node in nodes.elements() {
                if node.get("ignored").and_then(Value::boolean) == Some(true) {
                    continue;
                }
                if count == 2048 {
                    truncated = true;
                    break;
                }
                if count > 0 {
                    output.push(',');
                }
                output.push('{');
                let mut first = true;
                for key in [
                    "nodeId",
                    "parentId",
                    "childIds",
                    "role",
                    "name",
                    "description",
                    "backendDOMNodeId",
                ] {
                    if let Some(value) = node.get(key) {
                        if !first {
                            output.push(',');
                        }
                        first = false;
                        output.push_str(&quote(key));
                        output.push(':');
                        output.push_str(value.raw());
                    }
                }
                output.push('}');
                count += 1;
            }
            output.push_str(&format!("],\"truncated\":{truncated}}}"));
            Ok(output)
        })
    }

    /// Read only the selected DOM container. Form values and images are omitted.
    pub fn observe_scope(&mut self, selector: &str) -> Result<String> {
        self.observation_command("observe", "", selector)
    }

    pub(crate) fn start_observation(&mut self, id: &str, selector: &str) -> Result<String> {
        if self.observation_ids.is_empty() {
            self.request("Runtime.enable", "{}", true)?;
            self.request(
                "Runtime.addBinding",
                &format!(r#"{{"name":{}}}"#, quote(&self.observation_binding)),
                true,
            )?;
        }
        let snapshot = self.observation_command("watch", id, selector)?;
        self.observation_ids.insert(id.to_owned());
        Ok(snapshot)
    }

    pub(crate) fn observation_snapshot(&mut self, id: &str) -> Result<String> {
        if !self.observation_ids.contains(id) {
            return Err(
                "observation watch is no longer active after navigation or tab selection".into(),
            );
        }
        self.observation_command("snapshot", id, "")
    }

    pub(crate) fn stop_observation(&mut self, id: &str) -> Result<String> {
        if !self.observation_ids.contains(id) {
            return Ok(r#"{"closed":true}"#.into());
        }
        let result = self.observation_command("close", id, "")?;
        self.observation_ids.remove(id);
        if self.observation_ids.is_empty() {
            self.request(
                "Runtime.removeBinding",
                &format!(r#"{{"name":{}}}"#, quote(&self.observation_binding)),
                true,
            )?;
        }
        Ok(result)
    }

    fn clear_observations(&mut self) -> Result<()> {
        if self.observation_ids.is_empty() {
            return Ok(());
        }
        let ids = self
            .observation_ids
            .iter()
            .map(|id| quote(id))
            .collect::<Vec<_>>()
            .join(",");
        self.evaluate(&format!("(()=>{{const watches=globalThis[Symbol.for('zuse.observation.v1')];for(const id of [{ids}]){{watches?.get(id)?.close();watches?.delete(id)}}return true}})()"))?;
        self.observation_ids.clear();
        self.request(
            "Runtime.removeBinding",
            &format!(r#"{{"name":{}}}"#, quote(&self.observation_binding)),
            true,
        )?;
        Ok(())
    }

    fn clear_attached_observations(&mut self) -> Result<()> {
        if self.observation_ids.is_empty() {
            return Ok(());
        }
        // A failed click-watch cleanup must not be followed by a 30-second
        // Drop retry. Existing tabs remain open even when the peer is stalled.
        let deadline = Instant::now() + Duration::from_millis(500);
        let ids = self
            .observation_ids
            .iter()
            .map(|id| quote(id))
            .collect::<Vec<_>>()
            .join(",");
        let expression = format!(
            "(()=>{{const watches=globalThis[Symbol.for('zuse.observation.v1')];for(const id of [{ids}]){{watches?.get(id)?.close();watches?.delete(id)}}return true}})()"
        );
        let response = self.request_until(
            "Runtime.evaluate",
            &format!(
                r#"{{"expression":{},"returnByValue":true,"awaitPromise":false,"timeout":500}}"#,
                quote(&expression)
            ),
            true,
            deadline,
            None,
        )?;
        with_json(&response, |root| {
            if let Some(error) = root.get("exceptionDetails") {
                return Err(format!(
                    "browser observation cleanup failed: {}",
                    error.raw()
                ));
            }
            Ok(())
        })?;
        self.observation_ids.clear();
        self.request_until(
            "Runtime.removeBinding",
            &format!(r#"{{"name":{}}}"#, quote(&self.observation_binding)),
            true,
            deadline,
            None,
        )?;
        Ok(())
    }

    fn observation_command(&mut self, action: &str, id: &str, selector: &str) -> Result<String> {
        if selector.len() > 4096 || selector.contains('\0') || id.len() > 128 {
            return Err("invalid observation selector or watch ID".into());
        }
        let expression = format!(
            "(()=>{{const action={},id={},selector={},binding={};{} }})()",
            quote(action),
            quote(id),
            quote(selector),
            quote(&self.observation_binding),
            include_str!("browser_observation.js")
        );
        self.evaluate(&expression)
    }

    /// Wait on CDP binding events from the installed MutationObserver. No page
    /// query or screenshot is issued while idle. Cancellation retains partial
    /// WebSocket bytes so the next observation can reuse this browser session.
    pub(crate) fn wait_observation_event(
        &mut self,
        id: &str,
        deadline: Instant,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<bool> {
        self.observation_event(id, deadline, cancellation)
            .map(|event| event.is_some())
    }

    fn observation_event(
        &mut self,
        id: &str,
        deadline: Instant,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<Option<String>> {
        if !self.observation_ids.contains(id) {
            return Err(
                "observation watch is no longer active after navigation or tab selection".into(),
            );
        }
        loop {
            cancellation.check()?;
            let message = if let Some(event) = self.events.pop_front() {
                event
            } else {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                match self
                    .socket
                    .read_text_cancellable(deadline, Some(cancellation))
                {
                    Ok(message) => message,
                    Err(error) if error == "browser response deadline timed out" => {
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                }
            };
            let event = with_json(&message, |root| {
                if !self.session.is_empty()
                    && !root
                        .get("sessionId")
                        .and_then(Value::string)
                        .is_some_and(|value| value.matches(&self.session))
                {
                    return Ok(None);
                }
                if root
                    .get("method")
                    .and_then(Value::string)
                    .is_some_and(|method| method.matches("Runtime.executionContextsCleared"))
                {
                    return Err(
                        "observation scope was replaced by navigation; create a new watch".into(),
                    );
                }
                if !root
                    .get("method")
                    .and_then(Value::string)
                    .is_some_and(|method| method.matches("Runtime.bindingCalled"))
                {
                    return Ok(None);
                }
                let Some(params) = root.get("params") else {
                    return Ok(None);
                };
                if !params
                    .get("name")
                    .and_then(Value::string)
                    .is_some_and(|name| name.matches(&self.observation_binding))
                {
                    return Ok(None);
                }
                let Some(payload) = params.get("payload").and_then(Value::string) else {
                    return Ok(None);
                };
                let payload: String = payload.chars().collect();
                let matches = with_json(&payload, |root| {
                    Ok(root
                        .get("watch")
                        .and_then(Value::string)
                        .is_some_and(|watch| watch.matches(id)))
                })?;
                Ok(if matches { Some(payload) } else { None })
            })?;
            if event.is_some() {
                cancellation.check()?;
                return Ok(event);
            }
        }
    }

    /// Text currently rendered in the page, bounded to 200,000 UTF-16 units.
    pub fn text(&mut self) -> Result<String> {
        self.evaluate("(document.body?.innerText ?? '').slice(0,200000)")
    }

    /// Click the visible center of a CSS-selected element using trusted CDP
    /// mouse input. Occlusion and zero-sized targets fail explicitly.
    pub fn click(&mut self, selector: &str) -> Result<String> {
        let expression = format!(
            r#"(()=>{{const e=document.querySelector({});if(!e)throw Error('selector not found');e.scrollIntoView({{block:'center',inline:'center'}});const r=e.getBoundingClientRect();if(r.width<=0||r.height<=0)throw Error('element has no visible area');const x=Math.max(0,Math.min(innerWidth-1,r.x+r.width/2)),y=Math.max(0,Math.min(innerHeight-1,r.y+r.height/2));const h=document.elementFromPoint(x,y);if(!h||!(h===e||e.contains(h)))throw Error('element is obscured');return{{x,y}}}})()"#,
            quote(selector)
        );
        let point = self.evaluate(&expression)?;
        let (x, y) = with_json(&point, |root| {
            Ok((number(root.get("x"))?, number(root.get("y"))?))
        })?;
        for (kind, button, count) in [
            ("mouseMoved", "none", 0),
            ("mousePressed", "left", 1),
            ("mouseReleased", "left", 1),
        ] {
            self.request("Input.dispatchMouseEvent", &format!(r#"{{"type":"{kind}","x":{x},"y":{y},"button":"{button}","clickCount":{count}}}"#), true)?;
        }
        Ok(point)
    }

    /// Focus and replace the selected input's content through CDP text input.
    pub fn fill(&mut self, selector: &str, text: &str) -> Result<String> {
        let expression = format!(
            r#"(()=>{{const e=document.querySelector({});if(!e)throw Error('selector not found');if(e.disabled||e.readOnly)throw Error('element is not editable');e.focus();if(e instanceof HTMLInputElement||e instanceof HTMLTextAreaElement){{if(e.type==='password')throw Error('password input requires human assistance');e.select();}}else if(e.isContentEditable){{const r=document.createRange();r.selectNodeContents(e);const s=getSelection();s.removeAllRanges();s.addRange(r);}}else throw Error('element is not a text input');return true}})()"#,
            quote(selector)
        );
        self.evaluate(&expression)?;
        self.request(
            "Input.insertText",
            &format!(r#"{{"text":{}}}"#, quote(text)),
            true,
        )
    }

    /// Press a key or chord, e.g. `Enter`, `Control+a`, or `Meta+l`.
    pub fn key(&mut self, chord: &str) -> Result<String> {
        let mut parts = chord.split('+').collect::<Vec<_>>();
        let key = parts.pop().filter(|v| !v.is_empty()).ok_or("missing key")?;
        let mut modifiers = 0;
        for part in parts {
            modifiers |= match part.to_ascii_lowercase().as_str() {
                "alt" => 1,
                "control" | "ctrl" => 2,
                "meta" | "super" | "cmd" => 4,
                "shift" => 8,
                _ => return Err(format!("unknown modifier: {part}")),
            };
        }
        let (code, vk, printable) = key_code(key)?;
        let text = if printable && modifiers & 7 == 0 {
            format!(",\"text\":{}", quote(key))
        } else if key == "Enter" {
            ",\"text\":\"\\r\"".into()
        } else {
            String::new()
        };
        self.request("Input.dispatchKeyEvent", &format!(r#"{{"type":"keyDown","key":{},"code":{},"windowsVirtualKeyCode":{vk},"modifiers":{modifiers}{text}}}"#, quote(key), quote(&code)), true)?;
        self.request("Input.dispatchKeyEvent", &format!(r#"{{"type":"keyUp","key":{},"code":{},"windowsVirtualKeyCode":{vk},"modifiers":{modifiers}}}"#, quote(key), quote(&code)), true)
    }

    /// Save the selected page as a PNG screenshot.
    pub fn screenshot(&mut self, path: &str) -> Result<String> {
        let result = self.request(
            "Page.captureScreenshot",
            r#"{"format":"png","fromSurface":true,"captureBeyondViewport":false}"#,
            true,
        )?;
        let encoded = field_string(&result, &["data"])?;
        let bytes = decode_base64(&encoded)?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("browser returned invalid PNG data".into());
        }
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|e| format!("screenshot output: {e}"))?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        Ok(format!(
            r#"{{"path":{},"mimeType":"image/png","bytes":{}}}"#,
            quote(path),
            bytes.len()
        ))
    }

    /// Set the page viewport in CSS pixels.
    pub fn viewport(&mut self, width: u32, height: u32) -> Result<String> {
        if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
            return Err("viewport dimensions must be 1..8192".into());
        }
        self.request(
            "Emulation.setDeviceMetricsOverride",
            &format!(
                r#"{{"width":{width},"height":{height},"deviceScaleFactor":1,"mobile":false}}"#
            ),
            true,
        )
    }

    /// Close owned resources. A connected user-owned browser is left running.
    pub fn close(&mut self) -> Result<String> {
        if self.closed {
            return match &self.close_error {
                Some(error) => Err(error.clone()),
                None => Ok(r#"{"closed":true}"#.into()),
            };
        }
        let mut error = if self.child.is_none() && !self.owned_target {
            self.clear_attached_observations().err()
        } else {
            self.clear_observations().err()
        };
        if self.child.is_none() && self.owned_target && !self.target.is_empty() {
            let closed = self.close_target(&self.target.clone());
            if let Err(value) = closed {
                error = Some(value);
            }
        }
        if self.child.is_some() {
            // Graceful browser shutdown also closes its renderer processes.
            let _ = self.socket.send(
                1,
                format!(
                    r#"{{"id":{},"method":"Browser.close","params":{{}}}}"#,
                    self.next_id
                )
                .as_bytes(),
            );
        }
        self.closed = true;
        let _ = self.socket.stream.shutdown(std::net::Shutdown::Both);
        if let Some(mut child) = self.child.take() {
            stop_owned_browser(&mut child, Duration::from_secs(2));
        }
        if let Some(profile) = self.profile.take()
            && let Err(value) = fs::remove_dir_all(&profile)
            && value.kind() != std::io::ErrorKind::NotFound
        {
            error = Some(format!(
                "browser stopped, but temporary profile cleanup failed: {value}"
            ));
        }
        match error {
            Some(error) => {
                self.close_error = Some(error.clone());
                Err(error)
            }
            None => Ok(r#"{"closed":true}"#.into()),
        }
    }

    fn request(&mut self, method: &str, params: &str, page: bool) -> Result<String> {
        let deadline = Instant::now() + TIMEOUT;
        self.request_until(method, params, page, deadline, None)
    }

    fn request_until(
        &mut self,
        method: &str,
        params: &str,
        page: bool,
        deadline: Instant,
        cancellation: Option<&crate::observation::Cancellation>,
    ) -> Result<String> {
        if let Some(token) = cancellation {
            token.check()?;
        }
        let id = self.send_request_until(method, params, page, deadline)?;
        self.response_until(id, method, deadline, cancellation)
    }

    fn send_request_until(
        &mut self,
        method: &str,
        params: &str,
        page: bool,
        deadline: Instant,
    ) -> Result<u64> {
        if self.closed {
            return Err("browser connection is closed".into());
        }
        if Instant::now() >= deadline {
            return Err("browser dispatch deadline timed out".into());
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("browser request ID exhausted")?;
        let session = if page && !self.session.is_empty() {
            format!(",\"sessionId\":{}", quote(&self.session))
        } else {
            String::new()
        };
        self.socket
            .send_until(
                1,
                format!(
                    r#"{{"id":{id},"method":{},"params":{params}{session}}}"#,
                    quote(method)
                )
                .as_bytes(),
                deadline.min(Instant::now() + Duration::from_secs(5)),
            )
            .map_err(|error| format!("CDP {method} write: {error}"))?;
        Ok(id)
    }

    fn response_until(
        &mut self,
        id: u64,
        method: &str,
        deadline: Instant,
        cancellation: Option<&crate::observation::Cancellation>,
    ) -> Result<String> {
        for _ in 0..10000 {
            if let Some(token) = cancellation {
                token.check()?;
            }
            // A paired press/release can reply out of order. Also preserve
            // replies left pending by a cancelled read for the next request.
            let mut queued = None;
            for (index, message) in self.events.iter().enumerate() {
                if with_json(message, |root| {
                    Ok(root.get("id").and_then(|v| v.integer().ok()) == Some(id as i64))
                })? {
                    queued = Some(index);
                    break;
                }
            }
            let message = if let Some(index) = queued {
                self.events.remove(index).unwrap()
            } else {
                self.socket
                    .read_text_cancellable(deadline, cancellation)
                    .map_err(|error| {
                        if error.starts_with("cancelled:") {
                            error
                        } else {
                            format!("CDP {method} read: {error}")
                        }
                    })?
            };
            let response = with_json(&message, |root| {
                if root.get("id").and_then(|v| v.integer().ok()) == Some(id as i64) {
                    if let Some(error) = root.get("error") {
                        return Err(format!("CDP {method}: {}", error.raw()));
                    }
                    return Ok(Some(
                        root.get("result")
                            .map(|v| v.raw().to_owned())
                            .unwrap_or_else(|| "{}".into()),
                    ));
                }
                Ok(None)
            })?;
            if let Some(result) = response {
                return Ok(result);
            }
            if self.events.len() == 64 {
                self.events.pop_front();
            }
            self.events.push_back(message);
        }
        Err("browser event limit exceeded".into())
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn lifecycle_loaded(message: &str, loader: &str, session: &str) -> Result<bool> {
    with_json(message, |root| {
        let method = root.get("method").and_then(Value::string);
        if !method.is_some_and(|s| s.matches("Page.lifecycleEvent")) {
            return Ok(false);
        }
        if !session.is_empty()
            && !root
                .get("sessionId")
                .and_then(Value::string)
                .is_some_and(|s| s.matches(session))
        {
            return Ok(false);
        }
        let Some(params) = root.get("params") else {
            return Ok(false);
        };
        Ok(params
            .get("name")
            .and_then(Value::string)
            .is_some_and(|s| s.matches("load"))
            && params
                .get("loaderId")
                .and_then(Value::string)
                .is_some_and(|s| s.matches(loader)))
    })
}

fn browser_executable() -> Result<String> {
    if let Some(path) = std::env::var_os("ZUSE_BROWSER") {
        return path
            .into_string()
            .map_err(|_| "ZUSE_BROWSER is not UTF-8".into());
    }
    #[cfg(target_os = "macos")]
    let candidates = vec![
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_owned(),
        "/Applications/Chromium.app/Contents/MacOS/Chromium".to_owned(),
        "chromium".to_owned(),
    ];
    #[cfg(target_os = "windows")]
    let candidates = {
        let mut candidates = vec![
            "chrome.exe".to_owned(),
            "msedge.exe".to_owned(),
            "chromium.exe".to_owned(),
        ];
        for variable in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(directory) = std::env::var_os(variable) {
                for relative in [
                    "Google/Chrome/Application/chrome.exe",
                    "Microsoft/Edge/Application/msedge.exe",
                    "Chromium/Application/chrome.exe",
                ] {
                    candidates.push(
                        PathBuf::from(&directory)
                            .join(relative)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        candidates
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let candidates = vec![
        "chromium".to_owned(),
        "chromium-browser".to_owned(),
        "google-chrome".to_owned(),
        "google-chrome-stable".to_owned(),
    ];
    for candidate in candidates {
        if Path::new(&candidate).is_file() {
            return Ok(candidate);
        }
        if let Some(paths) = std::env::var_os("PATH")
            && std::env::split_paths(&paths).any(|path| path.join(&candidate).is_file())
        {
            return Ok(candidate);
        }
    }
    Err("Chromium was not found; set ZUSE_BROWSER to a Chrome/Chromium executable".into())
}

fn new_profile() -> Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "zuse-browser-{}-{now}-{}",
        std::process::id(),
        PROFILE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    fs::create_dir(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

pub(crate) fn with_json<T>(input: &str, f: impl FnOnce(Value<'_>) -> Result<T>) -> Result<T> {
    let mut capacity = 128;
    loop {
        let mut tokens = vec![Token::EMPTY; capacity];
        let limits = Limits {
            max_bytes: MAX_MESSAGE,
            max_depth: 64,
            max_members: 65536,
            max_string_bytes: MAX_MESSAGE,
        };
        match Document::parse(input.as_bytes(), &mut tokens, limits) {
            Ok(document) => return f(document.root()),
            Err(error) if error.kind == zmcpio::ErrorKind::Tokens && capacity < 262144 => {
                capacity *= 2
            }
            Err(error) => return Err(format!("browser JSON: {error}")),
        }
    }
}

fn optional_string(input: &str, path: &[&str]) -> Result<Option<String>> {
    with_json(input, |mut value| {
        for key in path {
            match value.get(key) {
                Some(next) => value = next,
                None => return Ok(None),
            }
        }
        Ok(value.string().map(|s| s.chars().collect()))
    })
}
fn field_string(input: &str, path: &[&str]) -> Result<String> {
    optional_string(input, path)?.ok_or_else(|| format!("missing string field: {}", path.join(".")))
}
fn number(value: Option<Value<'_>>) -> Result<f64> {
    let number = value
        .ok_or("missing coordinate")?
        .raw()
        .parse::<f64>()
        .map_err(|_| "invalid coordinate")?;
    if !number.is_finite() {
        return Err("coordinate is not finite".into());
    }
    Ok(number)
}

pub(crate) fn quote(text: &str) -> String {
    let mut output = String::with_capacity(text.len() + 2);
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c <= '\u{1f}' => output.push_str(&format!("\\u{:04x}", c as u32)),
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

fn key_code(key: &str) -> Result<(String, u32, bool)> {
    let code = match key {
        "Enter" => 13,
        "Tab" => 9,
        "Escape" => 27,
        "Backspace" => 8,
        "Delete" => 46,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Home" => 36,
        "End" => 35,
        "PageUp" => 33,
        "PageDown" => 34,
        " " | "Space" => 32,
        _ if key.chars().count() == 1 => {
            let c = key.chars().next().unwrap();
            let code = if c.is_ascii_alphabetic() {
                format!("Key{}", c.to_ascii_uppercase())
            } else if c.is_ascii_digit() {
                format!("Digit{c}")
            } else {
                String::new()
            };
            return Ok((
                code,
                if c.is_ascii() {
                    c.to_ascii_uppercase() as u32
                } else {
                    0
                },
                true,
            ));
        }
        _ => return Err(format!("unsupported browser key: {key}")),
    };
    Ok((
        if key == " " { "Space" } else { key }.into(),
        code,
        key == " ",
    ))
}

struct Endpoint {
    address: SocketAddr,
    host: String,
    path: String,
}
fn endpoint(input: &str, scheme: &str) -> Result<Endpoint> {
    if input.len() > 8192 {
        return Err("browser endpoint exceeds 8 KiB".into());
    }

    let rest = input
        .strip_prefix(scheme)
        .ok_or("expected local ws:// or http:// endpoint")?;
    if rest
        .chars()
        .any(|c| c.is_ascii_control() || c == ' ' || c == '#')
        || rest.contains('@')
    {
        return Err("invalid debugging endpoint".into());
    }
    let (authority, path) = rest
        .split_once('/')
        .map(|(a, p)| (a, format!("/{p}")))
        .unwrap_or((rest, "/".into()));
    let (host, port) = if authority.starts_with('[') {
        let (host, port) = authority
            .split_once("]:")
            .ok_or("endpoint requires an explicit port")?;
        (&host[1..], port)
    } else {
        authority
            .rsplit_once(':')
            .ok_or("endpoint requires an explicit port")?
    };
    let port = port.parse::<u16>().map_err(|_| "invalid endpoint port")?;
    if port == 0 {
        return Err("endpoint port must be nonzero".into());
    }
    let ip = if host.eq_ignore_ascii_case("localhost") {
        IpAddr::from([127, 0, 0, 1])
    } else {
        host.parse::<IpAddr>()
            .map_err(|_| "debugging endpoint must use a literal loopback IP or localhost")?
    };
    if !ip.is_loopback() {
        return Err("browser debugging endpoints must be loopback".into());
    }
    Ok(Endpoint {
        address: SocketAddr::new(ip, port),
        host: authority.to_owned(),
        path,
    })
}

fn android_debugging_path(input: &str) -> Result<String> {
    let rest = input
        .strip_prefix("ws://")
        .ok_or("Android Chrome did not advertise a WebSocket debugging endpoint")?;
    let (authority, path) = rest
        .split_once('/')
        .ok_or("Android Chrome debugging endpoint has no path")?;
    // Chrome Android commonly advertises localhost without an explicit TCP port.
    // Only the verified path is used; both HTTP and WebSocket traffic stays in the
    // same selected device/socket tunnel, with no connection to this authority.
    let authority = if authority.eq_ignore_ascii_case("localhost")
        || authority == "127.0.0.1"
        || authority == "[::1]"
    {
        format!("{authority}:80")
    } else {
        authority.to_owned()
    };
    let parsed = endpoint(&format!("ws://{authority}/{path}"), "ws://")?;
    let valid = parsed.path == "/devtools/browser"
        || parsed
            .path
            .strip_prefix("/devtools/browser/")
            .is_some_and(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            });
    if !valid {
        return Err(
            "Android Chrome discovery did not return a browser-level DevTools endpoint".into(),
        );
    }
    Ok(parsed.path)
}

fn existing_stream(
    address: &SocketAddr,
    cancellation: &crate::observation::Cancellation,
) -> Result<(TcpStream, crate::observation::WakeGuard)> {
    cancellation.check()?;
    // No DNS is used and only loopback addresses reach this helper. Bound the
    // pre-socket connect itself; later blocking I/O has a cancellation waker.
    let stream = TcpStream::connect_timeout(address, Duration::from_millis(500));
    cancellation.check()?;
    let stream = stream.map_err(|error| error.to_string())?;
    let wake = stream.try_clone().map_err(|error| error.to_string())?;
    let registration = cancellation.on_cancel(move || {
        let _ = wake.shutdown(std::net::Shutdown::Both);
    });
    cancellation.check()?;
    Ok((stream, registration))
}

fn discover_endpoint(input: &str) -> Result<String> {
    let endpoint = endpoint(input, "http://")?;
    let mut socket = TcpStream::connect_timeout(&endpoint.address, Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    discover_stream(&mut socket, &endpoint.host)
}

fn discover_stream(socket: &mut TcpStream, host: &str) -> Result<String> {
    const LIMIT: usize = 65_536;
    let deadline = Instant::now() + Duration::from_secs(5);
    let request = format!(
        "GET /json/version HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        host
    );
    write_all_deadline(socket, request.as_bytes(), deadline)?;
    // Read exactly through the header terminator so no bytes beyond a declared
    // body are consumed. ADB tunnels need not reach EOF after a complete reply.
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() == LIMIT {
            return Err("browser discovery exceeds 64 KiB".into());
        }
        let mut byte = [0];
        read_exact_deadline(socket, &mut byte, deadline)?;
        header.push(byte[0]);
    }
    let head = std::str::from_utf8(&header[..header.len() - 4])
        .map_err(|_| "invalid HTTP discovery headers")?;
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    if !status.starts_with("HTTP/1.1 200 ") && !status.starts_with("HTTP/1.0 200 ") {
        return Err("browser discovery HTTP status is not 200".into());
    }
    let mut length = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or("invalid HTTP discovery header field")?;
        if name.is_empty()
            || !name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | 0x60
                            | b'|'
                            | b'~'
                    )
            })
            || value
                .bytes()
                .any(|byte| byte == 0x7f || (byte < 0x20 && byte != b'\t'))
        {
            return Err("invalid HTTP discovery header field".into());
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("unsupported HTTP discovery Transfer-Encoding".into());
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("duplicate HTTP discovery Content-Length".into());
            }
            let value = value.trim_matches(|character| character == ' ' || character == '\t');
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("invalid HTTP discovery Content-Length".into());
            }
            let count = value
                .parse::<usize>()
                .map_err(|_| "invalid HTTP discovery Content-Length")?;
            if count > LIMIT - header.len() {
                return Err("browser discovery exceeds 64 KiB".into());
            }
            length = Some(count);
        }
    }
    let body = if let Some(length) = length {
        let mut body = vec![0; length];
        read_exact_deadline(socket, &mut body, deadline)?;
        body
    } else {
        // A response without length/framing is delimited by EOF, still under
        // the same absolute deadline and total header-plus-body size bound.
        let mut body = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = read_some_deadline(socket, &mut buffer, deadline)?;
            if count == 0 {
                break;
            }
            if body.len() + count > LIMIT - header.len() {
                return Err("browser discovery exceeds 64 KiB".into());
            }
            body.extend_from_slice(&buffer[..count]);
        }
        body
    };
    let body = String::from_utf8(body).map_err(|_| "invalid HTTP discovery response")?;
    field_string(&body, &["webSocketDebuggerUrl"])
}

struct WebSocket {
    stream: TcpStream,
    incoming: Vec<u8>,
    message: Vec<u8>,
    fragmented: bool,
}
impl WebSocket {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            incoming: Vec::new(),
            message: Vec::new(),
            fragmented: false,
        }
    }
    fn connect(url: &str) -> Result<Self> {
        let endpoint = endpoint(url, "ws://")?;
        let stream = TcpStream::connect_timeout(&endpoint.address, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        Self::connect_stream(stream, &endpoint.host, &endpoint.path)
    }

    fn connect_stream(mut stream: TcpStream, host: &str, path: &str) -> Result<Self> {
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        let mut random = [0; 16];
        entropy(&mut random)?;
        let key = encode_base64(&random);
        let deadline = Instant::now() + Duration::from_secs(5);
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            path, host, key
        );
        write_all_deadline(&mut stream, request.as_bytes(), deadline)?;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() == 16384 {
                return Err("WebSocket headers exceed 16 KiB".into());
            }
            let mut byte = [0];
            read_exact_deadline(&mut stream, &mut byte, deadline)?;
            head.push(byte[0]);
        }
        let head = std::str::from_utf8(&head).map_err(|_| "invalid WebSocket handshake")?;
        if !head.starts_with("HTTP/1.1 101 ") {
            return Err(format!(
                "WebSocket upgrade failed: {}",
                head.lines().next().unwrap_or("")
            ));
        }
        let headers = head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .collect::<Vec<_>>();
        let header = |name: &str| -> Option<&str> {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.trim())
        };
        let accept = encode_base64(&sha1(
            format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
        ));
        if header("sec-websocket-accept") != Some(accept.as_str())
            || !header("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            || !header("connection").is_some_and(|v| {
                v.split(',')
                    .any(|p| p.trim().eq_ignore_ascii_case("upgrade"))
            })
        {
            return Err("invalid WebSocket handshake acceptance".into());
        }
        Ok(Self::new(stream))
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        self.send_until(opcode, payload, Instant::now() + Duration::from_secs(5))
    }

    fn send_until(&mut self, opcode: u8, payload: &[u8], deadline: Instant) -> Result<()> {
        if payload.len() > MAX_MESSAGE {
            return Err("WebSocket output exceeds 16 MiB".into());
        }
        let mut header = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => header.push(0x80 | n as u8),
            n if n <= 65535 => {
                header.push(0xfe);
                header.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                header.push(0xff);
                header.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        let mut mask = [0; 4];
        entropy(&mut mask)?;
        header.extend_from_slice(&mask);
        write_all_deadline(&mut self.stream, &header, deadline)?;
        let mut buffer = [0; 8192];
        for (block, bytes) in payload.chunks(buffer.len()).enumerate() {
            for (index, byte) in bytes.iter().enumerate() {
                buffer[index] = byte ^ mask[(block * 8192 + index) % 4];
            }
            write_all_deadline(&mut self.stream, &buffer[..bytes.len()], deadline)?;
        }
        Ok(())
    }

    fn read_text(&mut self, deadline: Instant) -> Result<String> {
        self.read_text_cancellable(deadline, None)
    }

    fn read_text_cancellable(
        &mut self,
        deadline: Instant,
        cancellation: Option<&crate::observation::Cancellation>,
    ) -> Result<String> {
        loop {
            if let Some(cancel) = cancellation {
                cancel.check()?;
            }
            if let Some(message) = self.buffered_text()? {
                return Ok(message);
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|time| !time.is_zero())
                .ok_or("browser response deadline timed out")?;
            // This timeout checks a local cancellation flag, never page state.
            // Incomplete frames remain buffered across cancellation and timeout.
            let timeout = if cancellation.is_some() {
                remaining.min(Duration::from_millis(25))
            } else {
                remaining
            };
            self.stream
                .set_read_timeout(Some(timeout))
                .map_err(|e| e.to_string())?;
            let mut bytes = [0; 8192];
            match self.stream.read(&mut bytes) {
                Ok(0) => return Err("browser closed the WebSocket".into()),
                Ok(count) => self.incoming.extend_from_slice(&bytes[..count]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(format!("browser WebSocket read: {error}")),
            }
        }
    }

    fn buffered_text(&mut self) -> Result<Option<String>> {
        loop {
            if self.incoming.len() < 2 {
                return Ok(None);
            }
            let head = &self.incoming;
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 15;
            if head[0] & 0x70 != 0 || head[1] & 0x80 != 0 {
                return Err("unsupported WebSocket flags or masked server frame".into());
            }
            let marker = head[1] & 0x7f;
            let header = match marker {
                126 => 4,
                127 => 10,
                _ => 2,
            };
            if head.len() < header {
                return Ok(None);
            }
            let length = match marker {
                126 => {
                    let n = u16::from_be_bytes(head[2..4].try_into().map_err(|_| "frame length")?)
                        as u64;
                    if n < 126 {
                        return Err("non-canonical frame length".into());
                    }
                    n
                }
                127 => {
                    let n = u64::from_be_bytes(head[2..10].try_into().map_err(|_| "frame length")?);
                    if n < 65536 || n >> 63 != 0 {
                        return Err("invalid frame length".into());
                    }
                    n
                }
                n => u64::from(n),
            };
            let control = opcode & 8 != 0;
            if length > MAX_MESSAGE as u64
                || (!control && self.message.len().saturating_add(length as usize) > MAX_MESSAGE)
                || (control && (!fin || length > 125))
            {
                return Err("WebSocket frame exceeds limits".into());
            }
            let end = header + length as usize;
            if self.incoming.len() < end {
                return Ok(None);
            }
            let payload = self.incoming[header..end].to_vec();
            self.incoming.drain(..end);
            match opcode {
                8 => return Err("browser closed the WebSocket".into()),
                9 => {
                    self.send(10, &payload)?;
                    continue;
                }
                10 => continue,
                1 if !self.fragmented => {
                    self.fragmented = !fin;
                    self.message.extend_from_slice(&payload);
                }
                0 if self.fragmented => {
                    self.fragmented = !fin;
                    self.message.extend_from_slice(&payload);
                }
                _ => return Err("unexpected WebSocket opcode or continuation".into()),
            }
            if fin {
                return String::from_utf8(std::mem::take(&mut self.message))
                    .map(Some)
                    .map_err(|_| "browser frame is not UTF-8".into());
            }
        }
    }
}

/// Chromium is started in its own Unix process group, so failed launches and
/// shutdown also reap that launch's renderer/helper descendants.
fn stop_owned_browser(child: &mut Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => break,
        }
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: launch configured this child's dedicated process group.
            // A negative PID targets only that owned group, including renderers.
            unsafe {
                kill(-pid, 9);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| "browser I/O deadline exceeded".into())
}
fn read_some_deadline(
    stream: &mut TcpStream,
    output: &mut [u8],
    deadline: Instant,
) -> Result<usize> {
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|e| e.to_string())?;
        match stream.read(output) {
            Ok(count) => return Ok(count),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err("browser I/O deadline exceeded".into());
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}
fn read_exact_deadline(stream: &mut TcpStream, output: &mut [u8], deadline: Instant) -> Result<()> {
    let mut position = 0;
    while position < output.len() {
        let count = read_some_deadline(stream, &mut output[position..], deadline)?;
        if count == 0 {
            return Err("browser connection ended before the frame was complete".into());
        }
        position += count;
    }
    Ok(())
}
fn write_all_deadline(stream: &mut TcpStream, input: &[u8], deadline: Instant) -> Result<()> {
    let mut position = 0;
    while position < input.len() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|e| e.to_string())?;
        match stream.write(&input[position..]) {
            Ok(0) => return Err("browser connection stopped accepting bytes".into()),
            Ok(count) => position += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err("browser I/O deadline exceeded".into());
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn entropy(bytes: &mut [u8]) -> Result<()> {
    #[cfg(unix)]
    {
        fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(bytes))
            .map_err(|e| format!("OS randomness: {e}"))
    }
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                algorithm: *mut core::ffi::c_void,
                buffer: *mut u8,
                length: u32,
                flags: u32,
            ) -> i32;
        }
        // SAFETY: writable buffer spans its declared length; system-preferred RNG uses a null algorithm.
        let status = unsafe {
            BCryptGenRandom(
                core::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                2,
            )
        };
        if status < 0 {
            Err(format!("OS randomness failed: {status}"))
        } else {
            Ok(())
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = bytes;
        Err("OS randomness is unavailable".into())
    }
}

pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        output.push(ALPHABET[(bits >> 18) as usize] as char);
        output.push(ALPHABET[((bits >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((bits >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn decode_base64(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(4) || text.len() > MAX_MESSAGE {
        return Err("invalid base64 length".into());
    }
    let mut output = Vec::with_capacity(text.len() / 4 * 3);
    let value = |byte: u8| -> Result<u8> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err("invalid base64 character".into()),
        }
    };
    for (index, chunk) in text.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let last = (index + 1) * 4 == text.len();
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        let pad_c = chunk[2] == b'=';
        let pad_d = chunk[3] == b'=';
        if (pad_c && !pad_d) || ((pad_c || pad_d) && !last) {
            return Err("invalid base64 padding".into());
        }
        let c = if pad_c { 0 } else { value(chunk[2])? };
        let d = if pad_d { 0 } else { value(chunk[3])? };
        if (pad_c && b & 15 != 0) || (pad_d && !pad_c && c & 3 != 0) {
            return Err("non-canonical base64 padding".into());
        }
        output.push((a << 2) | (b >> 4));
        if !pad_c {
            output.push((b << 4) | (c >> 2));
        }
        if !pad_d {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn sha1(input: &[u8]) -> [u8; 20] {
    let mut bytes = input.to_vec();
    bytes.push(0x80);
    while bytes.len() % 64 != 56 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&((input.len() as u64) * 8).to_be_bytes());
    let mut state = [
        0x67452301u32,
        0xefcdab89,
        0x98badcfe,
        0x10325476,
        0xc3d2e1f0,
    ];
    for chunk in bytes.as_chunks::<64>().0 {
        let mut words = [0u32; 80];
        for (i, bytes) in chunk.as_chunks::<4>().0.iter().enumerate() {
            words[i] = u32::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            words[i] = (words[i - 3] ^ words[i - 8] ^ words[i - 14] ^ words[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, word) in words.into_iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (item, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *item = item.wrapping_add(value);
        }
    }
    let mut output = [0; 20];
    for (chunk, value) in output.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn websocket_accept_vector() {
        assert_eq!(
            encode_base64(&sha1(
                b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            )),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
    #[test]
    fn base64_round_trip_and_padding() {
        for n in 0..128 {
            let bytes = (0..n).map(|v| v as u8).collect::<Vec<_>>();
            assert_eq!(decode_base64(&encode_base64(&bytes)).unwrap(), bytes);
        }
        for text in ["A", "====", "AB==", "AAB=", "AA=A", "AA==AA==", "?AAA"] {
            assert!(decode_base64(text).is_err(), "{text}");
        }
    }
    #[test]
    fn endpoint_is_loopback_only() {
        for good in [
            "ws://127.0.0.1:9222/devtools/browser/a",
            "ws://[::1]:9222/a",
            "ws://localhost:9222/",
        ] {
            assert!(endpoint(good, "ws://").is_ok());
        }
        for bad in [
            "ws://example.com:9222/",
            "ws://127.0.0.1.evil:9222/",
            "ws://user@127.0.0.1:9222/",
            "ws://127.0.0.1:0/",
            "ws://192.168.1.1:9222/",
            "ws://127.0.0.1:9222/\r\nX:a",
        ] {
            assert!(endpoint(bad, "ws://").is_err());
        }
    }
    #[test]
    fn quoted_selector_stays_data() {
        assert_eq!(quote("a\"\\\n中"), "\"a\\\"\\\\\\n中\"");
    }
    #[test]
    fn frames_allow_fragmented_text_and_ping() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(&[1, 2, b'h', b'e', 0x89, 1, b'!', 0x80, 3, b'l', b'l', b'o'])
                .unwrap();
            let mut pong = [0; 7];
            stream.read_exact(&mut pong).unwrap();
            assert_eq!(pong[0], 0x8a);
            assert_eq!(pong[1], 0x81);
            assert_eq!(pong[6] ^ pong[2], b'!');
        });
        let mut websocket = WebSocket::new(TcpStream::connect(address).unwrap());
        assert_eq!(
            websocket
                .read_text(Instant::now() + Duration::from_secs(2))
                .unwrap(),
            "hello"
        );
        server.join().unwrap();
    }
    #[test]
    fn partial_frame_reads_cannot_extend_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&[0x81, 4, b'a']).unwrap();
            for byte in *b"bcd" {
                thread::sleep(Duration::from_millis(40));
                if stream.write_all(&[byte]).is_err() {
                    break;
                }
            }
        });
        let mut websocket = WebSocket::new(TcpStream::connect(address).unwrap());
        assert!(
            websocket
                .read_text(Instant::now() + Duration::from_millis(70))
                .unwrap_err()
                .contains("deadline")
        );
        server.join().unwrap();
    }
    #[test]
    fn cancelled_partial_frame_is_preserved_for_the_next_call() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let cancellation = crate::observation::Cancellation::default();
        let external = cancellation.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&[0x81, 5, b'h']).unwrap();
            ready_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(40));
            external.cancel();
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            stream
                .write_all(&[b'e', b'l', b'l', b'o', 0x81, 3, b'b', b'y', b'e'])
                .unwrap();
        });
        let mut websocket = WebSocket::new(TcpStream::connect(address).unwrap());
        ready_rx.recv().unwrap();
        let error = websocket
            .read_text_cancellable(Instant::now() + Duration::from_secs(2), Some(&cancellation))
            .unwrap_err();
        assert!(error.starts_with("cancelled:"));
        finish_tx.send(()).unwrap();
        assert_eq!(
            websocket
                .read_text(Instant::now() + Duration::from_secs(2))
                .unwrap(),
            "hello"
        );
        assert_eq!(
            websocket
                .read_text(Instant::now() + Duration::from_secs(2))
                .unwrap(),
            "bye"
        );
        server.join().unwrap();
    }

    #[test]
    fn frame_size_is_rejected_before_allocation() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&[0x81, 127]).unwrap();
            stream
                .write_all(&((MAX_MESSAGE + 1) as u64).to_be_bytes())
                .unwrap();
        });
        let mut websocket = WebSocket::new(TcpStream::connect(address).unwrap());
        assert!(
            websocket
                .read_text(Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .contains("limits")
        );
        server.join().unwrap();
    }
}

#[cfg(test)]
#[path = "../tests/support/android_browser.rs"]
mod android_browser_tests;

#[cfg(test)]
#[path = "../tests/support/browser_selection.rs"]
mod browser_selection_tests;
