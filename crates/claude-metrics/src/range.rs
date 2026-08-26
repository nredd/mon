//! Selectable time ranges for the token-history graph.
//!
//! Each range pairs a window with a bucket width. The pairing is not free choice: the
//! number of buckets a range yields is what the graph actually plots, and a terminal has
//! on the order of a couple of hundred usable columns. So every range here lands between
//! sixty and a hundred and eighty buckets, whatever its span. Holding the bucket width
//! fixed instead -- a minute, say -- would give a thirty-minute view thirty points and a
//! thirty-day view forty-three thousand, and the second of those is forty thousand points
//! of allocation per tick for a graph that can draw two hundred.

use std::{fmt, str::FromStr, time::Duration};

/// How far back the token-history graph reaches, and how finely it is divided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StatsRange {
    /// Half an hour, in thirty-second buckets.
    ThirtyMinutes,
    /// Two hours, in two-minute buckets.
    #[default]
    TwoHours,
    /// Eight hours, in five-minute buckets.
    EightHours,
    /// A day, in fifteen-minute buckets.
    OneDay,
    /// A week, in hourly buckets.
    SevenDays,
    /// Thirty days, in six-hour buckets.
    ThirtyDays,
}

impl StatsRange {
    /// Every range, shortest first. This is also the cycle order and the order the
    /// selector row is drawn in.
    pub const ALL: [Self; 6] = [
        Self::ThirtyMinutes,
        Self::TwoHours,
        Self::EightHours,
        Self::OneDay,
        Self::SevenDays,
        Self::ThirtyDays,
    ];

    /// How far back this range reaches.
    #[must_use]
    pub const fn window(self) -> Duration {
        match self {
            Self::ThirtyMinutes => Duration::from_mins(30),
            Self::TwoHours => Duration::from_hours(2),
            Self::EightHours => Duration::from_hours(8),
            Self::OneDay => Duration::from_hours(24),
            Self::SevenDays => Duration::from_hours(7 * 24),
            Self::ThirtyDays => Duration::from_hours(30 * 24),
        }
    }

    /// How finely that window is divided.
    #[must_use]
    pub const fn bucket(self) -> Duration {
        match self {
            Self::ThirtyMinutes => Duration::from_secs(30),
            Self::TwoHours => Duration::from_mins(2),
            Self::EightHours => Duration::from_mins(5),
            Self::OneDay => Duration::from_mins(15),
            Self::SevenDays => Duration::from_hours(1),
            Self::ThirtyDays => Duration::from_hours(6),
        }
    }

    /// The short form drawn in the selector row and accepted in config.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ThirtyMinutes => "30m",
            Self::TwoHours => "2h",
            Self::EightHours => "8h",
            Self::OneDay => "24h",
            Self::SevenDays => "7d",
            Self::ThirtyDays => "30d",
        }
    }

    /// Whether this range wants dates rather than clock times on the x-axis.
    ///
    /// The cutover is at a day: below it two labels an hour apart are the same date and
    /// only the time distinguishes them, above it the reverse.
    #[must_use]
    pub const fn spans_days(self) -> bool {
        matches!(self, Self::SevenDays | Self::ThirtyDays)
    }

    /// The next range, wrapping back to the shortest.
    ///
    /// Wrapping is right for a dedicated cycle key: it is the only way to reach every range
    /// without also needing a "go back" key.
    #[must_use]
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// The next range down, stopping at the shortest.
    ///
    /// Clamping rather than wrapping, because this is what the zoom-in key does and zoom
    /// has always clamped -- holding `+` should settle at the finest view, not loop round
    /// to a month.
    #[must_use]
    pub fn shorter(self) -> Self {
        Self::ALL[self.index().saturating_sub(1)]
    }

    /// The next range up, stopping at the widest.
    #[must_use]
    pub fn longer(self) -> Self {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)
    }

    /// The widest range, which is what the history has to retain to serve all of them.
    #[must_use]
    pub fn widest() -> Self {
        Self::ThirtyDays
    }
}

impl fmt::Display for StatsRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for StatsRange {
    type Err = UnknownRange;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim().to_ascii_lowercase();

        Self::ALL
            .into_iter()
            .find(|range| range.label() == trimmed)
            .ok_or(UnknownRange)
    }
}

/// The string did not name a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownRange;

impl fmt::Display for UnknownRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expected one of: ")?;

        for (index, range) in StatsRange::ALL.into_iter().enumerate() {
            if index > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{range}")?;
        }

        Ok(())
    }
}

impl std::error::Error for UnknownRange {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_range_plots_a_drawable_number_of_buckets() {
        // The whole reason window and bucket are paired rather than chosen independently.
        // A terminal graph has a couple of hundred columns; a range that yielded tens of
        // thousands of buckets would allocate them all every tick to draw two hundred.
        for range in StatsRange::ALL {
            let buckets = range.window().as_secs() / range.bucket().as_secs();

            assert!(
                (60..=180).contains(&buckets),
                "{range} yields {buckets} buckets"
            );
        }
    }

    #[test]
    fn a_bucket_divides_its_window_exactly() {
        // A remainder would leave the oldest slot covering less time than the rest, which
        // draws as a short bar that means nothing.
        for range in StatsRange::ALL {
            assert_eq!(
                range.window().as_secs() % range.bucket().as_secs(),
                0,
                "{range} has a ragged oldest bucket"
            );
        }
    }

    #[test]
    fn cycling_visits_every_range_and_wraps() {
        let mut range = StatsRange::ALL[0];
        let mut seen = vec![range];

        for _ in 1..StatsRange::ALL.len() {
            range = range.next();
            seen.push(range);
        }

        assert_eq!(seen, StatsRange::ALL.to_vec());
        assert_eq!(range.next(), StatsRange::ALL[0]);
    }

    #[test]
    fn zooming_clamps_where_cycling_wraps() {
        // Holding the zoom key should settle at an end rather than loop round to the other
        // one; the dedicated cycle key is the one that has to reach everything.
        let shortest = StatsRange::ALL[0];
        let longest = StatsRange::ALL[StatsRange::ALL.len() - 1];

        assert_eq!(shortest.shorter(), shortest);
        assert_eq!(longest.longer(), longest);
        assert_eq!(longest.next(), shortest);

        assert_eq!(shortest.longer(), StatsRange::ALL[1]);
        assert_eq!(StatsRange::ALL[1].shorter(), shortest);
    }

    #[test]
    fn labels_round_trip_through_parsing() {
        for range in StatsRange::ALL {
            assert_eq!(range.label().parse(), Ok(range));
        }

        assert_eq!(" 24H ".parse(), Ok(StatsRange::OneDay));
        assert_eq!("1y".parse::<StatsRange>(), Err(UnknownRange));
    }

    #[test]
    fn the_widest_range_really_is_the_widest() {
        // The history retains this much; anything wider would draw dead space.
        let widest = StatsRange::widest();

        for range in StatsRange::ALL {
            assert!(range.window() <= widest.window());
        }
    }
}
