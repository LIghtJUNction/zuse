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
pub mod browser;
#[cfg(feature = "std")]
pub mod human;
#[cfg(feature = "std")]
pub mod integrations;
#[cfg(feature = "std")]
pub mod json;
#[cfg(feature = "std")]
pub mod mcp;
#[cfg(feature = "std")]
pub mod platform;
#[cfg(feature = "std")]
pub mod runtime;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
