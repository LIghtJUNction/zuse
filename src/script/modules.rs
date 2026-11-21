//! Explicit module loading and bounded, transactional function expansion.
//! Loading parses data only. The caller validates the complete prepared source
//! with the selected executor before replacing its session with next_session.
use super::syntax::{self, Argument, Call, Expr, Span, Statement, StatementKind};
use crate::{json, runtime::Options};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::OnceLock,
};

type Result<T> = std::result::Result<T, String>;
const MAX_MODULES: usize = 64;
const MAX_FUNCTIONS: usize = 256;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_IMPORT_DEPTH: usize = 16;
const MAX_CALL_DEPTH: usize = 32;
const MAX_DESCRIPTION_BYTES: usize = 4096;

/// Callable declarations retained in one session, without executable bodies.
/// Parameter names preserve declaration order. Descriptions are explicit source
/// documentation, not inferred types, permissions or execution guarantees.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionMetadata {
    pub name: String,
    pub parameters: Vec<String>,
    pub description: String,
    pub source: String,
    pub line: usize,
    pub column: usize,
}

impl FunctionMetadata {
    pub fn to_json(&self) -> String {
        let parameters = self
            .parameters
            .iter()
            .map(|p| json::quote(p))
            .collect::<Vec<_>>();
        format!(
            "{{\"name\":{},\"parameters\":[{}],\"description\":{},\"source\":{},\"line\":{},\"column\":{}}}",
            json::quote(&self.name),
            parameters.join(","),
            json::quote(&self.description),
            json::quote(&self.source),
            self.line,
            self.column,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramMode {
    Flat,
    Scoped,
}

/// A preparation is not a commit. Failed runtime/scoped preflight must discard
/// next_session, including definitions supplied by an inline eval.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub source: String,
    pub next_session: ModuleSession,
    pub mode: ProgramMode,
}

impl Prepared {
    /// Validate a prepared scoped program without loading its modules again.
    pub fn compile_scoped(&self, options: Options) -> Result<super::CompiledScript> {
        if self.mode != ProgramMode::Scoped {
            return Err("prepared program uses flat Runtime commands".into());
        }
        super::compile::compile(&self.source, options)
    }
}

#[derive(Clone, Debug)]
struct Function {
    description: String,
    params: Vec<String>,
    body: Vec<Statement>,
    namespace: String,
    origin: String,
    span: Span,
    canonical: String,
}

impl Function {
    fn metadata(&self, name: &str) -> FunctionMetadata {
        FunctionMetadata {
            name: name.into(),
            parameters: self.params.clone(),
            description: self.description.clone(),
            source: self.origin.clone(),
            line: self.span.line,
            column: self.span.column,
        }
    }
}

#[derive(Clone, Debug)]
struct Module {
    identity: String,
    source: String,
}

/// Definitions are local to one Runtime/session. There is no global search
/// path, shell expansion, network import or automatic filesystem reload.
#[derive(Clone, Debug, Default)]
pub struct ModuleSession {
    functions: BTreeMap<String, Function>,
    modules: BTreeMap<String, Module>,
}

#[derive(Clone)]
struct Item {
    statement: Statement,
    namespace: String,
    origin: String,
}

impl ModuleSession {
    pub fn functions(&self) -> Vec<String> {
        self.functions.keys().cloned().collect()
    }

    /// Inspect retained declarations without reopening files or executing bodies.
    pub fn function_metadata(&self) -> Vec<FunctionMetadata> {
        self.functions
            .iter()
            .map(|(name, function)| function.metadata(name))
            .collect()
    }

    /// An empty namespace selects all functions; a namespace selects descendants
    /// at a dot boundary. Results are sorted by fully qualified function name.
    pub fn functions_json(&self, namespace: &str) -> String {
        let prefix = format!("{namespace}.");
        let functions = self
            .functions
            .iter()
            .filter(|(name, _)| namespace.is_empty() || name.starts_with(&prefix))
            .map(|(name, function)| function.metadata(name).to_json())
            .collect::<Vec<_>>();
        format!(
            "{{\"functions\":[{}],\"count\":{}}}",
            functions.join(","),
            functions.len()
        )
    }

    /// Expand a flat program, or identify a scoped mouse program while keeping
    /// its borrowed-handle function boundaries intact.
    pub fn prepare(&self, source: &str, options: Options) -> Result<Prepared> {
        self.prepare_mode(source, options, false, None)
    }

    /// Resolve entry-point relative imports against its directory without
    /// changing the process working directory.
    pub fn prepare_with_base(
        &self,
        source: &str,
        options: Options,
        base: &Path,
    ) -> Result<Prepared> {
        self.prepare_mode(source, options, false, Some(base))
    }

    /// Preserve scoped mouse functions for the ownership-checking compiler.
    pub fn prepare_scoped(&self, source: &str, options: Options) -> Result<Prepared> {
        self.prepare_mode(source, options, true, None)
    }

    pub fn prepare_scoped_with_base(
        &self,
        source: &str,
        options: Options,
        base: &Path,
    ) -> Result<Prepared> {
        self.prepare_mode(source, options, true, Some(base))
    }

    fn prepare_mode(
        &self,
        source: &str,
        options: Options,
        force_scoped: bool,
        base: Option<&Path>,
    ) -> Result<Prepared> {
        let (program, descriptions) =
            syntax::parse_module_documented(source).map_err(|error| format!("<input>: {error}"))?;
        let had_directives = program.statements.iter().any(is_directive);
        let mut loader = Loader {
            next: self.clone(),
            read_bytes: source.len(),
            files: Vec::new(),
            depth: 0,
        };
        let items = loader.load(
            program.statements,
            &descriptions,
            "",
            "<input>",
            base,
            false,
        )?;
        loader.next.check_limits()?;
        validate_graph(&loader.next)?;
        let reachable = reachable_functions(&loader.next, &items)?;
        let scoped = force_scoped
            || items
                .iter()
                .any(|item| statements_are_scoped(std::slice::from_ref(&item.statement), &[]))
            || reachable.iter().any(|name| {
                let function = &loader.next.functions[name];
                statements_are_scoped(&function.body, &function.params)
            });
        let mode = if scoped {
            ProgramMode::Scoped
        } else {
            ProgramMode::Flat
        };
        let expanded = if force_scoped && !had_directives && self.functions.is_empty() {
            // Keep old source, spans, ownership and parser diagnostics exactly
            // as they were when no module mechanism participates.
            source.to_owned()
        } else if scoped {
            scoped_source(&loader.next, &items, &reachable)?
        } else {
            let mut expansion = Expansion {
                session: &loader.next,
                options,
                stack: Vec::new(),
                output: Output::default(),
                steps: 0,
                work: 0,
            };
            for item in &items {
                expansion.statements(
                    std::slice::from_ref(&item.statement),
                    &item.namespace,
                    &item.origin,
                    &BTreeMap::new(),
                )?;
            }
            expansion.output.text
        };
        Ok(Prepared {
            source: expanded,
            next_session: loader.next,
            mode,
        })
    }

    fn check_limits(&self) -> Result<()> {
        if self.modules.len() > MAX_MODULES {
            return Err(format!("module count exceeds {MAX_MODULES}"));
        }
        if self.functions.len() > MAX_FUNCTIONS {
            return Err(format!("function count exceeds {MAX_FUNCTIONS}"));
        }
        let bytes = self
            .modules
            .values()
            .map(|module| module.source.len())
            .sum::<usize>()
            .saturating_add(
                self.functions
                    .values()
                    .map(|function| function.canonical.len() + function.description.len())
                    .sum::<usize>(),
            );
        if bytes > MAX_SOURCE_BYTES {
            return Err("retained module source exceeds 4 MiB".into());
        }
        Ok(())
    }

    fn remove_namespace(&mut self, namespace: &str) {
        let prefix = format!("{namespace}.");
        self.functions
            .retain(|name, _| name != namespace && !name.starts_with(&prefix));
        self.modules
            .retain(|name, _| name != namespace && !name.starts_with(&prefix));
    }
}

/// Inspect syntax, never text inside quoted values, when choosing the existing
/// scoped engine. Prepared.mode also sees functions reached through imports.
pub fn uses_scoped_syntax(source: &str) -> Result<bool> {
    let program = syntax::parse_module(source).map_err(|error| format!("<input>: {error}"))?;
    Ok(statements_are_scoped(&program.statements, &[]))
}

fn statements_are_scoped(statements: &[Statement], params: &[String]) -> bool {
    statements.iter().any(|statement| match &statement.kind {
        StatementKind::Let { .. } | StatementKind::Parallel(_) => true,
        StatementKind::Block(body)
        | StatementKind::Sequence(body)
        | StatementKind::Repeat { body, .. } => statements_are_scoped(body, params),
        StatementKind::Function { params, body, .. } => statements_are_scoped(body, params),
        StatementKind::Call(call) => {
            (call.receiver.is_none() && matches!(call.name.as_str(), "mouse" | "user_mouse"))
                || call.receiver.as_ref().is_some_and(|receiver| {
                    params.contains(receiver)
                        && matches!(
                            call.name.as_str(),
                            "move"
                                | "move_to"
                                | "click"
                                | "down"
                                | "up"
                                | "scroll"
                                | "appearance"
                                | "position"
                        )
                })
        }
    })
}

struct Loader {
    next: ModuleSession,
    read_bytes: usize,
    files: Vec<PathBuf>,
    depth: usize,
}

impl Loader {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.read_bytes = self
            .read_bytes
            .checked_add(bytes)
            .ok_or("module source byte count overflow")?;
        if self.read_bytes > MAX_SOURCE_BYTES {
            return Err("one preparation may read at most 4 MiB of module source".into());
        }
        Ok(())
    }

    fn load(
        &mut self,
        statements: Vec<Statement>,
        descriptions: &BTreeMap<usize, String>,
        namespace: &str,
        origin: &str,
        base: Option<&Path>,
        declarations_only: bool,
    ) -> Result<Vec<Item>> {
        if self.depth >= MAX_IMPORT_DEPTH {
            return Err(format!(
                "{origin}: import/eval depth exceeds {MAX_IMPORT_DEPTH}"
            ));
        }
        self.depth += 1;
        let result = (|| {
            let mut items = Vec::new();
            for statement in statements {
                match &statement.kind {
                    StatementKind::Function { name, params, body } => {
                        valid_name(name).map_err(|error| at(origin, statement.span, error))?;
                        for parameter in params {
                            valid_name(parameter)
                                .map_err(|error| at(origin, statement.span, error))?;
                        }
                        if name.starts_with("__zuse_module_")
                            || matches!(
                                name.as_str(),
                                "import" | "eval" | "mouse" | "user_mouse" | "touchpad"
                            )
                        {
                            return Err(at(
                                origin,
                                statement.span,
                                format!("function name {name:?} is reserved"),
                            ));
                        }
                        let key = qualify(namespace, name)?;
                        if builtin(&key) {
                            return Err(at(
                                origin,
                                statement.span,
                                format!(
                                    "function name {key:?} is reserved for a built-in operation"
                                ),
                            ));
                        }
                        validate_body(body, origin)?;
                        let mut output = Output::default();
                        write_function(&mut output, name, params, body)?;
                        let description = descriptions
                            .get(&statement.span.start)
                            .cloned()
                            .unwrap_or_default();
                        if description.len() > MAX_DESCRIPTION_BYTES {
                            return Err(at(
                                origin,
                                statement.span,
                                "function description exceeds 4096 bytes",
                            ));
                        }
                        let function = Function {
                            description,
                            params: params.clone(),
                            body: body.clone(),
                            namespace: namespace.into(),
                            origin: origin.into(),
                            span: statement.span,
                            canonical: output.text,
                        };
                        if let Some(existing) = self.next.functions.get(&key) {
                            if existing.canonical != function.canonical
                                || existing.description != function.description
                            {
                                return Err(at(
                                    origin,
                                    statement.span,
                                    format!(
                                        "function {key:?} already exists; use an explicit namespaced replace=true import/eval"
                                    ),
                                ));
                            }
                        } else {
                            self.next.functions.insert(key, function);
                        }
                        self.next.check_limits()?;
                    }
                    StatementKind::Call(call) if directive(call).is_some() => {
                        items.extend(self.directive(
                            call,
                            namespace,
                            origin,
                            base,
                            declarations_only,
                        )?);
                    }
                    _ if declarations_only => {
                        return Err(at(
                            origin,
                            statement.span,
                            "imported modules may contain only function declarations and declarative imports/evals; use an explicit top-level eval to execute actions",
                        ));
                    }
                    _ => {
                        validate_body(std::slice::from_ref(&statement), origin)?;
                        items.push(Item {
                            statement,
                            namespace: namespace.into(),
                            origin: origin.into(),
                        });
                    }
                }
            }
            Ok(items)
        })();
        self.depth -= 1;
        result
    }

    fn directive(
        &mut self,
        call: &Call,
        namespace: &str,
        origin: &str,
        base: Option<&Path>,
        declarations_only: bool,
    ) -> Result<Vec<Item>> {
        let kind = directive(call).unwrap();
        let mut values = BTreeMap::new();
        for argument in &call.args {
            let name = argument.name.as_deref().ok_or_else(|| {
                at(
                    origin,
                    argument.span,
                    "import/eval arguments must be named literals",
                )
            })?;
            if !matches!(name, "path" | "source" | "as" | "replace") {
                return Err(at(
                    origin,
                    argument.span,
                    format!("unknown {kind} argument {name:?}"),
                ));
            }
            if values.insert(name, &argument.value).is_some() {
                return Err(at(
                    origin,
                    argument.span,
                    format!("duplicate {kind} argument {name:?}"),
                ));
            }
        }
        let text = |name: &str| -> Result<Option<String>> {
            match values.get(name) {
                None => Ok(None),
                Some(Expr::String(value)) if !value.contains('\0') => Ok(Some(value.clone())),
                _ => Err(at(
                    origin,
                    call.span,
                    format!("{kind} {name} must be a literal, NUL-free string"),
                )),
            }
        };
        let path = text("path")?;
        let source = text("source")?;
        let alias = text("as")?;
        let replace = match values.get("replace") {
            None => false,
            Some(Expr::Bool(value)) => *value,
            _ => return Err(at(origin, call.span, "replace must be a literal boolean")),
        };
        if path.is_some() == source.is_some() || (kind == "eval" && path.is_some()) {
            return Err(at(
                origin,
                call.span,
                if kind == "eval" {
                    "eval requires exactly source=..."
                } else {
                    "import requires exactly one of path=... or source=..."
                },
            ));
        }
        if replace && alias.is_none() {
            return Err(at(
                origin,
                call.span,
                "replace=true requires an explicit as namespace",
            ));
        }
        if let Some(alias) = &alias {
            valid_name(alias).map_err(|error| at(origin, call.span, error))?;
        }
        if let Some(path) = path {
            if path.is_empty()
                || path.len() > 4096
                || path.chars().any(char::is_control)
                || path.contains("://")
            {
                return Err(at(
                    origin,
                    call.span,
                    "import path must be a literal local file path of 1..4096 bytes",
                ));
            }
            let requested = base.map_or_else(|| PathBuf::from(&path), |base| base.join(&path));
            let canonical = requested.canonicalize().map_err(|error| {
                at(
                    origin,
                    call.span,
                    format!("import {}: {error}", requested.display()),
                )
            })?;
            if self.files.contains(&canonical) {
                let mut chain = self
                    .files
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>();
                chain.push(canonical.display().to_string());
                return Err(at(
                    origin,
                    call.span,
                    format!("cyclic import: {}", chain.join(" -> ")),
                ));
            }
            let metadata = canonical
                .metadata()
                .map_err(|error| format!("import {}: {error}", canonical.display()))?;
            if !metadata.is_file() {
                return Err(format!(
                    "import {}: module source must be a regular file",
                    canonical.display()
                ));
            }
            if metadata.len() > syntax::MAX_SCRIPT_BYTES as u64 {
                return Err(format!(
                    "import {}: module source exceeds 1 MiB",
                    canonical.display()
                ));
            }
            let mut bytes = Vec::new();
            File::open(&canonical)
                .and_then(|file| {
                    file.take(syntax::MAX_SCRIPT_BYTES as u64 + 1)
                        .read_to_end(&mut bytes)
                })
                .map_err(|error| format!("import {}: {error}", canonical.display()))?;
            if bytes.len() > syntax::MAX_SCRIPT_BYTES {
                return Err(format!(
                    "import {}: module source exceeds 1 MiB",
                    canonical.display()
                ));
            }
            self.charge(bytes.len())?;
            let source = String::from_utf8(bytes).map_err(|_| {
                format!("import {}: module source is not UTF-8", canonical.display())
            })?;
            let alias = alias
                .or_else(|| {
                    canonical
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
                .ok_or_else(|| {
                    at(
                        origin,
                        call.span,
                        "module filename needs an explicit as namespace",
                    )
                })?;
            valid_name(&alias).map_err(|error| at(origin, call.span, error))?;
            let child_namespace = qualify(namespace, &alias)?;
            let label = canonical.display().to_string();
            let fresh = self.module(&child_namespace, &label, &source, replace)?;
            if !fresh {
                return Ok(Vec::new());
            }
            let (program, descriptions) = syntax::parse_module_documented(&source)
                .map_err(|error| format!("{label}: {error}"))?;
            self.files.push(canonical.clone());
            let loaded = self.load(
                program.statements,
                &descriptions,
                &child_namespace,
                &label,
                canonical.parent(),
                true,
            );
            self.files.pop();
            loaded
        } else {
            let source = source.unwrap();
            self.charge(source.len())?;
            let child_namespace = alias
                .as_ref()
                .map(|alias| qualify(namespace, alias))
                .transpose()?
                .unwrap_or_else(|| namespace.into());
            let label = format!(
                "<{} at {}:{}:{}>",
                kind, origin, call.span.line, call.span.column
            );
            if alias.is_some() {
                let fresh = self.module(&child_namespace, "<inline>", &source, replace)?;
                if !fresh && kind == "import" {
                    return Ok(Vec::new());
                }
            }
            let (program, descriptions) = syntax::parse_module_documented(&source)
                .map_err(|error| format!("{label}: {error}"))?;
            self.load(
                program.statements,
                &descriptions,
                &child_namespace,
                &label,
                base,
                declarations_only || kind == "import",
            )
        }
    }

    fn module(
        &mut self,
        namespace: &str,
        identity: &str,
        source: &str,
        replace: bool,
    ) -> Result<bool> {
        if let Some(existing) = self.next.modules.get(namespace) {
            if !replace && existing.identity == identity && existing.source == source {
                return Ok(false);
            }
            if !replace {
                return Err(format!(
                    "module namespace {namespace:?} already exists; use replace=true explicitly"
                ));
            }
        }
        if replace {
            self.next.remove_namespace(namespace);
        }
        self.next.modules.insert(
            namespace.into(),
            Module {
                identity: identity.into(),
                source: source.into(),
            },
        );
        self.next.check_limits()?;
        Ok(true)
    }
}

fn is_directive(statement: &Statement) -> bool {
    matches!(&statement.kind, StatementKind::Call(call) if directive(call).is_some())
}

fn directive(call: &Call) -> Option<&str> {
    if call.receiver.is_none() && matches!(call.name.as_str(), "import" | "eval") {
        Some(&call.name)
    } else {
        None
    }
}

fn valid_name(name: &str) -> Result<()> {
    let mut bytes = name.bytes();
    if name.len() > syntax::MAX_NAME_BYTES
        || !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        || bytes.any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
    {
        return Err("module/function names must be ASCII identifiers of at most 128 bytes".into());
    }
    if matches!(
        name,
        "let" | "fn" | "parallel" | "sequence" | "serial" | "repeat" | "true" | "false"
    ) {
        return Err(format!(
            "module/function name {name:?} is a reserved keyword"
        ));
    }
    Ok(())
}

fn qualify(namespace: &str, name: &str) -> Result<String> {
    let result = if namespace.is_empty() {
        name.into()
    } else {
        format!("{namespace}.{name}")
    };
    if result.len() > syntax::MAX_NAME_BYTES {
        return Err("qualified module/function name exceeds 128 bytes".into());
    }
    Ok(result)
}

fn call_name(call: &Call) -> String {
    call.receiver.as_ref().map_or_else(
        || call.name.clone(),
        |receiver| format!("{receiver}.{}", call.name),
    )
}

fn builtin(name: &str) -> bool {
    static NAMES: OnceLock<BTreeSet<String>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            crate::browser::with_json(&crate::runtime::schema(), |root| {
                let mut names = BTreeSet::new();
                for entry in root
                    .get("commands")
                    .ok_or("command schema has no commands")?
                    .elements()
                {
                    if let Some(name) = entry.get("name").and_then(|value| value.string()) {
                        names.insert(name.chars().collect());
                    }
                }
                Ok(names)
            })
            .expect("built-in command schema is valid JSON")
        })
        .contains(name)
}

fn at(origin: &str, span: Span, message: impl std::fmt::Display) -> String {
    format!(
        "{origin}: {message} at {}:{} (byte {})",
        span.line, span.column, span.start
    )
}

fn validate_body(statements: &[Statement], origin: &str) -> Result<()> {
    for statement in statements {
        match &statement.kind {
            StatementKind::Function { .. } => {
                return Err(at(
                    origin,
                    statement.span,
                    "functions may be declared only at script top level",
                ));
            }
            StatementKind::Call(call) => validate_expression(&Expr::Call(call.clone()), origin)?,
            StatementKind::Let { value, .. } => validate_expression(value, origin)?,
            StatementKind::Block(body)
            | StatementKind::Parallel(body)
            | StatementKind::Sequence(body)
            | StatementKind::Repeat { body, .. } => validate_body(body, origin)?,
        }
    }
    Ok(())
}

fn validate_expression(expression: &Expr, origin: &str) -> Result<()> {
    if let Expr::Call(call) = expression {
        if directive(call).is_some() {
            return Err(at(
                origin,
                call.span,
                "import/eval must be an explicit top-level statement",
            ));
        }
        for argument in &call.args {
            validate_expression(&argument.value, origin)?;
        }
    }
    Ok(())
}

fn visit_expression_calls(
    expression: &Expr,
    bound: &BTreeSet<String>,
    visitor: &mut impl FnMut(&Call, &BTreeSet<String>) -> Result<()>,
) -> Result<()> {
    if let Expr::Call(call) = expression {
        visitor(call, bound)?;
        for argument in &call.args {
            visit_expression_calls(&argument.value, bound, visitor)?;
        }
    }
    Ok(())
}

/// Follow the compiler's lexical scopes and declaration order when resolving
/// module calls. Each parallel statement has its own branch-local scope.
fn visit_calls(
    statements: &[Statement],
    bound: &mut BTreeSet<String>,
    visitor: &mut impl FnMut(&Call, &BTreeSet<String>) -> Result<()>,
) -> Result<()> {
    for statement in statements {
        match &statement.kind {
            StatementKind::Call(call) => {
                visitor(call, bound)?;
                for argument in &call.args {
                    visit_expression_calls(&argument.value, bound, visitor)?;
                }
            }
            StatementKind::Let { name, value } => {
                visit_expression_calls(value, bound, visitor)?;
                bound.insert(name.clone());
            }
            StatementKind::Block(body)
            | StatementKind::Sequence(body)
            | StatementKind::Repeat { body, .. } => {
                visit_calls(body, &mut bound.clone(), visitor)?;
            }
            StatementKind::Parallel(branches) => {
                for branch in branches {
                    visit_calls(std::slice::from_ref(branch), &mut bound.clone(), visitor)?;
                }
            }
            StatementKind::Function { .. } => {}
        }
    }
    Ok(())
}

fn resolve(
    session: &ModuleSession,
    call: &Call,
    namespace: &str,
    bound: &BTreeSet<String>,
) -> Option<String> {
    if call
        .receiver
        .as_ref()
        .is_some_and(|receiver| bound.contains(receiver))
    {
        return None;
    }
    let name = call_name(call);
    if !namespace.is_empty() {
        let local = format!("{namespace}.{name}");
        if session.functions.contains_key(&local) {
            return Some(local);
        }
    }
    session.functions.contains_key(&name).then_some(name)
}

fn validate_graph(session: &ModuleSession) -> Result<()> {
    fn visit(
        name: &str,
        session: &ModuleSession,
        stack: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<()> {
        if stack.iter().any(|active| active == name) {
            let function = &session.functions[name];
            return Err(at(
                &function.origin,
                function.span,
                format!("recursive function cycle: {} -> {name}", stack.join(" -> ")),
            ));
        }
        if done.contains(name) {
            return Ok(());
        }
        if stack.len() >= MAX_CALL_DEPTH {
            let function = &session.functions[name];
            return Err(at(
                &function.origin,
                function.span,
                format!("function call depth exceeds {MAX_CALL_DEPTH}: {name}"),
            ));
        }
        stack.push(name.into());
        let function = &session.functions[name];
        let mut bound = function.params.iter().cloned().collect();
        visit_calls(&function.body, &mut bound, &mut |call, bound| {
            if let Some(next) = resolve(session, call, &function.namespace, bound) {
                visit(&next, session, stack, done)?;
            }
            Ok(())
        })?;
        stack.pop();
        done.insert(name.into());
        Ok(())
    }
    let mut done = BTreeSet::new();
    for name in session.functions.keys() {
        visit(name, session, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

fn reachable_functions(session: &ModuleSession, items: &[Item]) -> Result<BTreeSet<String>> {
    fn visit(
        statements: &[Statement],
        namespace: &str,
        origin: &str,
        bound: &mut BTreeSet<String>,
        session: &ModuleSession,
        reached: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_CALL_DEPTH {
            let span = statements
                .first()
                .map(|statement| statement.span)
                .unwrap_or(Span {
                    start: 0,
                    end: 0,
                    line: 1,
                    column: 1,
                });
            return Err(at(
                origin,
                span,
                format!("function call depth exceeds {MAX_CALL_DEPTH}"),
            ));
        }
        visit_calls(statements, bound, &mut |call, bound| {
            if let Some(name) = resolve(session, call, namespace, bound)
                && reached.insert(name.clone())
            {
                let function = &session.functions[&name];
                visit(
                    &function.body,
                    &function.namespace,
                    &function.origin,
                    &mut function.params.iter().cloned().collect(),
                    session,
                    reached,
                    depth + 1,
                )?;
            }
            Ok(())
        })
    }
    let mut reached = BTreeSet::new();
    let mut bound = BTreeSet::new();
    for item in items {
        visit(
            std::slice::from_ref(&item.statement),
            &item.namespace,
            &item.origin,
            &mut bound,
            session,
            &mut reached,
            0,
        )?;
    }
    Ok(reached)
}

#[derive(Default)]
struct Output {
    text: String,
}
impl Output {
    fn push(&mut self, text: &str) -> Result<()> {
        if self.text.len().saturating_add(text.len()) > syntax::MAX_SCRIPT_BYTES {
            return Err("expanded module source exceeds 1 MiB".into());
        }
        self.text.push_str(text);
        Ok(())
    }
}

fn write_expression(output: &mut Output, expression: &Expr) -> Result<()> {
    match expression {
        Expr::Number(number) | Expr::Identifier(number) => output.push(number),
        Expr::String(text) => output.push(&json::quote(text)),
        Expr::Bool(value) => output.push(if *value { "true" } else { "false" }),
        Expr::Call(call) => write_call(output, call),
    }
}

fn write_call(output: &mut Output, call: &Call) -> Result<()> {
    output.push(&call_name(call))?;
    output.push("(")?;
    for (index, argument) in call.args.iter().enumerate() {
        if index != 0 {
            output.push(",")?;
        }
        if let Some(name) = &argument.name {
            output.push(name)?;
            output.push("=")?;
        }
        write_expression(output, &argument.value)?;
    }
    output.push(")")
}

fn write_function(
    output: &mut Output,
    name: &str,
    params: &[String],
    body: &[Statement],
) -> Result<()> {
    output.push("fn ")?;
    output.push(name)?;
    output.push("(")?;
    output.push(&params.join(","))?;
    output.push("){\n")?;
    write_statements(output, body)?;
    output.push("}\n")
}

fn write_statements(output: &mut Output, statements: &[Statement]) -> Result<()> {
    for statement in statements {
        match &statement.kind {
            StatementKind::Call(call) => {
                write_call(output, call)?;
                output.push(";\n")?;
            }
            StatementKind::Let { name, value } => {
                output.push("let ")?;
                output.push(name)?;
                output.push("=")?;
                write_expression(output, value)?;
                output.push(";\n")?;
            }
            StatementKind::Function { name, params, body } => {
                write_function(output, name, params, body)?
            }
            StatementKind::Block(body) => {
                output.push("{\n")?;
                write_statements(output, body)?;
                output.push("}\n")?;
            }
            StatementKind::Parallel(body) | StatementKind::Sequence(body) => {
                output.push(if matches!(statement.kind, StatementKind::Parallel(_)) {
                    "parallel{\n"
                } else {
                    "sequence{\n"
                })?;
                write_statements(output, body)?;
                output.push("}\n")?;
            }
            StatementKind::Repeat { count, body } => {
                output.push(&format!("repeat {count}{{\n"))?;
                write_statements(output, body)?;
                output.push("}\n")?;
            }
        }
    }
    Ok(())
}

fn scoped_source(
    session: &ModuleSession,
    items: &[Item],
    reachable: &BTreeSet<String>,
) -> Result<String> {
    let names = reachable
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), format!("__zuse_module_{index}")))
        .collect::<BTreeMap<_, _>>();
    fn rename_call(
        call: &Call,
        namespace: &str,
        bound: &BTreeSet<String>,
        session: &ModuleSession,
        names: &BTreeMap<String, String>,
    ) -> Call {
        let mut renamed = call.clone();
        if let Some(key) = resolve(session, call, namespace, bound)
            && let Some(name) = names.get(&key)
        {
            renamed.receiver = None;
            renamed.name.clone_from(name);
        }
        for argument in &mut renamed.args {
            rename_expression(&mut argument.value, namespace, bound, session, names);
        }
        renamed
    }
    fn rename_expression(
        expression: &mut Expr,
        namespace: &str,
        bound: &BTreeSet<String>,
        session: &ModuleSession,
        names: &BTreeMap<String, String>,
    ) {
        if let Expr::Call(call) = expression {
            *call = rename_call(call, namespace, bound, session, names);
        }
    }
    fn rename_statement(
        statement: &Statement,
        namespace: &str,
        bound: &mut BTreeSet<String>,
        session: &ModuleSession,
        names: &BTreeMap<String, String>,
    ) -> Statement {
        let mut result = statement.clone();
        match &mut result.kind {
            StatementKind::Call(call) => {
                *call = rename_call(call, namespace, bound, session, names)
            }
            StatementKind::Let { name, value } => {
                rename_expression(value, namespace, bound, session, names);
                bound.insert(name.clone());
            }
            StatementKind::Block(body)
            | StatementKind::Sequence(body)
            | StatementKind::Repeat { body, .. } => {
                *body = rename(body, namespace, &mut bound.clone(), session, names);
            }
            StatementKind::Parallel(branches) => {
                *branches = branches
                    .iter()
                    .map(|branch| {
                        rename_statement(branch, namespace, &mut bound.clone(), session, names)
                    })
                    .collect();
            }
            StatementKind::Function { .. } => {}
        }
        result
    }
    fn rename(
        statements: &[Statement],
        namespace: &str,
        bound: &mut BTreeSet<String>,
        session: &ModuleSession,
        names: &BTreeMap<String, String>,
    ) -> Vec<Statement> {
        statements
            .iter()
            .map(|statement| rename_statement(statement, namespace, bound, session, names))
            .collect()
    }
    let mut output = Output::default();
    for name in reachable {
        let function = &session.functions[name];
        let mut bound = function.params.iter().cloned().collect();
        let body = rename(
            &function.body,
            &function.namespace,
            &mut bound,
            session,
            &names,
        );
        write_function(&mut output, &names[name], &function.params, &body)?;
    }
    let mut bound = BTreeSet::new();
    for item in items {
        let statement = rename(
            std::slice::from_ref(&item.statement),
            &item.namespace,
            &mut bound,
            session,
            &names,
        );
        write_statements(&mut output, &statement)?;
    }
    Ok(output.text)
}

struct Expansion<'a> {
    session: &'a ModuleSession,
    options: Options,
    stack: Vec<String>,
    output: Output,
    steps: usize,
    work: usize,
}

impl Expansion<'_> {
    fn work(&mut self) -> Result<()> {
        self.work += 1;
        if self.work > 16_384 {
            Err("expanded module work exceeds 16384 nodes".into())
        } else {
            Ok(())
        }
    }

    fn statements(
        &mut self,
        statements: &[Statement],
        namespace: &str,
        origin: &str,
        values: &BTreeMap<String, Expr>,
    ) -> Result<()> {
        for statement in statements {
            self.work()?;
            match &statement.kind {
                StatementKind::Call(call) => self.call(call, namespace, origin, values)?,
                StatementKind::Block(body) | StatementKind::Sequence(body) => {
                    self.statements(body, namespace, origin, values)?
                }
                StatementKind::Repeat { count, body } => {
                    for _ in 0..*count {
                        self.work()?;
                        self.statements(body, namespace, origin, values)?;
                    }
                }
                StatementKind::Function { .. } => {
                    return Err(at(
                        origin,
                        statement.span,
                        "nested function declaration is not supported",
                    ));
                }
                StatementKind::Let { .. } | StatementKind::Parallel(_) => {
                    return Err(at(
                        origin,
                        statement.span,
                        "scoped mouse syntax requires the scoped script executor",
                    ));
                }
            }
        }
        Ok(())
    }

    fn call(
        &mut self,
        call: &Call,
        namespace: &str,
        origin: &str,
        values: &BTreeMap<String, Expr>,
    ) -> Result<()> {
        let bound = values.keys().cloned().collect();
        if let Some(name) = resolve(self.session, call, namespace, &bound) {
            if self.stack.contains(&name) {
                return Err(at(
                    origin,
                    call.span,
                    format!("recursive function {name:?} is not supported"),
                ));
            }
            if self.stack.len() >= MAX_CALL_DEPTH {
                return Err(at(origin, call.span, "function call depth exceeds 32"));
            }
            let function = self.session.functions[&name].clone();
            let arguments =
                bind_arguments(&function.params, &call.args, values, origin, call.span)?;
            self.stack.push(name);
            let result = self.statements(
                &function.body,
                &function.namespace,
                &function.origin,
                &arguments,
            );
            self.stack.pop();
            return result;
        }
        if self.steps >= self.options.max_steps {
            return Err(at(
                origin,
                call.span,
                format!("expanded step limit exceeded ({})", self.options.max_steps),
            ));
        }
        let mut expanded = call.clone();
        for argument in &mut expanded.args {
            argument.value = literal(&argument.value, values, origin, argument.span)?;
        }
        write_call(&mut self.output, &expanded)?;
        self.output.push(";\n")?;
        self.steps += 1;
        Ok(())
    }
}

fn literal(
    expression: &Expr,
    values: &BTreeMap<String, Expr>,
    origin: &str,
    span: Span,
) -> Result<Expr> {
    match expression {
        Expr::Identifier(name) => Ok(values.get(name).unwrap_or(expression).clone()),
        Expr::Call(_) => Err(at(
            origin,
            span,
            "module function arguments must be literals; functions do not return values",
        )),
        _ => Ok(expression.clone()),
    }
}

fn bind_arguments(
    params: &[String],
    args: &[Argument],
    outer: &BTreeMap<String, Expr>,
    origin: &str,
    span: Span,
) -> Result<BTreeMap<String, Expr>> {
    let mut result = BTreeMap::new();
    let mut positional = 0;
    for argument in args {
        let name = if let Some(name) = &argument.name {
            if !params.contains(name) {
                return Err(at(
                    origin,
                    argument.span,
                    format!("unknown function argument {name:?}"),
                ));
            }
            name
        } else {
            let name = params.get(positional).ok_or_else(|| {
                at(
                    origin,
                    argument.span,
                    "too many positional function arguments",
                )
            })?;
            positional += 1;
            name
        };
        let value = literal(&argument.value, outer, origin, argument.span)?;
        if result.insert(name.clone(), value).is_some() {
            return Err(at(
                origin,
                argument.span,
                format!("function argument {name:?} was supplied twice"),
            ));
        }
    }
    for parameter in params {
        if !result.contains_key(parameter) {
            return Err(at(
                origin,
                span,
                format!("missing function argument {parameter:?}"),
            ));
        }
    }
    Ok(result)
}
