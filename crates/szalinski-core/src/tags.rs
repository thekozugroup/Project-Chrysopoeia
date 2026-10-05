//! Matroska statistics tags, as ffprobe shows them.
//!
//! mkvmerge and other Matroska tools store statistics with each track:
//! `BPS`, `DURATION`, `NUMBER_OF_FRAMES`, `NUMBER_OF_BYTES` and the
//! `_STATISTICS_WRITING_APP`, `_STATISTICS_WRITING_DATE_UTC` and
//! `_STATISTICS_TAGS` bookkeeping. A tag written with a language other than
//! "und" reads back with the language appended (`DURATION-eng`).
//!
//! Tools that copy a track without counting it again keep the old values.
//! ffmpeg is one of them: a clip it cuts from a film keeps the film's
//! `DURATION-eng` (2:21:02) next to the plain `DURATION` (1:01) its MKV
//! writer adds for the clip. So the plain tag always wins over a localized
//! one, the choice never depends on the order the tags are listed in, and
//! callers that can compare a tag with the container's own timing do so
//! before trusting it.

/// Statistics mkvmerge counts per track.
const COUNTS: [&str; 4] = ["BPS", "DURATION", "NUMBER_OF_FRAMES", "NUMBER_OF_BYTES"];

/// Prefix of the tags that say which program counted the statistics, when,
/// and which tags it wrote.
const BOOKKEEPING: &str = "_STATISTICS_";

/// A tag's name and, for a localized tag, its language (`DURATION-eng` is
/// `("DURATION", Some("eng"))`).
fn split_language(key: &str) -> (&str, Option<&str>) {
    match key.split_once('-') {
        Some((name, language)) => (name, Some(language)),
        None => (key, None),
    }
}

/// Whether a stream tag is a Matroska statistics tag, plain or localized:
/// `BPS`, `DURATION`, `NUMBER_OF_FRAMES`, `NUMBER_OF_BYTES` and any
/// `_STATISTICS_…` tag, in any letter case, with or without a language
/// (`DURATION-eng`, `_STATISTICS_WRITING_APP-ger`).
pub fn is_statistics_tag(key: &str) -> bool {
    let (name, _) = split_language(key.trim());
    COUNTS.iter().any(|count| count.eq_ignore_ascii_case(name))
        || name
            .get(..BOOKKEEPING.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(BOOKKEEPING))
}

/// A statistic read from a track's tags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Statistic<T> {
    pub value: T,
    /// It came from a localized tag (`DURATION-eng`) because the track has
    /// no usable plain one. ffmpeg never writes these, so after an ffmpeg
    /// cut or remux they may describe the track as it was before.
    pub localized: bool,
}

/// The statistic `name` (`"BPS"`, `"DURATION"`, …) from a track's tags: the
/// first value `parse` accepts among the plain tags, else among the
/// localized ones. Tags are taken in the order of their keys (then values),
/// so the answer is the same whatever order a map hands them out in.
pub fn statistic<'a, T>(
    tags: impl IntoIterator<Item = (&'a str, &'a str)>,
    name: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<Statistic<T>> {
    let mut plain: Vec<(&str, &str)> = Vec::new();
    let mut localized: Vec<(&str, &str)> = Vec::new();
    for (key, value) in tags {
        let (base, language) = split_language(key.trim());
        if !base.eq_ignore_ascii_case(name) {
            continue;
        }
        match language {
            None => plain.push((key, value)),
            Some(_) => localized.push((key, value)),
        }
    }
    plain.sort_unstable();
    localized.sort_unstable();
    let first = |list: &[(&str, &str)], localized: bool| {
        list.iter()
            .find_map(|(_, value)| parse(value.trim()))
            .map(|value| Statistic { value, localized })
    };
    first(&plain, false).or_else(|| first(&localized, true))
}

/// A track's length from its `DURATION` tags, in seconds (see
/// [`statistic`]: the plain tag wins over a localized one).
pub fn tagged_duration<'a>(
    tags: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<Statistic<f64>> {
    statistic(tags, "DURATION", parse_clock)
}

/// Parse a clock duration such as mkvmerge's `01:23:45.678000000` (also
/// `MM:SS.s` and plain seconds). Zero, negative and unreadable values give
/// `None`.
pub fn parse_clock(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let (seconds, larger_units) = parts.split_last()?;
    let mut total: f64 = seconds.trim().parse().ok()?;
    for (part, unit_secs) in larger_units.iter().rev().zip([60.0, 3600.0]) {
        let value: u64 = part.trim().parse().ok()?;
        total += value as f64 * unit_secs;
    }
    (total.is_finite() && total > 0.0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-6)
    }

    #[test]
    fn recognises_statistics_tags() {
        for key in [
            "BPS",
            "bps",
            "BPS-eng",
            "DURATION",
            "DURATION-eng",
            "duration-GER",
            "NUMBER_OF_FRAMES",
            "NUMBER_OF_FRAMES-fre",
            "NUMBER_OF_BYTES",
            "NUMBER_OF_BYTES-eng",
            "_STATISTICS_WRITING_APP",
            "_STATISTICS_WRITING_APP-eng",
            "_STATISTICS_WRITING_DATE_UTC-eng",
            "_statistics_tags",
            "_STATISTICS_TAGS-und",
        ] {
            assert!(is_statistics_tag(key), "{key}");
        }
        for key in [
            "title",
            "language",
            "BPSX",
            "DURATIONS",
            "ENCODER",
            "handler_name",
            "filename",
            "mimetype",
            "SOURCE_ID-eng",
            "_STATISTIC",
            "",
        ] {
            assert!(!is_statistics_tag(key), "{key}");
        }
    }

    /// The owner's clip: a plain `DURATION` written by ffmpeg for the clip
    /// next to the film's `DURATION-eng`. The plain tag wins, whichever
    /// comes first.
    #[test]
    fn the_plain_duration_wins_in_either_order() {
        let fresh = ("DURATION", "00:01:01.061000000");
        let stale = ("DURATION-eng", "02:21:02.000000000");
        for tags in [[fresh, stale], [stale, fresh]] {
            let found = tagged_duration(tags).unwrap();
            assert!(close(Some(found.value), 61.061), "{found:?}");
            assert!(!found.localized);
        }
    }

    #[test]
    fn a_localized_duration_is_used_only_without_a_plain_one() {
        let found = tagged_duration([("DURATION-eng", "00:00:42.500000000")]).unwrap();
        assert!(close(Some(found.value), 42.5));
        assert!(found.localized);

        // Several localized tags: the same one whatever the order.
        let a = ("DURATION-ger", "00:00:10.000000000");
        let b = ("DURATION-eng", "00:00:20.000000000");
        assert!(close(tagged_duration([a, b]).map(|s| s.value), 20.0));
        assert!(close(tagged_duration([b, a]).map(|s| s.value), 20.0));

        // An unreadable plain tag doesn't hide a readable localized one.
        let found = tagged_duration([("DURATION", "N/A"), ("DURATION-eng", "5")]).unwrap();
        assert!(close(Some(found.value), 5.0));
        assert!(found.localized);

        assert_eq!(tagged_duration([("title", "Film")]), None);
        assert_eq!(tagged_duration([("DURATIONS", "00:00:05")]), None);
    }

    #[test]
    fn statistics_prefer_the_plain_key() {
        let tags = [
            ("BPS-eng", "1"),
            ("bps", "2"),
            ("BPSX", "3"),
            ("DURATION-fre", "00:00:01"),
        ];
        let number = |s: &str| s.parse::<u64>().ok();
        assert_eq!(statistic(tags, "BPS", number).map(|s| s.value), Some(2));
        assert_eq!(
            statistic(tags, "NUMBER_OF_FRAMES", number).map(|s| s.value),
            None
        );
        assert!(close(tagged_duration(tags).map(|s| s.value), 1.0));
    }

    #[test]
    fn clock_durations() {
        assert!(close(parse_clock("01:23:45.678000000"), 5025.678));
        assert!(close(parse_clock("02:21:02.000000000"), 8462.0));
        assert!(close(parse_clock("00:00:02.000000000"), 2.0));
        assert!(close(parse_clock("2:03.5"), 123.5));
        assert!(close(parse_clock("42.1"), 42.1));
        assert!(close(parse_clock(" 00:00:01.5 "), 1.5));
        assert_eq!(parse_clock("00:00:00.000000000"), None);
        assert_eq!(parse_clock("-00:00:05"), None);
        assert_eq!(parse_clock("garbage"), None);
        assert_eq!(parse_clock("inf"), None);
        assert_eq!(parse_clock("1:2:3:4:5"), None);
        assert_eq!(parse_clock(""), None);
    }
}
