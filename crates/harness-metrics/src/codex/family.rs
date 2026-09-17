//! Folding raw Codex CLI model IDs into families.

use std::fmt;

/// A Codex model family, folded from a raw model ID.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CodexFamily {
    /// The GPT-5 line.
    Gpt5,
    /// The GPT-4 line.
    Gpt4,
    /// The reasoning `o1`/`o3`/`o4` line.
    OSeries,
    /// `codex-mini` and similar Codex-branded models.
    CodexMini,
    /// Anything that did not match a known family.
    Other,
}

impl CodexFamily {
    /// Fold a raw model ID such as `gpt-5.1-codex` or `o3-mini`.
    ///
    /// A substring match, same reasoning as Claude's `ClaudeFamily::from_id`: model IDs
    /// drift, and an exact-match table would silently drop every future ID into `Other`.
    ///
    /// The `o1`/`o3`/`o4` check is the one exception -- it matches a **standalone token**
    /// (the ID split on non-alphanumeric characters) rather than a substring, because a
    /// substring match on `"o1"` would false-positive inside unrelated IDs.
    #[must_use]
    pub fn from_id(id: &str) -> Self {
        let id = id.to_ascii_lowercase();

        if id.contains("gpt-5") {
            CodexFamily::Gpt5
        } else if id.contains("gpt-4") {
            CodexFamily::Gpt4
        } else if has_standalone_o_series_token(&id) {
            CodexFamily::OSeries
        } else if id.contains("codex") {
            CodexFamily::CodexMini
        } else {
            CodexFamily::Other
        }
    }

    /// Every family, in a fixed order.
    ///
    /// Callers use this as a draw order and as a colour index. Fixed rather than sorted by
    /// volume or by first appearance on purpose: a family that goes quiet and drops out
    /// must not repaint the ones that remain.
    pub const ALL: [Self; 5] = [
        Self::Gpt5,
        Self::Gpt4,
        Self::OSeries,
        Self::CodexMini,
        Self::Other,
    ];

    /// A short display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            CodexFamily::Gpt5 => "GPT-5",
            CodexFamily::Gpt4 => "GPT-4",
            CodexFamily::OSeries => "o-series",
            CodexFamily::CodexMini => "Codex Mini",
            CodexFamily::Other => "Other",
        }
    }
}

impl fmt::Display for CodexFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Whether `id` contains `o1`, `o3`, or `o4` as a whole token, split on any character that
/// is not ASCII alphanumeric. `"foo1bar"` and `"promo1"` must not match; `"o1-mini"` and
/// `"gpt-o3"` must.
fn has_standalone_o_series_token(id: &str) -> bool {
    id.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| matches!(token, "o1" | "o3" | "o4"))
}

#[cfg(test)]
mod tests {
    use super::CodexFamily;

    #[test]
    fn gpt5_and_gpt4_fold_by_substring() {
        assert_eq!(CodexFamily::from_id("gpt-5.1-codex"), CodexFamily::Gpt5);
        assert_eq!(CodexFamily::from_id("gpt-4o"), CodexFamily::Gpt4);
        assert_eq!(
            CodexFamily::from_id("GPT-5"),
            CodexFamily::Gpt5,
            "case-insensitive"
        );
    }

    #[test]
    fn gpt5_wins_over_codex_when_both_present() {
        // `gpt-5.1-codex` contains both "gpt-5" and "codex"; the more specific model line
        // must win, checked before the generic "codex" fallback.
        assert_eq!(CodexFamily::from_id("gpt-5.1-codex"), CodexFamily::Gpt5);
    }

    #[test]
    fn o_series_matches_a_standalone_token_only() {
        assert_eq!(CodexFamily::from_id("o1-mini"), CodexFamily::OSeries);
        assert_eq!(CodexFamily::from_id("o3"), CodexFamily::OSeries);
        assert_eq!(CodexFamily::from_id("gpt-o4"), CodexFamily::OSeries);

        assert_eq!(
            CodexFamily::from_id("foo1bar"),
            CodexFamily::Other,
            "o1 embedded inside another token must not match"
        );
        assert_eq!(
            CodexFamily::from_id("promo1"),
            CodexFamily::Other,
            "o1 as a suffix of another word must not match"
        );
    }

    #[test]
    fn codex_branded_models_fold_to_codex_mini() {
        assert_eq!(
            CodexFamily::from_id("codex-mini-latest"),
            CodexFamily::CodexMini
        );
    }

    #[test]
    fn unknown_ids_fold_to_other_rather_than_failing() {
        assert_eq!(
            CodexFamily::from_id("some-future-model"),
            CodexFamily::Other
        );
        assert_eq!(CodexFamily::from_id(""), CodexFamily::Other);
    }
}
