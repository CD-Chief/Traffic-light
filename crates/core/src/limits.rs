//! Plan limits (5-hour and weekly), forwarded by the user's status-line
//! script. Account-wide, so they live beside the sessions, not on them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// 0 to 100.
    pub used_percentage: f64,
    /// Unix epoch **seconds**.
    pub resets_at: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Limits {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

/// The only part of a status-line payload that is read. Everything else
/// (cwd, transcript_path, model, cost) is dropped by the deserializer.
#[derive(Debug, Default, Deserialize)]
pub struct StatusPayload {
    rate_limits: Option<RateLimits>,
}

#[derive(Debug, Default, Deserialize)]
struct RateLimits {
    // A malformed window is treated as absent rather than failing the payload.
    #[serde(default, deserialize_with = "lenient")]
    five_hour: Option<Window>,
    #[serde(default, deserialize_with = "lenient")]
    seven_day: Option<Window>,
}

fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Window>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(d)?.and_then(|v| serde_json::from_value(v).ok()))
}

impl Limits {
    /// Merge a payload in, per window. Returns true when anything changed.
    pub fn update(&mut self, p: &StatusPayload) -> bool {
        let Some(rl) = &p.rate_limits else {
            return false;
        };
        let before = *self;
        for (slot, new) in [
            (&mut self.five_hour, rl.five_hour),
            (&mut self.seven_day, rl.seven_day),
        ] {
            if let Some(mut w) = new {
                w.used_percentage = if w.used_percentage.is_finite() {
                    w.used_percentage.clamp(0.0, 100.0)
                } else {
                    0.0
                };
                *slot = Some(merge(*slot, w));
            }
        }
        *self != before
    }
}

/// Two `resets_at` values this close are the same window.
const SAME_WINDOW_SECS: u64 = 60;

/// Every session's status line reports the account's limits as of that
/// session's own last reply, so an idle session can send numbers older than
/// what we already have. A later `resets_at` is a new window; within one
/// window usage only grows; an earlier window is stale.
fn merge(old: Option<Window>, new: Window) -> Window {
    match old {
        Some(o) if new.resets_at + SAME_WINDOW_SECS < o.resets_at => o,
        Some(o) if new.resets_at <= o.resets_at + SAME_WINDOW_SECS => Window {
            used_percentage: o.used_percentage.max(new.used_percentage),
            resets_at: o.resets_at.max(new.resets_at),
        },
        _ => new,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(five: f64, five_reset: u64) -> StatusPayload {
        serde_json::from_str(&format!(
            r#"{{"rate_limits":{{"five_hour":{{"used_percentage":{five},"resets_at":{five_reset}}}}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn idle_session_cannot_roll_numbers_back() {
        let mut l = Limits::default();
        assert!(l.update(&payload(45.0, 10_000)));
        // Same window, lower number from a session that replied earlier.
        assert!(!l.update(&payload(20.0, 10_000)));
        assert_eq!(l.five_hour.unwrap().used_percentage, 45.0);
        // A report from the previous window is ignored outright.
        assert!(!l.update(&payload(90.0, 2_000)));
        assert_eq!(l.five_hour.unwrap().resets_at, 10_000);
    }

    #[test]
    fn new_window_replaces_old_and_same_window_grows() {
        let mut l = Limits::default();
        l.update(&payload(45.0, 10_000));
        assert!(l.update(&payload(50.0, 10_030))); // within tolerance: same window
        assert_eq!(l.five_hour.unwrap().used_percentage, 50.0);
        assert!(l.update(&payload(3.0, 28_000))); // window rolled over
        assert_eq!(l.five_hour.unwrap().used_percentage, 3.0);
        assert_eq!(l.five_hour.unwrap().resets_at, 28_000);
    }
}
