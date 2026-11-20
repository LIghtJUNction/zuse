//! Focus control flow shared by Quartz and device-free regression tests.
use super::Result;

pub(super) fn ax_status(status: i32, operation: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{operation} failed with error {status}"))
    }
}

/// Resolve a unique title before activating the application or raising a window.
///
/// The native iterator owns the borrowed window objects and must remain alive
/// through both side effects. An unreadable title or a duplicate match makes the
/// target uncertain; neither case may choose an arbitrary window. This does not
/// make AX lookup and activation atomic against concurrent application changes.
pub(super) fn focus_window<W: Copy, I: IntoIterator<Item = W>>(
    target_title: &str,
    frontmost: impl FnOnce() -> Result<()>,
    windows: impl FnOnce() -> Result<I>,
    mut title: impl FnMut(W) -> Result<String>,
    mut raise: impl FnMut(W) -> Result<()>,
) -> Result<()> {
    let mut windows = windows()?.into_iter();
    let mut selected = None;
    for window in windows.by_ref() {
        if title(window)? == target_title {
            if selected.is_some() {
                return Err(
                    "requested macOS window title is ambiguous in AXWindows; no window was focused"
                        .into(),
                );
            }
            selected = Some(window);
        }
    }
    let selected = selected.ok_or("requested macOS window was not found in AXWindows")?;
    frontmost()?;
    raise(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn resolves_the_unique_window_before_activating_and_raising() {
        let calls = RefCell::new(Vec::new());
        let result = focus_window(
            "target",
            || {
                calls.borrow_mut().push("frontmost".to_string());
                Ok(())
            },
            || {
                calls.borrow_mut().push("windows".to_string());
                Ok(0..3)
            },
            |index| {
                calls.borrow_mut().push(format!("title:{index}"));
                Ok(if index == 1 { "target" } else { "other" }.into())
            },
            |index| {
                calls.borrow_mut().push(format!("raise:{index}"));
                ax_status(0, "AXRaise")
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            *calls.borrow(),
            [
                "windows",
                "title:0",
                "title:1",
                "title:2",
                "frontmost",
                "raise:1"
            ]
        );
    }

    #[test]
    fn missing_target_and_empty_window_list_are_errors() {
        for count in [0, 2] {
            let queried = Cell::new(0);
            let result = focus_window(
                "target",
                || panic!("a missing window must not activate the application"),
                || Ok(0..count),
                |_| {
                    queried.set(queried.get() + 1);
                    Ok("other".into())
                },
                |_| panic!("no matching window may be raised"),
            );
            assert_eq!(
                result.unwrap_err(),
                "requested macOS window was not found in AXWindows"
            );
            assert_eq!(queried.get(), count);
        }
    }

    #[test]
    fn frontmost_failure_stops_before_raising_after_unique_lookup() {
        let calls = RefCell::new(Vec::new());
        let result = focus_window(
            "target",
            || {
                calls.borrow_mut().push("frontmost");
                ax_status(-25204, "AXFrontmost")
            },
            || {
                calls.borrow_mut().push("windows");
                Ok(0..1)
            },
            |_| {
                calls.borrow_mut().push("title");
                Ok("target".into())
            },
            |_| panic!("no raise after AXFrontmost failure"),
        );
        assert_eq!(result.unwrap_err(), "AXFrontmost failed with error -25204");
        assert_eq!(*calls.borrow(), ["windows", "title", "frontmost"]);
    }

    #[test]
    fn windows_query_failure_is_returned_without_querying_or_raising() {
        let calls = RefCell::new(Vec::new());
        let result = focus_window(
            "target",
            || {
                calls.borrow_mut().push("frontmost");
                Ok(())
            },
            || {
                calls.borrow_mut().push("windows");
                ax_status(-25204, "AXWindows")?;
                Ok(0..1)
            },
            |_| panic!("no title query after AXWindows failure"),
            |_| panic!("no raise after AXWindows failure"),
        );
        assert_eq!(result.unwrap_err(), "AXWindows failed with error -25204");
        assert_eq!(*calls.borrow(), ["windows"]);
    }

    #[test]
    fn unreadable_window_rejects_selection_before_activation() {
        let calls = RefCell::new(Vec::new());
        let result = focus_window(
            "target",
            || panic!("unreadable titles must not activate the application"),
            || Ok(0..3),
            |index| {
                calls.borrow_mut().push(format!("title:{index}"));
                if index == 0 {
                    ax_status(-25212, "AXTitle")?;
                }
                Ok("target".into())
            },
            |index| {
                calls.borrow_mut().push(format!("raise:{index}"));
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err(), "AXTitle failed with error -25212");
        assert_eq!(*calls.borrow(), ["title:0"]);
    }

    #[test]
    fn unmatched_window_with_title_error_returns_the_native_error() {
        let queried = Cell::new(0);
        let result = focus_window(
            "target",
            || Ok(()),
            || Ok(0..2),
            |index| {
                queried.set(queried.get() + 1);
                if index == 0 {
                    ax_status(-25202, "AXTitle")?;
                }
                Ok("other".into())
            },
            |_| panic!("an unreadable or nonmatching window may not be raised"),
        );
        assert_eq!(result.unwrap_err(), "AXTitle failed with error -25202");
        assert_eq!(queried.get(), 1);
    }

    #[test]
    fn raise_failure_is_returned_without_retrying_another_window() {
        let calls = RefCell::new(Vec::new());
        let result = focus_window(
            "target",
            || {
                calls.borrow_mut().push("frontmost".to_string());
                Ok(())
            },
            || {
                calls.borrow_mut().push("windows".to_string());
                Ok(0..2)
            },
            |index| {
                calls.borrow_mut().push(format!("title:{index}"));
                Ok(if index == 0 { "target" } else { "other" }.into())
            },
            |index| {
                calls.borrow_mut().push(format!("raise:{index}"));
                ax_status(-25206, "AXRaise")
            },
        );
        assert_eq!(result.unwrap_err(), "AXRaise failed with error -25206");
        assert_eq!(
            *calls.borrow(),
            ["windows", "title:0", "title:1", "frontmost", "raise:0"]
        );
    }

    #[test]
    fn window_owner_lives_through_raise_and_is_released_on_all_exits() {
        struct Windows<'a> {
            remaining: std::ops::Range<usize>,
            dropped: &'a Cell<usize>,
        }
        impl Iterator for Windows<'_> {
            type Item = usize;
            fn next(&mut self) -> Option<Self::Item> {
                self.remaining.next()
            }
        }
        impl Drop for Windows<'_> {
            fn drop(&mut self) {
                self.dropped.set(self.dropped.get() + 1);
            }
        }
        for (target_exists, status) in [(true, 0), (true, -25206), (false, 0)] {
            let dropped = Cell::new(0);
            let result = focus_window(
                "target",
                || {
                    assert_eq!(dropped.get(), 0);
                    Ok(())
                },
                || {
                    Ok(Windows {
                        remaining: 0..1,
                        dropped: &dropped,
                    })
                },
                |_| {
                    assert_eq!(dropped.get(), 0);
                    Ok(if target_exists { "target" } else { "other" }.into())
                },
                |_| {
                    assert_eq!(dropped.get(), 0);
                    ax_status(status, "AXRaise")
                },
            );
            assert_eq!(result.is_ok(), target_exists && status == 0);
            assert_eq!(dropped.get(), 1);
        }
    }
    #[test]
    fn rejects_duplicate_titles_before_foreground_activation() {
        let activations = Cell::new(0);
        let raised = RefCell::new(Vec::new());
        let result = focus_window(
            "shared title",
            || {
                activations.set(activations.get() + 1);
                Ok(())
            },
            || Ok(0..2),
            |_| Ok("shared title".into()),
            |window| {
                raised.borrow_mut().push(window);
                Ok(())
            },
        );
        assert!(
            result.is_err(),
            "duplicate titles must not pick the first window"
        );
        assert_eq!(activations.get(), 0);
        assert!(raised.borrow().is_empty());
    }

    #[test]
    fn missing_target_does_not_activate_the_application() {
        let activations = Cell::new(0);
        let result = focus_window(
            "gone",
            || {
                activations.set(activations.get() + 1);
                Ok(())
            },
            || Ok(0..1),
            |_| Ok("other".into()),
            |_| panic!("no matching window may be raised"),
        );
        assert!(result.is_err());
        assert_eq!(activations.get(), 0);
    }

    #[test]
    fn unreadable_title_after_a_candidate_still_prevents_activation() {
        let calls = RefCell::new(Vec::new());
        let result = focus_window(
            "target",
            || panic!("an incomplete title scan must not activate the application"),
            || Ok(0..2),
            |index| {
                calls.borrow_mut().push(index);
                if index == 0 {
                    Ok("target".into())
                } else {
                    ax_status(-25202, "AXTitle")?;
                    unreachable!()
                }
            },
            |_| panic!("an incomplete title scan must not raise a candidate"),
        );
        assert_eq!(result.unwrap_err(), "AXTitle failed with error -25202");
        assert_eq!(*calls.borrow(), [0, 1]);
    }

    #[test]
    fn separated_duplicate_titles_and_empty_titles_are_ambiguous() {
        for (target, titles) in [("same", ["same", "other", "same"]), ("", ["", "other", ""])] {
            let result = focus_window(
                target,
                || panic!("ambiguous titles must not activate the application"),
                || Ok(0..titles.len()),
                |index| Ok(titles[index].into()),
                |_| panic!("ambiguous titles must not raise a window"),
            );
            assert!(result.unwrap_err().contains("ambiguous"));
        }
    }
}
