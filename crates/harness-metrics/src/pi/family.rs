//! Folding pi's `provider` field into families.

use std::fmt;

/// A pi provider family.
///
/// Unlike Claude and Codex, this is folded from the `provider` string pi writes directly on
/// every assistant message rather than from the model id -- pi already normalises which
/// vendor served a model, so there is no id-substring guessing to do.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PiFamily {
    /// Anthropic-served models.
    Anthropic,
    /// OpenAI-served models.
    OpenAi,
    /// Google-served models.
    Google,
    /// Anything that did not match a known provider.
    Other,
}

impl PiFamily {
    /// Fold a raw `provider` value such as `"anthropic"` or `"openai"`.
    #[must_use]
    pub fn from_provider(provider: &str) -> Self {
        let provider = provider.to_ascii_lowercase();

        if provider.contains("anthropic") {
            PiFamily::Anthropic
        } else if provider.contains("openai") {
            PiFamily::OpenAi
        } else if provider.contains("google") || provider.contains("gemini") {
            PiFamily::Google
        } else {
            PiFamily::Other
        }
    }

    /// Every family, in a fixed order.
    ///
    /// Callers use this as a draw order and as a colour index. Fixed rather than sorted by
    /// volume or by first appearance on purpose: a family that goes quiet and drops out
    /// must not repaint the ones that remain.
    pub const ALL: [Self; 4] = [Self::Anthropic, Self::OpenAi, Self::Google, Self::Other];

    /// A short display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PiFamily::Anthropic => "Anthropic",
            PiFamily::OpenAi => "OpenAI",
            PiFamily::Google => "Google",
            PiFamily::Other => "Other",
        }
    }
}

impl fmt::Display for PiFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::PiFamily;

    #[test]
    fn known_providers_fold_case_insensitively() {
        assert_eq!(PiFamily::from_provider("anthropic"), PiFamily::Anthropic);
        assert_eq!(PiFamily::from_provider("OpenAI"), PiFamily::OpenAi);
        assert_eq!(PiFamily::from_provider("google"), PiFamily::Google);
        assert_eq!(
            PiFamily::from_provider("GEMINI"),
            PiFamily::Google,
            "gemini folds to Google"
        );
    }

    #[test]
    fn unknown_providers_fold_to_other_rather_than_failing() {
        assert_eq!(
            PiFamily::from_provider("some-future-vendor"),
            PiFamily::Other
        );
        assert_eq!(PiFamily::from_provider(""), PiFamily::Other);
    }
}
