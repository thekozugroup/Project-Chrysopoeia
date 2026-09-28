//! Human-friendly numbers for activity messages.

/// `1204` → `1,204`.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
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

/// Byte size in decimal units, as Finder shows it: `1.2 GB`, `340 MB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return if n == 1 {
            "1 byte".to_string()
        } else {
            format!("{n} bytes")
        };
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Signed percentage of `part` in `whole`, rounded: `38`.
pub fn percent(part: i64, whole: u64) -> i64 {
    if whole == 0 {
        return 0;
    }
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let p = (part as f64 / whole as f64 * 100.0).round() as i64;
    p
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

    #[test]
    fn byte_sizes() {
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(512), "512 bytes");
        assert_eq!(bytes(1_200_000_000), "1.2 GB");
        assert_eq!(bytes(340_000_000), "340 MB");
        assert_eq!(percent(38, 100), 38);
        assert_eq!(percent(-5, 100), -5);
        assert_eq!(percent(1, 0), 0);
    }
}
