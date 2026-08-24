//! Reading the statusline payload cache.
//!
//! Cost, context-window occupancy, and the 5h/7d rate limits are handed to the statusline
//! command on stdin and written nowhere else on disk. A small tee in `~/.claude/statusline.sh`
//! keeps the most recent payload per session at
//! `~/.claude/statusline-cache/<key>.json`; this module reads it back.
//!
//! Verified against a real payload, which carries: `context_window`, `cost`, `cwd`,
//! `effort`, `exceeds_200k_tokens`, `fast_mode`, `model`, `output_style`, `prompt_id`,
//! `rate_limits`, `session_id`, `session_name`, `thinking`, `transcript_path`, `version`,
//! `vim`, and `workspace`.
//!
//! So `session_id` **is** present and is the cache key. The tee still falls back to the
//! working directory slugified the way [`crate::session::project_slug`] does it, and this
//! module still tries that second, in case the key ever goes away.
//!
//! `transcript_path` is the useful surprise: it is the exact path to the session's
//! transcript, which removes the slug-guessing that [`crate::session::find_transcript`]
//! otherwise has to do.
//!
//! Without the tee installed there is simply no cache, and every read here returns `None`.
//! That is a normal state, not an error.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::session::project_slug;

/// Spend and edit counters for a session.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Cost {
    /// Total cost so far, in USD.
    pub total_cost_usd: f64,
    /// Wall-clock duration of the session, in milliseconds.
    pub total_duration_ms: u64,
    /// Lines added across the session.
    pub total_lines_added: u64,
    /// Lines removed across the session.
    pub total_lines_removed: u64,
}

/// How full the context window is.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContextWindow {
    /// Occupancy as a percentage, `0.0..=100.0`.
    pub used_percentage: f64,
    /// Total window size in tokens.
    pub context_window_size: u64,
    /// Tokens currently in the window.
    pub total_input_tokens: u64,
}

/// One rate-limit bucket.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RateLimit {
    /// Consumption as a percentage, `0.0..=100.0`.
    pub used_percentage: f64,
    /// Unix epoch seconds at which the bucket resets.
    pub resets_at: u64,
}

/// The 5-hour and 7-day rate-limit buckets.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RateLimits {
    /// The rolling 5-hour bucket.
    pub five_hour: RateLimit,
    /// The rolling 7-day bucket.
    pub seven_day: RateLimit,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct ModelInfo {
    id: String,
    display_name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Effort {
    level: String,
}

/// A cached statusline payload.
///
/// Every field is optional and defaulted: this is an undocumented schema that will drift,
/// and a missing key must never take the widget down.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Statusline {
    /// Spend and edit counters.
    pub cost: Cost,
    /// Context-window occupancy.
    pub context_window: ContextWindow,
    /// Rate-limit buckets. The only source for these anywhere on disk.
    pub rate_limits: RateLimits,
    /// Whether the session has exceeded the 200k-token tier.
    pub exceeds_200k_tokens: bool,
    /// Whether fast mode is on.
    pub fast_mode: bool,
    /// The session's id. Present on real payloads; the cache key is derived from it.
    pub session_id: Option<String>,
    /// The session's human-facing name.
    pub session_name: Option<String>,
    /// Exact path to the session's transcript. More reliable than deriving it from `cwd`.
    pub transcript_path: Option<String>,
    /// The Claude Code version that wrote this payload.
    pub version: Option<String>,
    model: ModelInfo,
    effort: Effort,
}

impl Statusline {
    /// The model's display name, e.g. `Opus 5`.
    #[must_use]
    pub fn model_display_name(&self) -> Option<&str> {
        (!self.model.display_name.is_empty()).then_some(self.model.display_name.as_str())
    }

    /// The raw model id, e.g. `claude-opus-5[1m]`.
    ///
    /// Feed this to [`crate::ModelFamily::from_id`] rather than parsing the display name.
    #[must_use]
    pub fn model_id(&self) -> Option<&str> {
        (!self.model.id.is_empty()).then_some(self.model.id.as_str())
    }

    /// The configured effort level, e.g. `high`.
    #[must_use]
    pub fn effort_level(&self) -> Option<&str> {
        (!self.effort.level.is_empty()).then_some(self.effort.level.as_str())
    }
}

/// The directory the statusline tee writes to.
#[must_use]
pub fn cache_dir(root: &Path) -> PathBuf {
    root.join("statusline-cache")
}

/// Read the cached payload for a session.
///
/// Tries the session id first, then the slugified working directory, matching the key the
/// tee writes. Returns `None` when the tee is not installed, has not run yet, or wrote
/// something unparseable.
#[must_use]
pub fn read(root: &Path, session_id: Option<&str>, cwd: Option<&str>) -> Option<Statusline> {
    let dir = cache_dir(root);

    let candidates = session_id
        .map(|id| dir.join(format!("{id}.json")))
        .into_iter()
        .chain(cwd.map(|cwd| dir.join(format!("{}.json", project_slug(cwd)))));

    for path in candidates {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };

        if let Ok(statusline) = serde_json::from_str::<Statusline>(&contents) {
            return Some(statusline);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;

    use super::*;

    /// A payload in the older shape, carrying no session id, to exercise the cwd fallback.
    const PAYLOAD: &str = r#"{"model":{"display_name":"Opus 5"},"effort":{"level":"high"},
        "fast_mode":false,"workspace":{"current_dir":"/Users/redd/code"},"cwd":"/Users/redd/code",
        "cost":{"total_cost_usd":12.3456,"total_duration_ms":3600000,"total_lines_added":420,"total_lines_removed":69},
        "context_window":{"used_percentage":41.5,"context_window_size":1000000,"total_input_tokens":415000},
        "rate_limits":{"five_hour":{"used_percentage":22.5,"resets_at":9999999999},
                       "seven_day":{"used_percentage":61.0,"resets_at":8888888888}},
        "output_style":{"name":"default"},"exceeds_200k_tokens":true,"vim":{"mode":""}}"#;

    fn root(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("claude-metrics-statusline-{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("statusline-cache")).unwrap();
        base
    }

    #[test]
    fn a_payload_keyed_by_cwd_slug_is_found() {
        // The fallback path: a payload with no session id, keyed by slugified cwd.
        let base = root("cwd");
        fs::write(base.join("statusline-cache/-Users-redd-code.json"), PAYLOAD).unwrap();

        let found = read(&base, Some("no-such-session"), Some("/Users/redd/code"))
            .expect("the cwd-slug fallback must resolve");

        assert!((found.cost.total_cost_usd - 12.3456).abs() < f64::EPSILON);
        assert!((found.rate_limits.five_hour.used_percentage - 22.5).abs() < f64::EPSILON);
        assert_eq!(found.rate_limits.seven_day.resets_at, 8_888_888_888);
        assert!((found.context_window.used_percentage - 41.5).abs() < f64::EPSILON);
        assert!(found.exceeds_200k_tokens);
        assert_eq!(found.model_display_name(), Some("Opus 5"));
        assert_eq!(found.effort_level(), Some("high"));

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_session_keyed_payload_wins_over_the_cwd_fallback() {
        let base = root("session");
        fs::write(base.join("statusline-cache/sess-1.json"), PAYLOAD).unwrap();
        fs::write(
            base.join("statusline-cache/-Users-redd-code.json"),
            r#"{"cost":{"total_cost_usd":999.0}}"#,
        )
        .unwrap();

        let found = read(&base, Some("sess-1"), Some("/Users/redd/code")).unwrap();
        assert!(
            (found.cost.total_cost_usd - 12.3456).abs() < f64::EPSILON,
            "the session-keyed file must be preferred"
        );

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_missing_or_broken_cache_is_not_an_error() {
        let base = root("missing");

        assert!(read(&base, Some("nope"), Some("/nowhere")).is_none());

        // A half-written or garbage file must be skipped, not panicked on.
        fs::write(base.join("statusline-cache/-nowhere.json"), "{not json").unwrap();
        assert!(read(&base, None, Some("/nowhere")).is_none());

        // And an unknown future key set must still parse, with defaults for what is absent.
        fs::write(
            base.join("statusline-cache/-nowhere.json"),
            r#"{"brand_new_thing":{"a":1}}"#,
        )
        .unwrap();
        let found = read(&base, None, Some("/nowhere")).expect("unknown fields must be ignored");
        assert!((found.cost.total_cost_usd - 0.0).abs() < f64::EPSILON);

        fs::remove_dir_all(&base).unwrap();
    }
}
