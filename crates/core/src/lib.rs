//! Shared domain primitives for the Rudder control plane, agent, and CLI.
//!
//! Infrastructure crates depend on this crate. It does not talk to Docker,
//! PostgreSQL, or the network.

mod buildspec;
mod certs;
mod compose;
mod crypto;
mod database;
mod driver;
mod error;
mod github_app;
mod image_ref;
mod retention;
mod rollout;
mod traefik;
mod validate;
mod webhook;

pub use buildspec::{
    can_start, docker_build_argv, git_clone_argv, nixpacks_argv, redact, static_dockerfile,
    validate_branch, validate_git_url, validate_rel_path, BuildPlan, BuildStrategy,
};
pub use certs::{
    certificate_matches_host, matching_certificate, validate_key_and_chain, CertificateDetails,
    HostMatch,
};
pub use compose::{compose_args, ComposeAction};
pub use crypto::Cipher;
pub use database::{connection_string, database_runtime, DatabaseRequest, DatabaseRuntime, Engine};
pub use driver::{
    ContainerSpec, DockerInfo, ExecOutput, HealthState, NodeDriver, PortMapping, RegistryAuth,
    VolumeMount,
};
pub use error::Error;
pub use github_app::github_app_jwt;
pub use image_ref::{
    container_name, image_registry, normalize_registry, select_registry, validate_registry_url,
    RegistryRecord,
};
pub use retention::{expired_ids, BackupPoint, RetentionPolicy};
pub use rollout::{
    retire_container, rollout, HealthKind, HealthSpec, RolloutOutput, RolloutPlan, StartedContainer,
};
pub use traefik::{
    preview_hostname, render_dynamic_config, validate_host, validate_path_prefix, ProxyRoute,
    TlsMaterial, TlsMode, PROXY_NETWORK, TRAEFIK_CERT_DIR,
};
pub use validate::{validate_env_key, validate_image_ref, validate_name};
pub use webhook::{evaluate_hook, HookInput, HookKind, HookOutcome};

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
