//! Coordinate-free queries and actions within one explicitly selected window.
use super::{Result, validate_identity_target};
use crate::observation::Cancellation;

pub const MAX_TIMEOUT_MS: u64 = 10_000;

/// All filters are conjunctive. Role is exact; `exact` controls case-sensitive
/// name/text matching. A missing filter imposes no constraint on that field.
#[derive(Clone, Copy, Debug)]
pub struct Query<'a> {
    pub scope: &'a str,
    pub expected_pid: u32,
    pub role: Option<&'a str>,
    pub name: Option<&'a str>,
    pub text: Option<&'a str>,
    pub exact: bool,
}

impl Query<'_> {
    pub fn validate(&self) -> Result<()> {
        validate_identity_target(self.scope, Some(self.expected_pid), self.role, self.name)?;
        if self.scope.split('/').count() < 3 {
            return Err(
                "accessibility query requires an explicit window/subtree scope: 0/app/window"
                    .into(),
            );
        }
        if self
            .text
            .is_some_and(|value| value.is_empty() || value.len() > 65536 || value.contains('\0'))
        {
            return Err("query text must contain 1..65536 NUL-free bytes".into());
        }
        Ok(())
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn matches(&self, role: &str, name: &str, text: &str) -> bool {
        let matches = |actual: &str, expected: &str| {
            if self.exact {
                actual == expected
            } else {
                actual.contains(expected)
            }
        };
        self.role.is_none_or(|expected| role == expected)
            && self.name.is_none_or(|expected| matches(name, expected))
            && self.text.is_none_or(|expected| {
                text != "[redacted]"
                    && !role.to_ascii_lowercase().contains("password")
                    && matches(text, expected)
            })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Operation<'a> {
    Action(&'a str),
    Focus,
    /// Ask the native component to scroll into view, then verify showing.
    /// This never activates the control or falls back to pointer input.
    Reveal,
}

pub fn validate_timeout(timeout_ms: u64) -> Result<()> {
    if !(1..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err("accessibility timeout_ms must be 1..10000".into());
    }
    Ok(())
}

pub fn validate_invoke(
    query: &Query<'_>,
    operation: Operation<'_>,
    index: Option<usize>,
    timeout_ms: u64,
) -> Result<()> {
    query.validate()?;
    validate_timeout(timeout_ms)?;
    if query.role.is_none() || query.name.is_none() {
        return Err("accessibility.invoke requires role and name".into());
    }
    if index.is_some_and(|value| value >= 4096) {
        return Err("accessibility control index must be 0..4095".into());
    }
    if let Operation::Action(action) = operation
        && (action.is_empty() || action.len() > 4096 || action.contains('\0'))
    {
        return Err("accessibility action name must contain 1..4096 NUL-free bytes".into());
    }
    Ok(())
}

pub fn validate_wait(query: &Query<'_>, timeout_ms: u64) -> Result<()> {
    query.validate()?;
    validate_timeout(timeout_ms)?;
    if query.role.is_none() && query.name.is_none() && query.text.is_none() {
        return Err("accessibility.wait requires at least one role/name/text filter".into());
    }
    Ok(())
}

/// Enumerate every matching node, including disabled/hidden nodes and their
/// states. Partial results carry `truncated`/`errors` and cannot authorize input.
pub fn find(query: &Query<'_>, cancellation: &Cancellation) -> Result<String> {
    query.validate()?;
    cancellation.check()?;
    #[cfg(target_os = "linux")]
    {
        super::native::controls::run(query, None, None, 5000, false, cancellation)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("unsupported_state: semantic accessibility controls require Linux AT-SPI".into())
    }
}

/// Wait for a unique (or explicitly indexed) node, then invoke its advertised
/// action or request focus on the same owned native object. Never retries input.
pub fn invoke(
    query: &Query<'_>,
    operation: Operation<'_>,
    index: Option<usize>,
    timeout_ms: u64,
    cancellation: &Cancellation,
) -> Result<String> {
    validate_invoke(query, operation, index, timeout_ms)?;
    cancellation.check()?;
    #[cfg(target_os = "linux")]
    {
        super::native::controls::run(
            query,
            Some(operation),
            index,
            timeout_ms,
            true,
            cancellation,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("unsupported_state: semantic accessibility controls require Linux AT-SPI".into())
    }
}

/// Wait for at least one visible matching node. This only observes; multiple
/// matching paragraphs or static text nodes are returned without ambiguity.
pub fn wait(query: &Query<'_>, timeout_ms: u64, cancellation: &Cancellation) -> Result<String> {
    validate_wait(query, timeout_ms)?;
    cancellation.check()?;
    #[cfg(target_os = "linux")]
    {
        super::native::controls::run(query, None, None, timeout_ms, true, cancellation)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("unsupported_state: semantic accessibility controls require Linux AT-SPI".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_filters_do_not_require_geometry_and_never_match_redacted_text() {
        let mut query = Query {
            scope: "0/1/0",
            expected_pid: 42,
            role: Some("button"),
            name: Some("Send"),
            text: None,
            exact: true,
        };
        assert!(query.matches("button", "Send", ""));
        assert!(!query.matches("button", "send", ""));
        assert!(!query.matches("button", "Send now", ""));
        query.exact = false;
        assert!(query.matches("button", "Send now", ""));
        assert!(!query.matches("push button", "Send", ""));
        query.role = None;
        query.name = None;
        query.text = Some("secret");
        assert!(!query.matches("password text", "", "secret"));
        assert!(!query.matches("entry", "", "[redacted]"));
        assert!(query.matches("paragraph", "", "a secret reply"));
    }
}
