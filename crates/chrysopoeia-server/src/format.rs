//! Human-friendly numbers for activity messages.

/// `1204` → `1,204`.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `1, "file", "files"` → `1 file`; `1204` → `1,204 files`.
pub fn plural(n: u64, singular: &str, plural: &str) -> String {
    format!("{} {}", count(n), if n == 1 { singular } else { plural })
}

/// Byte size in decimal units, as Finder shows it: `1.2 GB`, `346 MB`,
/// `12 KB`. The same rules as the web UI's `formatBytes`, so the log and the
/// UI give one saving the same way: whole KB, then two decimals below 10,
/// one below 100 and none above (trailing zeros dropped); rounded before
/// moving up a unit, so 999,600 bytes is `1 MB`, not `1000 KB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return if n == 1 {
            "1 byte".to_string()
        } else {
            format!("{n} bytes")
        };
    }
    let mut value = n as f64 / 1000.0;
    let mut unit = 0;
    loop {
        let digits: usize = match value {
            _ if unit == 0 => 0,
            v if v < 10.0 => 2,
            v if v < 100.0 => 1,
            _ => 0,
        };
        let factor = 10f64.powi(i32::try_from(digits).unwrap_or(0));
        let rounded = (value * factor).round() / factor;
        if rounded >= 1000.0 && unit < UNITS.len() - 1 {
            value /= 1000.0;
            unit += 1;
            continue;
        }
        let text = format!("{rounded:.digits$}");
        let text = if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.')
        } else {
            text.as_str()
        };
        return format!("{text} {}", UNITS[unit]);
    }
}

/// Signed percentage of `part` in `whole`, rounded: `38`.
pub fn percent(part: i64, whole: u64) -> i64 {
    if whole == 0 {
        return 0;
    }
    (part as f64 / whole as f64 * 100.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1204), "1,204");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(plural(1, "file", "files"), "1 file");
        assert_eq!(plural(2, "file", "files"), "2 files");
    }

    /// The web UI's `formatBytes` expectations (web/src/lib/format.test.ts):
    /// the log and the UI show a saving the same way.
    #[test]
    fn byte_sizes_match_the_web_ui() {
        for (n, text) in [
            (0, "0 bytes"),
            (1, "1 byte"),
            (512, "512 bytes"),
            (999, "999 bytes"),
            (1_000, "1 KB"),
            (12_345, "12 KB"),
            (1_234_567, "1.23 MB"),
            (345_600_000, "346 MB"),
            (34_560_000, "34.6 MB"),
            (340_000_000, "340 MB"),
            (1_200_000_000, "1.2 GB"),
            (3_250_000_000, "3.25 GB"),
            (2_500_000_000, "2.5 GB"),
            // Rounded before moving up a unit.
            (999_600, "1 MB"),
            (999_700_000, "1 GB"),
            (999_960_000_000, "1 TB"),
            (99_960_000, "100 MB"),
            (9_996_000, "10 MB"),
            (933_000, "933 KB"),
            (4_500_000, "4.5 MB"),
        ] {
            assert_eq!(bytes(n), text, "{n}");
        }
    }

    #[test]
    fn percents() {
        assert_eq!(percent(38, 100), 38);
        assert_eq!(percent(-5, 100), -5);
        assert_eq!(percent(1, 0), 0);
    }
}
