//! Machine drivers. `LocalDriver` talks to the panel host's Docker; remote servers
//! are reached through their agent session.

pub mod remote;

pub use rudder_node::{LocalDriver, DEFAULT_CACHE_ADMIN};
