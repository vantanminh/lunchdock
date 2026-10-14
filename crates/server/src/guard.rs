//! Load guard on the panel host.
//!
//! Every few seconds the panel reads memory, CPU and pressure of its host. While
//! the host is busy, new builds wait in the queue; when free memory is about to
//! run out, running builds are stopped. Builds are what usually push a small
//! server over the edge, and the panel, its database and Traefik share that
//! server, so stopping them first keeps the panel reachable to fix the rest.
//!
//! On start, and every few minutes, the panel also raises the CPU weight of its
//! own container, the database and Traefik, and protects part of their memory
//! from reclaim. Apps run with a higher OOM score, so the kernel kills an app
//! before the panel when memory does run out.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rudder_core::{
    read_host_pressure, Assessment, CpuTimes, GuardLevel, GuardLimits, GuardReason, GuardTracker,
    HostPressure, CONTROL_PLANE_CPU_SHARES, CONTROL_PLANE_OOM_SCORE_ADJ, DEFAULT_GUARD_CPU_PERCENT,
    DEFAULT_MEMORY_RESERVE_PERCENT, GUARD_INTERVAL_SECS, PROXY_CPU_SHARES, TRAEFIK_CONTAINER,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::events::{self, Event};
use crate::state::AppState;

/// A build held only because of CPU starts anyway after this long, so a server
/// whose apps keep the CPU busy still deploys.
pub const CPU_HOLD_LIMIT: Duration = Duration::from_secs(20 * 60);
/// Name of the panel database container created by the installer.
pub const POSTGRES_CONTAINER: &str = "rudder-postgres";
const MIB: i64 = 1024 * 1024;
/// Re-apply container priorities this often, in samples (about 10 minutes).
const PROTECT_EVERY: u64 = 200;
/// Reload settings this often, in samples (about a minute).
const SETTINGS_EVERY: u64 = 20;

/// Load guard settings, stored in `panel_settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GuardSettings {
    /// Hold and stop builds when the host is overloaded.
    pub enabled: bool,
    /// An admin paused every new build.
    pub builds_paused: bool,
    /// New builds wait while free memory is below this share; running builds
    /// stop below half of it.
    pub memory_reserve_percent: u8,
    /// New builds wait while CPU stays above this share.
    pub cpu_percent: u8,
}

impl Default for GuardSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            builds_paused: false,
            memory_reserve_percent: DEFAULT_MEMORY_RESERVE_PERCENT,
            cpu_percent: DEFAULT_GUARD_CPU_PERCENT,
        }
    }
}

impl GuardSettings {
    pub fn limits(&self) -> GuardLimits {
        GuardLimits {
            memory_reserve_percent: self.memory_reserve_percent,
            cpu_percent: self.cpu_percent,
        }
    }
}

/// What the guard sees now.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GuardView {
    pub level: GuardLevel,
    /// Why the level is not `ok`; empty when it is.
    pub reasons: Vec<GuardReason>,
    /// When the level last changed.
    pub since: Option<DateTime<Utc>>,
    pub sampled_at: Option<DateTime<Utc>>,
    pub pressure: Option<HostPressure>,
    /// Why the host could not be read, when it could not.
    pub error: Option<String>,
    pub settings: GuardSettings,
    /// Available memory below which running builds are stopped, in percent.
    pub critical_memory_percent: f64,
    /// New builds wait now: paused, or the guard holds them.
    pub holding: bool,
    /// Builds and stack actions the guard stopped since the panel started.
    pub stopped_builds: u64,
    pub last_stopped_at: Option<DateTime<Utc>>,
}

/// How one control-plane container is protected.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProtectionView {
    pub name: String,
    /// `panel`, `database`, or `proxy`.
    pub role: String,
    /// The container exists on this host.
    pub present: bool,
    pub oom_score_adj: i64,
    pub cpu_shares: i64,
    pub memory_reservation_bytes: i64,
    /// Kept away from the OOM killer and ahead of apps for CPU.
    pub protected: bool,
}

#[derive(Default)]
struct Inner {
    settings: GuardSettings,
    level: GuardLevel,
    reasons: Vec<GuardReason>,
    since: Option<DateTime<Utc>>,
    sampled_at: Option<DateTime<Utc>>,
    pressure: Option<HostPressure>,
    error: Option<String>,
    stopped_builds: u64,
    last_stopped_at: Option<DateTime<Utc>>,
    protection: Vec<ProtectionView>,
}

/// Shared load guard state.
#[derive(Default)]
pub struct LoadGuard {
    inner: RwLock<Inner>,
    /// Why the guard stopped a build, by build queue id, until the build records it.
    stop_reasons: Mutex<HashMap<Uuid, String>>,
}

impl LoadGuard {
    pub fn settings(&self) -> GuardSettings {
        self.inner
            .read()
            .map(|inner| inner.settings)
            .unwrap_or_default()
    }

    pub fn set_settings(&self, settings: GuardSettings) {
        if let Ok(mut inner) = self.inner.write() {
            inner.settings = settings;
        }
    }

    pub fn level(&self) -> GuardLevel {
        self.inner
            .read()
            .map(|inner| inner.level)
            .unwrap_or_default()
    }

    /// Whether a build that has waited `waited` may start now, or why it waits:
    /// `paused`, `critical`, or `busy`.
    pub fn hold(&self, waited: Duration) -> Option<&'static str> {
        let inner = self.inner.read().ok()?;
        if inner.settings.builds_paused {
            return Some("paused");
        }
        if !inner.settings.enabled {
            return None;
        }
        match inner.level {
            GuardLevel::Ok => None,
            GuardLevel::Critical => Some("critical"),
            GuardLevel::Busy => {
                let memory = inner
                    .reasons
                    .iter()
                    .any(|reason| reason.code.starts_with("memory"));
                if !memory && waited >= CPU_HOLD_LIMIT {
                    None
                } else {
                    Some("busy")
                }
            }
        }
    }

    /// Wait until a new build may start, for at most `max`. True when it may.
    pub async fn wait_for_room(&self, max: Duration) -> bool {
        let started = std::time::Instant::now();
        loop {
            let waited = started.elapsed();
            if self.hold(waited).is_none() {
                return true;
            }
            if waited >= max {
                return false;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    pub fn view(&self) -> GuardView {
        let holding = self.hold(Duration::ZERO).is_some();
        let inner = self.inner.read();
        let Ok(inner) = inner else {
            return GuardView {
                level: GuardLevel::Ok,
                reasons: Vec::new(),
                since: None,
                sampled_at: None,
                pressure: None,
                error: Some("guard state is unavailable".into()),
                settings: GuardSettings::default(),
                critical_memory_percent: GuardSettings::default()
                    .limits()
                    .critical_memory_percent(),
                holding,
                stopped_builds: 0,
                last_stopped_at: None,
            };
        };
        GuardView {
            level: inner.level,
            reasons: inner.reasons.clone(),
            since: inner.since,
            sampled_at: inner.sampled_at,
            pressure: inner.pressure,
            error: inner.error.clone(),
            settings: inner.settings,
            critical_memory_percent: inner.settings.limits().critical_memory_percent(),
            holding,
            stopped_builds: inner.stopped_builds,
            last_stopped_at: inner.last_stopped_at,
        }
    }

    pub fn protection(&self) -> Vec<ProtectionView> {
        self.inner
            .read()
            .map(|inner| inner.protection.clone())
            .unwrap_or_default()
    }

    /// The reason the guard stopped build `queue_id`, once.
    pub fn take_stop_reason(&self, queue_id: Uuid) -> Option<String> {
        self.stop_reasons
            .lock()
            .ok()
            .and_then(|mut reasons| reasons.remove(&queue_id))
    }

    fn note_stop(&self, queue_id: Uuid, reason: &str) {
        if let Ok(mut reasons) = self.stop_reasons.lock() {
            reasons.insert(queue_id, reason.to_owned());
        }
    }

    fn count_stopped(&self, count: usize, at: DateTime<Utc>) {
        if let Ok(mut inner) = self.inner.write() {
            inner.stopped_builds += count as u64;
            inner.last_stopped_at = Some(at);
        }
    }

    fn record(&self, pressure: HostPressure, assessment: &Assessment, at: DateTime<Utc>) {
        if let Ok(mut inner) = self.inner.write() {
            if assessment.changed || inner.since.is_none() {
                inner.since = Some(at);
            }
            inner.level = assessment.level;
            inner.reasons = assessment.reasons.clone();
            inner.pressure = Some(pressure);
            inner.sampled_at = Some(at);
            inner.error = None;
        }
    }

    fn fail(&self, error: String) {
        if let Ok(mut inner) = self.inner.write() {
            inner.error = Some(error);
        }
    }

    fn set_protection(&self, protection: Vec<ProtectionView>) {
        if let Ok(mut inner) = self.inner.write() {
            inner.protection = protection;
        }
    }
}

pub async fn load_settings(pool: &PgPool) -> Result<GuardSettings, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT guard_enabled, builds_paused, guard_memory_reserve_percent, guard_cpu_percent
        FROM panel_settings
        WHERE id = TRUE
        "#
    )
    .fetch_one(pool)
    .await?;
    Ok(GuardSettings {
        enabled: row.guard_enabled,
        builds_paused: row.builds_paused,
        memory_reserve_percent: u8::try_from(row.guard_memory_reserve_percent)
            .unwrap_or(DEFAULT_MEMORY_RESERVE_PERCENT),
        cpu_percent: u8::try_from(row.guard_cpu_percent).unwrap_or(DEFAULT_GUARD_CPU_PERCENT),
    })
}

pub async fn save_settings(pool: &PgPool, settings: GuardSettings) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE panel_settings
        SET guard_enabled = $1, builds_paused = $2,
            guard_memory_reserve_percent = $3, guard_cpu_percent = $4
        WHERE id = TRUE
        "#,
        settings.enabled,
        settings.builds_paused,
        i16::from(settings.memory_reserve_percent),
        i16::from(settings.cpu_percent),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Sample the host until the panel stops.
pub async fn run(state: AppState) {
    match load_settings(&state.pool).await {
        Ok(settings) => state.guard.set_settings(settings),
        Err(error) => tracing::warn!(%error, "load guard settings could not be read"),
    }
    protect_control_plane(&state).await;
    let mut tracker = GuardTracker::default();
    let mut cpu: Option<CpuTimes> = None;
    let mut ticker = tokio::time::interval(Duration::from_secs(GUARD_INTERVAL_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;
    loop {
        ticker.tick().await;
        ticks += 1;
        if ticks.is_multiple_of(SETTINGS_EVERY) {
            if let Ok(settings) = load_settings(&state.pool).await {
                state.guard.set_settings(settings);
            }
        }
        if ticks.is_multiple_of(PROTECT_EVERY) {
            protect_control_plane(&state).await;
        }
        let proc_root = state.config.host_proc.clone();
        let mut previous = cpu;
        let reading = tokio::task::spawn_blocking(move || {
            let result = read_host_pressure(&proc_root, &mut previous);
            (result, previous)
        })
        .await;
        let pressure = match reading {
            Ok((Ok(pressure), next)) => {
                cpu = next;
                pressure
            }
            Ok((Err(error), _)) => {
                state.guard.fail(error.to_string());
                continue;
            }
            Err(error) => {
                state.guard.fail(error.to_string());
                continue;
            }
        };
        let settings = state.guard.settings();
        let assessment = tracker.observe(&pressure, settings.limits());
        let now = Utc::now();
        state.guard.record(pressure, &assessment, now);
        if assessment.changed {
            announce(&state, &assessment, settings).await;
        }
        if settings.enabled && assessment.level == GuardLevel::Critical {
            let why = format!(
                "only {:.0}% of the server's memory was free",
                pressure.memory_available_percent()
            );
            let stopped = stop_builds(&state, &format!("stopped by the load guard: {why}")).await;
            if stopped > 0 {
                state.guard.count_stopped(stopped, now);
                tracing::warn!(stopped, %why, "load guard stopped builds");
                events::record(
                    &state.pool,
                    Event::critical("guard.builds_stopped", "Builds stopped to free memory")
                        .message(format!(
                            "{stopped} build(s) were stopped so the panel keeps answering: {why}. Start them again when the server has room."
                        )),
                )
                .await;
            }
        }
    }
}

/// A percentage without a trailing `.0`: `7.5`, `15`.
fn pct(value: f64) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
}

/// Plain-language reasons for events and logs.
pub fn describe(reasons: &[GuardReason]) -> String {
    reasons
        .iter()
        .map(|reason| match reason.code.as_str() {
            "memory_low" => format!(
                "free memory {:.0}% is below the {}% reserve",
                reason.value,
                pct(reason.limit)
            ),
            "memory_critical" => format!(
                "free memory {:.0}% is below {}%",
                reason.value,
                pct(reason.limit)
            ),
            "memory_stall" => format!(
                "processes waited for memory {:.0}% of the time",
                reason.value
            ),
            "cpu_high" => format!("CPU at {:.0}% (limit {}%)", reason.value, pct(reason.limit)),
            _ => "waiting for the server to settle".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

async fn announce(state: &AppState, assessment: &Assessment, settings: GuardSettings) {
    let detail = describe(&assessment.reasons);
    let event = match assessment.level {
        GuardLevel::Ok => {
            tracing::info!("load guard: server load is back to normal");
            Event::info("guard.ok", "Server load is back to normal")
                .message("New builds start again.")
        }
        GuardLevel::Busy => {
            tracing::warn!(reasons = %detail, "load guard: server is busy");
            let title = if settings.enabled {
                "Server is busy: new builds wait"
            } else {
                "Server is busy"
            };
            Event::warning("guard.busy", title).message(detail)
        }
        GuardLevel::Critical => {
            tracing::warn!(reasons = %detail, "load guard: server is running out of memory");
            let title = if settings.enabled {
                "Server is running out of memory: builds are stopped"
            } else {
                "Server is running out of memory"
            };
            Event::critical("guard.critical", title).message(detail)
        }
    };
    events::record(
        &state.pool,
        event.payload(json!({
            "level": assessment.level.as_str(),
            "reasons": assessment.reasons,
        })),
    )
    .await;
}

/// Stop every build that runs on the panel host, with `error` as the error of
/// each deployment. Returns how many were stopped.
pub async fn stop_builds(state: &AppState, error: &str) -> usize {
    let queued = sqlx::query_scalar!(
        r#"
        UPDATE build_queue
        SET cancel_requested = TRUE
        WHERE status = 'running' AND NOT cancel_requested
        RETURNING id
        "#
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for id in &queued {
        state.guard.note_stop(*id, error);
    }
    queued.len() + state.stack_runs.stop_local_builds(error)
}

/// Raise the CPU weight of the panel, its database and Traefik, and remember how
/// each one is protected.
pub async fn protect_control_plane(state: &AppState) {
    let targets = [
        (
            state.config.self_container.clone(),
            "panel",
            CONTROL_PLANE_CPU_SHARES,
            128 * MIB,
        ),
        (
            POSTGRES_CONTAINER.to_owned(),
            "database",
            CONTROL_PLANE_CPU_SHARES,
            256 * MIB,
        ),
        (
            TRAEFIK_CONTAINER.to_owned(),
            "proxy",
            PROXY_CPU_SHARES,
            64 * MIB,
        ),
    ];
    let mut views = Vec::with_capacity(targets.len());
    for (name, role, shares, reservation) in targets {
        if let Err(error) = state
            .local_driver
            .prioritize(&name, shares, reservation)
            .await
        {
            tracing::debug!(container = %name, %error, "could not raise container priority");
        }
        let priority = state.local_driver.priority(&name).await.ok().flatten();
        views.push(match priority {
            Some(priority) => ProtectionView {
                protected: priority.oom_score_adj < 0 && priority.cpu_shares >= shares,
                name,
                role: role.to_owned(),
                present: true,
                oom_score_adj: priority.oom_score_adj,
                cpu_shares: priority.cpu_shares,
                memory_reservation_bytes: priority.memory_reservation,
            },
            None => ProtectionView {
                name,
                role: role.to_owned(),
                present: false,
                oom_score_adj: 0,
                cpu_shares: 0,
                memory_reservation_bytes: 0,
                protected: false,
            },
        });
    }
    if let Some(panel) = views.iter().find(|view| view.role == "panel") {
        if panel.present && panel.oom_score_adj > CONTROL_PLANE_OOM_SCORE_ADJ / 2 {
            tracing::info!(
                "the panel container has no low OOM score yet; the next panel update or a re-run of install.sh sets it"
            );
        }
    }
    state.guard.set_protection(views);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard_with(settings: GuardSettings, level: GuardLevel, codes: &[&str]) -> LoadGuard {
        let guard = LoadGuard::default();
        guard.set_settings(settings);
        if let Ok(mut inner) = guard.inner.write() {
            inner.level = level;
            inner.reasons = codes
                .iter()
                .map(|code| GuardReason {
                    code: (*code).to_owned(),
                    value: 0.0,
                    limit: 0.0,
                })
                .collect();
        }
        guard
    }

    #[test]
    fn paused_builds_always_wait() {
        let settings = GuardSettings {
            builds_paused: true,
            enabled: false,
            ..GuardSettings::default()
        };
        let guard = guard_with(settings, GuardLevel::Ok, &[]);
        assert_eq!(guard.hold(Duration::from_secs(86_400)), Some("paused"));
    }

    #[test]
    fn a_disabled_guard_never_holds() {
        let settings = GuardSettings {
            enabled: false,
            ..GuardSettings::default()
        };
        let guard = guard_with(settings, GuardLevel::Critical, &["memory_critical"]);
        assert_eq!(guard.hold(Duration::ZERO), None);
    }

    #[test]
    fn cpu_holds_end_after_the_limit_but_memory_holds_do_not() {
        let cpu = guard_with(GuardSettings::default(), GuardLevel::Busy, &["cpu_high"]);
        assert_eq!(cpu.hold(Duration::ZERO), Some("busy"));
        assert_eq!(cpu.hold(CPU_HOLD_LIMIT), None);

        let memory = guard_with(GuardSettings::default(), GuardLevel::Busy, &["memory_low"]);
        assert_eq!(memory.hold(CPU_HOLD_LIMIT * 10), Some("busy"));

        let critical = guard_with(
            GuardSettings::default(),
            GuardLevel::Critical,
            &["memory_critical"],
        );
        assert_eq!(critical.hold(CPU_HOLD_LIMIT * 10), Some("critical"));
        assert!(guard_with(GuardSettings::default(), GuardLevel::Ok, &[])
            .hold(Duration::ZERO)
            .is_none());
    }

    #[test]
    fn stop_reasons_are_taken_once() {
        let guard = LoadGuard::default();
        let id = Uuid::new_v4();
        guard.note_stop(id, "no memory");
        assert_eq!(guard.take_stop_reason(id).as_deref(), Some("no memory"));
        assert_eq!(guard.take_stop_reason(id), None);
    }

    #[test]
    fn reasons_read_as_sentences() {
        let reasons = vec![
            GuardReason {
                code: "memory_low".into(),
                value: 9.6,
                limit: 15.0,
            },
            GuardReason {
                code: "cpu_high".into(),
                value: 97.0,
                limit: 90.0,
            },
        ];
        assert_eq!(
            describe(&reasons),
            "free memory 10% is below the 15% reserve; CPU at 97% (limit 90%)"
        );
        let critical = [GuardReason {
            code: "memory_critical".into(),
            value: 5.0,
            limit: 7.5,
        }];
        assert_eq!(describe(&critical), "free memory 5% is below 7.5%");
    }
}
