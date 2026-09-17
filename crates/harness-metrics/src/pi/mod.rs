//! The pi backend: reads `~/.pi/agent/sessions/<slug>/*.jsonl`.

mod discover;
mod family;
mod transcript;

pub use family::PiFamily;
pub(crate) use transcript::Record;

pub(crate) use discover::discover;
