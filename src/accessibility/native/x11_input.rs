//! Confirmed UTF-8 insertion for an identified local X11 foreground window.
//! Uses the existing serialized/pinned AT-SPI connection, never the clipboard.
use super::*;

type Bounds = (i32, i32, u32, u32);

fn unsupported(message: &str) -> String {
    format!("Unsupported X11 Unicode text: {message}; no input was sent")
}

fn uncertain(message: impl std::fmt::Display) -> String {
    format!(
        "X11 Unicode insertion was not fully confirmed: {message}; text may have changed, do not retry automatically"
    )
}

fn integer(
    api: &Api,
    object: Obj,
    getter: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
) -> Result<c_int> {
    let mut error = core::ptr::null_mut();
    let value = unsafe { getter(object, &mut error) };
    api.error(error)?;
    if value < 0 {
        return Err("AT-SPI returned a negative text offset/count".into());
    }
    Ok(value)
}

fn contains(api: &Api, states: &Object<'_>, state: c_int) -> bool {
    unsafe { (api.state_contains)(states.ptr, state) != 0 }
}

fn fresh_states(api: &Api, object: Obj) -> Result<Object<'_>> {
    // Invalidate this object, then obtain one live state snapshot. Disabling the
    // application cache instead makes state_set_contains perform another RPC
    // for every bit and can reenter libatspi's pending peer-cache dispatch.
    unsafe { (api.clear_cache)(object) };
    api.object(unsafe { (api.state_set)(object) })
}

fn bounds(api: &Api, object: Obj) -> Result<Bounds> {
    let interface = api.object(unsafe { (api.component_iface)(object) })?;
    let mut error = core::ptr::null_mut();
    // ATSPI_COORD_TYPE_SCREEN is zero. g_free owns the returned rectangle.
    let rectangle = unsafe { (api.extents)(interface.ptr, 0, &mut error) };
    let value = if rectangle.is_null() {
        None
    } else {
        unsafe {
            let r = &*rectangle;
            let value = (r.x, r.y, r.width, r.height);
            (api.free)(rectangle.cast());
            Some(value)
        }
    };
    api.error(error)?;
    let (x, y, width, height) = value.ok_or("AT-SPI did not return screen bounds")?;
    if width <= 0 || height <= 0 {
        return Err("AT-SPI returned empty screen bounds".into());
    }
    Ok((x, y, width as u32, height as u32))
}

fn inside(inner: Bounds, outer: Bounds) -> bool {
    inner.0 >= outer.0
        && inner.1 >= outer.1
        && i64::from(inner.0) + i64::from(inner.2) <= i64::from(outer.0) + i64::from(outer.2)
        && i64::from(inner.1) + i64::from(inner.3) <= i64::from(outer.1) + i64::from(outer.3)
}

struct FocusScan<'a> {
    api: &'a Api,
    client: Bounds,
    frame: Option<Bounds>,
    visited: usize,
    deadline: Instant,
    focused: Vec<(Object<'a>, bool)>,
}

impl<'a> FocusScan<'a> {
    fn visit(&mut self, object: Object<'a>, depth: usize, matched_frame: bool) -> Result<()> {
        // This traversal starts at the application, one level below desktop 0.
        if self.visited >= 4096 || depth >= MAX_DEPTH || Instant::now() >= self.deadline {
            return Err(unsupported("focus discovery exceeded its bounds"));
        }
        self.visited += 1;
        let states = fresh_states(self.api, object.ptr)?;
        let focused = contains(self.api, &states, 12); // ATSPI_STATE_FOCUSED
        let mut matched_frame = matched_frame;
        if contains(self.api, &states, 1) {
            // ATSPI_STATE_ACTIVE
            let role = self.api.get_string(object.ptr, self.api.role)?;
            if matches!(role.as_str(), "frame" | "dialog" | "window" | "alert") {
                let rect = bounds(self.api, object.ptr)?;
                matched_frame = rect == self.client || Some(rect) == self.frame;
            }
        }
        let count = self.api.count(object.ptr)?;
        if count > 4096 {
            return Err(unsupported("focus discovery has too many children"));
        }
        for index in 0..count {
            self.visit(self.api.child(object.ptr, index)?, depth + 1, matched_frame)?;
        }
        if focused {
            self.focused.push((object, matched_frame));
            if self.focused.len() > 1 {
                return Err(unsupported(
                    "multiple accessibility objects claim keyboard focus",
                ));
            }
        }
        Ok(())
    }
}

fn editable(api: &Api, object: Obj, client: Bounds) -> Result<()> {
    let states = fresh_states(api, object)?;
    // EDITABLE, ENABLED, FOCUSED, SENSITIVE, SHOWING, VISIBLE. Refuse a stale,
    // defunct, read-only or hidden control before even requesting its text.
    if ![7, 8, 12, 24, 25, 30]
        .iter()
        .all(|&state| contains(api, &states, state))
        || [6, 27, 43]
            .iter()
            .any(|&state| contains(api, &states, state))
        || !inside(bounds(api, object)?, client)
    {
        return Err(unsupported(
            "the focused object is not an editable visible control in the X11 window",
        ));
    }
    if api
        .get_string(object, api.role)?
        .to_ascii_lowercase()
        .contains("password")
    {
        return Err(unsupported("password input requires human assistance"));
    }
    Ok(())
}

fn exact_range(api: &Api, object: Obj, start: c_int, end: c_int, expected: &str) -> Result<()> {
    let mut error = core::ptr::null_mut();
    let pointer = unsafe { (api.text)(object, start, end, &mut error) };
    let matches = if pointer.is_null() {
        false
    } else {
        unsafe {
            let matches = CStr::from_ptr(pointer).to_bytes() == expected.as_bytes();
            (api.free)(pointer.cast());
            matches
        }
    };
    api.error(error)?;
    if !matches {
        return Err("the receiver did not retain the exact UTF-8 text".into());
    }
    Ok(())
}

fn accepted(api: &Api, success: c_int, error: ErrPtr, action: &str) -> Result<()> {
    api.error(error)?;
    if success == 0 {
        return Err(format!("the receiver rejected {action}"));
    }
    Ok(())
}

fn selected_text(api: &Api, object: Obj, start: c_int, end: c_int) -> Result<String> {
    // Bound the request before asking the application to allocate a response.
    // At most 64 Ki Unicode characters, hence at most 256 KiB of valid UTF-8.
    let characters = end - start;
    if characters > 65_536 {
        return Err(unsupported(
            "selection exceeds the 64 Ki-character snapshot limit",
        ));
    }
    let mut error = core::ptr::null_mut();
    let pointer = unsafe { (api.text)(object, start, end, &mut error) };
    let value = if pointer.is_null() {
        Err(unsupported("selection text is unavailable"))
    } else {
        unsafe {
            let bytes = CStr::from_ptr(pointer).to_bytes();
            let value = if bytes.len() > 262_144 {
                Err(unsupported(
                    "selection response exceeds the UTF-8 snapshot limit",
                ))
            } else {
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|_| unsupported("selection response is not valid UTF-8"))
            };
            (api.free)(pointer.cast());
            value
        }
    };
    api.error(error)?;
    let value = value?;
    if value.chars().count() != characters as usize {
        return Err(unsupported(
            "selection text does not match its character offsets",
        ));
    }
    Ok(value)
}

pub(in crate::accessibility) fn insert(
    pid: u32,
    client: Bounds,
    frame: Option<Bounds>,
    text: &str,
    still_focused: impl Fn() -> Result<()>,
) -> Result<()> {
    let _guard = ACCESS
        .lock()
        .map_err(|_| "accessibility state is poisoned")?;
    let api = load().map_err(|error| unsupported(&error))?;
    let root = api.resolve("0").map_err(|error| unsupported(&error))?;
    let applications = api.count(root.ptr)?;
    if applications > 4096 {
        return Err(unsupported("too many accessibility applications"));
    }
    let mut scan = FocusScan {
        api,
        client,
        frame,
        visited: 0,
        deadline: Instant::now() + Duration::from_secs(5),
        focused: Vec::new(),
    };
    for index in 0..applications {
        if Instant::now() >= scan.deadline {
            return Err(unsupported("application discovery timed out"));
        }
        let app = api.child(root.ptr, index)?;
        let mut error = core::ptr::null_mut();
        let actual_pid = unsafe { (api.process_id)(app.ptr, &mut error) };
        api.error(error)?;
        if actual_pid == pid {
            scan.visit(app, 0, false)?;
        }
    }
    let (object, matched_frame) = scan
        .focused
        .pop()
        .ok_or_else(|| unsupported("no focused accessibility object belongs to the X11 client"))?;
    if !matched_frame {
        return Err(unsupported(
            "the active accessibility window does not match the X11 client geometry",
        ));
    }
    editable(api, object.ptr, client)?;
    let interface = api.object(unsafe { (api.editable_iface)(object.ptr) })?;
    let text_interface = api.object(unsafe { (api.text_iface)(object.ptr) })?;
    let length = integer(api, text_interface.ptr, api.text_count)?;
    let caret = integer(api, text_interface.ptr, api.caret)?;
    let selections = integer(api, text_interface.ptr, api.selections)?;
    if caret > length || selections > 1 {
        return Err(unsupported("invalid caret or multiple text selections"));
    }
    let (start, end) = if selections == 1 {
        let mut error = core::ptr::null_mut();
        let range = unsafe { (api.selection)(text_interface.ptr, 0, &mut error) };
        let value = if range.is_null() {
            None
        } else {
            unsafe {
                let value = ((*range).start, (*range).end);
                (api.free)(range.cast());
                Some(value)
            }
        };
        api.error(error)?;
        value.ok_or_else(|| unsupported("selection boundaries are unavailable"))?
    } else {
        (caret, caret)
    };
    if start < 0 || end < start || end > length {
        return Err(unsupported("selection boundaries are invalid"));
    }
    let original_selection = if end > start {
        Some(selected_text(api, text_interface.ptr, start, end)?)
    } else {
        None
    };
    let characters =
        c_int::try_from(text.chars().count()).map_err(|_| unsupported("text is too long"))?;
    let bytes = c_int::try_from(text.len()).map_err(|_| unsupported("text is too long"))?;
    let expanded_length = length
        .checked_add(characters)
        .ok_or_else(|| unsupported("text offset overflow"))?;
    let final_caret = start
        .checked_add(characters)
        .ok_or_else(|| unsupported("caret offset overflow"))?;
    let shifted_end = end
        .checked_add(characters)
        .ok_or_else(|| unsupported("selection offset overflow"))?;
    let string = CString::new(text).map_err(|_| unsupported("text contains NUL"))?;
    // Complete all discovery and validation before the first mutation. Once a
    // mutation starts, no error falls back to keys, retries, or old whole text.
    editable(api, object.ptr, client)?;
    still_focused().map_err(|error| format!("{error}; no input was sent"))?;
    let perform = || -> Result<()> {
        let mut error = core::ptr::null_mut();
        // AT-SPI position is in Unicode characters; InsertText length is BYTES.
        // Insert first: a rejected/filtered insertion must not delete the user's
        // selected text. Selection replacement is explicitly not a transaction.
        let success =
            unsafe { (api.insert)(interface.ptr, start, string.as_ptr(), bytes, &mut error) };
        accepted(api, success, error, "UTF-8 insertion")?;
        if integer(api, text_interface.ptr, api.text_count)? != expanded_length {
            return Err("the receiver filtered or truncated the insertion".into());
        }
        exact_range(api, text_interface.ptr, start, final_caret, text)?;
        if let Some(original) = original_selection.as_deref() {
            still_focused()?;
            // A callback can rewrite the old selection without changing the
            // character count. Never delete that replacement blindly.
            exact_range(api, text_interface.ptr, final_caret, shifted_end, original)?;
            let mut error = core::ptr::null_mut();
            let success =
                unsafe { (api.delete)(interface.ptr, final_caret, shifted_end, &mut error) };
            accepted(api, success, error, "selection replacement")?;
        }
        if integer(api, text_interface.ptr, api.text_count)? != expanded_length - (end - start) {
            return Err("the receiver did not confirm selection replacement".into());
        }
        exact_range(api, text_interface.ptr, start, final_caret, text)?;
        still_focused()?;
        if integer(api, text_interface.ptr, api.selections)? > 0 {
            let mut error = core::ptr::null_mut();
            let success = unsafe { (api.remove_selection)(text_interface.ptr, 0, &mut error) };
            accepted(api, success, error, "clearing the completed selection")?;
        }
        let mut error = core::ptr::null_mut();
        let success = unsafe { (api.set_caret)(text_interface.ptr, final_caret, &mut error) };
        accepted(api, success, error, "caret placement")?;
        if integer(api, text_interface.ptr, api.caret)? != final_caret
            || integer(api, text_interface.ptr, api.selections)? != 0
        {
            return Err("the receiver did not confirm the final caret/selection".into());
        }
        Ok(())
    };
    perform().map_err(uncertain)
}
