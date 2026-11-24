use super::plan::{Action, CompiledScript, MouseId, Node, OwnedTarget};
use crate::{
    accessibility::ElementAction,
    cursor::CursorImage,
    json,
    motion::Point,
    mouse::{MotionReceipt, Mouse, MouseOptions},
    pointer::MovePlan,
    runtime::Options,
    target::{self, LocatedTarget},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

trait Host {
    fn create(&mut self, id: MouseId, options: &MouseOptions) -> Result<Option<i32>, String>;
    fn close(&mut self, id: MouseId) -> Result<(), String>;
    fn position(&self, id: MouseId) -> Result<Point, String>;
    fn submit(&mut self, id: MouseId, point: Point) -> Result<(), String>;
    fn button(&mut self, id: MouseId, button: &str, action: &str) -> Result<(), String>;
    fn scroll(&mut self, id: MouseId, dx: i32, dy: i32) -> Result<(), String>;
    fn appearance(&mut self, id: MouseId, image: &CursorImage) -> Result<(), String>;
    fn activate(
        &mut self,
        _id: MouseId,
        _target: &ElementAction,
        _cancel: &mut dyn FnMut() -> bool,
    ) -> Result<String, String> {
        Err("semantic action is unavailable on this host".into())
    }
    fn locate(&mut self, query: &OwnedTarget) -> Result<LocatedTarget, String> {
        target::locate(&query.query())
    }
}

#[derive(Default)]
struct NativeHost(BTreeMap<MouseId, Mouse>);
impl NativeHost {
    fn mouse(&mut self, id: MouseId) -> Result<&mut Mouse, String> {
        self.0
            .get_mut(&id)
            .ok_or_else(|| format!("mouse {id} is out of scope"))
    }
}
impl Host for NativeHost {
    fn create(&mut self, id: MouseId, options: &MouseOptions) -> Result<Option<i32>, String> {
        if self.0.contains_key(&id) {
            return Err("duplicate live mouse identifier".into());
        }
        let mouse = Mouse::create(options)?;
        let device = mouse.device_id();
        self.0.insert(id, mouse);
        Ok(device)
    }
    fn close(&mut self, id: MouseId) -> Result<(), String> {
        if let Some(mut mouse) = self.0.remove(&id) {
            mouse.close()
        } else {
            Ok(())
        }
    }
    fn position(&self, id: MouseId) -> Result<Point, String> {
        self.0.get(&id).ok_or("mouse is out of scope")?.position()
    }
    fn submit(&mut self, id: MouseId, p: Point) -> Result<(), String> {
        self.mouse(id)?.submit(p)
    }
    fn button(&mut self, id: MouseId, b: &str, a: &str) -> Result<(), String> {
        self.mouse(id)?.button(b, a)
    }
    fn scroll(&mut self, id: MouseId, x: i32, y: i32) -> Result<(), String> {
        self.mouse(id)?.scroll(x, y)
    }
    fn appearance(&mut self, id: MouseId, image: &CursorImage) -> Result<(), String> {
        self.mouse(id)?.set_cursor(image)
    }
    fn activate(
        &mut self,
        id: MouseId,
        target: &ElementAction,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<String, String> {
        self.mouse(id)?.activate_checked(target, None, || {
            if cancel() {
                Err("script cancelled".into())
            } else {
                Ok(())
            }
        })
    }
}

struct Context<H> {
    host: H,
    live: BTreeSet<MouseId>,
    events: Vec<String>,
    event_bytes: usize,
    event_tokens: usize,
    events_omitted: usize,
    cleanup_attempted: usize,
    cleanup_failed: usize,
    dynamic_remaining: u64,
    started: Instant,
}
impl<H: Host> Context<H> {
    fn event(&mut self, id: MouseId, kind: &str, detail: &str) -> Result<(), String> {
        let event = format!(
            "{{\"mouse\":{id},\"event\":{},\"at_ms\":{},\"detail\":{detail}}}",
            json::quote(kind),
            self.started.elapsed().as_millis()
        );
        self.record(event)
    }
    fn global_event(&mut self, kind: &str, detail: &str) -> Result<(), String> {
        self.record(format!(
            "{{\"mouse\":null,\"event\":{},\"at_ms\":{},\"detail\":{detail}}}",
            json::quote(kind),
            self.started.elapsed().as_millis()
        ))
    }
    fn record(&mut self, event: String) -> Result<(), String> {
        // Preserve an explicit, bounded prefix without turning successful input
        // into a failure because its observation log no longer fits.
        if self.events_omitted != 0 || self.event_bytes.saturating_add(event.len() + 1) > 768 * 1024
        {
            self.events_omitted += 1;
            return Ok(());
        }
        let tokens = json::token_count(&event)?;
        // Reserve tokens for the plan, status/error, cleanup summary and the
        // MCP envelope, which adds fields around this same structured value.
        if self.event_tokens.saturating_add(tokens) > json::MAX_TOKENS - 256 {
            self.events_omitted += 1;
            return Ok(());
        }
        self.event_bytes += event.len() + 1;
        self.event_tokens += tokens;
        self.events.push(event);
        Ok(())
    }
    fn close(&mut self, id: MouseId) -> Result<(), String> {
        if self.live.remove(&id) {
            self.cleanup_attempted += 1;
            let result = self.host.close(id);
            self.cleanup_failed += usize::from(result.is_err());
            let recorded = self.event(
                id,
                "close",
                if result.is_ok() {
                    "{\"released\":true}"
                } else {
                    "{\"released\":false}"
                },
            );
            result.and(recorded)
        } else {
            Ok(())
        }
    }
    fn cleanup(&mut self) -> Result<(), String> {
        let ids = self.live.iter().copied().rev().collect::<Vec<_>>();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(e) = self.close(id) {
                errors.push(format!("mouse {id}: {e}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

enum Task<'a> {
    Ready(&'a Node),
    Scope {
        nodes: &'a [Node],
        next: usize,
        child: Option<Box<Task<'a>>>,
        owned: &'a [MouseId],
    },
    Parallel {
        branches: Vec<Task<'a>>,
        next: usize,
    },
    Motion {
        id: MouseId,
        plan: MovePlan,
        target: Option<LocatedTarget>,
        index: usize,
        started: Instant,
    },
    Sleep(Instant),
    Clicks {
        id: MouseId,
        button: &'a str,
        left: u32,
        interval: u32,
        next: Instant,
    },
    Done,
}

impl<'a> Task<'a> {
    /// One poll emits at most one motion sample per branch, allowing devices
    /// to share a single monotonic scheduler without sharing Xlib threads.
    fn poll<H: Host>(
        &mut self,
        ctx: &mut Context<H>,
        work: &mut usize,
        cancel: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Instant>, String> {
        loop {
            if cancel() {
                return Err("script cancelled".into());
            }
            if *work == 0 {
                return Ok(Some(Instant::now()));
            }
            // Walking an already-active scope/parallel ancestor is not work.
            // Charging it on every poll can exhaust the quantum before a
            // valid deeply nested leaf is reached, so that leaf never runs.
            match self {
                Self::Ready(node) => {
                    *work -= 1;
                    match node {
                        Node::Scope { body, owned } => {
                            *self = Self::Scope {
                                nodes: body,
                                next: 0,
                                child: None,
                                owned,
                            }
                        }
                        Node::Parallel(nodes) => {
                            *self = Self::Parallel {
                                branches: nodes.iter().map(Self::Ready).collect(),
                                next: 0,
                            }
                        }
                        Node::Create { id, options } => {
                            if ctx.live.len() >= 32 {
                                return Err("script exceeds 32 simultaneously live mice".into());
                            }
                            let device = ctx.host.create(*id, options)?;
                            ctx.live.insert(*id);
                            let detail = format!(
                                "{{\"mode\":{},\"native_device_id\":{}}}",
                                json::quote(options.mode.name()),
                                device
                                    .map(|x| x.to_string())
                                    .unwrap_or_else(|| "null".into())
                            );
                            ctx.event(*id, "create", &detail)?;
                            *self = Self::Done;
                        }
                        Node::Wait(ms) => {
                            *self =
                                Self::Sleep(Instant::now() + Duration::from_millis(u64::from(*ms)))
                        }
                        Node::Touchpad(command) => {
                            use crate::touchpad::Command;
                            match command {
                                Command::Capabilities => ctx.global_event(
                                    "touchpad.capabilities",
                                    &crate::touchpad::capabilities_json(),
                                )?,
                                Command::Gesture(plan) => crate::touchpad::execute(plan)?,
                                Command::Cancel => crate::touchpad::cancel()?,
                            }
                            *self = Self::Done;
                        }
                        Node::Action { id, action } => match action {
                            Action::Move { to, options } => {
                                let plan = MovePlan::new(ctx.host.position(*id)?, *to, options)?;
                                if options.speed.is_some() {
                                    charge(ctx, plan.duration_ms())?;
                                }
                                *self = Self::Motion {
                                    id: *id,
                                    plan,
                                    target: None,
                                    index: 0,
                                    started: Instant::now(),
                                };
                            }
                            Action::MoveTo { query, options } => {
                                let target = ctx.host.locate(query)?;
                                let to = Point {
                                    x: target.bounds.x + target.bounds.width / 2.,
                                    y: target.bounds.y + target.bounds.height / 2.,
                                };
                                let plan = MovePlan::new(ctx.host.position(*id)?, to, options)?;
                                if options.speed.is_some() {
                                    charge(ctx, plan.duration_ms())?;
                                }
                                *self = Self::Motion {
                                    id: *id,
                                    plan,
                                    target: Some(target),
                                    index: 0,
                                    started: Instant::now(),
                                };
                            }
                            Action::Click {
                                button,
                                count,
                                interval,
                            } => {
                                *self = Self::Clicks {
                                    id: *id,
                                    button,
                                    left: *count,
                                    interval: *interval,
                                    next: Instant::now(),
                                }
                            }
                            Action::Button { button, down } => {
                                ctx.host
                                    .button(*id, button, if *down { "down" } else { "up" })?;
                                ctx.event(
                                    *id,
                                    if *down { "down" } else { "up" },
                                    &format!("{{\"button\":{}}}", json::quote(button)),
                                )?;
                                *self = Self::Done;
                            }
                            Action::Scroll { dx, dy } => {
                                ctx.host.scroll(*id, *dx, *dy)?;
                                ctx.event(*id, "scroll", &format!("{{\"x\":{dx},\"y\":{dy}}}"))?;
                                *self = Self::Done;
                            }
                            Action::Activate(target) => {
                                let result = ctx.host.activate(*id, target, cancel)?;
                                ctx.event(*id, "activate", &result)?;
                                *self = Self::Done;
                            }
                            Action::Appearance(image) => {
                                ctx.host.appearance(*id, image)?;
                                ctx.event(
                                    *id,
                                    "appearance",
                                    &format!(
                                        "{{\"width\":{},\"height\":{}}}",
                                        image.width, image.height
                                    ),
                                )?;
                                *self = Self::Done;
                            }
                            Action::Position => {
                                let p = ctx.host.position(*id)?;
                                ctx.event(
                                    *id,
                                    "position",
                                    &format!("{{\"x\":{},\"y\":{}}}", p.x, p.y),
                                )?;
                                *self = Self::Done;
                            }
                        },
                    }
                }
                Self::Scope {
                    nodes,
                    next,
                    child,
                    owned,
                } => {
                    if let Some(active) = child {
                        if let Some(deadline) = active.poll(ctx, work, cancel)? {
                            return Ok(Some(deadline));
                        }
                        *child = None;
                    }
                    if *work == 0 {
                        return Ok(Some(Instant::now()));
                    }
                    *work -= 1;
                    if *next < nodes.len() {
                        *child = Some(Box::new(Self::Ready(&nodes[*next])));
                        *next += 1;
                    } else {
                        let mut errors = Vec::new();
                        for id in owned.iter().rev() {
                            if let Err(e) = ctx.close(*id) {
                                errors.push(e);
                            }
                        }
                        if !errors.is_empty() {
                            return Err(errors.join("; "));
                        }
                        *self = Self::Done;
                    }
                }
                Self::Parallel { branches, next } => {
                    let mut earliest = None;
                    let count = branches.len();
                    let start = *next;
                    for offset in 0..count {
                        if *work == 0 {
                            return Ok(Some(Instant::now()));
                        }
                        let index = (start + offset) % count;
                        *next = (index + 1) % count;
                        if let Some(deadline) = branches[index].poll(ctx, work, cancel)? {
                            earliest =
                                Some(earliest.map_or(deadline, |old: Instant| old.min(deadline)));
                        }
                    }
                    if let Some(deadline) = earliest {
                        return Ok(Some(deadline));
                    }
                    *work -= 1;
                    *self = Self::Done;
                }
                Self::Motion {
                    id,
                    plan,
                    target,
                    index,
                    started,
                } => {
                    let sample = plan
                        .samples()
                        .get(*index)
                        .ok_or("invalid motion schedule")?;
                    let deadline = *started + Duration::from_millis(u64::from(sample.at_ms));
                    if Instant::now() < deadline {
                        return Ok(Some(deadline));
                    }
                    *work -= 1;
                    ctx.host.submit(*id, sample.point).map_err(|e| {
                        format!("mouse {id} movement stopped after {index} samples: {e}")
                    })?;
                    *index += 1;
                    if *index == plan.samples().len() {
                        let receipt = MotionReceipt::new(
                            u64::from(*id),
                            plan,
                            target.take(),
                            started.elapsed().as_millis(),
                        );
                        ctx.event(*id, "move", &receipt.to_json())?;
                        *self = Self::Done;
                    } else {
                        return Ok(Some(
                            *started
                                + Duration::from_millis(u64::from(plan.samples()[*index].at_ms)),
                        ));
                    }
                }
                Self::Sleep(deadline) => {
                    if Instant::now() < *deadline {
                        return Ok(Some(*deadline));
                    }
                    *work -= 1;
                    *self = Self::Done;
                }
                Self::Clicks {
                    id,
                    button,
                    left,
                    interval,
                    next,
                } => {
                    if Instant::now() < *next {
                        return Ok(Some(*next));
                    }
                    *work -= 1;
                    if *left > 0 {
                        ctx.host.button(*id, button, "click")?;
                        ctx.event(
                            *id,
                            "click",
                            &format!("{{\"button\":{}}}", json::quote(button)),
                        )?;
                        *left -= 1;
                    }
                    if *left == 0 {
                        *self = Self::Done;
                    } else {
                        *next += Duration::from_millis(u64::from(*interval));
                        return Ok(Some(*next));
                    }
                }
                Self::Done => return Ok(None),
            }
        }
    }
}

fn charge<H>(ctx: &mut Context<H>, ms: u32) -> Result<(), String> {
    ctx.dynamic_remaining = ctx
        .dynamic_remaining
        .checked_sub(u64::from(ms))
        .ok_or("speed-based movement exceeds remaining max-wait-ms budget")?;
    Ok(())
}

pub(super) fn execute<F: FnMut() -> bool>(
    plan: &CompiledScript,
    options: Options,
    cancel: F,
) -> Result<String, String> {
    execute_on(NativeHost::default(), plan, options, cancel).map(|(receipt, _)| receipt)
}

fn execute_on<H: Host, F: FnMut() -> bool>(
    host: H,
    plan: &CompiledScript,
    options: Options,
    mut cancel: F,
) -> Result<(String, H), String> {
    let mut ctx = Context {
        host,
        live: BTreeSet::new(),
        events: Vec::new(),
        event_bytes: 0,
        event_tokens: 0,
        events_omitted: 0,
        cleanup_attempted: 0,
        cleanup_failed: 0,
        dynamic_remaining: options
            .max_wait_ms
            .checked_sub(plan.summary.static_wait_ms)
            .ok_or("script exceeds max-wait-ms budget")?,
        started: Instant::now(),
    };
    let mut task = Task::Ready(&plan.root);
    let result = (|| -> Result<(), String> {
        loop {
            if cancel() {
                return Err("script cancelled".into());
            }
            let Some(deadline) = task.poll(&mut ctx, &mut 64, &mut cancel)? else {
                return Ok(());
            };
            let delay = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10));
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
    })();
    let cleanup = ctx.cleanup();
    let error = match (result, cleanup) {
        (Ok(()), Ok(())) => None,
        (Err(e), Ok(())) => Some(e),
        (Ok(()), Err(e)) => Some(format!("mouse cleanup failed: {e}")),
        (Err(e), Err(cleanup)) => Some(format!("{e}; mouse cleanup failed: {cleanup}")),
    };
    let receipt = format!(
        "{{\"status\":{},\"language\":\"zuse script\",\"plan\":{},\"elapsed_ms\":{},\"events\":[{}],\"events_omitted\":{},\"cleanup\":{{\"attempted\":{},\"failed\":{}}},\"error\":{}}}",
        json::quote(if error.is_some() {
            "failed"
        } else {
            "completed"
        }),
        plan.summary.to_json(),
        ctx.started.elapsed().as_millis(),
        ctx.events.join(","),
        ctx.events_omitted,
        ctx.cleanup_attempted,
        ctx.cleanup_failed,
        error
            .as_deref()
            .map(json::quote)
            .unwrap_or_else(|| "null".into())
    );
    Ok((receipt, ctx.host))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct MockHost {
        points: BTreeMap<MouseId, Point>,
        calls: Vec<(String, MouseId)>,
        submitted: Vec<(MouseId, Point)>,
        fail_move: bool,
        fail_close: bool,
        cancel_after_sample: Option<super::super::CancellationToken>,
        cancel_on_locate: Option<super::super::CancellationToken>,
        observed_name: Option<String>,
    }
    impl Host for MockHost {
        fn create(&mut self, id: MouseId, o: &MouseOptions) -> Result<Option<i32>, String> {
            self.points.insert(id, o.start.unwrap_or_default());
            self.calls.push(("create".into(), id));
            Ok(Some(id as i32))
        }
        fn close(&mut self, id: MouseId) -> Result<(), String> {
            self.points.remove(&id);
            self.calls.push(("close".into(), id));
            if self.fail_close {
                Err("injected cleanup failure".into())
            } else {
                Ok(())
            }
        }
        fn position(&self, id: MouseId) -> Result<Point, String> {
            self.points.get(&id).copied().ok_or("missing mouse".into())
        }
        fn submit(&mut self, id: MouseId, p: Point) -> Result<(), String> {
            if self.fail_move {
                return Err("injected native failure".into());
            }
            self.points.insert(id, p);
            self.submitted.push((id, p));
            if let Some(token) = &self.cancel_after_sample {
                token.cancel();
            }
            Ok(())
        }
        fn button(&mut self, id: MouseId, _: &str, a: &str) -> Result<(), String> {
            self.calls.push((a.into(), id));
            Ok(())
        }
        fn scroll(&mut self, _: MouseId, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn appearance(&mut self, _: MouseId, _: &CursorImage) -> Result<(), String> {
            Ok(())
        }
        fn locate(&mut self, _: &OwnedTarget) -> Result<LocatedTarget, String> {
            if self.cancel_on_locate.is_some() || self.observed_name.is_some() {
                if let Some(token) = &self.cancel_on_locate {
                    token.cancel();
                }
                Ok(LocatedTarget {
                    path: "0/0".into(),
                    name: self.observed_name.clone().unwrap_or_else(|| "Save".into()),
                    role: "button".into(),
                    bounds: crate::target::Bounds {
                        x: 10.,
                        y: 10.,
                        width: 20.,
                        height: 20.,
                    },
                })
            } else {
                Err("injected missing target".into())
            }
        }
    }
    fn run(source: &str, host: MockHost, options: Options) -> (String, MockHost) {
        let plan = super::super::compile(source, options).unwrap();
        execute_on(host, &plan, options, || false).unwrap()
    }
    #[test]
    fn function_locals_end_before_caller_continues_and_borrow_survives() {
        let (out, host) = run(
            "fn f(p) { let local = mouse(); p.click(); } let a = mouse(); f(a); a.click();",
            MockHost::default(),
            Options::default(),
        );
        assert!(!crate::runtime::failed(&out), "{out}");
        let ids = host
            .calls
            .iter()
            .filter(|(event, _)| event == "create")
            .map(|(_, id)| *id)
            .collect::<Vec<_>>();
        assert_eq!(
            host.calls,
            vec![
                ("create".into(), ids[0]),
                ("create".into(), ids[1]),
                ("click".into(), ids[0]),
                ("close".into(), ids[1]),
                ("click".into(), ids[0]),
                ("close".into(), ids[0])
            ]
        );
        assert!(host.points.is_empty());
    }
    #[test]
    fn native_failure_closes_every_live_branch_mouse() {
        let (out, host) = run(
            "let a=mouse(); let b=mouse(); parallel {a.move(x=20,y=30,duration=0); b.click();}",
            MockHost {
                fail_move: true,
                ..MockHost::default()
            },
            Options::default(),
        );
        assert!(crate::runtime::failed(&out));
        assert!(out.contains("injected native failure"));
        assert!(host.points.is_empty());
        assert_eq!(host.calls.iter().filter(|(e, _)| e == "close").count(), 2);
        assert!(!host.calls.iter().any(|(e, _)| e == "click"));
    }
    #[test]
    fn cancellation_closes_devices_while_a_motion_is_in_flight() {
        let options = Options::default();
        let plan = super::super::compile(
            "let a=mouse(); a.down(); a.move(x=100,y=50,duration=1000);",
            options,
        )
        .unwrap();
        let token = super::super::CancellationToken::default();
        let host = MockHost {
            cancel_after_sample: Some(token.clone()),
            ..MockHost::default()
        };
        let (out, host) = execute_on(host, &plan, options, || token.is_cancelled()).unwrap();
        assert!(crate::runtime::failed(&out));
        assert!(out.contains("script cancelled"));
        assert!(host.points.is_empty());
        assert!(host.calls.iter().any(|(e, _)| e == "down"));
        assert_eq!(host.submitted.len(), 1);
    }
    #[test]
    fn speed_budget_and_missing_target_fail_before_any_motion_sample() {
        for source in [
            "let p=mouse(); p.move(x=100,y=0,speed=1000);",
            "let p=mouse(); p.move_to(text=\"Missing\",duration=0);",
        ] {
            let (out, host) = run(
                source,
                MockHost::default(),
                Options {
                    max_wait_ms: 50,
                    ..Options::default()
                },
            );
            assert!(crate::runtime::failed(&out), "{out}");
            assert!(host.submitted.is_empty());
            assert!(host.points.is_empty());
        }
    }
    #[test]
    fn parallel_samples_interleave_and_keep_exact_endpoints() {
        let (out, host) = run(
            "let a=mouse(x=0,y=0); let b=mouse(x=10,y=20); parallel { a.move(x=80,y=40,duration=20,steps=4); b.move(x=90,y=60,duration=20,steps=4); }",
            MockHost::default(),
            Options::default(),
        );
        assert!(!crate::runtime::failed(&out), "{out}");
        assert_eq!(host.submitted.len(), 10);
        assert_ne!(host.submitted[0].0, host.submitted[1].0);
        let first = host.submitted[0].0;
        assert_eq!(
            host.submitted
                .iter()
                .rfind(|(id, _)| *id == first)
                .unwrap()
                .1,
            Point { x: 80., y: 40. }
        );
        assert_eq!(
            host.submitted
                .iter()
                .rfind(|(id, _)| *id != first)
                .unwrap()
                .1,
            Point { x: 90., y: 60. }
        );
        assert!(host.points.is_empty());
    }
    #[test]
    fn cancellation_during_target_lookup_prevents_first_sample() {
        let options = Options::default();
        let plan =
            super::super::compile("let p=mouse();p.move_to(text=\"Save\");", options).unwrap();
        let token = super::super::CancellationToken::default();
        let host = MockHost {
            cancel_on_locate: Some(token.clone()),
            ..MockHost::default()
        };
        let (out, host) = execute_on(host, &plan, options, || token.is_cancelled()).unwrap();
        assert!(crate::runtime::failed(&out));
        assert!(out.contains("script cancelled"));
        assert!(host.submitted.is_empty());
        assert!(host.points.is_empty());
    }
    #[test]
    fn nested_ready_actions_yield_to_other_parallel_branches() {
        let source =
            "let a=mouse();let b=mouse();parallel { {repeat 500 { {a.click();} }} {b.click();} }";
        let (out, host) = run(
            source,
            MockHost::default(),
            Options {
                max_steps: 1000,
                ..Options::default()
            },
        );
        assert!(!crate::runtime::failed(&out), "{out}");
        let second = host
            .calls
            .iter()
            .filter(|(event, _)| event == "create")
            .nth(1)
            .unwrap()
            .1;
        let first_second_click = host
            .calls
            .iter()
            .position(|(event, id)| event == "click" && *id == second)
            .unwrap();
        assert!(
            first_second_click < 100,
            "nested work starved its sibling until {first_second_click}"
        );
    }
    #[test]
    fn deeply_expanded_function_scopes_make_progress_with_a_small_quantum() {
        let mut source = String::new();
        for index in 0..8 {
            let body = if index == 7 {
                "p.click();".into()
            } else {
                format!("f{}(p);", index + 1)
            };
            source.push_str(&format!(
                "fn f{index}(p) {{ {} {body} {} }}",
                "{".repeat(8),
                "}".repeat(8),
            ));
        }
        source.push_str("let p=mouse(); f0(p); p.click();");
        let options = Options::default();
        let plan = super::super::compile(&source, options).unwrap();
        let mut traversals = 0;
        let (out, host) = execute_on(MockHost::default(), &plan, options, || {
            traversals += 1;
            // Keep a regression finite even if an ancestor consumes every
            // future poll's quantum again.
            traversals > 20_000
        })
        .unwrap();
        assert!(!crate::runtime::failed(&out), "{out}");
        assert_eq!(
            host.calls
                .iter()
                .filter(|(event, _)| event == "click")
                .count(),
            2
        );
        assert!(host.points.is_empty());
    }
    #[test]
    fn oversized_observation_receipts_are_bounded_without_stopping_actions() {
        let (out, host) = run(
            "let p=mouse();repeat 3 {p.move_to(text=\"Save\",duration=0);}p.click();",
            MockHost {
                observed_name: Some("x".repeat(300_000)),
                ..MockHost::default()
            },
            Options::default(),
        );
        assert!(!crate::runtime::failed(&out));
        assert!(out.len() < 1_048_576);
        assert!(host.points.is_empty());
        assert_eq!(
            host.calls
                .iter()
                .filter(|(name, _)| name == "click")
                .count(),
            1
        );
        json::with_value(&out, |value| {
            assert_eq!(value.get("events_omitted").unwrap().integer().unwrap(), 3);
            assert_eq!(value.get("events").unwrap().elements().count(), 3);
            let cleanup = value.get("cleanup").unwrap();
            assert_eq!(cleanup.get("attempted").unwrap().integer().unwrap(), 1);
            assert_eq!(cleanup.get("failed").unwrap().integer().unwrap(), 0);
            assert_eq!(value.get("error").unwrap().raw(), "null");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn script_receipt_summary_preserves_effects_errors_and_cleanup() {
        for (fail_move, fail_close, cancel) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (false, false, true),
        ] {
            let options = Options {
                max_steps: 2600,
                ..Options::default()
            };
            let plan = super::super::compile(
                "let p=mouse();p.down();repeat 2500 {touchpad.capabilities();}p.move(x=20,y=30,duration=0);p.up();",
                options,
            )
            .unwrap();
            let token = super::super::CancellationToken::default();
            let host = MockHost {
                fail_move,
                fail_close,
                cancel_after_sample: cancel.then(|| token.clone()),
                ..MockHost::default()
            };
            let (out, host) = execute_on(host, &plan, options, || token.is_cancelled()).unwrap();
            assert_eq!(
                crate::runtime::failed(&out),
                fail_move || fail_close || cancel
            );
            assert!(host.points.is_empty());
            assert_eq!(
                host.calls.iter().filter(|(name, _)| name == "down").count(),
                1
            );
            assert_eq!(
                host.calls
                    .iter()
                    .filter(|(name, _)| name == "close")
                    .count(),
                1
            );
            assert_eq!(host.submitted.is_empty(), fail_move);
            json::with_value(&out, |value| {
                assert!(value.get("events_omitted").unwrap().integer().unwrap() > 0);
                let cleanup = value.get("cleanup").unwrap();
                assert_eq!(cleanup.get("attempted").unwrap().integer().unwrap(), 1);
                assert_eq!(
                    cleanup.get("failed").unwrap().integer().unwrap(),
                    i64::from(fail_close)
                );
                let error = value.get("error").unwrap();
                if fail_move {
                    assert!(json::text(error)?.contains("injected native failure"));
                }
                if fail_close {
                    assert!(json::text(error)?.contains("injected cleanup failure"));
                }
                if cancel {
                    assert!(json::text(error)?.contains("script cancelled"));
                }
                if !fail_move && !fail_close && !cancel {
                    assert_eq!(error.raw(), "null");
                    assert_eq!(
                        host.calls.iter().filter(|(name, _)| name == "up").count(),
                        1
                    );
                }
                Ok(())
            })
            .unwrap();
        }
    }
}
