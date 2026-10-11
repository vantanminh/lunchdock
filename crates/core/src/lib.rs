//! Shared domain primitives for the Rudder control plane, agent, and CLI.
//!
//! Infrastructure crates depend on this crate. It does not talk to Docker,
//! PostgreSQL, or the network.

mod certs;
mod compose;
mod crypto;
mod driver;
mod error;
mod image_ref;
mod rollout;
mod traefik;
mod validate;

pub use certs::{
    certificate_matches_host, matching_certificate, validate_key_and_chain, CertificateDetails,
    HostMatch,
};
pub use compose::{compose_args, ComposeAction};
pub use crypto::Cipher;
pub use driver::{
    ContainerSpec, DockerInfo, ExecOutput, HealthState, NodeDriver, PortMapping, RegistryAuth,
    VolumeMount,
};
pub use error::Error;
pub use image_ref::{
    container_name, image_registry, normalize_registry, select_registry, validate_registry_url,
    RegistryRecord,
};
pub use rollout::{
    retire_container, rollout, HealthKind, HealthSpec, RolloutOutput, RolloutPlan, StartedContainer,
};
pub use traefik::{
    preview_hostname, render_dynamic_config, validate_host, validate_path_prefix, ProxyRoute,
    TlsMaterial, TlsMode, PROXY_NETWORK, TRAEFIK_CERT_DIR,
};
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
