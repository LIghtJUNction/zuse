//! A fully checked script, with lexical ownership resolved before input.
use crate::{
    cursor::CursorImage, motion::Point, mouse::MouseOptions, pointer::MoveOptions,
    target::TargetQuery,
};

pub(crate) type MouseId = u32;

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Scope {
        body: Vec<Node>,
        owned: Vec<MouseId>,
    },
    Parallel(Vec<Node>),
    Create {
        id: MouseId,
        options: MouseOptions,
    },
    Action {
        id: MouseId,
        action: Action,
    },
    Wait(u32),
    Touchpad(crate::touchpad::Command),
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Move {
        to: Point,
        options: MoveOptions,
    },
    MoveTo {
        query: OwnedTarget,
        options: MoveOptions,
    },
    Click {
        button: String,
        count: u32,
        interval: u32,
    },
    Button {
        button: String,
        down: bool,
    },
    Scroll {
        dx: i32,
        dy: i32,
    },
    Appearance(CursorImage),
    Position,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedTarget {
    pub text: String,
    pub role: Option<String>,
    pub scope: Option<String>,
    pub exact: bool,
    pub case_sensitive: bool,
    pub index: Option<usize>,
}

impl OwnedTarget {
    pub(crate) fn query(&self) -> TargetQuery<'_> {
        TargetQuery {
            text: &self.text,
            role: self.role.as_deref(),
            scope: self.scope.as_deref(),
            exact: self.exact,
            case_sensitive: self.case_sensitive,
            index: self.index,
        }
    }
}

/// Counts of expanded work, computed without opening a display or creating mice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanSummary {
    pub steps: usize,
    pub mice: usize,
    pub parallel_groups: usize,
    pub shared_mice: usize,
    pub static_wait_ms: u64,
}

/// A validated script. Recompile after changing source or its custom image files.
#[derive(Clone, Debug)]
pub struct CompiledScript {
    pub(crate) root: Node,
    pub summary: PlanSummary,
}

impl CompiledScript {
    /// Check the entire program before any native resource or input effect.
    /// Pure compilation and dry-run inspection do not require native support.
    pub fn require_native_capabilities(&self) -> Result<(), String> {
        fn visit(node: &Node) -> Result<(), String> {
            match node {
                Node::Touchpad(command) => command.require_native(),
                Node::Scope { body, .. } | Node::Parallel(body) => {
                    for child in body {
                        visit(child)?;
                    }
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        visit(&self.root)
    }
}
