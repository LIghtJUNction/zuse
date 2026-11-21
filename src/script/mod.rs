//! Scoped mouse programs: preflight first, then cooperative parallel execution.
mod compile;
pub mod interrupt;
mod modules;
mod plan;
mod scheduler;
pub mod syntax;

use crate::runtime::Options;
pub use modules::{ModuleSession, Prepared, ProgramMode, uses_scoped_syntax};
pub use plan::{CompiledScript, PlanSummary};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Library callers can cancel from another thread without installing signals.
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Resolve names, ownership, arguments and parallel conflicts without input.
pub fn compile(source: &str, options: Options) -> Result<CompiledScript, String> {
    let prepared = ModuleSession::default().prepare_scoped(source, options)?;
    compile::compile(&prepared.source, options)
}

pub fn check(source: &str, options: Options) -> Result<String, String> {
    let plan = compile(source, options)?;
    Ok(format!(
        "{{\"status\":\"planned\",\"language\":\"zuse script\",\"plan\":{}}}",
        plan.summary.to_json()
    ))
}

pub fn execute(source: &str, options: Options) -> Result<String, String> {
    execute_with_cancel(source, options, || false)
}

/// Cancelled and failed runs close every live mouse before returning their
/// JSON receipt. This does not install process-wide signal handlers.
pub fn execute_with_cancel<F: FnMut() -> bool>(
    source: &str,
    options: Options,
    cancel: F,
) -> Result<String, String> {
    let plan = compile(source, options)?;
    execute_plan(plan, options, cancel)
}

/// Run the already prepared scoped source after its full ownership/budget
/// preflight; imported files are not read a second time.
pub fn execute_prepared_with_cancel<F: FnMut() -> bool>(
    prepared: &Prepared,
    options: Options,
    cancel: F,
) -> Result<String, String> {
    execute_plan(prepared.compile_scoped(options)?, options, cancel)
}

fn execute_plan<F: FnMut() -> bool>(
    plan: CompiledScript,
    options: Options,
    cancel: F,
) -> Result<String, String> {
    if options.dry_run {
        return Ok(format!(
            "{{\"status\":\"planned\",\"language\":\"zuse script\",\"plan\":{}}}",
            plan.summary.to_json()
        ));
    }
    plan.require_native_capabilities()?;
    scheduler::execute(&plan, options, cancel)
}

impl PlanSummary {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"steps\":{},\"mice\":{},\"shared_mice\":{},\"parallel_groups\":{},\"static_wait_ms\":{}}}",
            self.steps, self.mice, self.shared_mice, self.parallel_groups, self.static_wait_ms
        )
    }
}
