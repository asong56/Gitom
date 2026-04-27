//! Global login failure limiter for single-user mode.
//!
//! Design
//! ──────
//! • In-memory `Mutex<LimiterState>` for lock-free reads (`is_blocked`).
//! • Every write (`record_failure`, `reset`) also persists the counter to
//!   SQLite so the state survives process restarts.
//! • Reads are sync and never touch the DB; writes are async.
//! • Using a global counter (not per-IP) is intentional: there is only one
//!   account to protect, and a global limit is strictly harder to bypass than
//!   a per-IP limit (no need to rotate IPs, no proxy-collapse problem).

use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};
use sqlx::SqlitePool;
use tracing::warn;

pub struct LoginLimiter {
    window:       Duration,
    max_attempts: u32,
    state:        Mutex<LimiterState>,
}

struct LimiterState {
    failures:         u32,
    /// Stored as seconds-since-epoch so we can round-trip through SQLite.
    window_start_unix: i64,
}

impl LimiterState {
    fn window_start_instant(&self) -> Instant {
        // Convert stored epoch timestamp back to an Instant by computing
        // how far in the past it was relative to now.
        let now_epoch = epoch_secs_now();
        let age_secs  = (now_epoch - self.window_start_unix).max(0) as u64;
        Instant::now()
            .checked_sub(Duration::from_secs(age_secs))
            .unwrap_or(Instant::now())
    }
}

fn epoch_secs_now() -> i64 {
    UNIX_EPOCH.elapsed().map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl LoginLimiter {
    pub fn new(window_secs: u64, max_attempts: u32) -> Self {
        Self {
            window:       Duration::from_secs(window_secs),
            max_attempts,
            state:        Mutex::new(LimiterState {
                failures:          0,
                window_start_unix: epoch_secs_now(),
            }),
        }
    }

    /// Seed the in-memory counter from the DB row written by a previous run.
    /// Call once after migrations, before serving any requests.
    pub async fn load_from_db(&self, pool: &SqlitePool) {
        match crate::db::queries::load_login_state(pool).await {
            Ok(row) => {
                // Parse the stored ISO-8601 timestamp back to epoch seconds.
                let window_start_unix = time::OffsetDateTime::parse(
                    &row.window_start,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|dt| dt.unix_timestamp())
                .unwrap_or_else(|_| epoch_secs_now());

                let mut s = self.state.lock().unwrap();
                s.failures          = row.count as u32;
                s.window_start_unix = window_start_unix;
            }
            Err(e) => warn!("could not load login limiter state from DB: {e}"),
        }
    }

    /// Returns `true` if the limit has been reached within the current window.
    /// Fast sync path — no DB access.
    pub fn is_blocked(&self) -> bool {
        let s       = self.state.lock().unwrap();
        let age     = Instant::now().duration_since(s.window_start_instant());
        if age >= self.window { return false; }
        s.failures >= self.max_attempts
    }

    /// Records a failed attempt. Persists to DB.
    /// Returns `true` if the caller is still under the limit (i.e. allowed to show a generic error),
    /// `false` once the limit is hit (caller should show the lockout message).
    pub async fn record_failure(&self, pool: &SqlitePool) -> bool {
        let (count, window_start_unix) = {
            let now_unix = epoch_secs_now();
            let mut s    = self.state.lock().unwrap();
            let age      = Instant::now().duration_since(s.window_start_instant());
            if age >= self.window {
                s.failures          = 1;
                s.window_start_unix = now_unix;
            } else {
                s.failures += 1;
            }
            (s.failures, s.window_start_unix)
        };
        if let Err(e) = crate::db::queries::save_login_state(pool, count, window_start_unix).await {
            warn!("failed to persist login limiter state: {e}");
        }
        count < self.max_attempts
    }

    /// Resets the counter on a successful login. Persists to DB.
    pub async fn reset(&self, pool: &SqlitePool) {
        let now_unix = epoch_secs_now();
        {
            let mut s = self.state.lock().unwrap();
            s.failures          = 0;
            s.window_start_unix = now_unix;
        }
        if let Err(e) = crate::db::queries::save_login_state(pool, 0, now_unix).await {
            warn!("failed to persist login limiter reset: {e}");
        }
    }
}
