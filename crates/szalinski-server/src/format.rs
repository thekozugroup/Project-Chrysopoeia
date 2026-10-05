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

/// Byte size in decimal units (`1.2 GB`, `346 MB`): the one formatter the
/// worker's check lines and the web UI follow too, so the log and the UI
/// give one saving the same way.
pub use szalinski_core::format::bytes;

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

    #[test]
    fn sizes_use_the_shared_formatter() {
        assert_eq!(bytes(1_234_567), "1.23 MB");
        assert_eq!(bytes(999_600), "1 MB");
    }

    #[test]
    fn percents() {
        assert_eq!(percent(38, 100), 38);
        assert_eq!(percent(-5, 100), -5);
        assert_eq!(percent(1, 0), 0);
    }
}
