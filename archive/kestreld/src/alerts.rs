//! Operator alerts — episode-deduped, rate-limited Telegram pushes over `notify::Notify`.
//!
//! Every alert is best-effort: `Notify::send` already swallows transport failures, and
//! nothing here can block or panic a trading task. The dedup primitives are plain structs
//! with pure transition logic (unit-tested without a network or a clock).
//!
//! Two dedup shapes, deliberately different:
//!   * `Episode` / `ConsecutiveFail` — edge-triggered: fire on ENTRY into a failure, stay
//!     silent while it persists, re-arm only after a recovery. Owned task-locally by the
//!     watchdog, so no shared lock is involved.
//!   * `DayOnce` — fire once per UTC day (daily-cap exhaustion). Shared across the
//!     per-nominee entry tasks, hence a short `std::sync::Mutex` that is never held
//!     across an `.await`.

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::{debug, info};

use crate::notify::Notify;

/// Rate-limit bucket. One per alert kind so a stream death can never mute a wedge alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertKind {
    Boot,
    StreamDeath,
    Wedge,
    KillSwitch,
    DailyCap,
    AnalystDown,
    AnalystUp,
}

impl AlertKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertKind::Boot => "boot",
            AlertKind::StreamDeath => "stream_death",
            AlertKind::Wedge => "wedge",
            AlertKind::KillSwitch => "kill_switch",
            AlertKind::DailyCap => "daily_cap",
            AlertKind::AnalystDown => "analyst_down",
            AlertKind::AnalystUp => "analyst_up",
        }
    }
}

/// At most one alert per kind per minute (spec R2: "rate-limited to 1/min per alert kind").
pub const ALERT_MIN_INTERVAL_MS: i64 = 60_000;

/// Pure: may an alert of this kind go out now? `None` = never sent. A backwards clock
/// step suppresses rather than spams (`saturating_sub` → 0 elapsed).
pub fn rate_limit_allows(last_sent_ms: Option<i64>, now_ms: i64, min_interval_ms: i64) -> bool {
    match last_sent_ms {
        None => true,
        Some(last) => now_ms.saturating_sub(last) >= min_interval_ms,
    }
}

/// Pure: `true` the first time a given day key is seen, `false` for repeats.
pub fn day_once(last_day: &mut String, day: &str) -> bool {
    if last_day == day {
        return false;
    }
    *last_day = day.to_string();
    true
}

#[derive(Debug, Default)]
pub struct RateLimiter {
    last: Mutex<HashMap<AlertKind, i64>>,
    min_interval_ms: i64,
}

impl RateLimiter {
    pub fn new(min_interval_ms: i64) -> Self {
        Self { last: Mutex::new(HashMap::new()), min_interval_ms }
    }

    /// Take the slot if allowed. The guard is dropped before the caller awaits anything.
    pub fn allow(&self, kind: AlertKind, now_ms: i64) -> bool {
        let mut g = match self.last.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let ok = rate_limit_allows(g.get(&kind).copied(), now_ms, self.min_interval_ms);
        if ok {
            g.insert(kind, now_ms);
        }
        ok
    }
}

/// Edge-triggered failure episode: one alert per episode, re-armed by recovery.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Episode {
    firing: bool,
}

impl Episode {
    pub fn new() -> Self {
        Self { firing: false }
    }

    /// Feed the current health. Returns `true` only on the transition into failure.
    pub fn observe(&mut self, failing: bool) -> bool {
        if failing {
            let first = !self.firing;
            self.firing = true;
            first
        } else {
            self.firing = false;
            false
        }
    }

    pub fn firing(&self) -> bool {
        self.firing
    }
}

/// N-consecutive-failures detector with the same one-alert-per-episode contract.
#[derive(Debug, Clone, Copy)]
pub struct ConsecutiveFail {
    threshold: u32,
    count: u32,
    episode: Episode,
}

/// Analyst failures are counted by the analyst itself. This only turns that count into
/// operator-alert edges: down at most hourly and one recovery once the count returns to zero.
#[derive(Debug, Default)]
pub struct AnalystFailureAlert {
    down: bool,
    last_down_ms: Option<i64>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AnalystFailureTransition {
    pub down: bool,
    pub up: bool,
}

impl AnalystFailureAlert {
    pub fn observe(&mut self, streak: u64, now_ms: i64) -> AnalystFailureTransition {
        if streak == 0 {
            let up = self.down;
            self.down = false;
            return AnalystFailureTransition { down: false, up };
        }
        if streak < 8 || !rate_limit_allows(self.last_down_ms, now_ms, 60 * 60 * 1000) {
            return AnalystFailureTransition::default();
        }
        self.down = true;
        self.last_down_ms = Some(now_ms);
        AnalystFailureTransition { down: true, up: false }
    }
}

impl ConsecutiveFail {
    pub fn new(threshold: u32) -> Self {
        Self { threshold: threshold.max(1), count: 0, episode: Episode::new() }
    }

    /// Feed one probe result. Returns `true` exactly once per episode — on the probe that
    /// reaches `threshold` consecutive failures.
    pub fn observe(&mut self, ok: bool) -> bool {
        if ok {
            self.count = 0;
            self.episode.observe(false);
            return false;
        }
        self.count = self.count.saturating_add(1);
        self.episode.observe(self.count >= self.threshold)
    }

    pub fn count(&self) -> u32 {
        self.count
    }
}

/// Fire-once-per-UTC-day latch, shared across tasks.
#[derive(Debug, Default)]
pub struct DayOnce {
    day: Mutex<String>,
}

impl DayOnce {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn first_today(&self, day: &str) -> bool {
        let mut g = match self.day.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        day_once(&mut g, day)
    }
}

/// Rate-limited alert sender. Cheap to clone-share behind an `Arc`.
#[derive(Debug)]
pub struct Alerter {
    notify: Notify,
    limiter: RateLimiter,
}

impl Alerter {
    pub fn new(notify: Notify) -> Self {
        Self { notify, limiter: RateLimiter::new(ALERT_MIN_INTERVAL_MS) }
    }

    /// Best-effort: rate-limited per kind, always logged, never fails a caller.
    /// Config-gated by the existing notify settings (empty token env / chat id → log only).
    ///
    /// `text` must be `notify::tpl` output — every message goes out with `parse_mode=HTML`,
    /// so anything interpolated from outside the daemon has to be escaped by the template.
    pub async fn send(&self, kind: AlertKind, text: &str) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if !self.limiter.allow(kind, now_ms) {
            debug!(kind = kind.as_str(), text, "alert suppressed by rate limit");
            return;
        }
        info!(kind = kind.as_str(), alert = text, "operator alert");
        self.notify.send(text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_boundary() {
        assert!(rate_limit_allows(None, 1_000, 60_000), "never sent -> allowed");
        assert!(!rate_limit_allows(Some(1_000), 1_000, 60_000), "same instant -> blocked");
        assert!(!rate_limit_allows(Some(1_000), 60_999, 60_000), "1ms early -> blocked");
        assert!(rate_limit_allows(Some(1_000), 61_000, 60_000), "exactly the interval -> allowed");
        assert!(!rate_limit_allows(Some(10_000), 5_000, 60_000), "clock went backwards -> blocked, never spam");
    }

    #[test]
    fn rate_limiter_is_per_kind_and_takes_the_slot() {
        let rl = RateLimiter::new(60_000);
        let t0 = 1_700_000_000_000i64;
        assert!(rl.allow(AlertKind::Wedge, t0), "first wedge alert");
        assert!(!rl.allow(AlertKind::Wedge, t0 + 59_999), "second within the minute suppressed");
        assert!(rl.allow(AlertKind::StreamDeath, t0 + 1), "different kind has its own bucket");
        assert!(rl.allow(AlertKind::Wedge, t0 + 60_000), "allowed again at the boundary");
        assert!(!rl.allow(AlertKind::Wedge, t0 + 60_001), "and the slot moved with it");
    }

    #[test]
    fn analyst_down_alert_is_hourly_and_recovery_rearms_it() {
        let mut episode = AnalystFailureAlert::default();
        let t0 = 1_700_000_000_000;
        assert!(episode.observe(8, t0).down);
        assert!(!episode.observe(9, t0 + 1_000).down);
        assert!(episode.observe(10, t0 + 60 * 60 * 1000).down);
        assert!(episode.observe(0, t0 + 60 * 60 * 1001).up);
        assert!(!episode.observe(0, t0 + 60 * 60 * 1002).up);
        assert!(!episode.observe(8, t0 + 60 * 60 * 1003).down);
        assert!(episode.observe(8, t0 + 2 * 60 * 60 * 1000).down);
    }

    #[test]
    fn episode_alerts_once_and_rearms_only_after_recovery() {
        let mut ep = Episode::new();
        assert!(!ep.observe(false), "healthy start is silent");
        assert!(ep.observe(true), "entry into failure alerts");
        assert!(!ep.observe(true), "still failing stays silent");
        assert!(!ep.observe(true), "and stays silent indefinitely");
        assert!(!ep.observe(false), "recovery itself does not alert");
        assert!(!ep.firing());
        assert!(ep.observe(true), "re-fail after recovery alerts again");
    }

    #[test]
    fn consecutive_fail_needs_threshold_then_dedupes() {
        let mut cf = ConsecutiveFail::new(2);
        assert!(!cf.observe(false), "one failure is not enough");
        assert_eq!(cf.count(), 1);
        assert!(cf.observe(false), "second consecutive failure alerts");
        assert!(!cf.observe(false), "third stays silent (same episode)");
        assert!(!cf.observe(true), "recovery is silent");
        assert_eq!(cf.count(), 0, "recovery resets the streak");
        assert!(!cf.observe(false), "streak restarts from one");
        assert!(cf.observe(false), "new episode alerts again");
    }

    #[test]
    fn consecutive_fail_alternating_never_alerts_at_threshold_2() {
        let mut cf = ConsecutiveFail::new(2);
        for _ in 0..10 {
            assert!(!cf.observe(false));
            assert!(!cf.observe(true));
        }
    }

    #[test]
    fn day_once_fires_on_each_new_day_only() {
        let mut last = String::new();
        assert!(day_once(&mut last, "2026-08-09"), "first refusal of the day");
        assert!(!day_once(&mut last, "2026-08-09"), "repeat same day suppressed");
        assert!(day_once(&mut last, "2026-08-10"), "day roll re-arms");
        assert!(!day_once(&mut last, "2026-08-10"));

        let latch = DayOnce::new();
        assert!(latch.first_today("2026-08-09"));
        assert!(!latch.first_today("2026-08-09"));
        assert!(latch.first_today("2026-08-10"));
    }

    #[tokio::test]
    async fn alerter_send_is_inert_without_notify_config() {
        // no chat id + unset token env -> log-only, must not panic or hang
        let a = Alerter::new(Notify::new("UNSET_TOKEN_ENV_XYZ".into(), String::new()));
        a.send(AlertKind::Boot, &crate::notify::tpl::boot(1000.0, 42, 7)).await;
        a.send(AlertKind::Boot, &crate::notify::tpl::boot(1000.0, 42, 8)).await;
    }
}
