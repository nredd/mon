//! Parsing transcript records and folding them into token totals.
//!
//! Everything here is deliberately permissive. The `~/.claude` schema is undocumented and
//! drifts between releases, so every field is optional, unknown fields are ignored, and a
//! line that will not parse is skipped rather than failing the whole read.

use std::collections::HashMap;

use serde::Deserialize;

use crate::model::ModelFamily;

/// Token counts for one model family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenTotals {
    /// Fresh input tokens.
    pub input: u64,
    /// Generated tokens.
    pub output: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
    /// Tokens written to the prompt cache.
    pub cache_creation: u64,
}

impl TokenTotals {
    /// Every token this family was billed for, cache included.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }

    /// Add the per-request fields. These repeat identically across a message's records, so
    /// they are contributed exactly once, the first time the message is seen.
    fn add_request_fields(&mut self, usage: &Usage) {
        self.input = self.input.saturating_add(usage.input);
        self.cache_read = self.cache_read.saturating_add(usage.cache_read);
        // NOTE(redd): the message-level `cache_creation_input_tokens` alone, never plus the
        // `cache_creation.ephemeral_*` buckets. Verified against a real record: the former
        // is exactly the sum of the latter, so adding both double-counts every cache write.
        self.cache_creation = self.cache_creation.saturating_add(usage.cache_creation);
    }

    /// Add newly-generated output tokens.
    fn add_output(&mut self, delta: u64) {
        self.output = self.output.saturating_add(delta);
    }
}

/// The `message.usage` object.
///
/// `iterations` is deliberately absent: it restates the same counts as the message-level
/// fields, so reading it would double-count.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Usage {
    #[serde(rename = "input_tokens")]
    pub(crate) input: u64,
    #[serde(rename = "output_tokens")]
    pub(crate) output: u64,
    #[serde(rename = "cache_read_input_tokens")]
    pub(crate) cache_read: u64,
    #[serde(rename = "cache_creation_input_tokens")]
    pub(crate) cache_creation: u64,
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
/// `usage` empty and get skipped. That is cheaper and far more drift-tolerant than an
/// enum over `type`, which would have to grow an arm for every new record kind.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Record {
    /// Record kind: `assistant`, `user`, `system`, `attachment`, and others.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Present on `system` records, e.g. `turn_duration`.
    pub subtype: Option<String>,
    /// Wall-clock duration of a turn, on `system` / `turn_duration` records.
    pub duration_ms: Option<u64>,
    /// The API request this record came from. Half of the dedupe key.
    pub request_id: Option<String>,
    /// The session this record belongs to.
    pub session_id: Option<String>,
    /// True on subagent records. Subagents carry the *parent* session's id.
    pub is_sidechain: bool,
    /// Set when the record is an API error rather than a real response.
    pub is_api_error_message: bool,
    /// ISO-8601 UTC instant the record was written, e.g. `2026-08-24T20:26:13.919Z`.
    ///
    /// Present on every billable record in practice, but optional like everything else
    /// here -- the schema is undocumented and drifts between releases.
    pub timestamp: Option<String>,
    message: Option<Message>,
}

impl Record {
    /// Parse one JSONL line, returning `None` if it is not usable.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        // A malformed or truncated line is skipped, never fatal -- a tailer can legitimately
        // read a half-written line at the end of a live file.
        serde_json::from_str(line).ok()
    }

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

    pub(crate) fn model_id(&self) -> Option<&str> {
        self.message.as_ref()?.model.as_deref()
    }

    /// This record's usage, but only if it should count toward totals at all.
    ///
    /// Bundling the billability check with the lookup keeps the two from drifting apart:
    /// every caller that reads usage needs exactly the same filter applied first.
    pub(crate) fn billable_usage(&self) -> Option<&Usage> {
        if !self.is_billable() {
            return None;
        }

        self.message.as_ref()?.usage.as_ref()
    }

    /// The record's instant, in Unix epoch milliseconds.
    ///
    /// Hand-parsed rather than pulled in through a date library. The format is fixed and
    /// always UTC -- `YYYY-MM-DDTHH:MM:SS.sssZ` -- so this is a few field reads and a
    /// days-since-epoch calculation, and it keeps a crate whose whole point is having no
    /// dependency on bottom from growing one on `chrono` for a single field.
    #[must_use]
    pub fn timestamp_ms(&self) -> Option<u64> {
        parse_iso8601_ms(self.timestamp.as_deref()?)
    }

    /// The dedupe key: request id plus message id.
    ///
    /// Retries and resumed sessions replay identical messages, so counting by line would
    /// inflate every total.
    pub(crate) fn dedupe_key(&self) -> Option<String> {
        let message = self.message.as_ref()?;
        let request = self.request_id.as_deref().unwrap_or("");
        let id = message.id.as_deref()?;
        Some(format!("{request}\u{0}{id}"))
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.sss]Z` into Unix epoch milliseconds.
///
/// Only the shape Claude Code actually writes is accepted: fixed-width fields, always UTC.
/// Anything else returns `None` and the caller drops the record rather than guessing a
/// time for it. Offsets other than `Z` are deliberately unsupported -- silently treating
/// `+02:00` as UTC would put records in the wrong bucket, which is worse than losing them.
fn parse_iso8601_ms(stamp: &str) -> Option<u64> {
    let bytes = stamp.as_bytes();

    // The shortest accepted form is `YYYY-MM-DDTHH:MM:SSZ`.
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }

    if bytes[13] != b':' || bytes[16] != b':' || !stamp.ends_with('Z') {
        return None;
    }

    let field = |from: usize, to: usize| stamp.get(from..to)?.parse::<u64>().ok();

    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);

    // The fractional part is optional and of unspecified length; take up to three digits
    // and pad, so both `.9` and `.919` mean what they say.
    let millis = match bytes.get(19) {
        Some(b'.') => {
            let digits: String = stamp[20..]
                .chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();

            // At most three digits were taken, so this cannot underflow.
            let missing = 3 - u32::try_from(digits.len()).ok()?;
            digits.parse::<u64>().ok()? * 10u64.pow(missing)
        }
        _ => 0,
    };

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day)?;

    Some(
        days.saturating_mul(86_400_000)
            .saturating_add(hour * 3_600_000)
            .saturating_add(minute * 60_000)
            .saturating_add(second * 1_000)
            .saturating_add(millis),
    )
}

/// Days from 1970-01-01 to a civil date, by Howard Hinnant's `days_from_civil`.
///
/// The shift-the-year-to-March trick is what makes leap days fall out of the arithmetic
/// instead of needing a special case: with March as month zero, the leap day lands at the
/// end of the year where it cannot disturb the month-length series.
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    // Before the epoch there is nothing to attribute, and the unsigned arithmetic below
    // would wrap rather than go negative.
    if year < 1970 {
        return None;
    }

    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;

    let month_shift = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shift + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    // 719468 is the day count from 0000-03-01 to 1970-01-01.
    Some(era * 146_097 + day_of_era - 719_468)
}

/// Accumulates token totals across records, skipping duplicates.
#[derive(Clone, Debug, Default)]
pub struct UsageAccumulator {
    /// Dedupe key, mapped to the model family plus the highest `output_tokens` counted
    /// for that message so far.
    seen: HashMap<String, (ModelFamily, u64)>,
    totals: Vec<(ModelFamily, TokenTotals)>,
    /// Number of subagent (`isSidechain`) messages counted.
    pub sidechain_messages: u64,
    /// Number of non-subagent messages counted.
    pub main_messages: u64,
    /// Most recent turn duration seen, in milliseconds.
    pub last_turn_duration_ms: Option<u64>,
}

impl UsageAccumulator {
    /// Fold one record in. Returns true if it contributed new usage.
    pub fn ingest(&mut self, record: &Record) -> bool {
        if record.kind.as_deref() == Some("system")
            && record.subtype.as_deref() == Some("turn_duration")
            && let Some(duration) = record.duration_ms
        {
            self.last_turn_duration_ms = Some(duration);
        }

        if !record.is_billable() {
            return false;
        }

        let Some(key) = record.dedupe_key() else {
            return false;
        };

        let Some(message) = record.message.as_ref() else {
            return false;
        };
        let Some(usage) = message.usage.as_ref() else {
            return false;
        };

        let family = ModelFamily::from_id(message.model.as_deref().unwrap_or_default());

        // A message is written as one record per content block. Verified against real
        // transcripts: `input_tokens`, `cache_read_input_tokens`, and
        // `cache_creation_input_tokens` are per-request and repeat identically across those
        // records, but `output_tokens` is a **running total** that grows with each block.
        // So the request-level fields are taken once and output is tracked as a high-water
        // mark.
        //
        // Taking the first record's output instead undercounts badly -- on one real session
        // that turned 18365 Opus output tokens into 1090.
        if let Some((seen_family, counted_output)) = self.seen.get_mut(&key) {
            if usage.output <= *counted_output {
                return false;
            }

            let delta = usage.output - *counted_output;
            *counted_output = usage.output;
            let seen_family = *seen_family;

            self.totals_for_mut(seen_family).add_output(delta);
            return true;
        }

        self.seen.insert(key, (family, usage.output));

        let totals = self.totals_for_mut(family);
        totals.add_request_fields(usage);
        totals.add_output(usage.output);

        if record.is_sidechain {
            self.sidechain_messages = self.sidechain_messages.saturating_add(1);
        } else {
            self.main_messages = self.main_messages.saturating_add(1);
        }

        true
    }

    /// Mutable totals for one family, inserting an empty entry if it is new.
    fn totals_for_mut(&mut self, family: ModelFamily) -> &mut TokenTotals {
        if let Some(index) = self.totals.iter().position(|(f, _)| *f == family) {
            return &mut self.totals[index].1;
        }

        self.totals.push((family, TokenTotals::default()));
        let last = self.totals.len() - 1;
        &mut self.totals[last].1
    }

    /// Per-family totals, ordered by family.
    #[must_use]
    pub fn totals(&self) -> Vec<(ModelFamily, TokenTotals)> {
        let mut totals = self.totals.clone();
        totals.sort_unstable_by_key(|(family, _)| *family);
        totals
    }

    /// Totals summed across every family.
    #[must_use]
    pub fn grand_total(&self) -> TokenTotals {
        self.totals
            .iter()
            .fold(TokenTotals::default(), |mut acc, (_, t)| {
                acc.input = acc.input.saturating_add(t.input);
                acc.output = acc.output.saturating_add(t.output);
                acc.cache_read = acc.cache_read.saturating_add(t.cache_read);
                acc.cache_creation = acc.cache_creation.saturating_add(t.cache_creation);
                acc
            })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_real_timestamp_parses_to_epoch_millis() {
        // Taken verbatim from a live transcript.
        let record = Record::parse(
            r#"{"type":"assistant","timestamp":"2026-08-24T20:26:13.919Z","message":{"id":"m","model":"claude-opus-5"}}"#,
        )
        .expect("fixture parses");

        assert_eq!(record.timestamp_ms(), Some(1_787_603_173_919));
    }

    #[test]
    fn the_epoch_itself_is_zero() {
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
    }

    #[test]
    fn leap_days_are_counted() {
        // The whole reason for the shift-the-year-to-March trick. A day either side of the
        // 2024 leap day must be exactly 86400000ms apart across it.
        let before = parse_iso8601_ms("2024-02-28T00:00:00Z").expect("valid");
        let leap = parse_iso8601_ms("2024-02-29T00:00:00Z").expect("valid");
        let after = parse_iso8601_ms("2024-03-01T00:00:00Z").expect("valid");

        assert_eq!(leap - before, 86_400_000);
        assert_eq!(after - leap, 86_400_000);
    }

    #[test]
    fn a_century_that_is_not_a_leap_year_is_handled() {
        // 1900 is not a leap year but 2000 is, which is the case a naive `% 4` gets wrong.
        let before = parse_iso8601_ms("2000-02-28T00:00:00Z").expect("valid");
        let after = parse_iso8601_ms("2000-03-01T00:00:00Z").expect("valid");

        assert_eq!(after - before, 2 * 86_400_000, "2000 had a leap day");
    }

    #[test]
    fn a_short_fraction_is_padded_rather_than_misread() {
        // `.9` is nine hundred milliseconds, not nine.
        assert_eq!(
            parse_iso8601_ms("2026-08-24T20:26:13.9Z"),
            parse_iso8601_ms("2026-08-24T20:26:13.900Z")
        );
    }

    #[test]
    fn a_missing_fraction_is_fine() {
        assert_eq!(
            parse_iso8601_ms("2026-08-24T20:26:13Z"),
            parse_iso8601_ms("2026-08-24T20:26:13.000Z")
        );
    }

    #[test]
    fn a_non_utc_offset_is_refused_rather_than_silently_misread() {
        // Treating `+02:00` as UTC would put records two hours into the wrong bucket, which
        // is worse than losing them.
        assert_eq!(parse_iso8601_ms("2026-08-24T20:26:13.919+02:00"), None);
    }

    #[test]
    fn junk_timestamps_yield_nothing_rather_than_panicking() {
        for stamp in [
            "",
            "not a date",
            "2026-08-24",
            "2026-13-01T00:00:00Z",
            "2026-08-24T25:00:00Z",
            "1969-12-31T23:59:59Z",
        ] {
            assert_eq!(parse_iso8601_ms(stamp), None, "{stamp:?} must not parse");
        }
    }
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.

    /// Build one record of a streaming assistant message.
    ///
    /// This mirrors what real transcripts contain: a message is written as one record per
    /// content block, all sharing `requestId` + `message.id`. The per-request fields repeat
    /// identically; `output_tokens` is a **running total** that grows with each block.
    fn block(
        request: &str, message: &str, model: &str, block: &str, running_output: u64,
    ) -> String {
        format!(
            r#"{{"type":"assistant","requestId":"{request}","sessionId":"s1","isSidechain":false,"message":{{"id":"{message}","model":"{model}","content":[{{"type":"{block}"}}],"usage":{{"input_tokens":10,"output_tokens":{running_output},"cache_read_input_tokens":30,"cache_creation_input_tokens":40,"cache_creation":{{"ephemeral_1h_input_tokens":40,"ephemeral_5m_input_tokens":0}},"iterations":[{{"input_tokens":10,"output_tokens":{running_output},"cache_read_input_tokens":30,"cache_creation_input_tokens":40}}]}}}}}}"#
        )
    }

    /// The three records of one streamed message: thinking, then text, then a tool call.
    fn multi_block() -> Vec<String> {
        vec![
            block("req_1", "msg_1", "claude-sonnet-5", "thinking", 3),
            block("req_1", "msg_1", "claude-sonnet-5", "text", 12),
            block("req_1", "msg_1", "claude-sonnet-5", "tool_use", 20),
        ]
    }

    #[test]
    fn a_multi_content_block_message_counts_once() {
        // The load-bearing assertion. One message, three content blocks, three records.
        let mut acc = UsageAccumulator::default();
        for line in multi_block() {
            acc.ingest(&Record::parse(&line).expect("record must parse"));
        }

        let totals = acc.grand_total();
        assert_eq!(
            totals.input, 10,
            "per-request fields repeat across blocks and must be counted once"
        );
        assert_eq!(totals.cache_read, 30);
        assert_eq!(
            totals.cache_creation, 40,
            "cache_creation must come from the message-level field alone -- adding the \
             ephemeral_* buckets on top double-counts, they sum to the same number"
        );
        assert_eq!(acc.main_messages, 1, "three records, but one message");
    }

    #[test]
    fn output_tokens_are_a_running_total_not_a_per_block_amount() {
        // Verified against real transcripts: `output_tokens` grows with each content block
        // and the last record carries the final figure. Counting the first record alone
        // undercounts badly -- on one real session that turned 18365 tokens into 1090.
        // Summing every record instead over-counts, here 3 + 12 + 20 = 35.
        let mut acc = UsageAccumulator::default();
        for line in multi_block() {
            acc.ingest(&Record::parse(&line).unwrap());
        }

        assert_eq!(acc.grand_total().output, 20);
    }

    #[test]
    fn a_replayed_message_is_not_counted_twice() {
        // Retries and resumed sessions replay identical lines.
        let lines = multi_block();
        let mut acc = UsageAccumulator::default();

        for line in &lines {
            acc.ingest(&Record::parse(line).unwrap());
        }
        for line in &lines {
            assert!(
                !acc.ingest(&Record::parse(line).unwrap()),
                "a replay adds no new output, so nothing must be contributed"
            );
        }

        let totals = acc.grand_total();
        assert_eq!(totals.output, 20);
        assert_eq!(totals.input, 10);
        assert_eq!(acc.main_messages, 1);
    }

    #[test]
    fn iterations_are_ignored_rather_than_summed() {
        // `iterations[]` restates the message-level counts. If it were read, output doubles.
        let mut acc = UsageAccumulator::default();
        for line in multi_block() {
            acc.ingest(&Record::parse(&line).unwrap());
        }
        assert_eq!(acc.grand_total().output, 20);
    }

    #[test]
    fn synthetic_and_api_error_records_are_filtered() {
        let synthetic = r#"{"type":"assistant","requestId":"r","message":{"id":"m1","model":"<synthetic>","usage":{"output_tokens":99}}}"#;
        let api_error = r#"{"type":"assistant","isApiErrorMessage":true,"requestId":"r","message":{"id":"m2","model":"claude-opus-5","usage":{"output_tokens":99}}}"#;
        let user = r#"{"type":"user","message":{"id":"m3","usage":{"output_tokens":99}}}"#;

        let mut acc = UsageAccumulator::default();
        for line in [synthetic, api_error, user] {
            acc.ingest(&Record::parse(line).unwrap());
        }

        assert_eq!(acc.grand_total().total(), 0);
    }

    #[test]
    fn sidechain_records_are_counted_separately() {
        let sub = r#"{"type":"assistant","requestId":"r2","sessionId":"s1","isSidechain":true,"message":{"id":"m9","model":"claude-haiku-4-5-20251001","usage":{"output_tokens":5}}}"#;

        let mut acc = UsageAccumulator::default();
        for line in multi_block() {
            acc.ingest(&Record::parse(&line).unwrap());
        }
        acc.ingest(&Record::parse(sub).unwrap());

        assert_eq!(acc.main_messages, 1);
        assert_eq!(acc.sidechain_messages, 1);

        let totals = acc.totals();
        assert_eq!(totals.len(), 2, "each family gets its own bucket");
    }

    #[test]
    fn turn_duration_is_picked_up_from_system_records() {
        let line =
            r#"{"type":"system","subtype":"turn_duration","durationMs":5783500,"sessionId":"s1"}"#;
        let mut acc = UsageAccumulator::default();
        acc.ingest(&Record::parse(line).unwrap());
        assert_eq!(acc.last_turn_duration_ms, Some(5_783_500));
    }

    #[test]
    fn unknown_fields_and_junk_lines_do_not_fail() {
        // The whole point: `~/.claude` drifts, and a widget must never die on a surprise.
        let future = r#"{"type":"assistant","requestId":"r","brandNewField":{"nested":true},"message":{"id":"m","model":"claude-opus-6","usage":{"output_tokens":7,"someNewCounter":123}}}"#;
        let record = Record::parse(future).expect("unknown fields must be ignored");

        let mut acc = UsageAccumulator::default();
        assert!(acc.ingest(&record));
        assert_eq!(acc.grand_total().output, 7);

        assert!(Record::parse("{not json").is_none());
        assert!(Record::parse("").is_none());
        assert!(
            Record::parse(r#"{"type":"assistant","message":{"id":"x"#).is_none(),
            "a half-written trailing line must be skipped, not fatal"
        );
    }
}
