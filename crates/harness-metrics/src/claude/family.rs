//! Folding raw Claude model IDs into families.

use std::fmt;

/// A Claude model family, folded from a raw model ID.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClaudeFamily {
    /// Claude Opus.
    Opus,
    /// Claude Sonnet.
    Sonnet,
    /// Claude Haiku.
    Haiku,
    /// Claude Fable.
    Fable,
    /// Anything that did not match a known family.
    Other,
}

impl ClaudeFamily {
    /// Fold a raw model ID such as `claude-opus-5[1m]` or `claude-haiku-4-5-20251001`.
    ///
    /// This is a **substring** match on purpose. Model IDs are not stable in shape: real
    /// transcripts on this machine carry both `claude-sonnet-5` (undated) and
    /// `claude-haiku-4-5-20251001` (dated), and new IDs appear without warning. An
    /// exact-match table silently drops every future ID into `Other`, which is exactly the
    /// bug the `claude-gtop` prototype had.
    #[must_use]
    pub fn from_id(id: &str) -> Self {
        // Lowercase so a capitalised or vendor-prefixed ID (`us.anthropic.claude-opus-5`)
        // still folds correctly.
        let id = id.to_ascii_lowercase();

        // Match on the family segment anywhere in the ID rather than anchoring at the
        // start, so vendor-prefixed IDs work without a separate table.
        if id.contains("opus") {
            ClaudeFamily::Opus
        } else if id.contains("sonnet") {
            ClaudeFamily::Sonnet
        } else if id.contains("haiku") {
            ClaudeFamily::Haiku
        } else if id.contains("fable") {
            ClaudeFamily::Fable
        } else {
            ClaudeFamily::Other
        }
    }

    /// Every family, in a fixed order.
    ///
    /// Callers use this as a draw order and as a colour index. Fixed rather than sorted by
    /// volume or by first appearance on purpose: a family that goes quiet and drops out
    /// must not repaint the ones that remain.
    pub const ALL: [Self; 5] = [
        Self::Opus,
        Self::Sonnet,
        Self::Haiku,
        Self::Fable,
        Self::Other,
    ];

    /// A short display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ClaudeFamily::Opus => "Opus",
            ClaudeFamily::Sonnet => "Sonnet",
            ClaudeFamily::Haiku => "Haiku",
            ClaudeFamily::Fable => "Fable",
            ClaudeFamily::Other => "Other",
        }
    }
}

impl fmt::Display for ClaudeFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::ClaudeFamily;

    #[test]
    fn undated_and_dated_ids_fold_to_the_same_family() {
        // Both of these are real IDs seen in transcripts on this machine. An exact-match
        // table would put one of them in `Other`.
        assert_eq!(
            ClaudeFamily::from_id("claude-sonnet-5"),
            ClaudeFamily::Sonnet
        );
        assert_eq!(
            ClaudeFamily::from_id("claude-haiku-4-5-20251001"),
            ClaudeFamily::Haiku
        );
    }

    #[test]
    fn suffixed_and_vendor_prefixed_ids_still_fold() {
        assert_eq!(
            ClaudeFamily::from_id("claude-opus-5[1m]"),
            ClaudeFamily::Opus,
            "a context-window suffix must not change the family"
        );
        assert_eq!(
            ClaudeFamily::from_id("us.anthropic.claude-opus-5-v1:0"),
            ClaudeFamily::Opus,
            "a Bedrock-style vendor prefix must not change the family"
        );
        assert_eq!(ClaudeFamily::from_id("claude-fable-5"), ClaudeFamily::Fable);
    }

    #[test]
    fn unknown_ids_fold_to_other_rather_than_failing() {
        assert_eq!(
            ClaudeFamily::from_id("some-future-model"),
            ClaudeFamily::Other
        );
        assert_eq!(ClaudeFamily::from_id(""), ClaudeFamily::Other);
    }
}
