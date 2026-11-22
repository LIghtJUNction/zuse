//! PID-first discovery of directly exported application children. No name,
//! text, state or child count is requested from an unrelated application.
use super::*;

struct Window {
    object: Object<'static>,
    path: String,
    role: String,
    name: String,
    showing: bool,
}

fn pid(object: &Object<'_>, budget: &Budget<'_>, context: Context<'_>) -> Result<u32> {
    context.read(
        "ProcessId",
        budget.call(|| {
            let mut error = core::ptr::null_mut();
            // libatspi may perform GetConnectionUnixProcessID using D-Bus's own
            // default timeout on the first call. Its PID is then cached per app;
            // atspi_set_timeout cannot make this particular first lookup bounded.
            let value = unsafe { (budget.api.process_id)(object.ptr, &mut error) };
            budget.api.error(error)?;
            if value == 0 {
                return Err("application process identity is unavailable".into());
            }
            Ok(value)
        }),
    )
}

fn note(
    diagnostics: &mut Vec<String>,
    errors: &mut usize,
    path: &str,
    error: String,
) -> Result<()> {
    if error.starts_with("cancelled:") || error.starts_with("timeout:") {
        return Err(error);
    }
    *errors += 1;
    if diagnostics.len() < 32 {
        diagnostics.push(quote(&format!(
            "phase=windows path={path}: {}",
            error.chars().take(1024).collect::<String>()
        )));
    }
    Ok(())
}

pub(super) fn run(expected_pid: u32, cancellation: &Cancellation) -> Result<String> {
    let context = Context::new("0", "0", "windows");
    context.read("Cancellation", cancellation.check())?;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(5);
    let _access = context.read("Access", access(deadline, cancellation))?;
    let startup = deadline.saturating_duration_since(Instant::now());
    if startup.is_zero() {
        return Err(context.error(
            "Initialize",
            "timeout: accessibility window discovery initialization expired".into(),
            false,
        ));
    }
    let api = context.read(
        "Initialize",
        load_bounded(startup.as_millis().clamp(1, 250) as c_int),
    )?;
    let budget = Budget {
        api,
        deadline,
        cancellation,
    };
    let desktop = context.read(
        "Desktop",
        budget.call(|| api.object(unsafe { (api.desktop)(0) })),
    )?;
    context.read("Budget", budget.check())?;
    unsafe { (api.clear_cache)(desktop.ptr) };
    let count = context.read("ChildCount", budget.call(|| api.count(desktop.ptr)))?;
    let mut truncated = count > MAX_NODES;
    let mut visited = 0;
    let mut applications = 0;
    let mut errors = 0;
    let mut diagnostics = Vec::new();
    let mut windows = Vec::new();
    let mut bytes = 0;
    let mut nodes = 0;
    for index in 0..count.min(MAX_NODES) {
        context.read("Budget", budget.check())?;
        if nodes == MAX_NODES {
            truncated = true;
            break;
        }
        nodes += 1;
        let app_path = format!("0/{index}");
        let app = match context.read(
            &format!("ChildAtIndex({index})"),
            budget.call(|| api.child(desktop.ptr, index)),
        ) {
            Ok(app) => app,
            Err(error) => {
                note(&mut diagnostics, &mut errors, &app_path, error)?;
                continue;
            }
        };
        visited += 1;
        let app_context = Context {
            path: &app_path,
            ..context
        };
        match pid(&app, &budget, app_context) {
            Ok(value) if value == expected_pid => (),
            Ok(_) => continue,
            Err(error) => {
                note(&mut diagnostics, &mut errors, &app_path, error)?;
                continue;
            }
        }
        applications += 1;
        unsafe { (api.clear_cache)(app.ptr) };
        let count = match app_context.read("ChildCount", budget.call(|| api.count(app.ptr))) {
            Ok(count) => count,
            Err(error) => {
                note(&mut diagnostics, &mut errors, &app_path, error)?;
                continue;
            }
        };
        let capacity = MAX_NODES.saturating_sub(nodes);
        truncated |= count > capacity;
        for child in 0..count.min(capacity) {
            app_context.read("Budget", budget.check())?;
            nodes += 1;
            let path = format!("{app_path}/{child}");
            let observed = (|| {
                let object = app_context.read(
                    &format!("ChildAtIndex({child})"),
                    budget.call(|| api.child(app.ptr, child)),
                )?;
                let window_context = Context {
                    path: &path,
                    ..context
                };
                unsafe { (api.clear_cache)(object.ptr) };
                if pid(&object, &budget, window_context)? != expected_pid {
                    return Err(window_context.error(
                        "ProcessId",
                        "stale_target: direct window process no longer matches expected_pid".into(),
                        false,
                    ));
                }
                let role = window_context.read(
                    "Role",
                    budget.call(|| api.identity_string(object.ptr, api.role, 4096)),
                )?;
                let name = window_context.read(
                    "Name",
                    budget.call(|| api.identity_string(object.ptr, api.name, 65536)),
                )?;
                let current = window_context.read("StateSet", states(&object, &budget))?;
                if current.defunct {
                    return Err(window_context.error(
                        "StateSet",
                        "stale_target: direct window is defunct".into(),
                        false,
                    ));
                }
                Ok(Window {
                    object,
                    path: path.clone(),
                    role,
                    name,
                    showing: current.showing,
                })
            })();
            let window = match observed {
                Ok(window) => window,
                Err(error) => {
                    note(&mut diagnostics, &mut errors, &path, error)?;
                    continue;
                }
            };
            // Hold every observed native reference through final path checks.
            // Reserve space for bounded diagnostics and receipt metadata.
            bytes +=
                window.path.len() * 2 + quote(&window.role).len() + quote(&window.name).len() + 256;
            if bytes > MAX_BYTES - 262144 {
                truncated = true;
                break;
            }
            windows.push(window);
        }
    }
    let mut output = Vec::new();
    for window in windows {
        let current = match resolve(&window.path, &budget) {
            Ok(current) => current,
            Err(error) => {
                note(&mut diagnostics, &mut errors, &window.path, error)?;
                continue;
            }
        };
        if current.ptr != window.object.ptr {
            note(
                &mut diagnostics,
                &mut errors,
                &window.path,
                Context {
                    path: &window.path,
                    ..context
                }
                .error(
                    "WindowPointer",
                    "stale_target: window path changed during discovery".into(),
                    false,
                ),
            )?;
            continue;
        }
        output.push(format!(
            r#"{{"scope":{},"path":{},"pid":{expected_pid},"role":{},"name":{},"showing":{}}}"#,
            quote(&window.path),
            quote(&window.path),
            quote(&window.role),
            quote(&window.name),
            window.showing
        ));
    }
    context.read("Budget", budget.check())?;
    Ok(format!(
        r#"{{"backend":"atspi","expected_pid":{expected_pid},"windows":[{}],"count":{},"applications":{applications},"visited_applications":{visited},"truncated":{truncated},"errors":{errors},"diagnostics":[{}],"elapsed_ms":{}}}"#,
        output.join(","),
        output.len(),
        diagnostics.join(","),
        start.elapsed().as_millis()
    ))
}
