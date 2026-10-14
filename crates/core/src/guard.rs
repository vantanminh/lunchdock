//! Load guard: decides whether the panel host has room for more work.
//!
//! The panel samples its host every few seconds. New builds wait while the host
//! is busy, and builds are stopped when memory is about to run out, so the panel,
//! its database and Traefik keep enough memory and CPU to answer. Decisions use
//! hysteresis so a single spike does not flip the state back and forth.

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{cpu_percent, parse_meminfo, parse_proc_stat, CpuTimes, Error};

/// Seconds between load guard samples.
pub const GUARD_INTERVAL_SECS: u64 = 3;
/// Samples averaged for the CPU decision (about 15 seconds).
const WINDOW: usize = 5;
/// Consecutive critical samples before the guard reports critical.
const CRITICAL_STREAK: u32 = 2;
/// Memory must be this many points above the threshold before the guard relaxes.
const MEMORY_MARGIN: f64 = 5.0;
/// CPU must be this many points below the limit before the guard relaxes.
const CPU_MARGIN: f64 = 10.0;
/// Share of time every task waited for memory (PSI `full avg10`) that marks the host busy.
const STALL_BUSY: f64 = 5.0;
/// Share of time every task waited for memory that marks the host critical.
const STALL_CRITICAL: f64 = 25.0;
/// The critical memory floor never drops below this many percent.
const MIN_CRITICAL_PERCENT: f64 = 3.0;

/// `oom_score_adj` of app, database and stack containers: when the host runs out
/// of memory the kernel kills them before the panel, its database and Traefik.
pub const WORKLOAD_OOM_SCORE_ADJ: i64 = 300;
/// `oom_score_adj` of the panel, its database and Traefik.
pub const CONTROL_PLANE_OOM_SCORE_ADJ: i64 = -900;
/// CPU shares of the panel and its database; Docker's default is 1024, so they
/// win the CPU whenever they need it while apps share the rest.
pub const CONTROL_PLANE_CPU_SHARES: i64 = 8192;
/// CPU shares of Traefik, which also carries app traffic.
pub const PROXY_CPU_SHARES: i64 = 2048;

pub const DEFAULT_MEMORY_RESERVE_PERCENT: u8 = 15;
pub const DEFAULT_GUARD_CPU_PERCENT: u8 = 90;

/// Memory, CPU and pressure of one host at one instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HostPressure {
    /// Busy CPU time since the previous sample, 0–100 for the whole machine.
    pub cpu_percent: f64,
    pub cpu_count: u32,
    pub memory_total_bytes: u64,
    /// Memory the kernel can hand out without swapping (`MemAvailable`).
    pub memory_available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    /// Share of the last 10 s some task waited for CPU (PSI), when the kernel reports it.
    pub cpu_pressure: Option<f64>,
    /// Share of the last 10 s some task waited for memory (PSI).
    pub memory_pressure: Option<f64>,
    /// Share of the last 10 s every task waited for memory (PSI `full`): the host was thrashing.
    pub memory_stall: Option<f64>,
    /// Share of the last 10 s some task waited for disk or network I/O (PSI).
    pub io_pressure: Option<f64>,
}

impl HostPressure {
    /// Available memory as a share of the total, or 100 when the total is unknown.
    pub fn memory_available_percent(&self) -> f64 {
        if self.memory_total_bytes == 0 {
            100.0
        } else {
            (self.memory_available_bytes as f64 / self.memory_total_bytes as f64 * 100.0)
                .clamp(0.0, 100.0)
        }
    }

    pub fn memory_used_percent(&self) -> f64 {
        100.0 - self.memory_available_percent()
    }

    pub fn swap_used_percent(&self) -> f64 {
        crate::percent(
            self.swap_total_bytes.saturating_sub(self.swap_free_bytes),
            self.swap_total_bytes,
        )
    }
}

/// `(some avg10, full avg10)` from a `/proc/pressure/*` file.
pub fn parse_pressure(text: &str) -> Option<(f64, Option<f64>)> {
    let avg10 = |prefix: &str| {
        text.lines()
            .find(|line| line.starts_with(prefix))?
            .split_whitespace()
            .find_map(|field| field.strip_prefix("avg10="))?
            .parse::<f64>()
            .ok()
            .map(|value| value.clamp(0.0, 100.0))
    };
    Some((avg10("some ")?, avg10("full ")))
}

/// The 1, 5 and 15 minute load averages from `/proc/loadavg`.
pub fn parse_load_averages(text: &str) -> Option<(f64, f64, f64)> {
    let mut fields = text.split_whitespace().map(|value| value.parse::<f64>());
    match (fields.next(), fields.next(), fields.next()) {
        (Some(Ok(one)), Some(Ok(five)), Some(Ok(fifteen))) => Some((one, five, fifteen)),
        _ => None,
    }
}

/// `(total, free)` swap bytes from `/proc/meminfo`.
pub fn parse_swap(text: &str) -> (u64, u64) {
    let field = |name: &str| {
        text.lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u64>().ok())
            .map_or(0, |kib| kib.saturating_mul(1024))
    };
    let total = field("SwapTotal:");
    (total, field("SwapFree:").min(total))
}

/// Read memory, CPU and pressure from a procfs root. `previous` carries CPU
/// counters between calls; the first call reports 0% CPU. Pressure fields are
/// `None` on kernels without PSI.
pub fn read_host_pressure(
    proc_root: &Path,
    previous: &mut Option<CpuTimes>,
) -> Result<HostPressure, Error> {
    let read = |name: &str| std::fs::read_to_string(proc_root.join(name));
    let stat = read("stat").map_err(|error| Error::Node(format!("read stat: {error}")))?;
    let current = parse_proc_stat(&stat).ok_or_else(|| Error::Node("parse /proc/stat".into()))?;
    let cpu = previous.map_or(0.0, |prev| cpu_percent(prev, current));
    *previous = Some(current);
    let cpu_count = stat
        .lines()
        .filter(|line| {
            line.starts_with("cpu") && line.as_bytes().get(3).is_some_and(u8::is_ascii_digit)
        })
        .count() as u32;
    let meminfo = read("meminfo").map_err(|error| Error::Node(format!("read meminfo: {error}")))?;
    let (memory_total, available) =
        parse_meminfo(&meminfo).ok_or_else(|| Error::Node("parse meminfo".into()))?;
    let (swap_total, swap_free) = parse_swap(&meminfo);
    let (load1, load5, load15) = read("loadavg")
        .ok()
        .and_then(|text| parse_load_averages(&text))
        .unwrap_or_default();
    let pressure = |name: &str| read(name).ok().and_then(|text| parse_pressure(&text));
    let cpu_psi = pressure("pressure/cpu");
    let memory_psi = pressure("pressure/memory");
    let io_psi = pressure("pressure/io");
    Ok(HostPressure {
        cpu_percent: cpu,
        cpu_count: cpu_count.max(1),
        memory_total_bytes: memory_total,
        memory_available_bytes: available,
        swap_total_bytes: swap_total,
        swap_free_bytes: swap_free,
        load1,
        load5,
        load15,
        cpu_pressure: cpu_psi.map(|(some, _)| some),
        memory_pressure: memory_psi.map(|(some, _)| some),
        memory_stall: memory_psi.and_then(|(_, full)| full),
        io_pressure: io_psi.map(|(some, _)| some),
    })
}

/// Thresholds an admin can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuardLimits {
    /// Keep at least this share of memory free: new builds wait below it, and
    /// running builds are stopped below half of it.
    pub memory_reserve_percent: u8,
    /// New builds wait while CPU stays above this share.
    pub cpu_percent: u8,
}

impl Default for GuardLimits {
    fn default() -> Self {
        Self {
            memory_reserve_percent: DEFAULT_MEMORY_RESERVE_PERCENT,
            cpu_percent: DEFAULT_GUARD_CPU_PERCENT,
        }
    }
}

impl GuardLimits {
    pub fn validate(&self) -> Result<(), Error> {
        if !(5..=50).contains(&self.memory_reserve_percent) {
            return Err(Error::Validation(
                "memory reserve must be between 5 and 50 percent".into(),
            ));
        }
        if !(50..=100).contains(&self.cpu_percent) {
            return Err(Error::Validation(
                "CPU limit must be between 50 and 100 percent".into(),
            ));
        }
        Ok(())
    }

    /// Available memory below which running builds are stopped.
    pub fn critical_memory_percent(&self) -> f64 {
        (f64::from(self.memory_reserve_percent) / 2.0).max(MIN_CRITICAL_PERCENT)
    }
}

/// How loaded the host is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum GuardLevel {
    /// Room for more work.
    #[default]
    Ok,
    /// New builds wait.
    Busy,
    /// Memory is about to run out: running builds are stopped.
    Critical,
}

impl GuardLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Busy => "busy",
            Self::Critical => "critical",
        }
    }
}

/// Why the guard is not at `ok`. `code` is one of `memory_low`,
/// `memory_critical`, `memory_stall`, `cpu_high`, or `settling` (the host is
/// back under the limits but not yet by the safety margin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuardReason {
    pub code: String,
    /// The measured value, in percent.
    pub value: f64,
    /// The threshold it crossed, in percent.
    pub limit: f64,
}

impl GuardReason {
    fn new(code: &str, value: f64, limit: f64) -> Self {
        Self {
            code: code.to_owned(),
            value: (value * 10.0).round() / 10.0,
            limit,
        }
    }
}

/// The outcome of one sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    pub level: GuardLevel,
    pub reasons: Vec<GuardReason>,
    /// The level differs from the previous sample's.
    pub changed: bool,
}

/// Turns samples into a level, with smoothing and hysteresis.
#[derive(Debug, Default)]
pub struct GuardTracker {
    window: VecDeque<f64>,
    level: GuardLevel,
    critical_streak: u32,
}

impl GuardTracker {
    pub fn level(&self) -> GuardLevel {
        self.level
    }

    pub fn observe(&mut self, sample: &HostPressure, limits: GuardLimits) -> Assessment {
        if self.window.len() == WINDOW {
            self.window.pop_front();
        }
        self.window.push_back(sample.cpu_percent);
        let cpu = self.window.iter().sum::<f64>() / self.window.len() as f64;
        let cpu_known = self.window.len() >= 3;
        let available = sample.memory_available_percent();
        let stall = sample.memory_stall.unwrap_or(0.0);
        let reserve = f64::from(limits.memory_reserve_percent);
        let critical_floor = limits.critical_memory_percent();
        let cpu_limit = f64::from(limits.cpu_percent);

        let critical_now = available < critical_floor || stall >= STALL_CRITICAL;
        self.critical_streak = if critical_now {
            self.critical_streak.saturating_add(1)
        } else {
            0
        };
        let busy_now =
            available < reserve || (cpu_known && cpu >= cpu_limit) || stall >= STALL_BUSY;
        let calm = available >= reserve + MEMORY_MARGIN
            && (!cpu_known || cpu < cpu_limit - CPU_MARGIN)
            && stall < STALL_BUSY / 2.0;
        let still_critical =
            available < critical_floor + MEMORY_MARGIN / 2.0 || stall >= STALL_CRITICAL / 2.0;

        let next = match self.level {
            _ if self.critical_streak >= CRITICAL_STREAK => GuardLevel::Critical,
            GuardLevel::Critical if still_critical => GuardLevel::Critical,
            GuardLevel::Ok if busy_now => GuardLevel::Busy,
            GuardLevel::Ok => GuardLevel::Ok,
            _ if calm => GuardLevel::Ok,
            _ => GuardLevel::Busy,
        };

        let mut reasons = Vec::new();
        if next != GuardLevel::Ok {
            if available < critical_floor {
                reasons.push(GuardReason::new(
                    "memory_critical",
                    available,
                    critical_floor,
                ));
            } else if available < reserve {
                reasons.push(GuardReason::new("memory_low", available, reserve));
            }
            if stall >= STALL_BUSY {
                reasons.push(GuardReason::new("memory_stall", stall, STALL_BUSY));
            }
            if cpu_known && cpu >= cpu_limit {
                reasons.push(GuardReason::new("cpu_high", cpu, cpu_limit));
            }
            if reasons.is_empty() {
                reasons.push(GuardReason::new(
                    "settling",
                    available,
                    reserve + MEMORY_MARGIN,
                ));
            }
        }
        let changed = next != self.level;
        self.level = next;
        Assessment {
            level: next,
            reasons,
            changed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(cpu: f64, available_percent: f64) -> HostPressure {
        HostPressure {
            cpu_percent: cpu,
            cpu_count: 4,
            memory_total_bytes: 1000,
            memory_available_bytes: (available_percent * 10.0) as u64,
            ..HostPressure::default()
        }
    }

    #[test]
    fn parses_pressure_files() {
        let memory = "some avg10=12.50 avg60=3.00 avg300=1.00 total=100\nfull avg10=4.25 avg60=1.00 avg300=0.10 total=40\n";
        assert_eq!(parse_pressure(memory), Some((12.5, Some(4.25))));
        let cpu = "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(parse_pressure(cpu), Some((0.0, None)));
        assert_eq!(parse_pressure("garbage"), None);
    }

    #[test]
    fn parses_load_and_swap() {
        assert_eq!(
            parse_load_averages("1.50 0.75 0.25 2/300 4000\n"),
            Some((1.5, 0.75, 0.25))
        );
        assert_eq!(parse_load_averages("1.5"), None);
        let meminfo = "MemTotal: 1000 kB\nSwapTotal: 2048 kB\nSwapFree: 1024 kB\n";
        assert_eq!(parse_swap(meminfo), (2048 * 1024, 1024 * 1024));
        assert_eq!(parse_swap("MemTotal: 1 kB\n"), (0, 0));
    }

    #[test]
    fn reads_a_procfs_root() {
        let dir = std::env::temp_dir().join(format!("rudder-guard-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pressure")).unwrap();
        std::fs::write(
            dir.join("stat"),
            "cpu  100 0 100 800 0 0 0 0\ncpu0 100 0 100 800 0 0 0 0\ncpu1 0 0 0 0 0 0 0 0\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("meminfo"),
            "MemTotal: 1000 kB\nMemAvailable: 250 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
        )
        .unwrap();
        std::fs::write(dir.join("loadavg"), "0.50 0.40 0.30 1/100 1\n").unwrap();
        std::fs::write(
            dir.join("pressure/memory"),
            "some avg10=1.00 avg60=0 avg300=0 total=0\nfull avg10=0.50 avg60=0 avg300=0 total=0\n",
        )
        .unwrap();
        let mut previous = None;
        let first = read_host_pressure(&dir, &mut previous).unwrap();
        assert_eq!(first.cpu_percent, 0.0);
        assert_eq!(first.cpu_count, 2);
        assert_eq!(first.memory_available_percent(), 25.0);
        assert_eq!(first.memory_stall, Some(0.5));
        assert_eq!(first.cpu_pressure, None);
        std::fs::write(dir.join("stat"), "cpu  200 0 200 800 0 0 0 0\n").unwrap();
        let second = read_host_pressure(&dir, &mut previous).unwrap();
        assert_eq!(second.cpu_percent, 100.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn limits_are_validated() {
        assert!(GuardLimits::default().validate().is_ok());
        let low = GuardLimits {
            memory_reserve_percent: 2,
            cpu_percent: 90,
        };
        assert!(low.validate().is_err());
        let cpu = GuardLimits {
            memory_reserve_percent: 15,
            cpu_percent: 30,
        };
        assert!(cpu.validate().is_err());
        assert_eq!(GuardLimits::default().critical_memory_percent(), 7.5);
        let small = GuardLimits {
            memory_reserve_percent: 5,
            cpu_percent: 90,
        };
        assert_eq!(small.critical_memory_percent(), 3.0);
    }

    #[test]
    fn low_memory_makes_the_host_busy_until_it_recovers_with_margin() {
        let limits = GuardLimits::default();
        let mut tracker = GuardTracker::default();
        assert_eq!(
            tracker.observe(&sample(10.0, 50.0), limits).level,
            GuardLevel::Ok
        );
        let busy = tracker.observe(&sample(10.0, 12.0), limits);
        assert_eq!(busy.level, GuardLevel::Busy);
        assert!(busy.changed);
        assert_eq!(busy.reasons[0].code, "memory_low");
        // Back above the reserve but not by the margin: still busy.
        let settling = tracker.observe(&sample(10.0, 17.0), limits);
        assert_eq!(settling.level, GuardLevel::Busy);
        assert_eq!(settling.reasons[0].code, "settling");
        let ok = tracker.observe(&sample(10.0, 25.0), limits);
        assert_eq!(ok.level, GuardLevel::Ok);
        assert!(ok.changed);
        assert!(ok.reasons.is_empty());
    }

    #[test]
    fn critical_needs_two_samples_in_a_row() {
        let limits = GuardLimits::default();
        let mut tracker = GuardTracker::default();
        let first = tracker.observe(&sample(10.0, 4.0), limits);
        assert_eq!(first.level, GuardLevel::Busy);
        // A blip back up resets the streak.
        tracker.observe(&sample(10.0, 30.0), limits);
        tracker.observe(&sample(10.0, 4.0), limits);
        let critical = tracker.observe(&sample(10.0, 4.0), limits);
        assert_eq!(critical.level, GuardLevel::Critical);
        assert_eq!(critical.reasons[0].code, "memory_critical");
        // Just above the floor is not enough to leave critical.
        assert_eq!(
            tracker.observe(&sample(10.0, 9.0), limits).level,
            GuardLevel::Critical
        );
        assert_eq!(
            tracker.observe(&sample(10.0, 12.0), limits).level,
            GuardLevel::Busy
        );
        assert_eq!(
            tracker.observe(&sample(10.0, 40.0), limits).level,
            GuardLevel::Ok
        );
    }

    #[test]
    fn cpu_is_averaged_before_it_holds_builds() {
        let limits = GuardLimits::default();
        let mut tracker = GuardTracker::default();
        // One or two hot samples are not enough.
        assert_eq!(
            tracker.observe(&sample(100.0, 60.0), limits).level,
            GuardLevel::Ok
        );
        assert_eq!(
            tracker.observe(&sample(100.0, 60.0), limits).level,
            GuardLevel::Ok
        );
        let busy = tracker.observe(&sample(100.0, 60.0), limits);
        assert_eq!(busy.level, GuardLevel::Busy);
        assert_eq!(busy.reasons[0].code, "cpu_high");
        // CPU alone never reaches critical.
        for _ in 0..10 {
            assert_eq!(
                tracker.observe(&sample(100.0, 60.0), limits).level,
                GuardLevel::Busy
            );
        }
        for _ in 0..5 {
            tracker.observe(&sample(20.0, 60.0), limits);
        }
        assert_eq!(tracker.level(), GuardLevel::Ok);
    }

    #[test]
    fn memory_stall_counts_even_with_free_memory() {
        let limits = GuardLimits::default();
        let mut tracker = GuardTracker::default();
        let mut thrashing = sample(10.0, 40.0);
        thrashing.memory_stall = Some(30.0);
        assert_eq!(tracker.observe(&thrashing, limits).level, GuardLevel::Busy);
        let critical = tracker.observe(&thrashing, limits);
        assert_eq!(critical.level, GuardLevel::Critical);
        assert_eq!(critical.reasons[0].code, "memory_stall");
    }

    #[test]
    fn unknown_memory_never_trips_the_guard() {
        let mut tracker = GuardTracker::default();
        let empty = HostPressure::default();
        assert_eq!(empty.memory_available_percent(), 100.0);
        assert_eq!(
            tracker.observe(&empty, GuardLimits::default()).level,
            GuardLevel::Ok
        );
    }
}
