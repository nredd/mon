//! The Claude Code backend: reads `~/.claude/projects/**/*.jsonl` transcripts, main
//! sessions and their subagents alike.

mod discover;
mod family;
mod transcript;

pub use family::ClaudeFamily;
pub(crate) use transcript::Record;

pub(crate) use discover::discover;
