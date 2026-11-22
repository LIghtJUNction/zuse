//! Coordinate-free, single-submission native scrolling. A successful reply
//! alone is insufficient: the same component must subsequently be showing.
use super::*;

const ATSPI_SCROLL_ANYWHERE: c_int = 6;

fn submitted_error(error: String) -> String {
    format!("{error}; native ScrollTo was already submitted once and was not retried")
}

fn receipt(node: &Node, current: States, submitted: bool) -> String {
    format!(
        r#"{{"operation":"reveal","target":{},"scroll_type":"anywhere","scroll_submitted":{submitted},"native_call_started":{submitted},"completed":true}}"#,
        node.json_with_states(current)
    )
}

pub(super) fn invoke(scan: &Scan, selected: usize, budget: &Budget<'_>) -> Result<String> {
    let node = &scan.matches[selected];
    let object = &scan.objects[node.object_index];
    let scope = &scan.objects[0];
    let api = budget.api;
    let context = Context::new(&scan.scope, &node.path, "pre_dispatch");
    let current = fresh_node(node, object, scope, Operation::Reveal, budget, context)?;
    if current.showing {
        return Ok(receipt(node, current, false));
    }
    let scroll_to = context.read("ScrollToCapability", api.scroll_to.ok_or_else(||
        "unsupported_state: native AT-SPI Component.ScrollTo is unavailable; no scroll was sent".into()
    ))?;
    let interface = context.read("ComponentIface", budget.call(|| {
        let pointer = unsafe { (api.component_iface)(object.ptr) };
        if pointer.is_null() {
            Err("unsupported_state: reveal target has no AT-SPI Component interface; no scroll was sent".into())
        } else {
            api.object(pointer)
        }
    }))?;
    let mut submitted = false;
    let phase = std::cell::Cell::new("permission");
    let result = budget.cancellation.dispatch_prepared(
        || {
            phase.set("prepare");
            let current = fresh_node(
                node,
                object,
                scope,
                Operation::Reveal,
                budget,
                Context {
                    phase: "prepare",
                    ..context
                },
            )?;
            phase.set("dispatch");
            Ok(current)
        },
        |current| {
            // Read RPCs are complete before acquiring the dispatch mutex. The
            // existing synchronous input method remains its only native call.
            budget.check()?;
            if current.showing {
                return Ok(Some(current));
            }
            let mut error = core::ptr::null_mut();
            submitted = true;
            // Public libatspi ABI: gboolean (AtspiComponent*, AtspiScrollType,
            // GError**). The request contains no screen coordinates.
            let success = unsafe { scroll_to(interface.ptr, ATSPI_SCROLL_ANYWHERE, &mut error) };
            api.error(error)?;
            if success == 0 {
                return Err(
                    "unsupported_state: native Component.ScrollTo was unsupported or rejected"
                        .into(),
                );
            }
            Ok(None)
        },
    );
    match result {
        Ok(Some(current)) => return Ok(receipt(node, current, false)),
        Ok(None) => (),
        Err(error) if !submitted && phase.get() == "prepare" => return Err(error),
        Err(error) => {
            let phase = if submitted {
                "native_input"
            } else {
                phase.get()
            };
            let error = if submitted {
                submitted_error(error)
            } else {
                error
            };
            return Err(Context { phase, ..context }.error("ScrollTo", error, submitted));
        }
    }
    let verify = Context {
        phase: "verify",
        native_call_started: submitted,
        ..context
    };
    loop {
        // Never call ScrollTo again, including when its reply succeeded but
        // the toolkit applies scrolling asynchronously or cancels it later.
        let result = (|| {
            verify.read("Budget", budget.check())?;
            let current = fresh_node(node, object, scope, Operation::Reveal, budget, verify)?;
            if current.showing {
                return Ok(Some(current));
            }
            verify.read(
                "Sleep",
                budget.cancellation.sleep(
                    Duration::from_millis(50)
                        .min(budget.deadline.saturating_duration_since(Instant::now())),
                ),
            )?;
            Ok(None)
        })();
        match result {
            Ok(Some(current)) => return Ok(receipt(node, current, submitted)),
            Ok(None) => (),
            Err(error) => {
                return Err(if submitted {
                    submitted_error(error)
                } else {
                    error
                });
            }
        }
    }
}
