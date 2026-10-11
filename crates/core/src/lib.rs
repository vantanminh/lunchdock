//! Shared domain primitives for the Rudder control plane, agent, and CLI.
//!
//! Infrastructure crates depend on this crate. It does not talk to Docker,
//! PostgreSQL, or the network.

mod compose;
mod crypto;
mod driver;
mod error;
mod validate;

pub use compose::{compose_args, ComposeAction};
pub use crypto::Cipher;
pub use driver::{
    ContainerSpec, DockerInfo, ExecOutput, NodeDriver, PortMapping, RegistryAuth, VolumeMount,
};
pub use error::Error;
pub use validate::{validate_env_key, validate_image_ref, validate_name};

/// Fill `N` bytes from the process CSPRNG.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::fill(&mut bytes);
    bytes
}

/// Crate version, shared by every Rudder binary.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name used in logs, installers, and the panel.
pub const PRODUCT: &str = "rudder";
