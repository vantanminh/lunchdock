//! Shared domain primitives for the Rudder control plane, agent, and CLI.
//!
//! Infrastructure crates depend on this crate. It does not talk to Docker,
//! PostgreSQL, or the network.

mod backup;
mod buildspec;
mod cache;
mod certs;
mod compose;
mod crypto;
mod database;
mod driver;
mod envelope;
mod error;
mod github_app;
mod image_ref;
mod logs;
mod metrics;
mod retention;
mod rollout;
mod schedule;
mod stack;
mod template;
mod traefik;
mod validate;
mod webhook;

pub use backup::{
    dump_command, dump_extension, filter_databases, mysql_list_databases_command, object_key,
    redis_bgsave_command, redis_persistence_command, restore_command, retry_backoff_secs,
    validate_extra_args, validate_prefix, validate_tool_options, Compression, Consistency,
    DumpCredentials, KeyVars, RedisPersistence, RetentionSpec, ToolCommand, ToolOptions,
};
pub use buildspec::{
    can_start, docker_build_argv, git_clone_argv, nixpacks_argv, redact, static_dockerfile,
    validate_branch, validate_git_url, validate_rel_path, BuildPlan, BuildStrategy,
};
pub use cache::{
    cache_path, cache_preset, encoding_class, validate_upstream, CacheControl, CacheDocument,
    CacheHostStats, CachePolicy, CachePreset, CachePurge, CachePurgeResult, CacheRequest,
    CacheRule, CacheSite, CacheStats, Freshness, RequestDecision, RuleAction, RuleMatch, RuleMode,
    CACHE_CONTAINER, CACHE_PORT, CACHE_PRESETS, CACHE_RULES_FILE, CACHE_TOKEN_FILE,
    DEFAULT_MAX_OBJECT_BYTES, DEFAULT_MEMORY_BYTES,
};
pub use certs::{
    certificate_matches_host, matching_certificate, validate_key_and_chain, CertificateDetails,
    HostMatch,
};
pub use compose::{compose_args, ComposeAction};
pub use crypto::Cipher;
pub use database::{connection_string, database_runtime, DatabaseRequest, DatabaseRuntime, Engine};
pub use driver::{
    validate_host_path, ArchiveSource, ByteStream, ContainerInfo, ContainerSpec, ContainerStats,
    DockerInfo, ExecOutput, ExecSpec, HealthState, LogLine, LogLineStream, LogStreamKind,
    NodeDriver, PortMapping, RegistryAuth, TerminalInput, TerminalInputStream, VolumeMount,
};
pub use envelope::{Decryptor, Encryptor};
pub use error::Error;
pub use github_app::github_app_jwt;
pub use image_ref::{
    container_name, image_registry, normalize_registry, select_registry, validate_registry_url,
    RegistryRecord,
};
pub use logs::{
    escape_like, normalize_level, parse_docker_timestamp, parse_line, partition_day,
    partitions_to_drop, validate_source as validate_log_source, ParsedLog, LOG_LEVELS,
    LOG_PARTITION_PREFIX, LOG_SOURCES, MAX_LOG_MESSAGE,
};
pub use metrics::{
    alert_needs_threshold, container_cpu_percent, cpu_percent, event_matches, parse_loadavg,
    parse_meminfo, parse_net_dev, parse_proc_stat, percent, rate, read_host_stats, resolution_for,
    step_alert, validate_alert_kind, AlertState, AlertTransition, CpuTimes, HostStats, Resolution,
    ALERT_KINDS, SAMPLE_INTERVAL_SECS,
};
pub use retention::{expired_ids, BackupPoint, RetentionPolicy};
pub use rollout::{
    retire_container, rollout, HealthKind, HealthSpec, RolloutOutput, RolloutPlan, StartedContainer,
};
pub use schedule::{is_due, next_after, parse_timezone, validate_schedule, PRESETS};
pub use stack::{
    compose_variables, render_env_file, transform_compose, validate_compose_service, StackPlan,
    StackService, MAX_COMPOSE_BYTES,
};
pub use template::{ResolvedTemplate, Template, TemplateVariable, VariableKind};
pub use traefik::{
    preview_hostname, render_dynamic_config, validate_host, validate_path_prefix, ProxyRoute,
    TlsMaterial, TlsMode, PROXY_NETWORK, TRAEFIK_CERT_DIR, TRAEFIK_CONTAINER,
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
