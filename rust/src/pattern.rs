//! AOB (array-of-bytes) signature parsing and scanning.
//!
//! This is pure, platform-agnostic logic so it can be unit-tested on any host.
//! The Windows-specific PE walk that *feeds* memory into `find_pattern_in` lives
//! in `platform::memory`.

/// Parse a signature string like `"48 8D 0D ? ? ? ?"` into a vector of optional
/// bytes, where `?` (a wildcard) becomes `None`.
pub fn parse_signature(sig: &str) -> Vec<Option<u8>> {
    sig.split_whitespace()
        .map(|tok| {
            if tok.contains('?') {
                None
            } else {
                // Signatures are hard-coded constants; a bad hex token is a
                // programming error, so treat an unparseable token as a wildcard
                // rather than panicking at runtime.
                u8::from_str_radix(tok, 16).ok()
            }
        })
        .collect()
}

/// Find the first occurrence of `pattern` inside `haystack`.
///
/// Returns the index of the match, or `None`. `None` entries in `pattern` match
/// any byte. Mirrors the semantics of the original C# `Memory.FindPattern`.
pub fn find_pattern_in(haystack: &[u8], pattern: &[Option<u8>]) -> Option<usize> {
    if pattern.is_empty() || haystack.len() < pattern.len() {
        return None;
    }

    let first = pattern[0];
    let last_start = haystack.len() - pattern.len();
    let mut i = 0usize;

    while i <= last_start {
        // Fast-path: if the first pattern byte is concrete, jump straight to the
        // next candidate position instead of testing every offset.
        if let Some(fb) = first {
            match haystack[i..=last_start].iter().position(|&b| b == fb) {
                Some(off) => i += off,
                None => break,
            }
        }

        let matched = pattern.iter().enumerate().all(|(j, p)| match p {
            None => true,
            Some(b) => haystack[i + j] == *b,
        });

        if matched {
            return Some(i);
        }

        i += 1;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bytes_and_wildcards() {
        let sig = parse_signature("48 8D 0D ? ?? 90");
        assert_eq!(sig, vec![Some(0x48), Some(0x8D), Some(0x0D), None, None, Some(0x90)]);
    }

    #[test]
    fn finds_exact_match() {
        let hay = [0x00, 0x11, 0x48, 0x8D, 0x0D, 0xAA, 0xBB, 0x90, 0xFF];
        let sig = parse_signature("48 8D 0D ? ? 90");
        assert_eq!(find_pattern_in(&hay, &sig), Some(2));
    }

    #[test]
    fn wildcard_first_byte() {
        let hay = [0x01, 0x02, 0x03, 0x04];
        let sig = parse_signature("? 03");
        assert_eq!(find_pattern_in(&hay, &sig), Some(1));
    }

    #[test]
    fn returns_none_when_absent() {
        let hay = [0x01, 0x02, 0x03];
        let sig = parse_signature("AA BB");
        assert_eq!(find_pattern_in(&hay, &sig), None);
    }

    #[test]
    fn match_at_very_end() {
        let hay = [0x00, 0x00, 0xDE, 0xAD];
        let sig = parse_signature("DE AD");
        assert_eq!(find_pattern_in(&hay, &sig), Some(2));
    }

    #[test]
    fn empty_pattern_is_none() {
        let hay = [0x01, 0x02];
        assert_eq!(find_pattern_in(&hay, &[]), None);
    }
}
