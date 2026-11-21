//! Locate visible desktop targets in native accessibility snapshots.
//!
//! Bounds use the native input backend's screen coordinates. A lookup observes
//! one snapshot; paths and bounds should be refreshed after the UI changes.
use zmcpio::json::Value;

type Result<T> = std::result::Result<T, String>;

/// Match a node's accessible name or text, optionally within a subtree.
///
/// `index` selects a zero-based match with screen bounds in snapshot order.
/// Without it, exactly one visible match and a complete snapshot are required.
/// Role matching recognizes native Linux, Windows and macOS button names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetQuery<'a> {
    pub text: &'a str,
    pub role: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub exact: bool,
    pub case_sensitive: bool,
    pub index: Option<usize>,
}

impl<'a> TargetQuery<'a> {
    /// Match an exact, case-sensitive accessible name or text.
    pub const fn text(text: &'a str) -> Self {
        Self {
            text,
            role: None,
            scope: None,
            exact: true,
            case_sensitive: true,
            index: None,
        }
    }

    /// Match an exact, case-sensitive button name or text.
    pub const fn button(text: &'a str) -> Self {
        Self {
            role: Some("button"),
            ..Self::text(text)
        }
    }
}

/// A positive-area rectangle in native screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A visible target from a single accessibility observation.
#[derive(Clone, Debug, PartialEq)]
pub struct LocatedTarget {
    pub path: String,
    pub role: String,
    pub name: String,
    pub bounds: Bounds,
}

/// The observed native window and its coordinate scale, without input effects.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetWindow {
    pub id: String,
    pub pid: u32,
    pub bounds: Bounds,
    pub scale: f64,
    pub focused: bool,
}

/// One semantic lookup retained for a later, inexpensive checked click.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowTarget {
    pub target: LocatedTarget,
    pub window: TargetWindow,
}

impl Bounds {
    pub fn contains(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }
}

impl TargetWindow {
    pub fn from_windows(snapshot: &str, id: &str) -> Result<Self> {
        crate::json::with_value(snapshot, |root| {
            let windows = root
                .elements()
                .filter(|node| {
                    if id == "active" {
                        node.get("focused").and_then(Value::boolean) == Some(true)
                    } else {
                        node.get("id")
                            .and_then(Value::string)
                            .is_some_and(|v| v.matches(id))
                    }
                })
                .collect::<Vec<_>>();
            if windows.len() != 1 {
                return Err("stale_target: a unique current target window is required".into());
            }
            let node = windows[0];
            if node.get("mapped").and_then(Value::boolean) != Some(true) {
                return Err("stale_target: the target window is not confirmed mapped".into());
            }
            let bounds = parse_bounds(Some(node))?;
            let pid = node
                .get("pid")
                .and_then(|v| v.integer().ok())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v != 0)
                .ok_or("unsupported_state: target window process identity is unavailable")?;
            let scale = node
                .get("scale")
                .and_then(|v| v.raw().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or("unsupported_state: target window scale is unavailable")?;
            let focused = node
                .get("focused")
                .and_then(Value::boolean)
                .ok_or("unsupported_state: target window focus is unavailable")?;
            Ok(Self {
                id: string(node, "id"),
                pid,
                bounds,
                scale,
                focused,
            })
        })
    }

    pub fn to_json(&self) -> String {
        let b = self.bounds;
        format!(
            "{{\"id\":{},\"pid\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"scale\":{},\"focused\":{},\"mapped\":true}}",
            crate::json::quote(&self.id),
            self.pid,
            b.x,
            b.y,
            b.width,
            b.height,
            self.scale,
            self.focused
        )
    }
}

impl WindowTarget {
    pub fn to_json(&self) -> String {
        let t = &self.target;
        let b = t.bounds;
        format!(
            "{{\"window\":{},\"target\":{{\"path\":{},\"role\":{},\"name\":{},\"bounds\":{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}}},\"x\":{},\"y\":{},\"basis\":\"accessibility\",\"coordinate_space\":\"native_desktop\",\"recognition_passes\":1}}",
            self.window.to_json(),
            crate::json::quote(&t.path),
            crate::json::quote(&t.role),
            crate::json::quote(&t.name),
            b.x,
            b.y,
            b.width,
            b.height,
            b.x + b.width / 2.0,
            b.y + b.height / 2.0
        )
    }

    /// Check native identity and this one retained node. No tree search or OCR.
    pub fn verify(&self, windows: &str, node: &str) -> Result<()> {
        let current = TargetWindow::from_windows(windows, &self.window.id)?;
        if current != self.window || !current.focused {
            return Err("stale_target: window identity, geometry, scale or focus changed; locate again before clicking".into());
        }
        crate::json::with_value(node, |node| {
            let pid = node.get("pid").and_then(|v| v.integer().ok());
            let visible = ["visible", "showing"]
                .iter()
                .all(|key| node.get(key).and_then(Value::boolean) == Some(true));
            if pid != Some(i64::from(self.window.pid))
                || !visible
                || string(node, "path") != self.target.path
                || string(node, "name") != self.target.name
                || string(node, "role") != self.target.role
                || parse_bounds(node.get("bounds"))? != self.target.bounds
            {
                return Err("stale_target: the located node changed or is not visible; locate again before clicking".into());
            }
            Ok(())
        })
    }
}

/// Resolve within a native window using process identity and current bounds.
/// Other applications and out-of-window nodes never enter the candidate set.
pub fn locate_in_window(
    snapshot: &str,
    query: &TargetQuery<'_>,
    window: &TargetWindow,
) -> Result<WindowTarget> {
    validate_query(query)?;
    let filtered = crate::json::with_value(snapshot, |root| {
        let nodes = root
            .get("nodes")
            .ok_or("accessibility snapshot has no nodes")?;
        let mut has_identity = false;
        let retained = nodes
            .elements()
            .filter(|node| {
                let pid = node.get("pid").and_then(|v| v.integer().ok());
                has_identity |= pid.is_some();
                pid == Some(i64::from(window.pid))
                    && parse_bounds(node.get("bounds"))
                        .is_ok_and(|bounds| window.bounds.contains(&bounds))
            })
            .map(|node| node.raw().to_owned())
            .collect::<Vec<_>>();
        if !has_identity {
            return Err("unsupported_state: accessibility snapshot has no process identities for window-scoped lookup".into());
        }
        Ok(format!(
            "{{\"nodes\":[{}],\"truncated\":{},\"errors\":{}}}",
            retained.join(","),
            root.get("truncated").map(|v| v.raw()).unwrap_or("false"),
            root.get("errors").map(|v| v.raw()).unwrap_or("0")
        ))
    })?;
    Ok(WindowTarget {
        target: locate_in(&filtered, query)?,
        window: window.clone(),
    })
}

/// Validate a query before observing the desktop or moving the pointer.
pub fn validate_query(query: &TargetQuery<'_>) -> Result<()> {
    if query.text.trim().is_empty() || query.text.contains('\0') || query.text.len() > 65536 {
        return Err("target text must be nonblank, NUL-free and at most 64 KiB".into());
    }
    if query
        .role
        .is_some_and(|role| role.trim().is_empty() || role.contains('\0'))
    {
        return Err("target role must be nonblank and NUL-free".into());
    }
    if let Some(scope) = query.scope {
        validate_path(scope)
            .map_err(|_| "invalid target scope; expected 0/child/child".to_owned())?;
    }
    Ok(())
}

/// Observe the desktop and locate a target without producing input events.
pub fn locate(query: &TargetQuery<'_>) -> Result<LocatedTarget> {
    validate_query(query)?;
    let snapshot = crate::accessibility::snapshot(4096, 32)?;
    locate_in(&snapshot, query)
}

/// Locate a target in a supplied native accessibility snapshot.
///
/// Scope includes the named path and its descendants. Hidden nodes, nodes with
/// missing/null bounds and redacted password text are excluded. Case-insensitive
/// text matching uses Unicode
/// lowercasing. An explicit index selects among visible, observed matches even
/// when the snapshot is partial; scope alone cannot certify completeness.
pub fn locate_in(snapshot: &str, query: &TargetQuery<'_>) -> Result<LocatedTarget> {
    validate_query(query)?;
    crate::browser::with_json(snapshot, |root| {
        let nodes = root
            .get("nodes")
            .filter(|value| value.raw().starts_with('['))
            .ok_or("accessibility snapshot has no nodes array")?;
        let truncated = match root.get("truncated") {
            Some(value) => value
                .boolean()
                .ok_or("accessibility snapshot has invalid truncated flag")?,
            None => false,
        };
        let errors = match root.get("errors") {
            Some(value) => value
                .integer()
                .ok()
                .filter(|errors| *errors >= 0)
                .ok_or("accessibility snapshot has invalid error count")?,
            None => 0,
        };
        let incomplete = truncated || errors != 0;
        let needle = if query.case_sensitive {
            query.text.to_owned()
        } else {
            query.text.to_lowercase()
        };
        let mut matches = Vec::new();
        let mut unpositioned = 0;
        for node in nodes.elements() {
            if ["visible", "showing"]
                .iter()
                .any(|key| node.get(key).and_then(Value::boolean) == Some(false))
            {
                continue;
            }
            let role = string(node, "role");
            if query
                .role
                .is_some_and(|expected| !role_matches(&role, expected))
            {
                continue;
            }
            let name = string(node, "name");
            let text = string(node, "text");
            let password = role.to_ascii_lowercase().contains("password")
                || role.eq_ignore_ascii_case("AXSecureTextField");
            if !text_matches(&name, &needle, query)
                && (password || !text_matches(&text, &needle, query))
            {
                continue;
            }
            let path = string(node, "path");
            validate_path(&path)?;
            if query.scope.is_some_and(|scope| {
                path != scope
                    && !path
                        .strip_prefix(scope)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            }) {
                continue;
            }
            if node
                .get("bounds")
                .is_none_or(|bounds| bounds.raw() == "null")
            {
                unpositioned += 1;
                continue;
            }
            matches.push((node, path, role, name));
        }
        if matches.is_empty() {
            let detail = if incomplete {
                "; accessibility snapshot is incomplete"
            } else {
                ""
            };
            return Err(if unpositioned != 0 {
                format!(
                    "{unpositioned} visible targets match {:?} but have no screen bounds{detail}",
                    query.text
                )
            } else {
                format!("no visible target matches {:?}{detail}", query.text)
            });
        }
        let index = match query.index {
            Some(index) => index,
            None if matches.len() != 1 => {
                return Err(format!(
                    "target {:?} is ambiguous: {} visible matches; specify scope or index",
                    query.text,
                    matches.len()
                ));
            }
            None if incomplete => {
                return Err(
                    "accessibility snapshot is incomplete; specify index to select an observed match or retry a complete snapshot"
                        .into(),
                );
            }
            None => 0,
        };
        let count = matches.len();
        let (node, path, role, name) = matches.into_iter().nth(index).ok_or_else(|| {
            format!("target index {index} is out of range for {count} visible matches")
        })?;
        let bounds = parse_bounds(node.get("bounds"))
            .map_err(|error| format!("target {path} has invalid screen bounds: {error}"))?;
        Ok(LocatedTarget {
            path,
            role,
            name,
            bounds,
        })
    })
}

fn string(node: Value<'_>, key: &str) -> String {
    node.get(key)
        .and_then(Value::string)
        .map(|value| value.chars().collect())
        .unwrap_or_default()
}

fn text_matches(value: &str, needle: &str, query: &TargetQuery<'_>) -> bool {
    if value.is_empty() || value == "[redacted]" {
        return false;
    }
    let lower;
    let value = if query.case_sensitive {
        value
    } else {
        lower = value.to_lowercase();
        &lower
    };
    if query.exact {
        value == needle
    } else {
        value.contains(needle)
    }
}

fn role_matches(actual: &str, expected: &str) -> bool {
    let button = |role: &str| {
        ["button", "push button", "ControlType.Button", "AXButton"]
            .iter()
            .any(|alias| role.eq_ignore_ascii_case(alias))
    };
    (button(actual) && button(expected)) || actual.eq_ignore_ascii_case(expected)
}

fn parse_bounds(value: Option<Value<'_>>) -> Result<Bounds> {
    let value = value.ok_or("missing rectangle")?;
    let number = |key| {
        value
            .get(key)
            .and_then(|value| value.raw().parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("{key} must be finite"))
    };
    let bounds = Bounds {
        x: number("x")?,
        y: number("y")?,
        width: number("width")?,
        height: number("height")?,
    };
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Err("width and height must be positive".into());
    }
    if !(bounds.x + bounds.width).is_finite() || !(bounds.y + bounds.height).is_finite() {
        return Err("rectangle edges must be finite".into());
    }
    Ok(bounds)
}

fn validate_path(path: &str) -> Result<()> {
    if path.len() > 1024
        || path.split('/').count() > 33
        || path.split('/').next() != Some("0")
        || path.split('/').any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || part.parse::<u32>().map_or(true, |index| index > 1000000)
        })
    {
        return Err("invalid accessibility target path; expected 0/child/child".into());
    }
    Ok(())
}
