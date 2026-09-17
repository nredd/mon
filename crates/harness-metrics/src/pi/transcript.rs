//! Parsing pi session records.
//!
//! Verified against pi's own session-log documentation. A session file is one JSON object
//! per line under `~/.pi/agent/sessions/<slug>/<timestamp>_<uuid>.jsonl`.
//!
//! # Counting rules
//!
//! - A file is validated as a pi session by checking that its **first line** has
//!   `type == "session"` and a numeric `version` field. Every later line in a file that
//!   fails this check is dropped.
//! - `{"type":"message","message":{"role":"assistant","provider":...,"usage":{...}}}` is
//!   the main source. Unlike Claude's running total, `usage.input`/`output`/`cacheRead`/
//!   `cacheWrite` are already a clean per-turn amount -- no high-water-mark tracking
//!   needed, it is taken as-is.
//! - `{"type":"compaction","usage":{...}}` and `{"type":"branch_summary","usage":{...}}`
//!   carry an *optional* top-level `usage` of the same shape, representing a real
//!   summarization call. Included when present. Neither entry type carries a
//!   `provider`/`model` field in the schema, so both fold to `Other` -- a rare, small
//!   contribution, not worth inventing an attribution for.
//! - Every other entry type (`user`, `toolResult`, `bashExecution`, `custom`,
//!   `custom_message`, `label`, `session_info`, `model_change`, `thinking_level_change`, and
//!   anything unrecognised) carries no billable usage and is skipped.
//! - Dedupe key: the entry's own `id` field, unique per entry within one session file. No
//!   cross-file replay concern the way Claude's subagent transcripts have.

use serde::Deserialize;

use crate::{
    iso8601,
    ledger::{HarnessRecord, TokenTotals},
};

use super::family::PiFamily;

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl From<RawUsage> for TokenTotals {
    fn from(raw: RawUsage) -> Self {
        TokenTotals {
            input: raw.input,
            output: raw.output,
            cache_read: raw.cache_read,
            cache_creation: raw.cache_write,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Message {
    role: Option<String>,
    provider: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    /// Only read for the first-line validation check; the value itself is not used.
    version: Option<serde_json::Value>,
    timestamp: Option<String>,
    message: Option<Message>,
    /// The top-level `usage` on `compaction` / `branch_summary` entries.
    usage: Option<RawUsage>,
}

/// Parsing state threaded across every line of one session file.
#[derive(Debug, Default)]
pub(crate) struct State {
    /// Set on the first line; `Some(false)` means the file failed the `session` header
    /// check and every later line is dropped without being parsed.
    valid: Option<bool>,
}

/// One billable pi entry.
#[derive(Clone, Debug)]
pub struct Record {
    id: Option<String>,
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

        let valid = *state.valid.get_or_insert_with(|| {
            parsed.kind.as_deref() == Some("session")
                && parsed
                    .version
                    .as_ref()
                    .is_some_and(serde_json::Value::is_number)
        });
        if !valid {
            return None;
        }

        let timestamp_ms = parsed.timestamp.as_deref().and_then(iso8601::parse_ms);

        match parsed.kind.as_deref() {
            Some("message") => {
                let message = parsed.message?;
                if message.role.as_deref() != Some("assistant") {
                    return None;
                }

                let usage = message.usage?;
                let family =
                    PiFamily::from_provider(message.provider.as_deref().unwrap_or_default())
                        .label();

                Some(Record {
                    id: parsed.id,
                    timestamp_ms,
                    family,
                    usage: TokenTotals::from(usage),
                })
            }
            Some("compaction" | "branch_summary") => {
                let usage = parsed.usage?;

                Some(Record {
                    id: parsed.id,
                    timestamp_ms,
                    // Neither entry type carries a provider/model in the schema.
                    family: PiFamily::Other.label(),
                    usage: TokenTotals::from(usage),
                })
            }
            _ => None,
        }
    }

    fn timestamp_ms(&self) -> Option<u64> {
        self.timestamp_ms
    }

    fn dedupe_key(&self) -> Option<String> {
        self.id.clone()
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

    const HEADER: &str = r#"{"type":"session","version":1,"id":"sess-1","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/repo"}"#;

    fn assistant_message(id: &str, stamp: &str, provider: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"message","id":"{id}","parentId":null,"timestamp":"{stamp}","message":{{"role":"assistant","provider":"{provider}","model":"m","usage":{{"input":{input},"output":{output},"cacheRead":3,"cacheWrite":4,"totalTokens":{},"cost":{{}}}}}}}}"#,
            input + output
        )
    }

    fn compaction(id: &str, stamp: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"compaction","id":"{id}","parentId":null,"timestamp":"{stamp}","usage":{{"input":{input},"output":{output},"cacheRead":0,"cacheWrite":0,"totalTokens":{}}}}}"#,
            input + output
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
    fn a_file_not_starting_with_a_session_header_is_rejected_entirely() {
        let lines = vec![assistant_message(
            "m1",
            "2026-01-01T00:00:01.000Z",
            "anthropic",
            10,
            5,
        )];
        assert!(parse_all(&lines).is_empty());
    }

    #[test]
    fn a_session_header_missing_a_numeric_version_is_rejected() {
        let bad_header = r#"{"type":"session","version":"not-a-number","id":"s","timestamp":"2026-01-01T00:00:00.000Z"}"#;
        let lines = vec![
            bad_header.to_owned(),
            assistant_message("m1", "2026-01-01T00:00:01.000Z", "anthropic", 10, 5),
        ];
        assert!(parse_all(&lines).is_empty());
    }

    #[test]
    fn assistant_usage_is_summed_directly_across_multiple_entries() {
        let lines = vec![
            HEADER.to_owned(),
            assistant_message("m1", "2026-01-01T00:00:01.000Z", "anthropic", 10, 5),
            assistant_message("m2", "2026-01-01T00:00:02.000Z", "anthropic", 20, 8),
        ];

        let records = parse_all(&lines);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].usage.input, 10);
        assert_eq!(records[0].usage.cache_read, 3);
        assert_eq!(
            records[0].usage.cache_creation, 4,
            "cacheWrite maps to cache_creation"
        );
        assert_eq!(
            records[1].usage.input, 20,
            "each entry is a direct amount, not a running total"
        );
        assert_eq!(records[0].family, "Anthropic");
    }

    #[test]
    fn compaction_and_branch_summary_usage_is_included() {
        let lines = vec![
            HEADER.to_owned(),
            compaction("c1", "2026-01-01T00:00:01.000Z", 100, 20),
        ];

        let records = parse_all(&lines);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage.input, 100);
        assert_eq!(
            records[0].family, "Other",
            "no provider on a compaction entry"
        );
    }

    #[test]
    fn an_entry_missing_usage_is_skipped_without_panicking() {
        let no_usage = r#"{"type":"message","id":"m3","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","provider":"anthropic"}}"#;
        let lines = vec![HEADER.to_owned(), no_usage.to_owned()];
        assert!(parse_all(&lines).is_empty());
    }

    #[test]
    fn unrelated_entry_types_are_ignored() {
        let lines = vec![
            HEADER.to_owned(),
            r#"{"type":"user","id":"u1","timestamp":"2026-01-01T00:00:01.000Z"}"#.to_owned(),
            r#"{"type":"toolResult","id":"t1","timestamp":"2026-01-01T00:00:01.000Z"}"#.to_owned(),
            r#"{"type":"bashExecution","id":"b1","timestamp":"2026-01-01T00:00:01.000Z"}"#
                .to_owned(),
            r#"{"type":"label","id":"l1","timestamp":"2026-01-01T00:00:01.000Z"}"#.to_owned(),
        ];
        assert!(parse_all(&lines).is_empty());
    }

    #[test]
    fn a_missing_directory_yields_empty_without_a_root_to_read() {
        assert!(super::super::discover::discover(std::path::Path::new("/nope"), 0).is_empty());
    }

    #[test]
    fn junk_and_empty_lines_are_skipped_rather_than_panicking() {
        let mut state = State::default();
        assert!(Record::parse("", &mut state).is_none());
        assert!(Record::parse("{not json", &mut state).is_none());
    }
}
