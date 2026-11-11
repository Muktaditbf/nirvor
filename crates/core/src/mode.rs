//! Operating mode: Normal, Degraded or Isolation, detected from international upstream health
//! with hysteresis, or forced by the operator.

use nirvor_zones::ZoneConfig;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Instant;

/// Current operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Mode {
    /// All international upstreams healthy.
    Normal = 0,
    /// Some international upstreams failing.
    Degraded,
    /// International links considered down: local upstreams, recursion from the local root,
    /// extended serve-stale and connectivity-check overrides.
    Isolation,
}

/// Who decides the mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Control {
    /// Detected automatically.
    Auto,
    /// Forced to Normal.
    Normal,
    /// Forced to Isolation.
    Isolation,
}

/// Isolation settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IsolationConfig {
    /// Enter isolation after every international upstream has been down this long, seconds.
    pub isolation_after_secs: u64,
    /// Leave isolation after an international upstream has been back this long, seconds.
    pub recover_after_secs: u64,
    /// Seconds between mode checks.
    pub check_interval_secs: u64,
    /// Zones answered only during isolation, e.g. OS connectivity-check names pointing at a local responder.
    pub overrides: Vec<ZoneConfig>,
    /// Address for the built-in connectivity-check HTTP responder (`None` = off).
    pub connectivity_responder: Option<SocketAddr>,
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            isolation_after_secs: 15,
            recover_after_secs: 30,
            check_interval_secs: 2,
            overrides: Vec::new(),
            connectivity_responder: None,
        }
    }
}

/// Mode state for the API.
#[derive(Debug, Clone, Serialize)]
pub struct ModeState {
    /// Mode in effect.
    pub mode: Mode,
    /// Automatic or forced.
    pub control: Control,
    /// Unix seconds when the mode last changed.
    pub since: u64,
    /// Why the mode is what it is.
    pub reason: String,
    /// Healthy international upstreams.
    pub international_healthy: usize,
    /// Enabled international upstreams.
    pub international_total: usize,
}

/// Timers for hysteresis between checks.
#[derive(Debug, Default)]
pub struct Monitor {
    all_down_since: Option<Instant>,
    back_up_since: Option<Instant>,
}

/// Next automatic mode from international upstream health. Returns the mode and a reason.
pub fn next_mode(
    current: Mode,
    healthy: usize,
    total: usize,
    mon: &mut Monitor,
    now: Instant,
    cfg: &IsolationConfig,
) -> (Mode, String) {
    if total == 0 {
        *mon = Monitor::default();
        return (Mode::Normal, "no international upstreams configured".into());
    }
    if healthy == 0 {
        mon.back_up_since = None;
        let since = *mon.all_down_since.get_or_insert(now);
        let down_for = now.duration_since(since).as_secs();
        if current == Mode::Isolation || down_for >= cfg.isolation_after_secs {
            return (
                Mode::Isolation,
                format!("all {total} international upstreams down for {down_for} s"),
            );
        }
        return (
            Mode::Degraded,
            format!("all {total} international upstreams failing for {down_for} s"),
        );
    }
    mon.all_down_since = None;
    if current == Mode::Isolation {
        let since = *mon.back_up_since.get_or_insert(now);
        let up_for = now.duration_since(since).as_secs();
        if up_for < cfg.recover_after_secs {
            return (
                Mode::Isolation,
                format!(
                    "{healthy}/{total} international upstreams back for {up_for} s, waiting {} s",
                    cfg.recover_after_secs
                ),
            );
        }
    }
    mon.back_up_since = None;
    if healthy == total {
        (
            Mode::Normal,
            format!("all {total} international upstreams healthy"),
        )
    } else {
        (
            Mode::Degraded,
            format!("{healthy}/{total} international upstreams healthy"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn enters_isolation_after_threshold_and_leaves_with_hysteresis() {
        let cfg = IsolationConfig::default();
        let mut mon = Monitor::default();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert_eq!(
            next_mode(Mode::Normal, 3, 3, &mut mon, at(0), &cfg).0,
            Mode::Normal
        );
        assert_eq!(
            next_mode(Mode::Normal, 1, 3, &mut mon, at(1), &cfg).0,
            Mode::Degraded
        );
        assert_eq!(
            next_mode(Mode::Degraded, 0, 3, &mut mon, at(2), &cfg).0,
            Mode::Degraded
        );
        assert_eq!(
            next_mode(Mode::Degraded, 0, 3, &mut mon, at(10), &cfg).0,
            Mode::Degraded
        );
        let (m, why) = next_mode(Mode::Degraded, 0, 3, &mut mon, at(17), &cfg);
        assert_eq!(m, Mode::Isolation);
        assert!(why.contains("15 s"), "{why}");
        // One upstream back: stay isolated until it has been up for 30 s.
        assert_eq!(
            next_mode(Mode::Isolation, 1, 3, &mut mon, at(20), &cfg).0,
            Mode::Isolation
        );
        assert_eq!(
            next_mode(Mode::Isolation, 1, 3, &mut mon, at(40), &cfg).0,
            Mode::Isolation
        );
        assert_eq!(
            next_mode(Mode::Isolation, 3, 3, &mut mon, at(50), &cfg).0,
            Mode::Normal
        );
    }

    #[test]
    fn flapping_restarts_the_timers() {
        let cfg = IsolationConfig::default();
        let mut mon = Monitor::default();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        next_mode(Mode::Normal, 0, 2, &mut mon, at(0), &cfg);
        next_mode(Mode::Degraded, 1, 2, &mut mon, at(10), &cfg); // brief recovery resets the down timer
        assert_eq!(
            next_mode(Mode::Degraded, 0, 2, &mut mon, at(20), &cfg).0,
            Mode::Degraded
        );
        assert_eq!(
            next_mode(Mode::Degraded, 0, 2, &mut mon, at(36), &cfg).0,
            Mode::Isolation
        );
        next_mode(Mode::Isolation, 2, 2, &mut mon, at(40), &cfg);
        next_mode(Mode::Isolation, 0, 2, &mut mon, at(50), &cfg); // drops again: recovery timer resets
        assert_eq!(
            next_mode(Mode::Isolation, 2, 2, &mut mon, at(60), &cfg).0,
            Mode::Isolation
        );
        assert_eq!(
            next_mode(Mode::Isolation, 2, 2, &mut mon, at(90), &cfg).0,
            Mode::Normal
        );
    }

    #[test]
    fn no_international_upstreams_is_normal() {
        let mut mon = Monitor::default();
        assert_eq!(
            next_mode(
                Mode::Isolation,
                0,
                0,
                &mut mon,
                Instant::now(),
                &IsolationConfig::default()
            )
            .0,
            Mode::Normal
        );
    }
}
