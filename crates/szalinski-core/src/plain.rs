//! Plain-language wording for low-level errors.
//!
//! Messages users see never carry raw operating-system errors such as
//! `File exists (os error 17)`: they say what happened in a few words, and
//! the sentence around them says what to do.

use std::io;

/// Why a file operation failed, in a few plain words that read well after
/// "because", e.g. "the disk is full" or "a file with that name is in the
/// way". Never includes an error number.
pub fn io_reason(e: &io::Error) -> String {
    use io::ErrorKind as K;
    let known = match e.kind() {
        K::NotFound => Some("it doesn't exist"),
        K::PermissionDenied => Some("Szalinski doesn't have permission"),
        K::AlreadyExists => Some("a file with that name is in the way"),
        K::NotADirectory => Some("a file is in the way where a folder should be"),
        K::IsADirectory => Some("a folder is in the way where a file should be"),
        K::DirectoryNotEmpty => Some("the folder isn't empty"),
        K::StorageFull => Some("the disk is full"),
        K::QuotaExceeded => Some("the disk quota is used up"),
        K::ReadOnlyFilesystem => Some("the drive is read-only"),
        K::FileTooLarge => Some("the file is too large for this drive"),
        K::StaleNetworkFileHandle => Some("the network share stopped answering"),
        K::TimedOut => Some("the drive took too long to answer"),
        K::ResourceBusy => Some("it is busy"),
        K::OutOfMemory => Some("the system ran out of memory"),
        K::NotConnected | K::HostUnreachable | K::NetworkUnreachable | K::NetworkDown => {
            Some("the network share can't be reached")
        }
        _ => None,
    };
    if let Some(reason) = known {
        return reason.to_string();
    }
    // Linux error numbers the kinds above don't cover.
    let by_number = match e.raw_os_error() {
        Some(5) => Some("the disk reported a read or write error"),
        Some(36) => Some("the name is too long"),
        Some(40) => Some("the folders link back to themselves"),
        Some(122) => Some("the disk quota is used up"),
        _ => None,
    };
    if let Some(reason) = by_number {
        return reason.to_string();
    }
    lower_first(&strip_os_error(&e.to_string()))
}

/// `text` without the ` (os error N)` that Rust appends to system errors.
pub fn strip_os_error(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(" (os error ") {
        out.push_str(&rest[..start]);
        let after = &rest[start + " (os error ".len()..];
        match after.find(')') {
            Some(end) if after[..end].chars().all(|c| c.is_ascii_digit() || c == '-') => {
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(" (os error ");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// `text` with its first letter in lower case, unless the first word is an
/// acronym (e.g. "I/O", "NFS").
fn lower_first(text: &str) -> String {
    let first_word = text.split_whitespace().next().unwrap_or("");
    let acronym = first_word.chars().filter(|c| c.is_alphabetic()).count() > 1
        && first_word
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(char::is_uppercase);
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if !acronym => first.to_lowercase().chain(chars).collect(),
        Some(_) => text.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_error_numbers_are_removed() {
        assert_eq!(strip_os_error("File exists (os error 17)"), "File exists");
        assert_eq!(
            strip_os_error("a (os error 2), then b (os error 13)"),
            "a, then b"
        );
        assert_eq!(
            strip_os_error("kept (os error unknown)"),
            "kept (os error unknown)"
        );
        assert_eq!(strip_os_error("nothing to strip"), "nothing to strip");
    }

    #[test]
    fn io_errors_read_as_plain_reasons() {
        let reason = |code: i32| io_reason(&io::Error::from_raw_os_error(code));
        assert_eq!(reason(17), "a file with that name is in the way");
        assert_eq!(reason(20), "a file is in the way where a folder should be");
        assert_eq!(reason(28), "the disk is full");
        assert_eq!(reason(30), "the drive is read-only");
        assert_eq!(reason(13), "Szalinski doesn't have permission");
        assert_eq!(reason(5), "the disk reported a read or write error");
        assert_eq!(reason(36), "the name is too long");
        for code in 1..=133 {
            let text = reason(code);
            assert!(!text.contains("os error"), "{code}: {text}");
            assert!(!text.is_empty(), "{code}");
        }
        assert_eq!(
            io_reason(&io::Error::other("Something odd")),
            "something odd"
        );
        assert_eq!(io_reason(&io::Error::other("NFS hiccup")), "NFS hiccup");
    }
}
