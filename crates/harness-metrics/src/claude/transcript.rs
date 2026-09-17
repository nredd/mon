//! Parsing Claude Code transcript records.
//!
//! Everything here is deliberately permissive. The `~/.claude` schema is undocumented and
//! drifts between releases, so every field is optional, unknown fields are ignored, and a
//! line that will not parse is skipped rather than failing the whole read.
//!
//! # Counting rules
//!
//! These are not obvious and getting any of them wrong inflates every number:
//!
//! - Dedupe on `requestId` + `message.id`. Retries and resumed sessions replay identical
//!   messages, so counting lines double-counts. The same message can also appear in both a
//!   session's main transcript and its subagent transcript, so the dedupe key is treated as
//!   global by [`crate::ledger::Ledger`] rather than scoped to one file.
//! - A message with several content blocks carries one `usage` object and counts **once**.
//! - Take `cache_creation_input_tokens` alone, never plus the `cache_creation.ephemeral_*`
//!   buckets -- the former is exactly the sum of the latter.
//! - Ignore `usage.iterations[]`; it restates the message-level counts.
//! - Skip `<synthetic>` models and `isApiErrorMessage` records.
//! - `isSidechain: true` marks a subagent, which carries the **parent** session's id.
//! - A message is written as one record **per content block**. The per-request fields
//!   (`input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`) repeat
//!   identically across those records and are counted once; `output_tokens` is a **running
//!   total**. [`billable_usage`](Record::billable_usage) hands back the running snapshot
//!   as-is on every record, and the ledger's generic keyed-delta logic (first occurrence of
//!   a key contributes the full snapshot, later occurrences contribute the field-by-field
//!   growth) turns that into the right per-block increment without this module having to
//!   track a high-water mark itself.
//!
//! # Known limits
//!
//! Totals were calibrated against `~/.claude.json`'s `projects[cwd].lastModelUsage`, which
//! records real per-model counts for the last session in each project. Two gaps remain, and
//! both are properties of the data source rather than of this crate:
//!
//! - **Background Haiku calls never reach a transcript.** Session titles and similar
//!   internal calls are billed but not written to `~/.claude/projects`, so Haiku totals read
//!   as zero even when `lastModelUsage` shows a small amount. Nothing here can recover them.
//! - **A few percent of a long session's tokens can be missing.** On one 1199-line session
//!   the Opus input, cache-read, and cache-write figures matched `lastModelUsage` exactly
//!   while output landed at 97.7%; a short session matched on all four fields exactly.
//!
//! Treat the numbers as a close live estimate, not as billing truth.

use serde::Deserialize;

use crate::{
    iso8601,
    ledger::{HarnessRecord, TokenTotals},
};

use super::family::ClaudeFamily;

/// The `message.usage` object.
///
/// `iterations` is deliberately absent: it restates the same counts as the message-level
/// fields, so reading it would double-count.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Usage {
    #[serde(rename = "input_tokens")]
    input: u64,
    #[serde(rename = "output_tokens")]
    output: u64,
    #[serde(rename = "cache_read_input_tokens")]
    cache_read: u64,
    #[serde(rename = "cache_creation_input_tokens")]
    cache_creation: u64,
}

impl From<&Usage> for TokenTotals {
    fn from(usage: &Usage) -> Self {
        TokenTotals {
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_creation: usage.cache_creation,
        }
    }
}

/// The `message` object on an assistant record.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

/// One transcript line.
///
/// A single struct covers every record type: irrelevant ones simply leave `message` and
/// `usage` empty and get skipped. That is cheaper and far more drift-tolerant than an enum
/// over `type`, which would have to grow an arm for every new record kind.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Record {
    /// Record kind: `assistant`, `user`, `system`, `attachment`, and others.
    #[serde(rename = "type")]
    kind: Option<String>,
    /// The API request this record came from. Half of the dedupe key.
    request_id: Option<String>,
    /// Set when the record is an API error rather than a real response.
    is_api_error_message: bool,
    /// ISO-8601 UTC instant the record was written, e.g. `2026-08-24T20:26:13.919Z`.
    ///
    /// Present on every billable record in practice, but optional like everything else
    /// here -- the schema is undocumented and drifts between releases.
    timestamp: Option<String>,
    message: Option<Message>,
}

impl Record {
    /// Whether this record's usage should count toward totals.
    fn is_billable(&self) -> bool {
        if self.is_api_error_message {
            return false;
        }

        if self.kind.as_deref() != Some("assistant") {
            return false;
        }

        // `<synthetic>` is the placeholder model on locally-generated messages that never
        // hit the API.
        !matches!(self.model_id(), Some("<synthetic>") | None)
    }

    fn model_id(&self) -> Option<&str> {
        self.message.as_ref()?.model.as_deref()
    }
}

impl HarnessRecord for Record {
    type State = ();

    fn parse(line: &str, (): &mut Self::State) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        // A malformed or truncated line is skipped, never fatal -- a tailer can legitimately
        // read a half-written line at the end of a live file.
        serde_json::from_str(line).ok()
    }

    /// The record's instant, in Unix epoch milliseconds.
    fn timestamp_ms(&self) -> Option<u64> {
        iso8601::parse_ms(self.timestamp.as_deref()?)
    }

    /// The dedupe key: request id plus message id.
    fn dedupe_key(&self) -> Option<String> {
        let message = self.message.as_ref()?;
        let request = self.request_id.as_deref().unwrap_or("");
        let id = message.id.as_deref()?;
        Some(format!("{request}\u{0}{id}"))
    }

    fn family_label(&self) -> &'static str {
        ClaudeFamily::from_id(self.model_id().unwrap_or_default()).label()
    }

    fn billable_usage(&self) -> Option<TokenTotals> {
        if !self.is_billable() {
            return None;
        }

        Some(TokenTotals::from(self.message.as_ref()?.usage.as_ref()?))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn parse(line: &str) -> Record {
        Record::parse(line, &mut ()).expect("fixture parses")
    }

    #[test]
    fn a_real_timestamp_parses_to_epoch_millis() {
        // Taken verbatim from a live transcript.
        let record = parse(
            r#"{"type":"assistant","timestamp":"2026-08-24T20:26:13.919Z","message":{"id":"m","model":"claude-opus-5"}}"#,
        );

        assert_eq!(record.timestamp_ms(), Some(1_787_603_173_919));
    }

    /// Build one record of a streaming assistant message.
    ///
    /// The ISO-8601 parser itself is tested in `crate::iso8601`; only this backend's use of
    /// it (feeding `Record::timestamp_ms`) is covered here.
    ///
    /// This mirrors what real transcripts contain: a message is written as one record per
    /// content block, all sharing `requestId` + `message.id`. The per-request fields repeat
    /// identically; `output_tokens` is a **running total** that grows with each block.
    fn block(
        stamp: &str, request: &str, message: &str, model: &str, block: &str, running_output: u64,
    ) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{stamp}","requestId":"{request}","sessionId":"s1","isSidechain":false,"message":{{"id":"{message}","model":"{model}","content":[{{"type":"{block}"}}],"usage":{{"input_tokens":10,"output_tokens":{running_output},"cache_read_input_tokens":30,"cache_creation_input_tokens":40,"cache_creation":{{"ephemeral_1h_input_tokens":40,"ephemeral_5m_input_tokens":0}},"iterations":[{{"input_tokens":10,"output_tokens":{running_output},"cache_read_input_tokens":30,"cache_creation_input_tokens":40}}]}}}}}}"#
        )
    }

    /// The three records of one streamed message: thinking, then text, then a tool call.
    fn multi_block() -> Vec<String> {
        vec![
            block(
                "2026-08-24T20:03:30.000Z",
                "req_1",
                "msg_1",
                "claude-sonnet-5",
                "thinking",
                3,
            ),
            block(
                "2026-08-24T20:03:30.500Z",
                "req_1",
                "msg_1",
                "claude-sonnet-5",
                "text",
                12,
            ),
            block(
                "2026-08-24T20:03:31.000Z",
                "req_1",
                "msg_1",
                "claude-sonnet-5",
                "tool_use",
                20,
            ),
        ]
    }

    #[test]
    fn a_multi_content_block_message_reports_the_same_snapshot_every_block() {
        // The per-request fields repeat identically across blocks; the ledger's keyed-delta
        // logic is what turns that into "counted once" -- this module just hands back the
        // running snapshot as-is on every record.
        for line in multi_block() {
            let record = parse(&line);
            let usage = record.billable_usage().expect("billable");
            assert_eq!(usage.input, 10);
            assert_eq!(usage.cache_read, 30);
            assert_eq!(
                usage.cache_creation, 40,
                "cache_creation must come from the message-level field alone -- adding the \
                 ephemeral_* buckets on top double-counts, they sum to the same number"
            );
        }
    }

    #[test]
    fn output_tokens_are_a_running_total_not_a_per_block_amount() {
        // Verified against real transcripts: `output_tokens` grows with each content block
        // and the last record carries the final figure.
        let outputs: Vec<u64> = multi_block()
            .iter()
            .map(|line| parse(line).billable_usage().unwrap().output)
            .collect();
        assert_eq!(outputs, vec![3, 12, 20]);
    }

    #[test]
    fn every_block_shares_one_dedupe_key() {
        let keys: Vec<Option<String>> = multi_block()
            .iter()
            .map(|line| parse(line).dedupe_key())
            .collect();
        assert_eq!(keys[0], keys[1]);
        assert_eq!(keys[1], keys[2]);
        assert!(keys[0].is_some());
    }

    #[test]
    fn synthetic_and_api_error_records_are_filtered() {
        let synthetic = r#"{"type":"assistant","requestId":"r","message":{"id":"m1","model":"<synthetic>","usage":{"output_tokens":99}}}"#;
        let api_error = r#"{"type":"assistant","isApiErrorMessage":true,"requestId":"r","message":{"id":"m2","model":"claude-opus-5","usage":{"output_tokens":99}}}"#;
        let user = r#"{"type":"user","message":{"id":"m3","usage":{"output_tokens":99}}}"#;

        for line in [synthetic, api_error, user] {
            assert!(parse(line).billable_usage().is_none(), "{line}");
        }
    }

    #[test]
    fn a_sidechain_record_is_still_billable_and_folds_to_its_own_family() {
        // `isSidechain` and `sessionId` are unknown fields to this struct now -- subagent
        // records are counted the same as main-transcript ones, they are just discovered
        // from a different file (see `claude::discover`).
        let line = r#"{"type":"assistant","requestId":"r2","sessionId":"parent-1","isSidechain":true,"message":{"id":"m9","model":"claude-haiku-4-5-20251001","usage":{"output_tokens":5}}}"#;
        let record = parse(line);
        assert!(record.billable_usage().is_some());
        assert_eq!(record.family_label(), "Haiku");
    }

    #[test]
    fn unknown_fields_and_junk_lines_do_not_fail() {
        // The whole point: `~/.claude` drifts, and a widget must never die on a surprise.
        let future = r#"{"type":"assistant","requestId":"r","brandNewField":{"nested":true},"message":{"id":"m","model":"claude-opus-6","usage":{"output_tokens":7,"someNewCounter":123}}}"#;
        let record = Record::parse(future, &mut ()).expect("unknown fields must be ignored");
        assert_eq!(record.billable_usage().unwrap().output, 7);

        assert!(Record::parse("{not json", &mut ()).is_none());
        assert!(Record::parse("", &mut ()).is_none());
        assert!(
            Record::parse(r#"{"type":"assistant","message":{"id":"x"#, &mut ()).is_none(),
            "a half-written trailing line must be skipped, not fatal"
        );
    }
}
