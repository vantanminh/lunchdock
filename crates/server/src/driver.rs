//! Local Docker driver. Remote servers use the same [`NodeDriver`] calls later.

mod local;

pub use local::LocalDriver;
