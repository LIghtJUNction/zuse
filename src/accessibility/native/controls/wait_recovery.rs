//! Only a read-only wait may discard a scan of a disappearing descendant.
//! libatspi maps D-Bus errors to ATSPI_ERROR_IPC and retains just the message,
//! so recognize the precise droute UnknownObject reply, not generic failures.
use super::Operation;

#[derive(Debug)]
pub(super) struct ReadFailure {
    path: String,
    operation: &'static str,
    message: String,
}

impl ReadFailure {
    pub(super) fn at(path: &str, operation: &'static str, message: String) -> Self {
        Self {
            path: path.into(),
            operation,
            message,
        }
    }

    pub(super) fn message(self) -> String {
        // Preserve timeout/cancelled/unsupported prefixes used by callers.
        format!(
            "{}; phase=scan path={} operation={}",
            self.message.trim_end_matches('\n'),
            self.path,
            self.operation
        )
    }

    pub(super) fn retryable(
        &self,
        scope: &str,
        waiting: bool,
        operation: Option<Operation<'_>>,
    ) -> bool {
        // Never recover find/invoke, scope/ancestor loss, or errors after input.
        // A descendant must have at least one complete index below the scope.
        waiting
            && operation.is_none()
            && self
                .path
                .strip_prefix(scope)
                .is_some_and(|suffix| suffix.starts_with('/') && suffix.len() > 1)
            && missing_object_reply(&self.message)
    }
}

fn missing_object_reply(message: &str) -> bool {
    // GNOME droute.c:droute_object_does_not_exist_error. The exact readonly
    // method/signature/interface triples below are issued by this scanner.
    // UnknownMethod, arbitrary "not found", negative counts, application/bus
    // loss and transport timeouts intentionally do not authorize rediscovery.
    let message = message.strip_suffix('\n').unwrap_or(message);
    let Some(message) = message.strip_prefix("Method \"") else {
        return false;
    };
    let Some((method, message)) = message.split_once("\" with signature \"") else {
        return false;
    };
    let Some((signature, message)) = message.split_once("\" on interface \"") else {
        return false;
    };
    let Some((interface, object)) = message.split_once("\" could not be processed as object ")
    else {
        return false;
    };
    let known_read = matches!(
        (method, signature, interface),
        ("GetRole", "", "org.a11y.atspi.Accessible")
            | ("GetChildAtIndex", "i", "org.a11y.atspi.Accessible")
            | ("Get", "ss", "org.freedesktop.DBus.Properties")
            | ("GetText", "ii", "org.a11y.atspi.Text")
            | ("GetName", "i", "org.a11y.atspi.Action")
    );
    if !known_read {
        return false;
    }
    let Some(path) = object.strip_suffix(" does not exist") else {
        return false;
    };
    let Some(suffix) = path.strip_prefix("/org/a11y/atspi/accessible/") else {
        return false;
    };
    !suffix.is_empty()
        && !matches!(suffix, "root" | "null")
        && suffix.split('/').all(|component| {
            !component.is_empty()
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vanished(method: &str, signature: &str, interface: &str, object: &str) -> String {
        format!(
            "Method \"{method}\" with signature \"{signature}\" on interface \"{interface}\" could not be processed as object {object} does not exist\n"
        )
    }

    fn role_error() -> String {
        vanished(
            "GetRole",
            "",
            "org.a11y.atspi.Accessible",
            "/org/a11y/atspi/accessible/5733",
        )
    }

    #[test]
    fn disappeared_descendant_is_recoverable_only_during_read_only_wait() {
        let error = ReadFailure::at("0/11/0/5/2", "Role", role_error());
        assert!(error.retryable("0/11/0", true, None));
        assert!(!error.retryable("0/11/0", false, None));
        for operation in [
            Operation::Action("press"),
            Operation::Focus,
            Operation::Reveal,
        ] {
            assert!(!error.retryable("0/11/0", true, Some(operation)));
        }
        for path in ["0", "0/11", "0/11/0", "0/11/01/2", "0/12/0/2"] {
            let error = ReadFailure::at(path, "Role", role_error());
            assert!(!error.retryable("0/11/0", true, None), "{path}");
        }
    }

    #[test]
    fn only_exact_native_unknown_object_reads_authorize_rediscovery() {
        for (method, signature, interface) in [
            ("GetRole", "", "org.a11y.atspi.Accessible"),
            ("GetChildAtIndex", "i", "org.a11y.atspi.Accessible"),
            ("Get", "ss", "org.freedesktop.DBus.Properties"),
            ("GetText", "ii", "org.a11y.atspi.Text"),
            ("GetName", "i", "org.a11y.atspi.Action"),
        ] {
            let message = vanished(
                method,
                signature,
                interface,
                "/org/a11y/atspi/accessible/node_5733",
            );
            assert!(missing_object_reply(&message), "{message}");
            assert!(missing_object_reply(message.trim_end_matches('\n')));
        }
        for message in [
            "AT-SPI returned a negative child count".into(),
            "timeout: scoped accessibility query reached its deadline".into(),
            "cancelled: observation cancelled".into(),
            "permission denied".into(),
            "The application no longer exists".into(),
            "The process appears to be hung.".into(),
            "org.freedesktop.DBus.Error.UnknownMethod: GetRole does not exist".into(),
            "object /org/a11y/atspi/accessible/5733 does not exist".into(),
            vanished(
                "DoAction",
                "i",
                "org.a11y.atspi.Action",
                "/org/a11y/atspi/accessible/5733",
            ),
            vanished(
                "GetRole",
                "i",
                "org.a11y.atspi.Accessible",
                "/org/a11y/atspi/accessible/5733",
            ),
            vanished(
                "GetRole",
                "",
                "org.a11y.atspi.Accessible",
                "/org/a11y/atspi/accessible/root",
            ),
            vanished(
                "GetRole",
                "",
                "org.a11y.atspi.Accessible",
                "/org/a11y/atspi/accessible/null",
            ),
            vanished(
                "GetRole",
                "",
                "org.a11y.atspi.Accessible",
                "/org/a11y/atspi/accessible/",
            ),
            vanished(
                "GetRole",
                "",
                "org.a11y.atspi.Accessible",
                "/org/a11y/atspi/accessible/57 33",
            ),
            vanished(
                "GetRole",
                "",
                "org.a11y.atspi.Accessible",
                "/other/object/5733",
            ),
            format!("{}unexpected suffix", role_error()),
        ] {
            assert!(!missing_object_reply(&message), "{message}");
        }
    }

    #[test]
    fn read_failure_keeps_native_message_and_exact_scan_location() {
        let error = ReadFailure::at(
            "0/11/0/3/2",
            "ChildCount",
            "AT-SPI returned a negative child count".into(),
        );
        assert!(!error.retryable("0/11/0", true, None));
        let message = error.message();
        assert!(message.contains("phase=scan path=0/11/0/3/2 operation=ChildCount"));
        assert!(message.starts_with("AT-SPI returned a negative child count"));
        for prefix in [
            "cancelled: operation cancelled",
            "timeout: deadline expired",
            "unsupported_state: query incomplete",
        ] {
            assert!(
                ReadFailure::at("0/11/0/3/2", "Budget", prefix.into())
                    .message()
                    .starts_with(prefix)
            );
        }
    }
}
