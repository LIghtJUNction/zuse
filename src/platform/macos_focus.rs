//! Focus control flow shared by Quartz and device-free regression tests.
use super::Result;

pub(super) fn ax_status(status: i32, operation: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{operation} failed with error {status}"))
    }
}

/// Keep native objects in the returned iterator alive until title lookup and
/// raising finish. The adapter supplies native operations; tests supply outcomes.
pub(super) fn focus_window<W: Copy, I: IntoIterator<Item = W>>(
    target_title: &str,
    frontmost: impl FnOnce() -> Result<()>,
    windows: impl FnOnce() -> Result<I>,
    mut title: impl FnMut(W) -> Result<String>,
    mut raise: impl FnMut(W) -> Result<()>,
) -> Result<()> {
    frontmost()?;
    let mut title_error = None;
    for window in windows()? {
        match title(window) {
            Ok(title) if title == target_title => return raise(window),
            Ok(_) => {}
            Err(error) => {
                // A stale or untitled unrelated window must not hide a later
                // match. Retain the failure if no requested window is found.
                title_error.get_or_insert(error);
            }
        }
    }
    Err(title_error.unwrap_or_else(|| "requested macOS window was not found in AXWindows".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn focuses_only_the_first_matching_window_in_native_order() {
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
                Ok(if index == 0 { "other" } else { "target" }.into())
            },
            |index| {
                calls.borrow_mut().push(format!("raise:{index}"));
                ax_status(0, "AXRaise")
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            *calls.borrow(),
            ["frontmost", "windows", "title:0", "title:1", "raise:1"]
        );
    }

    #[test]
    fn missing_target_and_empty_window_list_are_errors() {
        for count in [0, 2] {
            let queried = Cell::new(0);
            let result = focus_window(
                "target",
                || Ok(()),
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
    fn frontmost_failure_stops_before_enumerating_windows() {
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
            |_| panic!("no title query after AXFrontmost failure"),
            |_| panic!("no raise after AXFrontmost failure"),
        );
        assert_eq!(result.unwrap_err(), "AXFrontmost failed with error -25204");
        assert_eq!(*calls.borrow(), ["frontmost"]);
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
        assert_eq!(*calls.borrow(), ["frontmost", "windows"]);
    }

    #[test]
    fn unreadable_unrelated_window_does_not_hide_a_later_match() {
        let calls = RefCell::new(Vec::new());
        let result = focus_window(
            "target",
            || Ok(()),
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
        assert_eq!(result, Ok(()));
        assert_eq!(*calls.borrow(), ["title:0", "title:1", "raise:1"]);
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
        assert_eq!(queried.get(), 2);
    }

    #[test]
    fn raise_failure_is_returned_without_trying_a_second_window() {
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
                Ok("target".into())
            },
            |index| {
                calls.borrow_mut().push(format!("raise:{index}"));
                ax_status(-25206, "AXRaise")
            },
        );
        assert_eq!(result.unwrap_err(), "AXRaise failed with error -25206");
        assert_eq!(
            *calls.borrow(),
            ["frontmost", "windows", "title:0", "raise:0"]
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
                || Ok(()),
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
}
