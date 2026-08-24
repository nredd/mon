//! Reads live Claude Code metrics off the local `~/.claude` tree.
//!
//! This crate has no dependency on `bottom`. It hands back plain data; rendering lives in
//! `src/canvas/widgets/`.
//!
//! Everything here parses defensively. The `~/.claude` schema is undocumented, it drifts
//! between releases, and a schema surprise must never take a widget down -- unknown fields
//! are ignored and missing fields fall back to a default.
//!
//! References:
//! - Session registry: `~/.claude/sessions/<PID>.json`
//! - Transcripts: `~/.claude/projects/<cwd-slug>/<sessionId>.jsonl`
//! - Subagents: `~/.claude/projects/<cwd-slug>/<sessionId>/subagents/agent-*.jsonl`
