//! Parsing Codex CLI rollout records.
//!
//! Verified against upstream `openai/codex` and community documentation of the rollout
//! format, trusted over any other assumption. A rollout file is one JSON object per line:
//! `{"timestamp": "...", "type": "...", "payload": {...}}`.
//!
//! # Counting rules
//!
//! - A file is validated as a Codex rollout by checking that its **first line** has
//!   `type == "session_meta"`. Every later line in a file that fails this check is dropped.
//! - Only `event_msg` records with `payload.type == "token_count"` carry usage.
//!   `payload.info` is `null` on session-start pings and aborted turns; those are skipped.
//! - When `info` is present, `info.total_token_usage` is preferred: it is **cumulative for
//!   the whole file**. The previous cumulative snapshot seen in this file is tracked in
//!   [`Record`]'s parsing [`State`], and only the field-by-field growth since that snapshot
//!   is reported -- otherwise a repeated identical cumulative value would double-count, and
//!   every later event would restate everything that came before it. A cumulative value
//!   that *decreases* (the file was truncated and replaced out from under an in-flight
//!   read) is treated as "start over from here": the whole new value becomes the delta,
//!   never a negative one.
//! - If `total_token_usage` is absent but `info.last_token_usage` is present, that value is
//!   used directly as the delta -- it already describes one turn's usage, no running state
//!   needed.
//! - Field mapping into [`TokenTotals`]: `input = input_tokens - cached_input_tokens`,
//!   `output = output_tokens + reasoning_output_tokens`, `cache_read = cached_input_tokens`,
//!   `cache_creation = 0`. `OpenAI`'s usage payload exposes no cache-*write* count, unlike
//!   `Anthropic`'s -- there is nothing to put there.
//! - Model attribution: the most recently seen `payload.model` on a `turn_context` record,
//!   falling back to `session_meta.payload.model` if no `turn_context` has appeared yet,
//!   and finally to a hardcoded `"gpt-5"` guess if genuinely nothing has been seen. This
//!   mirrors upstream Codex tooling's own fallback for an unknown model.
//! - Every usage record already carries its own computed delta, so
//!   [`Record::dedupe_key`] always returns `None`: there is no cross-record repeat for the
//!   ledger to detect, because the repeat detection already happened here, in `State`.

use serde::Deserialize;

use crate::{
    iso8601,
    ledger::{HarnessRecord, TokenTotals},
};

use super::family::CodexFamily;

/// The raw four-field usage object nested under `info.last_token_usage` /
/// `info.total_token_usage`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
// The shared `_tokens` suffix names exactly what these fields are (a token count) and
// matches the wire format field names; stripping it would make them less legible, not more.
#[allow(clippy::struct_field_names)]
struct RawUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
}

impl From<RawUsage> for TokenTotals {
    fn from(raw: RawUsage) -> Self {
        TokenTotals {
            input: raw.input_tokens.saturating_sub(raw.cached_input_tokens),
            output: raw
                .output_tokens
                .saturating_add(raw.reasoning_output_tokens),
            cache_read: raw.cached_input_tokens,
            // OpenAI exposes no cache-write count in this payload.
            cache_creation: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct TokenCountInfo {
    last_token_usage: Option<RawUsage>,
    total_token_usage: Option<RawUsage>,
}

/// The union of every `payload` shape this module reads from. Irrelevant fields for a
/// given record kind are simply absent and ignored.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Payload {
    /// `session_meta`'s model, and `turn_context`'s model.
    model: Option<String>,
    /// `event_msg`'s own kind, e.g. `token_count`.
    #[serde(rename = "type")]
    kind: Option<String>,
    /// `event_msg` / `token_count`'s usage, `null` on a ping or an aborted turn.
    info: Option<TokenCountInfo>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

/// Parsing state threaded across every line of one rollout file.
#[derive(Debug, Default)]
pub(crate) struct State {
    /// Set on the first line; `Some(false)` means the file failed the `session_meta` check
    /// and every later line is dropped without being parsed.
    valid: Option<bool>,
    /// The most recently attributed model, updated by `session_meta` (once) and by every
    /// `turn_context`.
    model: Option<String>,
    /// The last cumulative `total_token_usage` snapshot seen in this file, used to compute
    /// the next event's delta.
    prev_cumulative: Option<TokenTotals>,
}

/// One billable Codex token-count event, already reduced to the exact delta to add.
#[derive(Clone, Copy, Debug)]
pub struct Record {
    timestamp_ms: Option<u64>,
    family: &'static str,
    usage: TokenTotals,
}

impl HarnessRecord for Record {
    type State = State;

    fn parse(line: &str, state: &mut Self::State) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        // A malformed or truncated line is skipped, never fatal -- a tailer can legitimately
        // read a half-written line at the end of a live file.
        let parsed: Line = serde_json::from_str(line).ok()?;

        let valid = *state
            .valid
            .get_or_insert_with(|| parsed.kind.as_deref() == Some("session_meta"));
        if !valid {
            return None;
        }

        match parsed.kind.as_deref() {
            Some("session_meta") => {
                // Only a fallback: `turn_context` overrides this the moment one appears.
                if state.model.is_none()
                    && let Some(model) = parsed.payload.and_then(|p| p.model)
                {
                    state.model = Some(model);
                }
                None
            }
            Some("turn_context") => {
                if let Some(model) = parsed.payload.and_then(|p| p.model) {
                    state.model = Some(model);
                }
                None
            }
            Some("event_msg") => {
                let payload = parsed.payload?;
                if payload.kind.as_deref() != Some("token_count") {
                    return None;
                }

                // `null` on a session-start ping or an aborted turn: nothing billable here.
                let info = payload.info?;

                let usage = if let Some(total) = info.total_token_usage {
                    let mapped = TokenTotals::from(total);
                    let previous = state.prev_cumulative.unwrap_or_default();
                    state.prev_cumulative = Some(mapped);
                    TokenTotals::delta_from(mapped, previous)
                } else {
                    TokenTotals::from(info.last_token_usage?)
                };

                let family =
                    CodexFamily::from_id(state.model.as_deref().unwrap_or("gpt-5")).label();

                Some(Record {
                    timestamp_ms: parsed.timestamp.as_deref().and_then(iso8601::parse_ms),
                    family,
                    usage,
                })
            }
            _ => None,
        }
    }

    fn timestamp_ms(&self) -> Option<u64> {
        self.timestamp_ms
    }

    /// Always `None`: the delta was already computed in [`State`] while parsing, so there
    /// is nothing left for the ledger to deduplicate.
    fn dedupe_key(&self) -> Option<String> {
        None
    }

    fn family_label(&self) -> &'static str {
        self.family
    }

    fn billable_usage(&self) -> Option<TokenTotals> {
        Some(self.usage)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const SESSION_META: &str = r#"{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"id":"sess-1","cwd":"/repo","originator":"cli","model_provider":"openai","model":"gpt-5"}}"#;

    fn turn_context(model: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-01-01T00:00:01.000Z","type":"turn_context","payload":{{"model":"{model}"}}}}"#
        )
    }

    fn token_count_total(
        stamp: &str, input: u64, cached: u64, output: u64, reasoning: u64,
    ) -> String {
        format!(
            r#"{{"timestamp":"{stamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"reasoning_output_tokens":{reasoning},"total_tokens":{}}}}}}}}}"#,
            input + output
        )
    }

    fn token_count_last(
        stamp: &str, input: u64, cached: u64, output: u64, reasoning: u64,
    ) -> String {
        format!(
            r#"{{"timestamp":"{stamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"reasoning_output_tokens":{reasoning},"total_tokens":{}}}}}}}}}"#,
            input + output
        )
    }

    fn token_count_null_info(stamp: &str) -> String {
        format!(
            r#"{{"timestamp":"{stamp}","type":"event_msg","payload":{{"type":"token_count","info":null}}}}"#
        )
    }

    fn parse_all(lines: &[String]) -> Vec<Record> {
        let mut state = State::default();
        lines
            .iter()
            .filter_map(|line| Record::parse(line, &mut state))
            .collect()
    }

    #[test]
    fn a_file_not_starting_with_session_meta_is_rejected_entirely() {
        let lines = vec![
            turn_context("gpt-5"),
            token_count_total("2026-01-01T00:00:02.000Z", 100, 0, 20, 0),
        ];

        assert!(
            parse_all(&lines).is_empty(),
            "no session_meta first line, nothing counts"
        );
    }

    #[test]
    fn cumulative_snapshots_are_reduced_to_their_growth() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_total("2026-01-01T00:00:02.000Z", 100, 0, 20, 0),
            token_count_total("2026-01-01T00:00:03.000Z", 250, 0, 55, 0),
        ];

        let records = parse_all(&lines);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].usage.input, 100);
        assert_eq!(records[0].usage.output, 20);
        assert_eq!(records[1].usage.input, 150, "250 - 100");
        assert_eq!(records[1].usage.output, 35, "55 - 20");
    }

    #[test]
    fn a_repeated_identical_cumulative_snapshot_does_not_double_count() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_total("2026-01-01T00:00:02.000Z", 100, 0, 20, 0),
            token_count_total("2026-01-01T00:00:03.000Z", 100, 0, 20, 0),
        ];

        let records = parse_all(&lines);
        assert_eq!(records[1].usage, TokenTotals::default());
    }

    #[test]
    fn a_decreasing_cumulative_snapshot_starts_over_rather_than_underflowing() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_total("2026-01-01T00:00:02.000Z", 100, 0, 20, 0),
            // Simulates the file having been truncated and replaced.
            token_count_total("2026-01-01T00:00:03.000Z", 10, 0, 5, 0),
        ];

        let records = parse_all(&lines);
        assert_eq!(
            records[1].usage.input, 10,
            "the lower value is taken fresh, not as -90"
        );
        assert_eq!(records[1].usage.output, 5);
    }

    #[test]
    fn null_info_events_are_skipped() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_null_info("2026-01-01T00:00:02.000Z"),
        ];

        assert!(parse_all(&lines).is_empty());
    }

    #[test]
    fn reasoning_tokens_fold_into_output() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_total("2026-01-01T00:00:02.000Z", 100, 0, 20, 15),
        ];

        assert_eq!(parse_all(&lines)[0].usage.output, 35);
    }

    #[test]
    fn cached_input_is_split_out_of_input_and_reported_as_cache_read() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_total("2026-01-01T00:00:02.000Z", 100, 40, 20, 0),
        ];

        let usage = parse_all(&lines)[0].usage;
        assert_eq!(usage.input, 60, "100 total input minus the 40 cached");
        assert_eq!(usage.cache_read, 40);
        assert_eq!(
            usage.cache_creation, 0,
            "OpenAI exposes no cache-write count"
        );
    }

    #[test]
    fn last_token_usage_is_used_directly_when_total_is_absent() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_last("2026-01-01T00:00:02.000Z", 30, 0, 10, 0),
        ];

        let usage = parse_all(&lines)[0].usage;
        assert_eq!(usage.input, 30);
        assert_eq!(usage.output, 10);
        assert!(
            parse_all(&lines)[0].family == "GPT-5",
            "falls back to the session_meta model"
        );
    }

    #[test]
    fn model_tracking_follows_the_most_recent_turn_context() {
        let lines = vec![
            SESSION_META.to_owned(),
            turn_context("o3"),
            token_count_total("2026-01-01T00:00:02.000Z", 10, 0, 5, 0),
            turn_context("gpt-4o"),
            token_count_total("2026-01-01T00:00:03.000Z", 20, 0, 10, 0),
        ];

        let records = parse_all(&lines);
        assert_eq!(records[0].family, "o-series");
        assert_eq!(records[1].family, "GPT-4");
    }

    #[test]
    fn dedupe_key_is_always_none() {
        let lines = vec![
            SESSION_META.to_owned(),
            token_count_total("2026-01-01T00:00:02.000Z", 10, 0, 5, 0),
        ];

        assert_eq!(parse_all(&lines)[0].dedupe_key(), None);
    }

    #[test]
    fn junk_and_empty_lines_are_skipped_rather_than_panicking() {
        let mut state = State::default();
        assert!(Record::parse("", &mut state).is_none());
        assert!(Record::parse("{not json", &mut state).is_none());
    }
}
