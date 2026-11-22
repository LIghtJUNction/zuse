// Included in browser::control_keys::tests; all peers and pages are owned fixtures.

fn request(peer: &mut TcpStream, method: &str) -> String {
    let mut header = [0; 2];
    peer.read_exact(&mut header).unwrap();
    assert_eq!(header[0], 0x81);
    assert_ne!(header[1] & 0x80, 0);
    let length = match header[1] & 0x7f {
        126 => { let mut b = [0; 2]; peer.read_exact(&mut b).unwrap(); u16::from_be_bytes(b) as usize }
        127 => { let mut b = [0; 8]; peer.read_exact(&mut b).unwrap(); usize::try_from(u64::from_be_bytes(b)).unwrap() }
        n => n as usize,
    };
    assert!(length <= 1024 * 1024);
    let mut mask = [0; 4];
    peer.read_exact(&mut mask).unwrap();
    let mut bytes = vec![0; length];
    peer.read_exact(&mut bytes).unwrap();
    for (i, byte) in bytes.iter_mut().enumerate() { *byte ^= mask[i % 4]; }
    let message = String::from_utf8(bytes).unwrap();
    assert_eq!(field_string(&message, &["method"]).unwrap(), method, "{message}");
    message
}

fn reply(peer: &mut TcpStream, message: &str, result: &str) {
    response(peer, message, "result", result);
}

fn response(peer: &mut TcpStream, message: &str, field: &str, result: &str) {
    let id = with_json(message, |root| Ok(root.get("id").unwrap().integer().unwrap())).unwrap();
    let text = format!(r#"{{"id":{id},"{field}":{result}}}"#);
    let mut frame = vec![0x81];
    if text.len() < 126 { frame.push(text.len() as u8); }
    else { frame.push(126); frame.extend_from_slice(&(text.len() as u16).to_be_bytes()); }
    frame.extend_from_slice(text.as_bytes());
    peer.write_all(&frame).unwrap();
}

fn fixture(run: impl FnOnce(&mut TcpStream) + Send + 'static) -> (TcpStream, thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    stream.set_nodelay(true).unwrap();
    let worker = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_nodelay(true).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        run(&mut peer);
        let mut byte = [0];
        match peer.read(&mut byte) {
            Ok(0) => {},
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {},
            other => panic!("unexpected request after cleanup: {other:?}"),
        }
    });
    (stream, worker)
}

fn setup(peer: &mut TcpStream, syntax_valid: bool) {
    let enable = request(peer, "Runtime.enable");
    reply(peer, &enable, "{}");
    let frames = request(peer, "Page.getFrameTree");
    reply(peer, &frames, r#"{"frameTree":{"frame":{"id":"frame-control"}}}"#);
    let world = request(peer, "Page.createIsolatedWorld");
    assert!(world.contains(r#""grantUniveralAccess":false"#));
    reply(peer, &world, r#"{"executionContextId":7}"#);
    let install = request(peer, "Runtime.evaluate");
    assert!(install.contains(r#""contextId":7"#));
    assert!(install.contains(r#""throwOnSideEffect":false"#));
    reply(peer, &install, r#"{"result":{"value":true}}"#);
    let compile = request(peer, "Runtime.compileScript");
    assert!(compile.contains(r#""executionContextId":7"#));
    assert!(compile.contains(r#""persistScript":false"#));
    reply(peer, &compile, if syntax_valid { "{}" } else { r#"{"exceptionDetails":{"text":"fixture SyntaxError"}}"# });
    if syntax_valid {
        let function = checked(peer);
        assert!(function.contains(r#""returnByValue":false"#));
        reply(peer, &function, r#"{"result":{"type":"function","objectId":"policy-7"}}"#);
        let bind = request(peer, "Runtime.callFunctionOn");
        assert_eq!(field_string(&bind, &["params", "objectId"]).unwrap(), "policy-7");
        reply(peer, &bind, r#"{"result":{"value":true}}"#);
    }
}

fn checked(peer: &mut TcpStream) -> String {
    let request = request(peer, "Runtime.evaluate");
    assert!(request.contains(r#""contextId":7"#));
    assert!(request.contains(r#""throwOnSideEffect":true"#));
    assert!(request.contains(r#""userGesture":false"#));
    request
}

fn policy(peer: &mut TcpStream) -> String {
    let prepare = request(peer, "Runtime.evaluate");
    assert!(prepare.contains(r#""contextId":7"#));
    assert!(prepare.contains(r#""throwOnSideEffect":false"#));
    assert!(field_string(&prepare, &["params", "expression"]).unwrap().contains("s.state=JSON.parse"));
    reply(peer, &prepare, r#"{"result":{"value":true}}"#);
    checked(peer)
}

fn decide(peer: &mut TcpStream, keys: &str, done: bool, memory: &str) -> String {
    let message = policy(peer);
    reply(peer, &message, &format!(r#"{{"result":{{"value":{{"status":"decision","decision":{{"keys":{keys},"done":{done}}},"memory":{memory}}}}}}}"#));
    message
}

fn key(peer: &mut TcpStream, kind: &str, name: &str) -> String {
    let request = request(peer, "Input.dispatchKeyEvent");
    assert_eq!(field_string(&request, &["params", "type"]).unwrap(), kind);
    assert_eq!(field_string(&request, &["params", "key"]).unwrap(), name);
    assert_eq!(field_string(&request, &["params", "code"]).unwrap(), name);
    request
}

fn cleanup(peer: &mut TcpStream) {
    cleanup_group(peer, true);
}

fn cleanup_group(peer: &mut TcpStream, group: bool) {
    let cleanup = request(peer, "Runtime.evaluate");
    assert!(field_string(&cleanup, &["params", "expression"]).unwrap().starts_with("delete globalThis["));
    assert!(cleanup.contains(r#""contextId":7"#));
    reply(peer, &cleanup, r#"{"result":{"value":true}}"#);
    if group {
        let release = request(peer, "Runtime.releaseObjectGroup");
        reply(peer, &release, "{}");
    }
}

fn run_fixture(stream: TcpStream, timeout: u64, cancellation: &Cancellation) -> Result<String> {
    let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
    let result = browser.control_keys("Up,Down,Left,Right", "({keys:[],done:true})", timeout, None, cancellation);
    assert!(browser.events.is_empty(), "unconsumed CDP reply: {:?}", browser.events);
    browser.close().unwrap();
    result
}

#[test]
fn controller_initial_done_invalid_decision_and_syntax_error_send_no_input() {
    for mode in ["done", "invalid", "syntax"] {
        let (stream, worker) = fixture(move |peer| {
            setup(peer, mode != "syntax");
            if mode == "done" { decide(peer, "[]", true, "{}"); }
            if mode == "invalid" {
                let message = policy(peer);
                reply(peer, &message, r#"{"result":{"value":{"status":"decision","decision":{"keys":["ArrowUp"],"done":false,"extra":true},"memory":{}}}}"#);
            }
            // Any input before this marker cleanup fails the peer assertion.
            cleanup(peer);
        });
        let result = run_fixture(stream, 1000, &Cancellation::default());
        worker.join().unwrap();
        match mode {
            "done" => {
                let receipt = result.unwrap();
                assert!(receipt.contains(r#""keydowns":0"#), "{receipt}");
                assert!(receipt.contains(r#""delivery":"none""#), "{receipt}");
            }
            "invalid" => assert!(result.unwrap_err().contains("exactly {keys,done}")),
            "syntax" => assert!(result.unwrap_err().contains("fixture SyntaxError")),
            _ => unreachable!(),
        }
    }
}

#[test]
fn controller_changes_only_the_difference_and_accepts_empty_chords() {
    let (stream, worker) = fixture(|peer| {
        setup(peer, true);
        decide(peer, r#"["ArrowUp","ArrowRight"]"#, false, r#"{"phase":1}"#);
        let up_down = key(peer, "rawKeyDown", "ArrowUp");
        let right_down = key(peer, "rawKeyDown", "ArrowRight");
        // Both downs arrive before either acknowledgement.
        reply(peer, &right_down, "{}"); reply(peer, &up_down, "{}");
        let same = decide(peer, r#"["ArrowRight","ArrowUp"]"#, false, r#"{"phase":2}"#);
        let expression = field_string(&same, &["params", "expression"]).unwrap();
        assert!(expression.contains(r#""{\"phase\":1}""#));
        // Unchanged chord produces no repeated presses.
        decide(peer, r#"["ArrowUp","ArrowLeft"]"#, false, r#"{"phase":3}"#);
        let right_up = key(peer, "keyUp", "ArrowRight");
        reply(peer, &right_up, "{}");
        let check = checked(peer);
        reply(peer, &check, r#"{"result":{"value":true}}"#);
        let left_down = key(peer, "rawKeyDown", "ArrowLeft");
        reply(peer, &left_down, "{}");
        decide(peer, "[]", false, r#"{"phase":4}"#);
        let up_up = key(peer, "keyUp", "ArrowUp");
        let left_up = key(peer, "keyUp", "ArrowLeft");
        reply(peer, &left_up, "{}"); reply(peer, &up_up, "{}");
        decide(peer, "[]", true, r#"{"phase":5}"#);
        cleanup(peer);
    });
    let receipt = run_fixture(stream, 1000, &Cancellation::default()).unwrap();
    worker.join().unwrap();
    assert!(receipt.contains(r#""keydowns":3,"keyups":3"#), "{receipt}");
    assert!(receipt.contains(r#""ticks":5"#), "{receipt}");
    assert!(receipt.contains(r#""memory":{"phase":5}"#), "{receipt}");
}

#[test]
fn controller_errors_cancel_and_deadline_release_before_pending_observation() {
    for mode in ["exception", "navigation", "invalid", "cancel", "late_done", "late_error", "release_error"] {
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let (stream, worker) = fixture(move |peer| {
            setup(peer, true);
            decide(peer, r#"["ArrowUp","ArrowRight"]"#, false, "{}");
            let down_up = key(peer, "rawKeyDown", "ArrowUp");
            let down_right = key(peer, "rawKeyDown", "ArrowRight");
            reply(peer, &down_up, "{}"); reply(peer, &down_right, "{}");
            let check = policy(peer);
            let waiting = Instant::now();
            match mode {
                "exception" => reply(peer, &check, r#"{"exceptionDetails":{"text":"fixture policy exception"}}"#),
                "navigation" => reply(peer, &check, r#"{"result":{"value":{"status":"stale"}}}"#),
                "invalid" => reply(peer, &check, r#"{"result":{"value":{"status":"decision","decision":{"keys":["ArrowUp","ArrowDown"],"done":false},"memory":{}}}}"#),
                "cancel" => { cancel.cancel(); reply(peer, &check, r#"{"result":{"value":{"status":"decision","decision":{"keys":[],"done":false},"memory":{}}}}"#); },
                "release_error" => reply(peer, &check, r#"{"result":{"value":{"status":"decision","decision":{"keys":[],"done":true},"memory":{}}}}"#),
                "late_done" | "late_error" => {},
                _ => unreachable!(),
            }
            let up_up = key(peer, "keyUp", "ArrowUp");
            let up_right = key(peer, "keyUp", "ArrowRight");
            assert!(waiting.elapsed() < Duration::from_secs(1), "release blocked on policy: {mode}");
            reply(peer, &up_right, "{}");
            if mode == "release_error" { response(peer, &up_up, "error", r#"{"message":"fixture keyup rejected"}"#); }
            else { reply(peer, &up_up, "{}"); }
            if mode == "late_done" {
                reply(peer, &check, r#"{"result":{"value":{"status":"decision","decision":{"keys":[],"done":true},"memory":{}}}}"#);
            } else if mode == "late_error" {
                response(peer, &check, "error", r#"{"message":"fixture late protocol failure"}"#);
            }
            cleanup(peer);
        });
        let result = run_fixture(stream, 250, &cancellation);
        worker.join().unwrap();
        let error = result.unwrap_err();
        let expected = match mode {
            "exception" => "fixture policy exception", "navigation" => "stale_target:",
            "invalid" => "non-opposite", "cancel" => "cancelled:", "late_done" => "timeout:",
            "late_error" => "fixture late protocol failure", "release_error" => "fixture keyup rejected",
            _ => unreachable!(),
        };
        assert!(error.contains(expected), "{mode}: {error}");
    }
}

#[test]
fn controller_never_presses_a_replacement_after_release_wait_exhausts_deadline_or_cancel() {
    for cancelled in [false, true] {
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let (stream, worker) = fixture(move |peer| {
            setup(peer, true);
            decide(peer, r#"["ArrowRight"]"#, false, "{}");
            let down = key(peer, "rawKeyDown", "ArrowRight");
            reply(peer, &down, "{}");
            decide(peer, r#"["ArrowLeft"]"#, false, "{}");
            let up = key(peer, "keyUp", "ArrowRight");
            if cancelled { cancel.cancel(); }
            else { thread::sleep(Duration::from_millis(300)); }
            reply(peer, &up, "{}");
            // No retry of the sent keyup, no guard evaluation, no Left down.
            cleanup(peer);
        });
        let error = run_fixture(stream, 200, &cancellation).unwrap_err();
        worker.join().unwrap();
        if cancelled { assert!(error.starts_with("cancelled:"), "{error}"); }
        else { assert!(error.contains("deadline"), "{error}"); }
    }
}

#[test]
fn controller_rechecks_deadline_after_the_final_dispatch_permit() {
    let (stream, worker) = fixture(|peer| {
        setup(peer, true);
        decide(peer, r#"["ArrowRight"]"#, false, "{}");
        cleanup(peer);
    });
    let permits = std::sync::atomic::AtomicUsize::new(0);
    let cancellation = Cancellation::with_dispatch_guard(move || {
        if permits.fetch_add(1, Ordering::Relaxed) == 1 {
            thread::sleep(Duration::from_millis(160));
        }
        Ok(Box::new(()))
    });
    let error = run_fixture(stream, 150, &cancellation).unwrap_err();
    worker.join().unwrap();
    assert!(error.starts_with("timeout:"), "{error}");
}

#[test]
fn controller_broken_transport_does_not_replay_or_claim_release() {
    let (stream, worker) = fixture(|peer| {
        setup(peer, true);
        decide(peer, r#"["ArrowUp"]"#, false, "{}");
        let down = key(peer, "rawKeyDown", "ArrowUp");
        reply(peer, &down, "{}");
        // The next observation is a core-only prepare, not another input.
        let _prepare = request(peer, "Runtime.evaluate");
        peer.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let error = run_fixture(stream, 500, &Cancellation::default()).unwrap_err();
    worker.join().unwrap();
    assert!(error.contains("release unconfirmed"), "{error}");
    assert!(!error.contains("\"released\":true"), "{error}");
}

#[test]
fn controller_strict_decisions_reject_unlisted_unknown_duplicate_opposite_and_wrong_types() {
    let allowed = allowlist("Up,Down,Left,Right").unwrap();
    for result in [
        r#"{"keys":["Space"],"done":false}"#,
        r#"{"keys":["ArrowUp","Up"],"done":false}"#,
        r#"{"keys":["Left","Right"],"done":false}"#,
        r#"{"keys":["Up","Left","Right"],"done":false}"#,
        r#"{"keys":[1],"done":false}"#,
        r#"{"keys":{},"done":false}"#,
        r#"{"keys":[],"done":1}"#,
        r#"{"keys":[],"done":false,"extra":1}"#,
        r#"{"keys":[],"keys":["Up"],"done":false}"#,
    ] {
        let value = format!(r#"{{"status":"decision","decision":{result},"memory":{{}}}}"#);
        assert!(decision(&value, &allowed).is_err(), "{value}");
    }
    let restricted = r#"{"status":"decision","decision":{"keys":["ArrowRight"],"done":false},"memory":{}}"#;
    assert!(decision(restricted, &[Direction::Up]).unwrap_err().contains("outside"));
    let memory = format!(r#"{{"status":"decision","decision":{{"keys":[],"done":true}},"memory":{{"v":{}}}}}"#, quote(&"x".repeat(4096)));
    assert!(decision(&memory, &allowed).is_err());
}

fn input_fixture() -> String {
    let html = r#"<!doctype html><meta charset="utf-8"><title>Zuse control keys fixture</title>
<output id="position">0</output><canvas id="drawing" width="100" height="40"></canvas><script>
globalThis.pagePrivateSecret='not exposed to isolated policy';
globalThis.fixtureEvents=[];globalThis.fixtureKeys=new Set();
let position=0;const output=document.getElementById('position');
const canvas=document.getElementById('drawing'),ctx=canvas.getContext('2d');
const sprite=document.createElement('canvas');sprite.width=2;sprite.height=2;
sprite.getContext('2d').fillRect(0,0,2,2);
for(const type of ['keydown','keyup'])addEventListener(type,event=>{
  fixtureEvents.push([event.type,event.key,event.isTrusted]);
  if(type==='keydown')fixtureKeys.add(event.key);else fixtureKeys.delete(event.key);
});
function frame(){if(fixtureKeys.has('ArrowRight'))position++;if(fixtureKeys.has('ArrowLeft'))position--;
  output.textContent=String(position);ctx.clearRect(0,0,100,40);ctx.drawImage(sprite,position,10);requestAnimationFrame(frame);}
requestAnimationFrame(frame);
</script>"#;
    format!("data:text/html;base64,{}", encode_base64(html.as_bytes()))
}

#[test]
#[ignore = "requires installed sandboxed Chromium; Zuse-owned read-only capability fixture"]
fn native_controller_side_effect_capabilities() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&input_fixture()).unwrap();
    browser.request("Page.bringToFront", "{}", true).unwrap();
    eprintln!("control_keys capability Chromium: {}", browser.request("Browser.getVersion", "{}", false).unwrap());
    browser.request("Runtime.enable", "{}", true).unwrap();
    let frames = browser.request("Page.getFrameTree", "{}", true).unwrap();
    let frame = field_string(&frames, &["frameTree", "frame", "id"]).unwrap();
    let world = browser.request("Page.createIsolatedWorld", &format!(r#"{{"frameId":{},"worldName":"zuse-control-keys-capabilities","grantUniveralAccess":false}}"#, quote(&frame)), true).unwrap();
    let context = with_json(&world, |root| Ok(root.get("executionContextId").unwrap().integer().unwrap())).unwrap();
    let marker = "__zuse_control_keys_capabilities";
    let install = browser.request("Runtime.evaluate", &format!(r#"{{"expression":{},"contextId":{context},"returnByValue":true,"userGesture":false,"throwOnSideEffect":false,"timeout":100}}"#, quote(&target_install(marker))), true).unwrap();
    assert_eq!(evaluation_value(&install).unwrap(), "true");
    let prepare = format!(r#"(()=>{{const s=globalThis[{}];s.state={{available:true,latest:[{{center:[1,2]}}]}};s.control={{tick:0,sample_age_ms:0}};return true}})()"#, quote(marker));
    let prepared = browser.request("Runtime.evaluate", &format!(r#"{{"expression":{},"contextId":{context},"returnByValue":true,"userGesture":false,"throwOnSideEffect":false,"timeout":100}}"#, quote(&prepare)), true).unwrap();
    assert_eq!(evaluation_value(&prepared).unwrap(), "true");
    // Only this fixed binding is unchecked. Creation and every invocation of
    // the retained policy are checked, matching Controller::run.
    let function = "(function(state,memory,control){'use strict';memory.ticks=(memory.ticks||0)+1;return {keys:[],done:true}})";
    let created = browser.request("Runtime.evaluate", &format!(r#"{{"expression":{},"contextId":{context},"objectGroup":"zuse-capabilities","returnByValue":false,"userGesture":false,"throwOnSideEffect":true,"timeout":50}}"#, quote(function)), true).unwrap();
    let object = field_string(&created, &["result", "objectId"]).unwrap();
    let bind = format!("function(){{globalThis[{}].policy=this;return true}}", quote(marker));
    let bound = browser.request("Runtime.callFunctionOn", &format!(r#"{{"objectId":{},"functionDeclaration":{},"returnByValue":true,"userGesture":false}}"#, quote(&object), quote(&bind)), true).unwrap();
    assert_eq!(evaluation_value(&bound).unwrap(), "true");
    let mut failures = Vec::new();
    let full = policy_expression(marker, "{}");
    let without_guard = full.replace(&target_check(marker), "true");
    for (label, expression, permitted) in [
        ("document.hasFocus", "document.hasFocus()".to_owned(), Some(true)),
        ("document.visibilityState", "document.visibilityState".to_owned(), Some(true)),
        ("activeElement.isConnected", "document.activeElement.isConnected".to_owned(), Some(true)),
        ("location.href", "location.href".to_owned(), Some(true)),
        ("innerWidth/innerHeight", "[innerWidth,innerHeight]".to_owned(), Some(true)),
        ("visualViewport.scale", "visualViewport?visualViewport.scale:1".to_owned(), Some(true)),
        ("isolated missing page global", "typeof pagePrivateSecret".to_owned(), Some(true)),
        // Chromium 144 omits Affects=Nothing on getElementById; newer
        // versions add it. Record this optional capability without requiring
        // a harmless method to pass a conservative, versioned V8 allowlist.
        ("getElementById/textContent", "Number(document.getElementById('position').textContent)".to_owned(), None),
        ("querySelector/textContent", "Number(document.querySelector('#position').textContent)".to_owned(), Some(true)),
        ("retained target guard", target_check(marker), Some(true)),
        ("fresh JSON memory", "(()=>{const memory=JSON.parse('{}');memory.tick=1;return memory})()".to_owned(), Some(true)),
        ("full retained policy wrapper", full, Some(true)),
        ("wrapper without target guard", without_guard, Some(true)),
        ("preexisting control mutation", format!("(globalThis[{}].control.tick=1,true)", quote(marker)), Some(false)),
        ("DOM attribute mutation", "(document.body.dataset.controllerSideEffect='written',true)".to_owned(), Some(false)),
    ] {
        let result = browser.request("Runtime.evaluate", &format!(r#"{{"expression":{},"contextId":{context},"objectGroup":"zuse-capabilities","returnByValue":true,"awaitPromise":false,"userGesture":false,"throwOnSideEffect":true,"timeout":50}}"#, quote(&expression)), true).and_then(|reply| evaluation_value(&reply));
        eprintln!("control_keys capability {label}: {result:?}");
        if permitted.is_some_and(|permitted| result.is_ok() != permitted)
            || (permitted.is_none() && result.as_ref().is_err_and(|error| !error.contains("Possible side-effect in debug-evaluate")))
        { failures.push(label); }
    }
    let cleanup = format!("delete globalThis[{}]", quote(marker));
    browser.request("Runtime.evaluate", &format!(r#"{{"expression":{},"contextId":{context},"returnByValue":true,"timeout":100}}"#, quote(&cleanup)), true).unwrap();
    browser.request("Runtime.releaseObjectGroup", r#"{"objectGroup":"zuse-capabilities"}"#, true).unwrap();
    assert_eq!(browser.evaluate("fixtureEvents.length").unwrap(), "0");
    assert_eq!(browser.evaluate("document.body.dataset.controllerSideEffect===undefined").unwrap(), "true");
    browser.close().unwrap();
    assert!(failures.is_empty(), "side-effect capability failures: {failures:?}");
}

#[test]
#[ignore = "requires installed sandboxed Chromium; private fixture and trusted input only"]
fn native_controller_reads_dom_and_canvas_carries_fresh_memory_and_rejects_side_effects() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&input_fixture()).unwrap();
    browser.request("Page.bringToFront", "{}", true).unwrap();
    let cancellation = Cancellation::default();
    // querySelector is explicitly side-effect-free in older Chromium too;
    // getElementById was not annotated there and is conservatively refused.
    let policy = r#"(()=>{
      if(typeof pagePrivateSecret!=='undefined')throw Error('page world leaked');
      const x=Number(document.querySelector('#position').textContent);
      memory.ticks=(memory.ticks||0)+1;
      if(!memory.phase)memory.phase='right';
      if(memory.phase==='right' && x>=4)memory.phase='left';
      if(memory.phase==='left' && x<=1)return {keys:[],done:true};
      return {keys:[memory.phase==='right'?'ArrowRight':'ArrowLeft'],done:false};
    })()"#;
    let receipt = browser.control_keys("Right,Left", policy, 3000, None, &cancellation).unwrap();
    assert!(receipt.contains(r#""released":true"#), "{receipt}");
    assert_eq!(browser.evaluate("fixtureKeys.size").unwrap(), "0");
    assert_eq!(browser.evaluate("JSON.stringify(fixtureEvents)").unwrap(), quote(r#"[["keydown","ArrowRight",true],["keyup","ArrowRight",true],["keydown","ArrowLeft",true],["keyup","ArrowLeft",true]]"#));
    browser.evaluate("fixtureEvents.length=0").unwrap();
    for invalid in [
        "(document.body.dataset.controllerSideEffect='written',{keys:[],done:true})",
        "(globalThis.controllerSharedSideEffect=1,{keys:[],done:true})",
        "({keys:[],done:true,unknown:1})",
        "(()=>{memory.bad=Infinity;return {keys:[],done:true}})()",
        "(()=>{memory.toJSON=()=>({});return {keys:[],done:true}})()",
        "(()=>{for(;;){};return {keys:[],done:true}})()",
    ] {
        assert!(browser.control_keys("Right", invalid, 1000, None, &cancellation).is_err(), "{invalid}");
        assert_eq!(browser.evaluate("fixtureEvents.length").unwrap(), "0");
    }
    assert_eq!(browser.evaluate("document.body.dataset.controllerSideEffect===undefined").unwrap(), "true");
    let isolated_clean = browser.control_keys("Right", "({keys:[],done:typeof globalThis.controllerSharedSideEffect==='undefined'})", 500, None, &cancellation).unwrap();
    assert!(isolated_clean.contains(r#""pressed":false"#));

    browser.canvas_watch_with_history("#drawing", "fixture", 4000, false, &cancellation).unwrap();
    let canvas_policy = r#"(()=>{
      memory.ticks=(memory.ticks||0)+1;
      if(!state.available)return {keys:[],done:false};
      const last=state.latest[0];
      if(last.age_ms+control.sample_age_ms>250)throw Error('draw is stale');
      if(memory.start===undefined)memory.start=last.center[0];
      return {keys:['ArrowRight'],done:last.center[0]>=memory.start+4};
    })()"#;
    let receipt = browser.control_keys("Right", canvas_policy, 2000, Some("fixture"), &cancellation).unwrap();
    assert!(receipt.contains(r#""keydowns":1,"keyups":1"#), "{receipt}");
    assert_eq!(browser.evaluate("fixtureKeys.size").unwrap(), "0");
    browser.evaluate("fixtureEvents.length=0").unwrap();
    for mutation in ["state.latest[0].matrix[0]=999", "state.latest.push({})", "control.tick=999"] {
        let policy = format!("({mutation},{{keys:[],done:true}})");
        assert!(browser.control_keys("Right", &policy, 1000, Some("fixture"), &cancellation).is_err(), "{mutation}");
        assert_eq!(browser.evaluate("fixtureEvents.length").unwrap(), "0");
    }
    browser.canvas_close("fixture", &cancellation).unwrap();
    browser.close().unwrap();
}
