//! The Codex CLI backend: reads `~/.codex/sessions/**/rollout-*.jsonl` and
//! `~/.codex/archived_sessions/rollout-*.jsonl`.

mod discover;
mod family;
mod transcript;

pub use family::CodexFamily;
pub(crate) use transcript::Record;

pub(crate) use discover::discover;
