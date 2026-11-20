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
                *slot = Some(w);
            }
        }
        *self != before
    }
}
