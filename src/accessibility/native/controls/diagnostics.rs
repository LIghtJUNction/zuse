//! Native-input diagnostics describe the exact stage without changing errors
//! used by the read-only recovery whitelist. `native_call_started` means the
//! input FFI method was entered, not that the application applied its effect.
use super::Result;

#[derive(Clone, Copy)]
pub(super) struct Context<'a> {
    pub(super) scope: &'a str,
    pub(super) path: &'a str,
    pub(super) phase: &'static str,
    pub(super) native_call_started: bool,
}

impl<'a> Context<'a> {
    pub(super) fn new(scope: &'a str, path: &'a str, phase: &'static str) -> Self {
        Self {
            scope,
            path,
            phase,
            native_call_started: false,
        }
    }

    pub(super) fn read<T>(self, operation: &str, result: Result<T>) -> Result<T> {
        result.map_err(|error| self.error(operation, error, self.native_call_started))
    }

    pub(super) fn error(self, operation: &str, error: String, native_call_started: bool) -> String {
        // Append context so timeout/cancelled/stale_target prefixes survive.
        format!(
            "{}; phase={} scope={} path={} operation={} native_call_started={native_call_started}",
            error.trim_end_matches('\n'),
            self.phase,
            self.scope,
            self.path,
            operation
        )
    }
}

pub(super) fn scan_error(scope: &str, error: String) -> String {
    // ReadFailure has already added its precise path/operation. Add this only
    // after deciding whether its original native reply permits a wait retry.
    format!("{error}; scope={scope} native_call_started=false")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_preserve_error_prefix_and_distinguish_native_input() {
        let context = Context::new("0/1/0", "0/1/0/2", "prepare");
        for prefix in [
            "timeout: deadline",
            "cancelled: execution",
            "stale_target: moved",
        ] {
            let error = context
                .read::<()>("GetParent", Err(prefix.into()))
                .unwrap_err();
            assert!(error.starts_with(prefix));
            assert!(error.contains("phase=prepare scope=0/1/0 path=0/1/0/2 operation=GetParent native_call_started=false"));
        }
        let error = context.error(
            "DoAction",
            "application rejected the operation".into(),
            true,
        );
        assert!(error.ends_with("operation=DoAction native_call_started=true"));
    }
}
