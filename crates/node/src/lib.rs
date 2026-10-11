//! The Docker Engine driver. The panel uses it for its own host, and the agent
//! uses the same code on every remote server.

mod local;
pub mod ops;

pub use local::{confined_path, LocalDriver, DEFAULT_CACHE_ADMIN, HELPER_IMAGE};
