//! Shared domain primitives for the Rudder control plane, agent, and CLI.
//!
//! Infrastructure crates depend on this crate. It does not talk to Docker,
//! PostgreSQL, or the network.

mod error;
mod validate;

pub use error::Error;
pub use validate::validate_name;

/// Crate version, shared by every Rudder binary.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name used in logs, installers, and the panel.
pub const PRODUCT: &str = "rudder";
