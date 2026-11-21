#![cfg_attr(not(feature = "std"), no_std)]
//! Small, inspectable computer-use programs for agents.
//!
//! Disable default features for the allocation-free parser and motion kernel.
//! The host runtime and MCP transports are available with `std`.

pub mod language;
pub mod motion;

#[cfg(feature = "std")]
pub mod accessibility;
#[cfg(feature = "std")]
pub mod android;
#[cfg(feature = "std")]
pub mod browser;
#[cfg(feature = "std")]
pub mod human;
#[cfg(feature = "std")]
pub mod integrations;
#[cfg(feature = "std")]
pub mod json;
#[cfg(feature = "std")]
mod key_hold;
#[cfg(feature = "std")]
pub mod mcp;
#[cfg(feature = "std")]
pub mod observation;
#[cfg(feature = "std")]
pub mod platform;
#[cfg(feature = "std")]
pub mod runtime;
#[cfg(feature = "std")]
mod timing;
#[cfg(feature = "vision")]
pub mod vision;
#[cfg(feature = "vision")]
mod vision_runtime;
#[cfg(feature = "std")]
pub mod visual_timeline;

#[cfg(feature = "std")]
pub mod cursor;
#[cfg(feature = "std")]
pub mod mouse;
#[cfg(feature = "std")]
pub mod pointer;
#[cfg(feature = "std")]
pub mod script;
#[cfg(feature = "std")]
pub mod target;
#[cfg(feature = "std")]
pub mod touchpad;

#[cfg(feature = "std")]
pub mod device_io;

#[cfg(feature = "std")]
pub mod ios;

#[cfg(feature = "std")]
pub mod serial;

#[cfg(feature = "std")]
pub mod usb;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(feature = "std")]
mod screenshot;
