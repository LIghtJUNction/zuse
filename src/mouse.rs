//! Owned pointers. Independent mice are the default; user input is opt-in.
use crate::{
    accessibility::ElementAction,
    cursor::{CursorImage, CursorStyle},
    motion::{MAX_COORDINATE, Point},
    observation::Cancellation,
    platform::Desktop,
    pointer::{MoveOptions, MovePlan},
    target::{self, LocatedTarget, TargetQuery},
};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// A native pointer, borrowed user pointer, or Wayland cursor with semantic actions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Independent,
    User,
    Semantic,
}

impl MouseMode {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::User => "user",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MouseOptions {
    pub mode: MouseMode,
    pub name: String,
    /// `None` preserves the system appearance; allowed only for the user mouse.
    /// Wayland custom appearances are click-through last-operation markers;
    /// the real input and system cursor still belong to the user mouse.
    pub appearance: Option<CursorStyle>,
    /// Optional custom premultiplied image, overriding the generated style.
    pub image: Option<CursorImage>,
    pub start: Option<Point>,
}

impl Default for MouseOptions {
    fn default() -> Self {
        Self {
            mode: MouseMode::Independent,
            name: "zuse".into(),
            appearance: Some(CursorStyle::default()),
            image: None,
            start: None,
        }
    }
}

impl MouseOptions {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 128 || self.name.contains('\0') {
            return Err("mouse name must contain 1..128 non-NUL bytes".into());
        }
        if let Some(style) = &self.appearance {
            style.validate()?;
        }
        if let Some(image) = &self.image {
            image.validate()?;
        }
        if self.mode != MouseMode::User && self.appearance.is_none() && self.image.is_none() {
            return Err(
                "independent and semantic mice require their own visible appearance".into(),
            );
        }
        if let Some(p) = self.start {
            validate_point(p)?;
        }
        Ok(())
    }

    pub fn resolved_image(&self) -> Result<Option<CursorImage>, String> {
        self.validate()?;
        if let Some(image) = &self.image {
            return Ok(Some(image.clone()));
        }
        self.appearance
            .as_ref()
            .map(CursorStyle::render)
            .transpose()
    }
}

enum Backend {
    #[cfg(target_os = "linux")]
    X11(crate::platform::pointer_x11::X11Pointer),
    #[cfg(target_os = "linux")]
    WaylandMarker {
        desktop: Desktop,
        marker: Box<crate::platform::wayland_marker::WaylandMarker>,
    },
    #[cfg(target_os = "linux")]
    Semantic(Box<crate::platform::wayland_marker::WaylandMarker>),
    System(Desktop),
}

/// A pointer whose native resources live exactly as long as this value.
///
/// Dropping it releases held synthetic buttons and removes its independent
/// native device or operation marker. `close()` also reports cleanup failures. It never silently
/// substitutes the user pointer when independent input is unavailable.
pub struct Mouse {
    id: u64,
    mode: MouseMode,
    backend: Option<Backend>,
    held: BTreeSet<String>,
    user_lease: Option<UserLease>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static USER_MOUSE_OWNED: AtomicBool = AtomicBool::new(false);
struct UserLease;
impl Drop for UserLease {
    fn drop(&mut self) {
        USER_MOUSE_OWNED.store(false, Ordering::Release);
    }
}

impl Mouse {
    pub fn create(options: &MouseOptions) -> Result<Self, String> {
        let image = options.resolved_image()?;
        let user_lease = if options.mode == MouseMode::User {
            USER_MOUSE_OWNED.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire)
                .map_err(|_|"the user mouse already has a live owner in this process; borrow the existing mouse or close it first")?;
            Some(UserLease)
        } else {
            None
        };
        let backend = if options.mode == MouseMode::User && image.is_none() {
            Backend::System(Desktop::connect()?)
        } else {
            #[cfg(target_os = "linux")]
            {
                let image = image.as_ref().ok_or("mouse appearance is missing")?;
                if options.mode == MouseMode::Independent {
                    require_x11()?;
                    Backend::X11(crate::platform::pointer_x11::X11Pointer::create(
                        &options.name,
                        image,
                    )?)
                } else if options.mode == MouseMode::Semantic {
                    require_semantic_wayland()?;
                    let mut marker = crate::platform::wayland_marker::WaylandMarker::connect(
                        &options.name,
                        image,
                    )?;
                    let point = match options.start {
                        Some(p) => (p.x.round() as i32, p.y.round() as i32),
                        None => Desktop::connect()?.position()?,
                    };
                    marker.move_to(point.0, point.1)?;
                    Backend::Semantic(Box::new(marker))
                } else {
                    let desktop = Desktop::connect()?;
                    if desktop.backend() == "wayland" {
                        let mut marker = crate::platform::wayland_marker::WaylandMarker::connect(
                            &options.name,
                            image,
                        )?;
                        let (x, y) = desktop.position()?;
                        marker.move_to(x, y)?;
                        Backend::WaylandMarker {
                            desktop,
                            marker: Box::new(marker),
                        }
                    } else {
                        Backend::X11(crate::platform::pointer_x11::X11Pointer::shared(image)?)
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err("independent mice and custom native cursors currently require Linux X11 MPX; user_mouse(appearance=\"system\") explicitly uses the supported system pointer".into());
            }
        };
        let mut mouse = Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            mode: options.mode,
            backend: Some(backend),
            held: BTreeSet::new(),
            user_lease,
        };
        if mouse.mode != MouseMode::Semantic
            && let Some(p) = options.start
        {
            mouse.submit(p)?;
        }
        Ok(mouse)
    }

    pub fn independent() -> Result<Self, String> {
        Self::create(&MouseOptions::default())
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn mode(&self) -> MouseMode {
        match self.backend.as_ref() {
            #[cfg(target_os = "linux")]
            Some(Backend::X11(native)) => {
                if native.is_independent() {
                    MouseMode::Independent
                } else {
                    MouseMode::User
                }
            }
            _ => self.mode,
        }
    }
    pub fn device_id(&self) -> Option<i32> {
        match self.backend.as_ref() {
            #[cfg(target_os = "linux")]
            Some(Backend::X11(native)) => Some(native.device_id()),
            _ => None,
        }
    }
    pub fn position(&self) -> Result<Point, String> {
        let (x, y) = match self.backend.as_ref().ok_or("mouse is closed")? {
            #[cfg(target_os = "linux")]
            Backend::X11(native) => native.position()?,
            #[cfg(target_os = "linux")]
            Backend::WaylandMarker { desktop, .. } => desktop.position()?,
            #[cfg(target_os = "linux")]
            Backend::Semantic(marker) => marker.position()?,
            Backend::System(desktop) => desktop.position()?,
        };
        Ok(Point {
            x: f64::from(x),
            y: f64::from(y),
        })
    }

    /// Plan completely before submitting any input; position is read at call time.
    pub fn move_to(&mut self, to: Point, options: &MoveOptions) -> Result<MotionReceipt, String> {
        let plan = self.plan(to, options)?;
        self.perform(plan, None)
    }

    pub fn move_to_text(
        &mut self,
        query: &TargetQuery<'_>,
        options: &MoveOptions,
    ) -> Result<MotionReceipt, String> {
        let (plan, target) = self.plan_target(query, options)?;
        self.perform(plan, Some(target))
    }

    pub fn move_with<F: FnMut(f64, Point, Point) -> Point>(
        &mut self,
        to: Point,
        options: &MoveOptions,
        path: F,
    ) -> Result<MotionReceipt, String> {
        options.validate()?;
        let plan = MovePlan::with_path(self.position()?, to, options, path)?;
        self.perform(plan, None)
    }

    pub fn move_to_text_with<F: FnMut(f64, Point, Point) -> Point>(
        &mut self,
        query: &TargetQuery<'_>,
        options: &MoveOptions,
        path: F,
    ) -> Result<MotionReceipt, String> {
        options.validate()?;
        let target = target::locate(query)?;
        let plan = MovePlan::with_path(self.position()?, center(&target), options, path)?;
        self.perform(plan, Some(target))
    }

    pub(crate) fn plan(&self, to: Point, options: &MoveOptions) -> Result<MovePlan, String> {
        options.validate()?;
        MovePlan::new(self.position()?, to, options)
    }

    pub(crate) fn plan_target(
        &self,
        query: &TargetQuery<'_>,
        options: &MoveOptions,
    ) -> Result<(MovePlan, LocatedTarget), String> {
        options.validate()?;
        let target = target::locate(query)?;
        let plan = MovePlan::new(self.position()?, center(&target), options)?;
        Ok((plan, target))
    }

    pub(crate) fn submit(&mut self, point: Point) -> Result<(), String> {
        validate_point(point)?;
        let (x, y) = (point.x.round() as i32, point.y.round() as i32);
        match self.backend.as_mut().ok_or("mouse is closed")? {
            #[cfg(target_os = "linux")]
            Backend::X11(native) => native.move_to(x, y),
            #[cfg(target_os = "linux")]
            Backend::WaylandMarker { desktop, marker } => {
                desktop.move_to(x, y)?;
                marker.move_to(x, y).map_err(|error| {
                    format!("user pointer moved but operation marker update failed: {error}")
                })
            }
            #[cfg(target_os = "linux")]
            Backend::Semantic(marker) => marker.animate_to(x, y),
            Backend::System(desktop) => desktop.move_to(x, y),
        }
    }

    fn perform(
        &mut self,
        plan: MovePlan,
        target: Option<LocatedTarget>,
    ) -> Result<MotionReceipt, String> {
        let started = Instant::now();
        for (i, sample) in plan.samples().iter().enumerate() {
            if let Some(delay) =
                Duration::from_millis(u64::from(sample.at_ms)).checked_sub(started.elapsed())
            {
                std::thread::sleep(delay);
            }
            self.submit(sample.point)
                .map_err(|e| format!("mouse movement stopped after {i} samples: {e}"))?;
        }
        Ok(MotionReceipt::new(
            self.id,
            &plan,
            target,
            started.elapsed().as_millis(),
        ))
    }

    /// Activate a named action on a verified accessibility object.
    pub fn activate(&self, target: &ElementAction) -> Result<String, String> {
        self.activate_checked(target, None, || Ok(()))
    }

    pub(crate) fn activate_checked(
        &self,
        target: &ElementAction,
        cancellation: Option<&Cancellation>,
        before_action: impl FnOnce() -> Result<(), String>,
    ) -> Result<String, String> {
        if self.mode != MouseMode::Semantic || self.backend.is_none() {
            return Err("activate requires a live semantic mouse".into());
        }
        target.activate(cancellation, before_action)
    }

    pub(crate) fn require_pointer_input(&self) -> Result<(), String> {
        if self.mode == MouseMode::Semantic {
            return Err(
                "unsupported_state: semantic mice use activate(path, pid, name, action) for input"
                    .into(),
            );
        }
        Ok(())
    }

    /// `action` is `down`, `up`, or `click`. Held buttons are released on close.
    pub fn button(&mut self, button: &str, action: &str) -> Result<(), String> {
        self.require_pointer_input()?;
        let button = match button {
            "1" => "left",
            "2" => "middle",
            "3" => "right",
            other => other,
        };
        if !matches!(button, "left" | "middle" | "right" | "back" | "forward") {
            return Err("mouse button must be left, middle, right, back or forward".into());
        }
        if !matches!(action, "down" | "up" | "click") {
            return Err("mouse button action must be down, up or click".into());
        }
        if action == "click" {
            if self.held.contains(button) {
                return Err("cannot click a button that this mouse already holds".into());
            }
            self.button(button, "down")?;
            return self.button(button, "up");
        }
        // A native helper may submit a press and then fail while waiting for
        // confirmation. Retain that attempted press so close still releases it.
        if action == "down" {
            self.held.insert(button.to_owned());
        }
        match self.backend.as_mut().ok_or("mouse is closed")? {
            #[cfg(target_os = "linux")]
            Backend::X11(native) => native.button(button, action)?,
            #[cfg(target_os = "linux")]
            Backend::WaylandMarker { desktop, marker } => {
                desktop.button(button, action)?;
                let (x, y) = desktop.position()?;
                marker.move_to(x, y).map_err(|error| {
                    format!("button input was sent but operation marker update failed: {error}")
                })?;
            }
            #[cfg(target_os = "linux")]
            Backend::Semantic(_) => unreachable!("pointer input was checked"),
            Backend::System(desktop) => desktop.button(button, action)?,
        }
        if action == "up" {
            self.held.remove(button);
        }
        Ok(())
    }

    pub fn click(&mut self, button: &str) -> Result<(), String> {
        self.button(button, "click")
    }

    pub(crate) fn holds_button(&self, button: &str) -> bool {
        self.held.contains(button)
    }

    pub fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        self.require_pointer_input()?;
        if dx.unsigned_abs() > 1000 || dy.unsigned_abs() > 1000 {
            return Err("scroll x/y must be -1000..1000".into());
        }
        match self.backend.as_mut().ok_or("mouse is closed")? {
            #[cfg(target_os = "linux")]
            Backend::X11(native) => native.scroll(dx, dy),
            #[cfg(target_os = "linux")]
            Backend::WaylandMarker { desktop, marker } => {
                desktop.scroll(dx, dy)?;
                let (x, y) = desktop.position()?;
                marker.move_to(x, y).map_err(|error| {
                    format!("scroll input was sent but operation marker update failed: {error}")
                })
            }
            #[cfg(target_os = "linux")]
            Backend::Semantic(_) => unreachable!("pointer input was checked"),
            Backend::System(desktop) => desktop.scroll(dx, dy),
        }
    }

    pub fn set_appearance(&mut self, style: &CursorStyle) -> Result<(), String> {
        self.set_cursor(&style.render()?)
    }
    pub fn set_cursor(&mut self, image: &CursorImage) -> Result<(), String> {
        image.validate()?;
        match self.backend.as_mut().ok_or("mouse is closed")? {
            #[cfg(target_os = "linux")]
            Backend::X11(native) => native.set_cursor(image),
            #[cfg(target_os = "linux")]
            Backend::WaylandMarker { marker, .. } | Backend::Semantic(marker) => {
                marker.set_cursor(image)
            }
            Backend::System(_) => {
                Err("custom appearance requires a mouse created with an appearance".into())
            }
        }
    }

    pub fn close(&mut self) -> Result<(), String> {
        let result = {
            let Some(mut backend) = self.backend.take() else {
                return Ok(());
            };
            match &mut backend {
                #[cfg(target_os = "linux")]
                Backend::X11(native) => {
                    self.held.clear();
                    native.close()
                }
                #[cfg(target_os = "linux")]
                Backend::WaylandMarker { desktop, marker } => {
                    let mut errors = Vec::new();
                    for b in std::mem::take(&mut self.held) {
                        if let Err(error) = desktop.button(&b, "up") {
                            errors.push(error);
                        }
                    }
                    if let Err(error) = marker.close() {
                        errors.push(error);
                    }
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(errors.join("; "))
                    }
                }
                #[cfg(target_os = "linux")]
                Backend::Semantic(marker) => marker.close(),
                Backend::System(desktop) => {
                    let mut errors = Vec::new();
                    for b in std::mem::take(&mut self.held) {
                        if let Err(e) = desktop.button(&b, "up") {
                            errors.push(e);
                        }
                    }
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(errors.join("; "))
                    }
                }
            }
        };
        self.user_lease.take();
        result
    }
}

impl Drop for Mouse {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn validate_point(point: Point) -> Result<(), String> {
    if !point.x.is_finite()
        || !point.y.is_finite()
        || point.x.abs() > MAX_COORDINATE
        || point.y.abs() > MAX_COORDINATE
    {
        Err("mouse coordinates must be finite and within the supported coordinate range".into())
    } else {
        Ok(())
    }
}
fn center(t: &LocatedTarget) -> Point {
    Point {
        x: t.bounds.x + t.bounds.width / 2.,
        y: t.bounds.y + t.bounds.height / 2.,
    }
}

pub(crate) fn require_semantic_wayland() -> Result<(), String> {
    if cfg!(target_os = "linux")
        && std::env::var("WAYLAND_DISPLAY").is_ok_and(|name| !name.is_empty())
        && matches!(
            std::env::var("ZUSE_BACKEND").as_deref(),
            Err(_) | Ok("" | "wayland")
        )
    {
        Ok(())
    } else {
        Err("unsupported_state: semantic mice require a native Linux Wayland session".into())
    }
}

#[cfg(target_os = "linux")]
fn require_x11() -> Result<(), String> {
    let selected = std::env::var("ZUSE_BACKEND").unwrap_or_default();
    match selected.as_str() {
        "x11"=>Ok(()),
        "" if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var("XDG_SESSION_TYPE").unwrap_or_default()!="wayland"=>Ok(()),
        ""|"wayland"=>Err("no independent-pointer support: independent mice require a native X11 MPX display; Wayland supports user_mouse with a click-through operation marker or appearance=\"system\"".into()),
        _=>Err("ZUSE_BACKEND must be x11 or wayland".into()),
    }
}

/// Evidence of submitted samples; application delivery is not asserted.
#[derive(Clone, Debug)]
pub struct MotionReceipt {
    pub mouse_id: u64,
    pub from: Point,
    pub to: Point,
    pub duration_ms: u32,
    pub elapsed_ms: u128,
    pub samples: usize,
    pub path_length: f64,
    pub target: Option<LocatedTarget>,
}
impl MotionReceipt {
    pub(crate) fn new(
        id: u64,
        p: &MovePlan,
        target: Option<LocatedTarget>,
        elapsed_ms: u128,
    ) -> Self {
        Self {
            mouse_id: id,
            from: p.from(),
            to: p.to(),
            duration_ms: p.duration_ms(),
            elapsed_ms,
            samples: p.samples().len(),
            path_length: p.length(),
            target,
        }
    }
    pub fn to_json(&self) -> String {
        let target=self.target.as_ref().map(|t|format!("{{\"path\":{},\"role\":{},\"name\":{},\"bounds\":{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}}}",crate::json::quote(&t.path),crate::json::quote(&t.role),crate::json::quote(&t.name),t.bounds.x,t.bounds.y,t.bounds.width,t.bounds.height)).unwrap_or_else(||"null".into());
        format!(
            "{{\"mouse_id\":{},\"from\":{{\"x\":{},\"y\":{}}},\"to\":{{\"x\":{},\"y\":{}}},\"duration_ms\":{},\"elapsed_ms\":{},\"samples\":{},\"path_length\":{},\"target\":{},\"delivery\":\"submitted\"}}",
            self.mouse_id,
            self.from.x,
            self.from.y,
            self.to.x,
            self.to.y,
            self.duration_ms,
            self.elapsed_ms,
            self.samples,
            self.path_length,
            target
        )
    }
}
