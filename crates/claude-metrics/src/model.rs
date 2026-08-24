//! Folding raw model IDs into families.

use std::fmt;

/// A model family, folded from a raw model ID.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModelFamily {
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

impl ModelFamily {
    /// Fold a raw model ID such as `claude-opus-5[1m]` or `claude-haiku-4-5-20251001`.
    ///
    /// This is a **prefix** match on purpose. Model IDs are not stable in shape: real
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
            ModelFamily::Opus
        } else if id.contains("sonnet") {
            ModelFamily::Sonnet
        } else if id.contains("haiku") {
            ModelFamily::Haiku
        } else if id.contains("fable") {
            ModelFamily::Fable
        } else {
            ModelFamily::Other
        }
    }

    /// A short display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ModelFamily::Opus => "Opus",
            ModelFamily::Sonnet => "Sonnet",
            ModelFamily::Haiku => "Haiku",
            ModelFamily::Fable => "Fable",
            ModelFamily::Other => "Other",
        }
    }
}

impl fmt::Display for ModelFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::ModelFamily;

    #[test]
    fn undated_and_dated_ids_fold_to_the_same_family() {
        // Both of these are real IDs seen in transcripts on this machine. An exact-match
        // table would put one of them in `Other`.
        assert_eq!(ModelFamily::from_id("claude-sonnet-5"), ModelFamily::Sonnet);
        assert_eq!(
            ModelFamily::from_id("claude-haiku-4-5-20251001"),
            ModelFamily::Haiku
        );
    }

    #[test]
    fn suffixed_and_vendor_prefixed_ids_still_fold() {
        assert_eq!(
            ModelFamily::from_id("claude-opus-5[1m]"),
            ModelFamily::Opus,
            "a context-window suffix must not change the family"
        );
        assert_eq!(
            ModelFamily::from_id("us.anthropic.claude-opus-5-v1:0"),
            ModelFamily::Opus,
            "a Bedrock-style vendor prefix must not change the family"
        );
        assert_eq!(ModelFamily::from_id("claude-fable-5"), ModelFamily::Fable);
    }

    #[test]
    fn unknown_ids_fold_to_other_rather_than_failing() {
        assert_eq!(
            ModelFamily::from_id("some-future-model"),
            ModelFamily::Other
        );
        assert_eq!(ModelFamily::from_id(""), ModelFamily::Other);
    }
}
