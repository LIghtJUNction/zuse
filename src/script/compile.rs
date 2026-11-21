//! Resolve lexical ownership and validate a complete script before input.

use super::{
    plan::{Action, CompiledScript, MouseId, Node, OwnedTarget, PlanSummary},
    syntax::{self, Argument, Call, Expr, Span, Statement, StatementKind},
};
use crate::{
    cursor::{CursorColor, CursorImage, CursorShape, CursorStyle},
    motion::{Curve, DEFAULT_MOVE_DURATION_MS, DEFAULT_MOVE_JITTER, MAX_COORDINATE, Point},
    mouse::{MouseMode, MouseOptions},
    pointer::MoveOptions,
    runtime::Options,
    target, touchpad,
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_EXPANDED_NODES: usize = 16_384;
const MAX_CREATED_MICE: usize = 256;
const MAX_LIVE_MICE: usize = 32;
const MAX_CALL_DEPTH: usize = 32;
const MAX_EXPANDED_DEPTH: usize = 128;
const MAX_IMAGE_PIXELS: usize = 4_194_304;
const MAX_RETAINED_STRING_BYTES: usize = 16 * 1024 * 1024;

const CONSTRUCTOR_ARGS: &[&str] = &[
    "name",
    "color",
    "shape",
    "size",
    "glow",
    "image",
    "hotspot_x",
    "hotspot_y",
    "x",
    "y",
    "shared",
    "appearance",
];
const MOVE_ARGS: &[&str] = &[
    "x",
    "y",
    "duration",
    "speed",
    "steps",
    "curve",
    "jitter",
    "speed_variation",
    "seed",
    "offset_x",
    "offset_y",
    "via",
];
const TARGET_ARGS: &[&str] = &[
    "text",
    "role",
    "scope",
    "exact",
    "case_sensitive",
    "index",
    "duration",
    "speed",
    "steps",
    "curve",
    "jitter",
    "speed_variation",
    "seed",
    "offset_x",
    "offset_y",
    "via",
];
const APPEARANCE_ARGS: &[&str] = &[
    "color",
    "shape",
    "size",
    "glow",
    "image",
    "hotspot_x",
    "hotspot_y",
];

type Result<T> = std::result::Result<T, String>;
type Bindings = BTreeMap<String, MouseId>;

#[derive(Default)]
struct Scope {
    bindings: Bindings,
    owned: Vec<MouseId>,
}

#[derive(Clone, Copy)]
struct Function<'a> {
    params: &'a [String],
    body: &'a [Statement],
}

struct Compiler<'a> {
    options: Options,
    functions: BTreeMap<&'a str, Function<'a>>,
    stack: Vec<&'a str>,
    modes: Vec<MouseMode>,
    cursor_support: Vec<bool>,
    held_buttons: Vec<BTreeSet<String>>,
    summary: PlanSummary,
    nodes: usize,
    depth: usize,
    image_pixels: usize,
    string_bytes: usize,
    gesture_samples: usize,
}

/// Compile without opening a display, observing targets, or submitting input.
/// Custom image files are read and retained during compilation.
pub(super) fn compile(source: &str, options: Options) -> Result<CompiledScript> {
    let program = syntax::parse(source).map_err(|error| error.to_string())?;
    let mut functions = BTreeMap::new();
    declarations(&program.statements, true, &mut functions)?;
    let span = program.statements.first().map_or(
        Span {
            start: 0,
            end: 0,
            line: 1,
            column: 1,
        },
        |statement| statement.span,
    );
    let mut compiler = Compiler {
        options,
        functions,
        stack: Vec::new(),
        modes: Vec::new(),
        cursor_support: Vec::new(),
        held_buttons: Vec::new(),
        summary: PlanSummary::default(),
        nodes: 0,
        depth: 0,
        image_pixels: 0,
        string_bytes: 0,
        gesture_samples: 0,
    };
    let root = compiler.scope(&program.statements, &mut Vec::new(), Bindings::new(), span)?;
    let (_, shared_peak) = live_mice(&root, &compiler.modes, true);
    if shared_peak > 1 {
        return Err(at(
            span,
            "distinct user_mouse handles cannot be alive at the same time; borrow an alias or end the earlier scope",
        ));
    }
    let (_, peak) = live_mice(&root, &compiler.modes, false);
    if peak > MAX_LIVE_MICE {
        return Err(at(
            span,
            format!("script may keep {peak} mice alive at once; limit is {MAX_LIVE_MICE}"),
        ));
    }
    Ok(CompiledScript {
        root,
        summary: compiler.summary,
    })
}

fn at(span: Span, message: impl std::fmt::Display) -> String {
    format!(
        "{message} at {}:{} (byte {})",
        span.line, span.column, span.start
    )
}

fn declarations<'a>(
    statements: &'a [Statement],
    top: bool,
    functions: &mut BTreeMap<&'a str, Function<'a>>,
) -> Result<()> {
    for statement in statements {
        match &statement.kind {
            StatementKind::Function { name, params, body } => {
                if !top {
                    return Err(at(
                        statement.span,
                        "functions may be declared only at script top level",
                    ));
                }
                if matches!(name.as_str(), "mouse" | "user_mouse" | "wait" | "touchpad") {
                    return Err(at(
                        statement.span,
                        format!("function name {name:?} is reserved"),
                    ));
                }
                if params.iter().any(|param| param == "touchpad") {
                    return Err(at(statement.span, "touchpad is a reserved global receiver"));
                }
                let function = Function { params, body };
                if functions.insert(name, function).is_some() {
                    return Err(at(statement.span, format!("duplicate function {name:?}")));
                }
                declarations(body, false, functions)?;
            }
            StatementKind::Block(body)
            | StatementKind::Parallel(body)
            | StatementKind::Sequence(body)
            | StatementKind::Repeat { body, .. } => declarations(body, false, functions)?,
            StatementKind::If {
                then_body,
                else_body,
                ..
            } => {
                declarations(then_body, false, functions)?;
                declarations(else_body, false, functions)?;
            }
            StatementKind::For { body, .. } => declarations(body, false, functions)?,
            StatementKind::Let { .. } | StatementKind::Call(_) => {}
        }
    }
    Ok(())
}

impl<'a> Compiler<'a> {
    fn reserve_node(&mut self, span: Span) -> Result<()> {
        if self.depth >= MAX_EXPANDED_DEPTH {
            return Err(at(
                span,
                format!("expanded tree depth exceeds {MAX_EXPANDED_DEPTH}"),
            ));
        }
        if self.nodes >= MAX_EXPANDED_NODES {
            return Err(at(
                span,
                format!("expanded node limit exceeded ({MAX_EXPANDED_NODES})"),
            ));
        }
        self.nodes += 1;
        Ok(())
    }

    fn enter_node(&mut self, span: Span) -> Result<()> {
        self.reserve_node(span)?;
        self.depth += 1;
        Ok(())
    }

    fn strings(&mut self, bytes: usize, span: Span) -> Result<()> {
        self.string_bytes = self
            .string_bytes
            .checked_add(bytes)
            .ok_or_else(|| at(span, "retained string budget overflow"))?;
        if self.string_bytes > MAX_RETAINED_STRING_BYTES {
            return Err(at(
                span,
                "expanded script strings exceed the 16 MiB retained string budget",
            ));
        }
        Ok(())
    }

    fn step(&mut self, span: Span, wait_ms: u64) -> Result<()> {
        if self.summary.steps >= self.options.max_steps {
            return Err(at(
                span,
                format!("step limit exceeded ({})", self.options.max_steps),
            ));
        }
        self.reserve_node(span)?;
        self.summary.steps += 1;
        self.summary.static_wait_ms = self
            .summary
            .static_wait_ms
            .checked_add(wait_ms)
            .ok_or_else(|| at(span, "program wait budget overflow"))?;
        if self.summary.static_wait_ms > self.options.max_wait_ms {
            return Err(at(
                span,
                format!("program wait budget exceeds {}ms", self.options.max_wait_ms),
            ));
        }
        Ok(())
    }

    fn image(&mut self, image: &CursorImage, span: Span) -> Result<()> {
        self.image_pixels = self
            .image_pixels
            .checked_add(image.pixels.len())
            .ok_or_else(|| at(span, "decoded cursor image budget overflow"))?;
        if self.image_pixels > MAX_IMAGE_PIXELS {
            return Err(at(
                span,
                "decoded cursor images exceed the 16 MiB pixel budget",
            ));
        }
        Ok(())
    }

    fn scope(
        &mut self,
        statements: &'a [Statement],
        environment: &mut Vec<Scope>,
        bindings: Bindings,
        span: Span,
    ) -> Result<Node> {
        self.enter_node(span)?;
        environment.push(Scope {
            bindings,
            owned: Vec::new(),
        });
        let result = (|| {
            let mut body = Vec::new();
            for statement in statements {
                if let Some(node) = self.statement(statement, environment)? {
                    body.push(node);
                }
            }
            Ok(body)
        })();
        let scope = environment.pop().expect("compiler scope was pushed");
        self.depth -= 1;
        result.map(|body| Node::Scope {
            body,
            owned: scope.owned,
        })
    }

    fn statement(
        &mut self,
        statement: &'a Statement,
        environment: &mut Vec<Scope>,
    ) -> Result<Option<Node>> {
        let span = statement.span;
        Ok(match &statement.kind {
            StatementKind::Function { .. } => None,
            StatementKind::If { .. } | StatementKind::For { .. } => {
                return Err(at(
                    span,
                    "scalar control must be expanded before scoped compilation",
                ));
            }
            StatementKind::Let { name, value } => {
                if name == "touchpad" {
                    return Err(at(span, "touchpad is a reserved global receiver"));
                }
                if environment
                    .last()
                    .is_some_and(|scope| scope.bindings.contains_key(name))
                {
                    return Err(at(
                        span,
                        format!("duplicate variable {name:?} in this scope"),
                    ));
                }
                match value {
                    Expr::Identifier(alias) => {
                        let id = lookup(environment, alias, span)?;
                        environment
                            .last_mut()
                            .expect("binding has a scope")
                            .bindings
                            .insert(name.clone(), id);
                        None
                    }
                    Expr::Call(call)
                        if call.receiver.is_none()
                            && matches!(call.name.as_str(), "mouse" | "user_mouse") =>
                    {
                        if self.options.read_only {
                            return Err(at(span, "mouse creation is forbidden in read-only mode"));
                        }
                        if self.modes.len() >= MAX_CREATED_MICE {
                            return Err(at(
                                span,
                                format!("created mouse limit exceeded ({MAX_CREATED_MICE})"),
                            ));
                        }
                        let options = mouse_options(call)?;
                        if let Some(image) = &options.image {
                            self.image(image, span)?;
                        }
                        self.strings(options.name.len(), span)?;
                        self.step(span, 0)?;
                        let id = self.modes.len() as MouseId;
                        if options.mode == MouseMode::User {
                            self.summary.shared_mice += 1;
                        }
                        self.modes.push(options.mode);
                        self.cursor_support
                            .push(options.image.is_some() || options.appearance.is_some());
                        self.held_buttons.push(BTreeSet::new());
                        self.summary.mice += 1;
                        let scope = environment.last_mut().expect("binding has a scope");
                        scope.bindings.insert(name.clone(), id);
                        scope.owned.push(id);
                        Some(Node::Create { id, options })
                    }
                    _ => {
                        return Err(at(
                            span,
                            "let requires mouse(...), user_mouse(...) or a mouse variable",
                        ));
                    }
                }
            }
            StatementKind::Call(call) => Some(self.call(call, environment)?),
            StatementKind::Block(body) | StatementKind::Sequence(body) => {
                Some(self.scope(body, environment, Bindings::new(), span)?)
            }
            StatementKind::Repeat { count, body } => {
                self.enter_node(span)?;
                let expanded: Result<Vec<Node>> = (|| {
                    let mut expanded = Vec::new();
                    for _ in 0..*count {
                        expanded.push(self.scope(body, environment, Bindings::new(), span)?);
                    }
                    Ok(expanded)
                })();
                self.depth -= 1;
                Some(Node::Scope {
                    body: expanded?,
                    owned: Vec::new(),
                })
            }
            StatementKind::Parallel(branches) => {
                self.enter_node(span)?;
                self.summary.parallel_groups += 1;
                let nodes = self.parallel(branches, environment);
                self.depth -= 1;
                Some(Node::Parallel(nodes?))
            }
        })
    }

    fn parallel(
        &mut self,
        branches: &'a [Statement],
        environment: &mut Vec<Scope>,
    ) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        let mut used = BTreeSet::new();
        for branch in branches {
            let node = self.scope(
                std::slice::from_ref(branch),
                environment,
                Bindings::new(),
                branch.span,
            )?;
            let mut resources = BTreeSet::new();
            self.resources(&node, &mut resources);
            if let Some(resource) = resources.intersection(&used).next() {
                let description = match resource {
                    Resource::User => "the shared user mouse".to_owned(),
                    Resource::Mouse(id) => format!("mouse resource {id}"),
                    Resource::Touchpad => "the touchpad gesture stream".to_owned(),
                };
                return Err(at(
                    branch.span,
                    format!(
                        "parallel branches both use {description}; use sequence or separate independent mice"
                    ),
                ));
            }
            used.extend(resources);
            nodes.push(node);
        }
        Ok(nodes)
    }

    fn call(&mut self, call: &'a Call, environment: &mut [Scope]) -> Result<Node> {
        if let Some(receiver) = &call.receiver {
            if receiver == "touchpad" {
                let command = touchpad_command(call)?;
                if self.options.read_only && command.is_mutating() {
                    return Err(at(
                        call.span,
                        "touchpad mutation is forbidden in read-only mode",
                    ));
                }
                if let touchpad::Command::Gesture(plan) = &command {
                    self.gesture_samples =
                        self.gesture_samples
                            .checked_add(plan.samples().len())
                            .ok_or_else(|| at(call.span, "touchpad sample budget overflow"))?;
                    if self.gesture_samples > touchpad::MAX_PROGRAM_SAMPLES {
                        return Err(at(call.span, "expanded touchpad samples exceed 65536"));
                    }
                }
                self.step(call.span, u64::from(command.scheduled_duration_ms()))?;
                return Ok(Node::Touchpad(command));
            }
            let id = lookup(environment, receiver, call.span)?;
            if self.options.read_only && call.name != "position" {
                return Err(at(
                    call.span,
                    "mouse mutation is forbidden in read-only mode",
                ));
            }
            if call.name == "appearance" && !self.cursor_support[id as usize] {
                return Err(at(
                    call.span,
                    "appearance cannot change on a user mouse created with appearance=\"system\"; create it with a glow or image",
                ));
            }
            let (action, wait) = action(call)?;
            match &action {
                Action::Click { button, .. } if self.held_buttons[id as usize].contains(button) => {
                    return Err(at(
                        call.span,
                        format!(
                            "cannot click {button}: this mouse already holds the button; release it with up first"
                        ),
                    ));
                }
                Action::Button { button, down } => {
                    if *down {
                        self.held_buttons[id as usize].insert(button.clone());
                    } else {
                        self.held_buttons[id as usize].remove(button);
                    }
                }
                _ => {}
            }
            match &action {
                Action::Appearance(image) => self.image(image, call.span)?,
                Action::MoveTo { query, .. } => self.strings(
                    query.text.len()
                        + query.role.as_ref().map_or(0, String::len)
                        + query.scope.as_ref().map_or(0, String::len),
                    call.span,
                )?,
                Action::Click { button, .. } | Action::Button { button, .. } => {
                    self.strings(button.len(), call.span)?
                }
                _ => {}
            }
            self.step(call.span, wait)?;
            return Ok(Node::Action { id, action });
        }
        if call.name == "wait" {
            let args = Args::new(call, &["ms"])?;
            args.require("ms")?;
            let ms = args.unsigned("ms", 0, 0, 60_000)? as u32;
            self.step(call.span, u64::from(ms))?;
            return Ok(Node::Wait(ms));
        }
        if matches!(call.name.as_str(), "mouse" | "user_mouse") {
            return Err(at(call.span, "mouse constructors must be bound with let"));
        }
        let function = self
            .functions
            .get(call.name.as_str())
            .copied()
            .ok_or_else(|| at(call.span, format!("unknown function {:?}", call.name)))?;
        if self.stack.len() >= MAX_CALL_DEPTH {
            return Err(at(
                call.span,
                format!("function call depth exceeds {MAX_CALL_DEPTH}"),
            ));
        }
        let names: Vec<&str> = function.params.iter().map(String::as_str).collect();
        let args = Args::new(call, &names)?;
        let mut bindings = Bindings::new();
        for name in function.params {
            let argument = args.require(name)?;
            let Expr::Identifier(variable) = &argument.value else {
                return Err(at(
                    argument.span,
                    "function arguments must be mouse variables",
                ));
            };
            bindings.insert(name.clone(), lookup(environment, variable, argument.span)?);
        }
        self.stack.push(call.name.as_str());
        // A function has its own lexical scope. Caller variables are available
        // only through explicitly borrowed parameters, never dynamic lookup.
        let result = self.scope(function.body, &mut Vec::new(), bindings, call.span);
        self.stack.pop();
        result
    }

    fn resources(&self, node: &Node, result: &mut BTreeSet<Resource>) {
        match node {
            Node::Create { id, .. } | Node::Action { id, .. } => {
                result.insert(if self.modes[*id as usize] == MouseMode::User {
                    Resource::User
                } else {
                    Resource::Mouse(*id)
                });
            }
            Node::Scope { body, .. } | Node::Parallel(body) => {
                for child in body {
                    self.resources(child, result);
                }
            }
            Node::Wait(_) => {}
            Node::Touchpad(command) => {
                if command.is_mutating() {
                    result.insert(Resource::Touchpad);
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Resource {
    User,
    Mouse(MouseId),
    Touchpad,
}

fn lookup(environment: &[Scope], name: &str, span: Span) -> Result<MouseId> {
    environment
        .iter()
        .rev()
        .find_map(|scope| scope.bindings.get(name).copied())
        .ok_or_else(|| {
            at(
                span,
                format!("unknown or out-of-scope mouse variable {name:?}"),
            )
        })
}

/// The second value is a conservative peak for every legal branch interleaving.
fn live_mice(node: &Node, modes: &[MouseMode], shared_only: bool) -> (usize, usize) {
    match node {
        Node::Create { id, .. } => {
            let count = usize::from(!shared_only || modes[*id as usize] == MouseMode::User);
            (count, count)
        }
        Node::Action { .. } | Node::Wait(_) | Node::Touchpad(_) => (0, 0),
        Node::Scope { body, owned } => {
            let (mut live, mut peak) = (0, 0);
            for child in body {
                let (created, child_peak) = live_mice(child, modes, shared_only);
                peak = peak.max(live + child_peak);
                live += created;
            }
            let dropped = owned
                .iter()
                .filter(|id| !shared_only || modes[**id as usize] == MouseMode::User)
                .count();
            (live.saturating_sub(dropped), peak)
        }
        Node::Parallel(branches) => branches
            .iter()
            .map(|node| live_mice(node, modes, shared_only))
            .fold((0, 0), |(live, peak), (branch_live, branch_peak)| {
                (live + branch_live, peak + branch_peak)
            }),
    }
}

struct Args<'a> {
    call: &'a Call,
    values: BTreeMap<String, &'a Argument>,
}

impl<'a> Args<'a> {
    fn new(call: &'a Call, names: &[&str]) -> Result<Self> {
        let mut values = BTreeMap::new();
        let mut positional = 0;
        for argument in &call.args {
            let name = match &argument.name {
                Some(name) if names.contains(&name.as_str()) => name.as_str(),
                Some(name) => {
                    return Err(at(
                        argument.span,
                        format!("{} has no argument {name:?}", call.name),
                    ));
                }
                None => {
                    let name = names.get(positional).ok_or_else(|| {
                        at(
                            argument.span,
                            format!("too many positional arguments for {}", call.name),
                        )
                    })?;
                    positional += 1;
                    name
                }
            };
            if values.insert(name.to_owned(), argument).is_some() {
                return Err(at(
                    argument.span,
                    format!("duplicate value for argument {name:?}"),
                ));
            }
        }
        Ok(Self { call, values })
    }

    fn get(&self, name: &str) -> Option<&'a Argument> {
        self.values.get(name).copied()
    }

    fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    fn require(&self, name: &str) -> Result<&'a Argument> {
        self.get(name).ok_or_else(|| {
            at(
                self.call.span,
                format!("{} requires {name}", self.call.name),
            )
        })
    }

    fn bad(&self, name: &str, requirement: &str) -> String {
        at(
            self.get(name)
                .map_or(self.call.span, |argument| argument.span),
            format!("{}: {name} {requirement}", self.call.name),
        )
    }

    fn text(&self, name: &str, default: &str) -> Result<String> {
        match self.get(name).map(|argument| &argument.value) {
            None => Ok(default.to_owned()),
            Some(Expr::String(value)) if !value.contains('\0') => Ok(value.clone()),
            _ => Err(self.bad(name, "must be a NUL-free string literal")),
        }
    }

    fn optional_text(&self, name: &str) -> Result<Option<String>> {
        self.has(name).then(|| self.text(name, "")).transpose()
    }

    fn boolean(&self, name: &str, default: bool) -> Result<bool> {
        match self.get(name).map(|argument| &argument.value) {
            None => Ok(default),
            Some(Expr::Bool(value)) => Ok(*value),
            _ => Err(self.bad(name, "must be a boolean literal")),
        }
    }

    fn number(&self, name: &str, default: f64) -> Result<f64> {
        match self.get(name).map(|argument| &argument.value) {
            None => Ok(default),
            Some(Expr::Number(value)) => value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| self.bad(name, "must be a finite number")),
            _ => Err(self.bad(name, "must be a numeric literal")),
        }
    }

    fn unsigned(&self, name: &str, default: u64, min: u64, max: u64) -> Result<u64> {
        let value = match self.get(name).map(|argument| &argument.value) {
            None => default,
            Some(Expr::Number(value)) => value
                .parse::<u64>()
                .map_err(|_| self.bad(name, "must be an unsigned decimal integer"))?,
            _ => return Err(self.bad(name, "must be an unsigned decimal integer")),
        };
        if !(min..=max).contains(&value) {
            return Err(self.bad(name, &format!("must be in {min}..{max}")));
        }
        Ok(value)
    }

    fn signed(&self, name: &str, default: i64, min: i64, max: i64) -> Result<i64> {
        let value = match self.get(name).map(|argument| &argument.value) {
            None => default,
            Some(Expr::Number(value)) => value
                .parse::<i64>()
                .map_err(|_| self.bad(name, "must be a decimal integer"))?,
            _ => return Err(self.bad(name, "must be a decimal integer")),
        };
        if !(min..=max).contains(&value) {
            return Err(self.bad(name, &format!("must be in {min}..{max}")));
        }
        Ok(value)
    }
}

fn touchpad_command(call: &Call) -> Result<touchpad::Command> {
    let names: &[&str] = match call.name.as_str() {
        "capabilities" | "cancel" => &[],
        "swipe" => &["dx", "dy", "fingers", "duration", "steps"],
        "pinch" => &["scale", "fingers", "duration", "steps"],
        "rotate" => &["degrees", "fingers", "duration", "steps"],
        _ => {
            return Err(at(
                call.span,
                format!("unknown touchpad operation {:?}", call.name),
            ));
        }
    };
    let args = Args::new(call, names)?;
    if call.name == "capabilities" {
        return Ok(touchpad::Command::Capabilities);
    }
    if call.name == "cancel" {
        return Ok(touchpad::Command::Cancel);
    }
    let gesture = match call.name.as_str() {
        "swipe" => touchpad::Gesture::Swipe {
            dx: args.number("dx", 0.0)?,
            dy: args.number("dy", 0.0)?,
        },
        "pinch" => {
            args.require("scale")?;
            touchpad::Gesture::Pinch {
                scale: args.number("scale", 1.0)?,
            }
        }
        "rotate" => {
            args.require("degrees")?;
            touchpad::Gesture::Rotate {
                degrees: args.number("degrees", 0.0)?,
            }
        }
        _ => unreachable!("gesture name was checked"),
    };
    let options = touchpad::GestureOptions {
        fingers: args.unsigned("fingers", 2, 2, 5)? as u8,
        duration_ms: args.unsigned("duration", 350, 0, 60_000)? as u32,
        steps: args
            .has("steps")
            .then(|| args.unsigned("steps", 1, 1, 4096).map(|value| value as u32))
            .transpose()?,
    };
    touchpad::GesturePlan::new(gesture, options)
        .map(touchpad::Command::Gesture)
        .map_err(|error| at(call.span, error))
}

fn mouse_options(call: &Call) -> Result<MouseOptions> {
    let args = Args::new(call, CONSTRUCTOR_ARGS)?;
    let explicit_user = call.name == "user_mouse";
    let shared = args.boolean("shared", explicit_user)?;
    if explicit_user && !shared {
        return Err(args.bad("shared", "cannot be false for user_mouse"));
    }
    let mode = if shared {
        MouseMode::User
    } else {
        MouseMode::Independent
    };
    let (appearance, image) = appearance(&args, shared)?;
    let start = match (args.has("x"), args.has("y")) {
        (false, false) => None,
        (true, true) => Some(Point {
            x: args.number("x", 0.0)?,
            y: args.number("y", 0.0)?,
        }),
        _ => return Err(at(call.span, "mouse start requires both x and y")),
    };
    if let Some(point) = start {
        point_in_range(point).map_err(|error| at(call.span, error))?;
    }
    let options = MouseOptions {
        mode,
        name: args.text("name", "zuse")?,
        appearance,
        image,
        start,
    };
    options.validate().map_err(|error| at(call.span, error))?;
    Ok(options)
}

fn appearance(args: &Args<'_>, shared: bool) -> Result<(Option<CursorStyle>, Option<CursorImage>)> {
    let selection = args.text("appearance", "glow")?;
    if !matches!(selection.as_str(), "glow" | "system") {
        return Err(args.bad("appearance", "must be \"glow\" or \"system\""));
    }
    let custom_style = ["color", "shape", "size", "glow"]
        .iter()
        .any(|name| args.has(name));
    if args.has("image") {
        if custom_style || selection == "system" {
            return Err(at(
                args.call.span,
                "image cannot be combined with style arguments or system appearance",
            ));
        }
        let path = args.text("image", "")?;
        if path.trim().is_empty() {
            return Err(args.bad("image", "must name a P7 PAM image"));
        }
        let x = args.unsigned("hotspot_x", 0, 0, 255)? as u32;
        let y = args.unsigned("hotspot_y", 0, 0, 255)? as u32;
        let image = CursorImage::load_pam(path, x, y).map_err(|error| at(args.call.span, error))?;
        return Ok((None, Some(image)));
    }
    if args.has("hotspot_x") || args.has("hotspot_y") {
        return Err(at(
            args.call.span,
            "hotspot_x and hotspot_y require an image",
        ));
    }
    if selection == "system" {
        if !shared {
            return Err(args.bad(
                "appearance",
                "\"system\" requires an explicit shared user mouse",
            ));
        }
        if custom_style {
            return Err(at(
                args.call.span,
                "system appearance cannot be combined with style arguments",
            ));
        }
        return Ok((None, None));
    }
    let mut style = CursorStyle::default();
    if args.has("color") {
        style.color = CursorColor::parse(&args.text("color", "")?)
            .map_err(|error| at(args.call.span, error))?;
    }
    if args.has("shape") {
        style.shape = CursorShape::parse(&args.text("shape", "")?)
            .map_err(|error| at(args.call.span, error))?;
    }
    style.size = args.unsigned("size", u64::from(style.size), 16, 96)? as u32;
    style.glow = args.number("glow", style.glow)?;
    style
        .validate()
        .map_err(|error| at(args.call.span, error))?;
    Ok((Some(style), None))
}

fn action(call: &Call) -> Result<(Action, u64)> {
    match call.name.as_str() {
        "move" => {
            let args = Args::new(call, MOVE_ARGS)?;
            args.require("x")?;
            args.require("y")?;
            let to = Point {
                x: args.number("x", 0.0)?,
                y: args.number("y", 0.0)?,
            };
            let options = movement(&args)?;
            point_in_range(to).map_err(|error| at(call.span, error))?;
            point_in_range(Point {
                x: to.x + options.offset.x,
                y: to.y + options.offset.y,
            })
            .map_err(|error| at(call.span, error))?;
            let wait = u64::from(options.scheduled_duration_ms());
            Ok((Action::Move { to, options }, wait))
        }
        "move_to" => {
            let args = Args::new(call, TARGET_ARGS)?;
            args.require("text")?;
            let query = OwnedTarget {
                text: args.text("text", "")?,
                role: args.optional_text("role")?,
                scope: args.optional_text("scope")?,
                exact: args.boolean("exact", true)?,
                case_sensitive: args.boolean("case_sensitive", true)?,
                index: args
                    .has("index")
                    .then(|| {
                        args.unsigned("index", 0, 0, 4095)
                            .map(|value| value as usize)
                    })
                    .transpose()?,
            };
            target::validate_query(&query.query()).map_err(|error| at(call.span, error))?;
            let options = movement(&args)?;
            let wait = u64::from(options.scheduled_duration_ms());
            Ok((Action::MoveTo { query, options }, wait))
        }
        "click" => {
            let args = Args::new(call, &["button", "count", "interval"])?;
            let button = button(&args)?;
            let count = args.unsigned("count", 1, 1, 10)? as u32;
            let interval = args.unsigned("interval", 80, 0, 1000)? as u32;
            Ok((
                Action::Click {
                    button,
                    count,
                    interval,
                },
                u64::from((count - 1) * interval),
            ))
        }
        "down" | "up" => {
            let args = Args::new(call, &["button"])?;
            Ok((
                Action::Button {
                    button: button(&args)?,
                    down: call.name == "down",
                },
                0,
            ))
        }
        "scroll" => {
            let args = Args::new(call, &["x", "y"])?;
            Ok((
                Action::Scroll {
                    dx: args.signed("x", 0, -1000, 1000)? as i32,
                    dy: args.signed("y", 0, -1000, 1000)? as i32,
                },
                0,
            ))
        }
        "appearance" => {
            let args = Args::new(call, APPEARANCE_ARGS)?;
            let (style, image) = appearance(&args, false)?;
            let image = match (image, style) {
                (Some(image), _) => image,
                (_, Some(style)) => style.render().map_err(|error| at(call.span, error))?,
                _ => return Err(at(call.span, "appearance requires a cursor image or style")),
            };
            Ok((Action::Appearance(image), 0))
        }
        "position" => {
            Args::new(call, &[])?;
            Ok((Action::Position, 0))
        }
        _ => Err(at(
            call.span,
            format!("unknown mouse method {:?}", call.name),
        )),
    }
}

fn button(args: &Args<'_>) -> Result<String> {
    let button = args.text("button", "left")?;
    let button = match button.as_str() {
        "1" => "left",
        "2" => "middle",
        "3" => "right",
        button => button,
    };
    if !["left", "right", "middle", "back", "forward"].contains(&button) {
        return Err(args.bad("button", "is not a supported mouse button"));
    }
    Ok(button.to_owned())
}

fn movement(args: &Args<'_>) -> Result<MoveOptions> {
    let raw = args.text("via", "")?;
    let mut via = Vec::new();
    if !raw.trim().is_empty() {
        for pair in raw.split(';') {
            if via.len() == 64 {
                return Err(args.bad("via", "accepts at most 64 points"));
            }
            let (x, y) = pair
                .split_once(',')
                .ok_or_else(|| args.bad("via", "must contain x,y pairs separated by semicolons"))?;
            let point = Point {
                x: x.trim()
                    .parse()
                    .map_err(|_| args.bad("via", "contains an invalid x coordinate"))?,
                y: y.trim()
                    .parse()
                    .map_err(|_| args.bad("via", "contains an invalid y coordinate"))?,
            };
            via.push(point);
        }
    }
    let options = MoveOptions {
        duration_ms: args
            .has("duration")
            .then(|| {
                args.unsigned("duration", u64::from(DEFAULT_MOVE_DURATION_MS), 0, 60_000)
                    .map(|value| value as u32)
            })
            .transpose()?,
        speed: args
            .has("speed")
            .then(|| args.number("speed", 0.0))
            .transpose()?,
        steps: args
            .has("steps")
            .then(|| args.unsigned("steps", 1, 1, 4096).map(|value| value as u32))
            .transpose()?,
        curve: Curve::parse(&args.text("curve", "smooth")?)
            .ok_or_else(|| args.bad("curve", "is not a supported motion curve"))?,
        jitter: args.number("jitter", DEFAULT_MOVE_JITTER)?,
        speed_variation: args.number("speed_variation", 0.0)?,
        seed: args.unsigned("seed", 1, 0, u64::MAX)?,
        offset: Point {
            x: args.number("offset_x", 0.0)?,
            y: args.number("offset_y", 0.0)?,
        },
        via,
    };
    options
        .validate()
        .map_err(|error| at(args.call.span, error))?;
    Ok(options)
}

fn point_in_range(point: Point) -> Result<()> {
    if !point.x.is_finite()
        || !point.y.is_finite()
        || point.x.abs() > MAX_COORDINATE
        || point.y.abs() > MAX_COORDINATE
    {
        return Err("pointer coordinates must be finite and in -16777216..16777216".into());
    }
    Ok(())
}
