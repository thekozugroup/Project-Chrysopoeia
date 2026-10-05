//! Where a file really ends, from ffprobe's list of its packets.
//!
//! The lengths a Matroska file states are not always its length:
//!
//! - its `DURATION` tags may be left over from before a cut (see
//!   [`crate::tags`]);
//! - a file written as a live stream (ffmpeg's `-live 1`, a recorder that
//!   never went back to fill it in) states no length of its own, so its
//!   tags are all it has;
//! - ffmpeg's Matroska writer states where the file *ends* (its own length
//!   and every `DURATION` tag), which is its length only when its
//!   timestamps start at zero: a one-minute clip whose timestamps start at
//!   10:00 states 11:01. Written through a pipe, the same clip states 1:01.
//!
//! When the stated lengths can't be trusted, ffprobe lists the packets near
//! the end of the file (`-read_intervals <from>% -show_entries packet=…`,
//! no decoding; when the file can't be read from near its end, all of its
//! packets), and the length is where the last one ends, counted from where
//! the file starts. A seek past the end lands on the last keyframe (with
//! Matroska Cues, and without them by reading through the file), so the
//! point to read from is worked out from the longest the file could be.

use std::collections::BTreeMap;

/// The ffprobe `-show_entries` value that lists each packet's stream, time
/// and duration; with `-of compact=p=0` one packet per line, read by
/// [`PacketEnds::push_line`].
pub const PACKET_ENTRIES: &str = "packet=stream_index,pts_time,dts_time,duration_time";

/// Seconds of a file read from its end to see where its streams stop.
/// Reading starts at the keyframe before, so a little more is read.
pub const TAIL_SECS: f64 = 10.0;

/// A file whose timestamps start this close to zero counts as starting at
/// zero (B-frame delays, audio priming): what it states is its length,
/// whether its writer meant an end or a length.
pub const START_SLACK_SECS: f64 = 0.5;

/// What ffprobe says, among its messages, when a file stops in the middle
/// of a packet or element (lower case).
const CUT_OFF_MESSAGES: &[&str] = &[
    "file ended prematurely",
    "unexpected end of file",
    "premature end",
    "partial file",
    "truncating packet",
];

/// Where a file's timestamps start, in seconds: its container start time
/// (ffprobe's `format.start_time`) when that is past [`START_SLACK_SECS`],
/// else zero.
pub fn start_offset(start_time: Option<f64>) -> f64 {
    start_time
        .filter(|s| s.is_finite() && *s > START_SLACK_SECS)
        .unwrap_or(0.0)
}

/// The time `at` (one of the file's timestamps) counted from its `start`
/// (see [`start_offset`]), to the microsecond ffprobe prints (so 620.038
/// from 600 is 20.038, not 20.03800000000001).
pub fn since_start(at: f64, start: f64) -> f64 {
    ((at - start) * 1e6).round() / 1e6
}

/// A length the file states (its container's or a tag's), as a length from
/// the file's `start` (see [`start_offset`]). Past the start it is read as
/// the end time ffmpeg's Matroska writer states; one smaller than the start
/// can only be a length.
pub fn stated_length(stated: f64, start: f64) -> f64 {
    if start > 0.0 && stated >= start {
        since_start(stated, start)
    } else {
        stated
    }
}

/// Where to start listing packets so the listing covers the last
/// [`TAIL_SECS`] of a file that starts at `start` (see [`start_offset`]):
/// from the container's stated length when it has one, else the longest of
/// `tags` (the stream lengths its tags state). Each is taken as a length
/// from the start, the longest it could mean; a point past the real end
/// lands on the last keyframe. `None` when nothing gives a length.
pub fn tail_start(start: f64, container: Option<f64>, tags: &[f64]) -> Option<f64> {
    let positive = |d: &f64| d.is_finite() && *d > 0.0;
    let stated = container
        .filter(positive)
        .or_else(|| tags.iter().copied().filter(positive).reduce(f64::max))?;
    Some((start + stated - TAIL_SECS).max(0.0))
}

/// Whether an ffprobe message says the file stops in the middle of a
/// packet or element (a cut-off download or copy).
pub fn is_cut_off_message(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    CUT_OFF_MESSAGES.iter().any(|m| lower.contains(m))
}

/// Where each stream's last packet ends, from a packet listing (see
/// [`PACKET_ENTRIES`]). Times are the file's own timestamps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PacketEnds {
    /// The earliest time among the packets listed: where the listing
    /// really began (a seek lands on the keyframe before the point asked
    /// for, or on the last one when asked past the end). A stream with no
    /// packet listed ended before this.
    pub first: Option<f64>,
    /// Stream index → where its last listed packet ends (presentation
    /// time, else decoding time, plus duration).
    pub ends: BTreeMap<u32, f64>,
    /// ffprobe said the file stops in the middle of a packet or element:
    /// it was cut short, so its packets don't show how long it should be.
    pub cut_off: bool,
}

impl PacketEnds {
    /// Read one line of `-show_entries packet=stream_index,pts_time,
    /// dts_time,duration_time -of compact=p=0` output
    /// (`stream_index=0|pts_time=1.0|dts_time=N/A|duration_time=0.04`).
    /// Lines without a stream and a time are ignored.
    pub fn push_line(&mut self, line: &str) {
        let mut index: Option<u32> = None;
        let (mut pts, mut dts, mut length) = (None, None, None);
        for field in line.trim().split('|') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let seconds = || value.trim().parse::<f64>().ok().filter(|v| v.is_finite());
            match key.trim() {
                "stream_index" => index = value.trim().parse().ok(),
                "pts_time" => pts = seconds(),
                "dts_time" => dts = seconds(),
                "duration_time" => length = seconds().filter(|d| *d >= 0.0),
                _ => {}
            }
        }
        let (Some(index), Some(at)) = (index, pts.or(dts)) else {
            return;
        };
        self.first = Some(self.first.map_or(at, |f| f.min(at)));
        let end = at + length.unwrap_or(0.0);
        self.ends
            .entry(index)
            .and_modify(|e| *e = e.max(end))
            .or_insert(end);
    }

    /// Take note of one of ffprobe's messages (see [`is_cut_off_message`]).
    pub fn note_message(&mut self, line: &str) {
        if is_cut_off_message(line) {
            self.cut_off = true;
        }
    }

    /// A whole listing and ffprobe's messages.
    pub fn parse(listing: &str, messages: &str) -> Self {
        let mut ends = Self::default();
        listing.lines().for_each(|l| ends.push_line(l));
        messages.lines().for_each(|l| ends.note_message(l));
        ends
    }

    /// Where the last packet listed ends, over every stream.
    pub fn end(&self) -> Option<f64> {
        self.ends.values().copied().reduce(f64::max)
    }

    /// The file's length from its `start` (see [`start_offset`]) by its
    /// packets: `None` when none were listed, or when it was cut off
    /// (its packets then show where it stops, not how long it is).
    pub fn length(&self, start: f64) -> Option<f64> {
        if self.cut_off {
            return None;
        }
        self.end()
            .map(|end| since_start(end, start))
            .filter(|l| l.is_finite() && *l > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-6)
    }

    #[test]
    fn small_start_times_count_as_zero() {
        assert_eq!(start_offset(None), 0.0);
        assert_eq!(start_offset(Some(0.0)), 0.0);
        assert_eq!(start_offset(Some(0.023)), 0.0);
        assert_eq!(start_offset(Some(-0.021)), 0.0);
        assert_eq!(start_offset(Some(f64::NAN)), 0.0);
        assert_eq!(start_offset(Some(1.4)), 1.4);
        assert_eq!(start_offset(Some(600.0)), 600.0);
    }

    #[test]
    fn stated_lengths_past_the_start_are_ends() {
        // ffmpeg's writer, timestamps from 10:00: 11:01 means 1:01.
        assert!(close(Some(stated_length(661.068, 600.0)), 61.068));
        // The same clip written through a pipe states its length.
        assert!(close(Some(stated_length(61.068, 600.0)), 61.068));
        // Starting at zero, a length is a length.
        assert!(close(Some(stated_length(8462.0, 0.0)), 8462.0));
        // To the microsecond, without the float noise of a subtraction.
        assert_eq!(since_start(620.038, 600.0), 20.038);
        assert_eq!(stated_length(620.038, 600.0), 20.038);
    }

    #[test]
    fn the_tail_is_read_from_the_longest_the_file_could_be() {
        // The container's length when it states one, tags otherwise.
        assert!(close(tail_start(0.0, Some(61.0), &[8462.0]), 51.0));
        assert!(close(tail_start(0.0, None, &[30.0, 8462.0]), 8452.0));
        // Short files are read whole.
        assert!(close(tail_start(0.0, Some(4.0), &[]), 0.0));
        // Timestamps from 10:00: read as a length from the start (a past-
        // the-end point lands on the last keyframe).
        assert!(close(tail_start(600.0, Some(61.0), &[]), 651.0));
        assert!(close(tail_start(600.0, Some(661.0), &[]), 1251.0));
        assert!(close(tail_start(600.0, None, &[8462.0]), 9052.0));
        assert_eq!(tail_start(0.0, None, &[]), None);
        assert_eq!(tail_start(0.0, Some(0.0), &[f64::NAN]), None);
    }

    #[test]
    fn packet_listings_give_each_streams_end() {
        let ends = PacketEnds::parse(
            "stream_index=0|pts_time=9.000000|dts_time=8.900000|duration_time=0.040000\n\
             stream_index=0|pts_time=8.960000|dts_time=8.940000|duration_time=0.040000\n\
             stream_index=1|pts_time=N/A|dts_time=9.500000|duration_time=N/A\n\
             garbage\n\
             stream_index=2|pts_time=N/A|dts_time=N/A|duration_time=0.1\n\
             \n",
            "",
        );
        assert_eq!(ends.ends.len(), 2);
        assert!(close(ends.ends.get(&0).copied(), 9.04));
        assert!(close(ends.ends.get(&1).copied(), 9.5));
        assert!(close(ends.first, 8.96));
        assert!(close(ends.end(), 9.5));
        assert!(close(ends.length(0.0), 9.5));
        assert!(!ends.cut_off);

        let empty = PacketEnds::parse("", "");
        assert_eq!(
            (empty.first, empty.end(), empty.length(0.0)),
            (None, None, None)
        );
    }

    /// A one-minute clip whose timestamps start at 10:00: 1:01 long.
    #[test]
    fn lengths_count_from_the_start() {
        let ends = PacketEnds::parse(
            "stream_index=0|pts_time=660.023000|duration_time=0.041000\n\
             stream_index=1|pts_time=661.022000|duration_time=0.023000\n",
            "",
        );
        assert!(close(ends.length(600.0), 61.045));
        assert!(close(ends.first, 660.023));
    }

    #[test]
    fn cut_off_files_are_noticed() {
        let ends = PacketEnds::parse(
            "stream_index=0|pts_time=30.398000|duration_time=0.041000\n",
            "[matroska,webm @ 0x55fe3db85900] File ended prematurely\n",
        );
        assert!(ends.cut_off);
        assert!(close(ends.end(), 30.439));
        assert_eq!(ends.length(0.0), None, "not the length it should be");
        for line in [
            "[mov,mp4 @ 0x1] Truncating packet of size 4096 to 1201",
            "Unexpected end of file",
            "[mpegts @ 0x2] Partial file",
        ] {
            assert!(is_cut_off_message(line), "{line}");
        }
        for line in ["[h264 @ 0x1] non-existing PPS 0 referenced", ""] {
            assert!(!is_cut_off_message(line), "{line}");
        }
    }
}
