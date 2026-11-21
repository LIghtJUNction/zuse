//! Pure, bounded evaluation before either desktop executor sees a program.
//! Mouse values are hygienic references, never copied resources. Function
//! environments contain only explicit parameters, so inlining cannot capture
//! caller variables. Blocks retaining mouse constructors retain their lifetime.
use super::*;
use syntax::BinaryOp;

type Values = BTreeMap<String, Expr>;
const MAX_WORK: usize = 16_384;
const MAX_EXPANSION_DEPTH: usize = 128;
const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;

/// Preserve existing unquoted flat words such as `left-handed`. A subtraction
/// involving a bound variable is always arithmetic, including `x-y`.
pub(super) fn legacy_word(expression: &Expr, bound: &BTreeSet<String>) -> Option<String> {
    match expression {
        Expr::Identifier(name) if !bound.contains(name) => Some(name.clone()),
        Expr::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } => Some(format!(
            "{}-{}",
            legacy_word(left, bound)?,
            legacy_word(right, bound)?
        )),
        _ => None,
    }
}

fn expression_required(value: &Expr, bound: &BTreeSet<String>) -> bool {
    match value {
        Expr::Neg(_) => true,
        Expr::Binary { .. } => legacy_word(value, bound).is_none(),
        Expr::Call(call) => {
            call.receiver.as_deref() == Some("math")
                || call
                    .args
                    .iter()
                    .any(|arg| expression_required(&arg.value, bound))
        }
        _ => false,
    }
}

fn statements_required(statements: &[Statement], bound: &mut BTreeSet<String>) -> bool {
    for statement in statements {
        let required = match &statement.kind {
            StatementKind::If { .. } | StatementKind::For { .. } => true,
            StatementKind::Let { name, value } => {
                let constructor = matches!(value, Expr::Call(call)
                    if call.receiver.is_none() && matches!(call.name.as_str(), "mouse" | "user_mouse"));
                let required = !constructor || expression_required(value, bound);
                bound.insert(name.clone());
                required
            }
            StatementKind::Call(call) => call
                .args
                .iter()
                .any(|arg| expression_required(&arg.value, bound)),
            StatementKind::Block(body)
            | StatementKind::Sequence(body)
            | StatementKind::Repeat { body, .. } => statements_required(body, &mut bound.clone()),
            StatementKind::Parallel(branches) => branches.iter().any(|branch| {
                statements_required(std::slice::from_ref(branch), &mut bound.clone())
            }),
            StatementKind::Function { params, body, .. } => {
                statements_required(body, &mut params.iter().cloned().collect())
            }
        };
        if required {
            return true;
        }
    }
    false
}

pub(super) fn required(
    session: &ModuleSession,
    items: &[Item],
    reachable: &BTreeSet<String>,
) -> bool {
    fn value_arguments(
        session: &ModuleSession,
        statements: &[Statement],
        namespace: &str,
        bound: &mut BTreeSet<String>,
    ) -> bool {
        let mut needed = false;
        let _ = visit_calls(statements, bound, &mut |call, bound| {
            if resolve(session, call, namespace, bound).is_some()
                && call
                    .args
                    .iter()
                    .any(|arg| !matches!(arg.value, Expr::Identifier(_)))
            {
                needed = true;
            }
            Ok(())
        });
        needed
    }
    let mut bound = BTreeSet::new();
    items
        .iter()
        .any(|item| statements_required(std::slice::from_ref(&item.statement), &mut bound))
        || reachable.iter().any(|name| {
            let function = &session.functions[name];
            statements_required(
                &function.body,
                &mut function.params.iter().cloned().collect(),
            )
        })
        || {
            let mut bound = BTreeSet::new();
            items.iter().any(|item| {
                value_arguments(
                    session,
                    std::slice::from_ref(&item.statement),
                    &item.namespace,
                    &mut bound,
                )
            }) || reachable.iter().any(|name| {
                let function = &session.functions[name];
                value_arguments(
                    session,
                    &function.body,
                    &function.namespace,
                    &mut function.params.iter().cloned().collect(),
                )
            })
        }
}

pub(super) fn lower(
    session: &ModuleSession,
    items: &[Item],
    options: Options,
) -> Result<Vec<Statement>> {
    let mut lowering = Lowering {
        session,
        options,
        work: 0,
        steps: 0,
        depth: 0,
        calls: 0,
        mouse_names: 0,
        output_bytes: 0,
        value_bytes: 0,
    };
    let mut values = Values::new();
    let mut locals = BTreeSet::new();
    let mut output = Vec::new();
    for item in items {
        output.extend(lowering.statements(
            std::slice::from_ref(&item.statement),
            &item.namespace,
            &item.origin,
            &mut values,
            &mut locals,
        )?);
    }
    Ok(output)
}

pub(super) fn write_flat(output: &mut Output, statements: &[Statement]) -> Result<()> {
    for statement in statements {
        match &statement.kind {
            StatementKind::Call(call) => {
                write_call(output, call)?;
                output.push(";\n")?;
            }
            StatementKind::Block(body) | StatementKind::Sequence(body) => write_flat(output, body)?,
            _ => return Err("unexpanded scoped/scalar statement in flat program".into()),
        }
    }
    Ok(())
}

struct Lowering<'a> {
    session: &'a ModuleSession,
    options: Options,
    work: usize,
    steps: usize,
    depth: usize,
    calls: usize,
    mouse_names: usize,
    output_bytes: usize,
    value_bytes: usize,
}

impl Lowering<'_> {
    fn work(&mut self, origin: &str, span: Span) -> Result<()> {
        self.work += 1;
        if self.work > MAX_WORK {
            return Err(at(origin, span, "expanded module work exceeds 16384 nodes"));
        }
        Ok(())
    }

    fn emit(&mut self, call: &Call, origin: &str) -> Result<()> {
        if self.steps >= self.options.max_steps {
            return Err(at(
                origin,
                call.span,
                format!("expanded step limit exceeded ({})", self.options.max_steps),
            ));
        }
        self.steps += 1;
        let mut encoded = Output::default();
        write_call(&mut encoded, call)?;
        self.output_bytes = self.output_bytes.saturating_add(encoded.text.len() + 64);
        if self.output_bytes > syntax::MAX_SCRIPT_BYTES {
            return Err(at(
                origin,
                call.span,
                "expanded module source exceeds 1 MiB",
            ));
        }
        Ok(())
    }

    fn statements(
        &mut self,
        statements: &[Statement],
        namespace: &str,
        origin: &str,
        values: &mut Values,
        locals: &mut BTreeSet<String>,
    ) -> Result<Vec<Statement>> {
        if self.depth >= MAX_EXPANSION_DEPTH {
            return Err(format!(
                "{origin}: expanded tree depth exceeds {MAX_EXPANSION_DEPTH}"
            ));
        }
        self.depth += 1;
        let result = (|| {
            let mut output = Vec::new();
            for statement in statements {
                self.work(origin, statement.span)?;
                output.extend(self.statement(statement, namespace, origin, values, locals)?);
            }
            Ok(output)
        })();
        self.depth -= 1;
        result
    }

    fn block(
        &mut self,
        body: &[Statement],
        namespace: &str,
        origin: &str,
        values: &Values,
        span: Span,
    ) -> Result<Vec<Statement>> {
        let body = self.statements(
            body,
            namespace,
            origin,
            &mut values.clone(),
            &mut BTreeSet::new(),
        )?;
        Ok(scoped_group(body, span))
    }

    fn statement(
        &mut self,
        statement: &Statement,
        namespace: &str,
        origin: &str,
        values: &mut Values,
        locals: &mut BTreeSet<String>,
    ) -> Result<Vec<Statement>> {
        let span = statement.span;
        match &statement.kind {
            StatementKind::Let { name, value } => {
                if name == "touchpad" {
                    return Err(at(origin, span, "touchpad is a reserved global receiver"));
                }
                if !locals.insert(name.clone()) {
                    return Err(at(
                        origin,
                        span,
                        format!("duplicate variable {name:?} in this scope"),
                    ));
                }
                if let Expr::Call(call) = value
                    && call.receiver.is_none()
                    && matches!(call.name.as_str(), "mouse" | "user_mouse")
                {
                    let call = self.arguments(call, values, origin)?;
                    self.emit(&call, origin)?;
                    let generated = format!("__zuse_scalar_mouse_{}", self.mouse_names);
                    self.mouse_names += 1;
                    values.insert(name.clone(), Expr::Identifier(generated.clone()));
                    return Ok(vec![Statement {
                        span,
                        kind: StatementKind::Let {
                            name: generated,
                            value: Expr::Call(call),
                        },
                    }]);
                }
                let value = self.expression(value, values, origin, span)?;
                values.insert(name.clone(), value);
                Ok(Vec::new())
            }
            StatementKind::Call(call) => self.call(call, namespace, origin, values),
            StatementKind::Block(body) => self.block(body, namespace, origin, values, span),
            StatementKind::Sequence(body) => {
                let body = self.block(body, namespace, origin, values, span)?;
                Ok(vec![Statement {
                    span,
                    kind: StatementKind::Sequence(body),
                }])
            }
            StatementKind::Parallel(branches) => {
                let mut lowered = Vec::new();
                for branch in branches {
                    let body = self.block(
                        std::slice::from_ref(branch),
                        namespace,
                        origin,
                        values,
                        branch.span,
                    )?;
                    // One source branch remains one branch after function/loop expansion.
                    lowered.push(Statement {
                        span: branch.span,
                        kind: StatementKind::Block(body),
                    });
                }
                Ok(vec![Statement {
                    span,
                    kind: StatementKind::Parallel(lowered),
                }])
            }
            StatementKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let Expr::Bool(condition) = self.expression(condition, values, origin, span)?
                else {
                    return Err(at(
                        origin,
                        span,
                        "if condition must be a boolean comparison or value",
                    ));
                };
                self.block(
                    if condition { then_body } else { else_body },
                    namespace,
                    origin,
                    values,
                    span,
                )
            }
            StatementKind::For {
                name,
                start,
                end,
                body,
            } => {
                let start = self.range_endpoint(start, values, origin, span)?;
                let end = self.range_endpoint(end, values, origin, span)?;
                let count = end.saturating_sub(start).max(0);
                if count > i64::from(syntax::MAX_REPEAT) {
                    return Err(at(origin, span, "for iteration limit exceeds 4096"));
                }
                if name == "touchpad" {
                    return Err(at(origin, span, "touchpad is a reserved global receiver"));
                }
                let mut output = Vec::new();
                for index in 0..count {
                    self.work(origin, span)?;
                    let mut inner = values.clone();
                    inner.insert(name.clone(), Expr::Number((start + index).to_string()));
                    let mut locals = BTreeSet::from([name.clone()]);
                    let lowered =
                        self.statements(body, namespace, origin, &mut inner, &mut locals)?;
                    output.extend(scoped_group(lowered, span));
                }
                Ok(output)
            }
            StatementKind::Repeat { count, body } => {
                let mut output = Vec::new();
                for _ in 0..*count {
                    self.work(origin, span)?;
                    output.extend(self.block(body, namespace, origin, values, span)?);
                }
                Ok(output)
            }
            StatementKind::Function { .. } => Err(at(
                origin,
                span,
                "functions may be declared only at script top level",
            )),
        }
    }

    fn call(
        &mut self,
        call: &Call,
        namespace: &str,
        origin: &str,
        values: &Values,
    ) -> Result<Vec<Statement>> {
        let bound = values.keys().cloned().collect();
        if let Some(name) = resolve(self.session, call, namespace, &bound) {
            if self.calls >= MAX_CALL_DEPTH {
                return Err(at(origin, call.span, "function call depth exceeds 32"));
            }
            let function = self.session.functions[&name].clone();
            if function.params.iter().any(|name| name == "touchpad") {
                return Err(at(
                    origin,
                    call.span,
                    "touchpad is a reserved global receiver",
                ));
            }
            let arguments = self.arguments(call, values, origin)?;
            let mut inner = bind_arguments(
                &function.params,
                &arguments.args,
                &Values::new(),
                origin,
                call.span,
            )?;
            let mut locals = function.params.iter().cloned().collect();
            self.calls += 1;
            let result = self.statements(
                &function.body,
                &function.namespace,
                &function.origin,
                &mut inner,
                &mut locals,
            );
            self.calls -= 1;
            return result.map(|body| scoped_group(body, call.span));
        }
        let mut lowered = self.arguments(call, values, origin)?;
        if let Some(receiver) = &call.receiver {
            match values.get(receiver) {
                Some(Expr::Identifier(mouse)) => lowered.receiver = Some(mouse.clone()),
                Some(_) => {
                    return Err(at(
                        origin,
                        call.span,
                        format!("receiver {receiver:?} is a scalar, not a mouse"),
                    ));
                }
                None if builtin(&call_name(call)) || receiver == "touchpad" => (),
                None => {
                    return Err(at(
                        origin,
                        call.span,
                        format!("unknown or out-of-scope receiver {receiver:?}"),
                    ));
                }
            }
        } else if !builtin(&call.name) {
            return Err(at(
                origin,
                call.span,
                format!("unknown function {:?}", call.name),
            ));
        }
        self.emit(&lowered, origin)?;
        Ok(vec![Statement {
            span: call.span,
            kind: StatementKind::Call(lowered),
        }])
    }

    fn arguments(&mut self, call: &Call, values: &Values, origin: &str) -> Result<Call> {
        let mut lowered = call.clone();
        for argument in &mut lowered.args {
            argument.value = self.expression(&argument.value, values, origin, argument.span)?;
        }
        Ok(lowered)
    }

    fn range_endpoint(
        &mut self,
        value: &Expr,
        values: &Values,
        origin: &str,
        span: Span,
    ) -> Result<i64> {
        let value = numeric(&self.expression(value, values, origin, span)?, origin, span)?;
        if value.fract() != 0.0 || value.abs() > 9_007_199_254_740_991.0 {
            return Err(at(
                origin,
                span,
                "for endpoints must be exact integers within +/-9007199254740991",
            ));
        }
        Ok(value as i64)
    }

    fn expression(
        &mut self,
        value: &Expr,
        values: &Values,
        origin: &str,
        span: Span,
    ) -> Result<Expr> {
        self.work(origin, span)?;
        let result = match value {
            Expr::Number(_) | Expr::Bool(_) | Expr::String(_) => value.clone(),
            Expr::Identifier(name) => values.get(name).cloned().ok_or_else(|| {
                at(
                    origin,
                    span,
                    format!("unknown or out-of-scope variable {name:?}; quote literal text"),
                )
            })?,
            Expr::Neg(value) => {
                let value = numeric(&self.expression(value, values, origin, span)?, origin, span)?;
                finite(-value, origin, span)?
            }
            Expr::Binary { left, op, right } => {
                let left = numeric(&self.expression(left, values, origin, span)?, origin, span)?;
                let right = numeric(&self.expression(right, values, origin, span)?, origin, span)?;
                match op {
                    BinaryOp::Eq => Expr::Bool(left == right),
                    BinaryOp::Ne => Expr::Bool(left != right),
                    BinaryOp::Lt => Expr::Bool(left < right),
                    BinaryOp::Le => Expr::Bool(left <= right),
                    BinaryOp::Gt => Expr::Bool(left > right),
                    BinaryOp::Ge => Expr::Bool(left >= right),
                    BinaryOp::Div | BinaryOp::Rem if right == 0.0 => {
                        return Err(at(origin, span, "division or remainder by zero"));
                    }
                    BinaryOp::Add => finite(left + right, origin, span)?,
                    BinaryOp::Sub => finite(left - right, origin, span)?,
                    BinaryOp::Mul => finite(left * right, origin, span)?,
                    BinaryOp::Div => finite(left / right, origin, span)?,
                    BinaryOp::Rem => finite(left % right, origin, span)?,
                }
            }
            Expr::Call(call)
                if call.receiver.as_deref() == Some("math") && !values.contains_key("math") =>
            {
                self.math(call, values, origin)?
            }
            Expr::Call(_) => {
                return Err(at(
                    origin,
                    span,
                    "only pure math helpers return values; desktop and user functions are statements",
                ));
            }
        };
        if let Expr::String(text) = &result {
            self.value_bytes = self.value_bytes.saturating_add(text.len());
            if self.value_bytes > MAX_VALUE_BYTES {
                return Err(at(
                    origin,
                    span,
                    "scalar evaluation exceeds the 16 MiB string budget",
                ));
            }
        }
        Ok(result)
    }

    fn math(&mut self, call: &Call, values: &Values, origin: &str) -> Result<Expr> {
        let arity = match call.name.as_str() {
            "pi" => 0,
            "sin" | "cos" | "sqrt" | "abs" | "floor" | "ceil" | "round" => 1,
            "min" | "max" => 2,
            _ => {
                return Err(at(
                    origin,
                    call.span,
                    format!("unknown pure math helper {:?}", call.name),
                ));
            }
        };
        if call.args.len() != arity || call.args.iter().any(|arg| arg.name.is_some()) {
            return Err(at(
                origin,
                call.span,
                format!(
                    "math.{} requires {arity} positional numeric arguments",
                    call.name
                ),
            ));
        }
        let mut args = Vec::with_capacity(arity);
        for argument in &call.args {
            args.push(numeric(
                &self.expression(&argument.value, values, origin, argument.span)?,
                origin,
                argument.span,
            )?);
        }
        let result = match call.name.as_str() {
            "pi" => std::f64::consts::PI,
            "sin" => args[0].sin(),
            "cos" => args[0].cos(),
            "sqrt" => args[0].sqrt(),
            "abs" => args[0].abs(),
            "floor" => args[0].floor(),
            "ceil" => args[0].ceil(),
            "round" => args[0].round(),
            "min" => args[0].min(args[1]),
            "max" => args[0].max(args[1]),
            _ => unreachable!(),
        };
        finite(result, origin, call.span)
    }
}

fn numeric(value: &Expr, origin: &str, span: Span) -> Result<f64> {
    match value {
        Expr::Number(value) => value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| at(origin, span, "numeric value must be finite")),
        _ => Err(at(
            origin,
            span,
            "arithmetic and numeric comparisons require numbers",
        )),
    }
}

fn finite(value: f64, origin: &str, span: Span) -> Result<Expr> {
    if !value.is_finite() {
        return Err(at(
            origin,
            span,
            "numeric result is nonfinite or outside the math helper's domain",
        ));
    }
    Ok(Expr::Number(if value == 0.0 {
        "0".into()
    } else {
        value.to_string()
    }))
}

fn scoped_group(body: Vec<Statement>, span: Span) -> Vec<Statement> {
    // Scalar names already have lexical lifetimes in evaluation. Only newly
    // owned mouse handles need an executor scope; references remain borrowed.
    if body
        .iter()
        .any(|statement| matches!(statement.kind, StatementKind::Let { .. }))
    {
        vec![Statement {
            span,
            kind: StatementKind::Block(body),
        }]
    } else {
        body
    }
}
