//! Live coding-agent metrics, read via the `harness-metrics` crate.
//!
//! Every harness (Claude Code, Codex CLI, pi) is read through the exact same lagging,
//! tailed-transcript ledger -- there is no live-session/PID-registry machinery for any of
//! them, Claude included. A refresh re-reads only what each backend's transcripts have
//! appended since the last tick, so a steady-state tick is cheap.
//!
//! A single layout can mix widget instances with *different* `source`s (e.g. one
//! `agent_graph` reading `source = "claude"` next to another reading `source = "all"`), so
//! this collector always computes all four possible source views -- claude, codex, pi, and
//! the merged "all" -- every tick whenever any agent widget is on screen. That is cheap:
//! `HarnessLedger::refresh` is mtime-gated per file, so paying for four views costs one
//! extra `merge_all` call, not four tree walks.

use std::time::Duration;

use harness_metrics::{Bucket, Harness, HarnessLedger, TokenTotals, merge_all};

use crate::options::OptionError;

/// How far back the stats history reaches.
const HISTORY_WINDOW: Duration = Duration::from_secs(60 * 60);

/// How finely that window is divided.
///
/// A minute is deliberately far finer than Claude Code's own `/status`, which bars by day.
/// At this width an hour is sixty points, which is enough to see the shape of a working
/// session rather than a single flat bar.
const HISTORY_BUCKET: Duration = Duration::from_secs(60);

/// Which harness (or harnesses) an `agent_graph`/`agent_stats` widget instance reads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AgentSource {
    One(Harness),
    All,
}

impl AgentSource {
    /// The stable key used to namespace this source's time-series entries, matching
    /// [`AgentSourceData::key`].
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            AgentSource::One(Harness::Claude) => "claude",
            AgentSource::One(Harness::Codex) => "codex",
            AgentSource::One(Harness::Pi) => "pi",
            AgentSource::All => "all",
        }
    }

    /// A short display word for widget titles, e.g. `" {} Tokens "`.
    #[must_use]
    pub fn title_word(self) -> &'static str {
        match self {
            AgentSource::One(harness) => harness.label(),
            AgentSource::All => "All",
        }
    }

    /// The fixed, ordered set of series labels this source draws, used as both draw order
    /// and colour index.
    #[must_use]
    pub fn family_labels(self) -> Vec<&'static str> {
        match self {
            AgentSource::One(Harness::Claude) => harness_metrics::claude::ClaudeFamily::ALL
                .iter()
                .map(|f| f.label())
                .collect(),
            AgentSource::One(Harness::Codex) => harness_metrics::codex::CodexFamily::ALL
                .iter()
                .map(|f| f.label())
                .collect(),
            AgentSource::One(Harness::Pi) => harness_metrics::pi::PiFamily::ALL
                .iter()
                .map(|f| f.label())
                .collect(),
            AgentSource::All => Harness::ALL.iter().map(|h| h.label()).collect(),
        }
    }
}

impl Default for AgentSource {
    fn default() -> Self {
        AgentSource::One(Harness::Claude)
    }
}

impl std::str::FromStr for AgentSource {
    type Err = OptionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Ok(AgentSource::One(Harness::Claude)),
            "codex" => Ok(AgentSource::One(Harness::Codex)),
            "pi" => Ok(AgentSource::One(Harness::Pi)),
            "all" => Ok(AgentSource::All),
            other => Err(OptionError::config(format!(
                "'{other}' is not a valid agent source (expected claude, codex, pi, or all)"
            ))),
        }
    }
}

/// One source's worth of data for a tick: either a single harness or the "all" merge.
///
/// `key` is the stable string a widget's parsed `source` matches against ("claude",
/// "codex", "pi", "all") -- used to namespace time-series keys so two widget instances
/// with different `source`s never collide on a family label that happens to be spelled
/// the same way in two backends (e.g. every backend has an `Other` bucket).
#[derive(Clone, Debug, Default)]
pub struct AgentSourceData {
    pub key: &'static str,
    pub cumulative_totals: Vec<(&'static str, TokenTotals)>,
    pub buckets: Vec<Bucket>,
    pub families: Vec<&'static str>,
}

/// A snapshot of everything the agent widgets draw, one entry per possible `source`.
#[derive(Clone, Debug, Default)]
pub struct AgentData {
    pub sources: Vec<AgentSourceData>,
}

/// Owns each harness's reader and turns a refresh into an [`AgentData`] snapshot.
pub struct AgentCollector {
    claude: Option<HarnessLedger>,
    codex: Option<HarnessLedger>,
    pi: Option<HarnessLedger>,
}

impl std::fmt::Debug for AgentCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCollector")
            .field("claude", &self.claude.is_some())
            .field("codex", &self.codex.is_some())
            .field("pi", &self.pi.is_some())
            .finish()
    }
}

impl Default for AgentCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentCollector {
    /// Build a collector over each harness's default root.
    ///
    /// A harness with no derivable root (no `$HOME`, and for Codex, no `$CODEX_HOME`
    /// either) yields `None` for that harness and every harvest reports it empty. That is a
    /// normal state on a machine that has never run that harness, not an error.
    pub fn new() -> Self {
        Self {
            claude: HarnessLedger::new(Harness::Claude, HISTORY_WINDOW, HISTORY_BUCKET),
            codex: HarnessLedger::new(Harness::Codex, HISTORY_WINDOW, HISTORY_BUCKET),
            pi: HarnessLedger::new(Harness::Pi, HISTORY_WINDOW, HISTORY_BUCKET),
        }
    }

    /// Re-read and snapshot every possible source.
    ///
    /// `want_history` gates the windowed bucket scan -- the rate graph needs
    /// `cumulative_totals` regardless of whether a stats widget is on screen, but the
    /// bucketed history is only worth computing when something will draw it.
    pub fn harvest(&mut self, want_history: bool) -> AgentData {
        for ledger in [&mut self.claude, &mut self.codex, &mut self.pi]
            .into_iter()
            .flatten()
        {
            ledger.refresh();
        }

        let claude_data = source_data("claude", self.claude.as_ref(), want_history);
        let codex_data = source_data("codex", self.codex.as_ref(), want_history);
        let pi_data = source_data("pi", self.pi.as_ref(), want_history);

        let ledgers: Vec<&HarnessLedger> = [&self.claude, &self.codex, &self.pi]
            .into_iter()
            .flatten()
            .collect();
        let all_data = merged_source_data(&ledgers, want_history);

        AgentData {
            sources: vec![claude_data, codex_data, pi_data, all_data],
        }
    }
}

/// Build one harness's [`AgentSourceData`], or an empty one if that harness has no ledger.
fn source_data(
    key: &'static str, ledger: Option<&HarnessLedger>, want_history: bool,
) -> AgentSourceData {
    let Some(ledger) = ledger else {
        return AgentSourceData {
            key,
            ..AgentSourceData::default()
        };
    };

    let cumulative_totals = ledger.cumulative_totals();
    let (buckets, families) = if want_history {
        (ledger.buckets(), ledger.families_present())
    } else {
        (Vec::new(), Vec::new())
    };

    AgentSourceData {
        key,
        cumulative_totals,
        buckets,
        families,
    }
}

/// Build the merged "all harnesses" [`AgentSourceData`].
fn merged_source_data(ledgers: &[&HarnessLedger], want_history: bool) -> AgentSourceData {
    let merged = merge_all(ledgers);

    let (buckets, families) = if want_history {
        let order: Vec<&'static str> = Harness::ALL.iter().map(|h| h.label()).collect();
        let families: Vec<&'static str> = order
            .into_iter()
            .filter(|label| {
                merged
                    .buckets
                    .iter()
                    .any(|bucket| bucket.total_for(label) > 0)
            })
            .collect();

        (merged.buckets, families)
    } else {
        (Vec::new(), Vec::new())
    };

    AgentSourceData {
        key: "all",
        cumulative_totals: merged.cumulative_totals,
        buckets,
        families,
    }
}
