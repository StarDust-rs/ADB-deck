//! Small text helpers shared by the filters.

/// True when `haystack` contains `needle`, ignoring case. An empty needle matches.
///
/// Filters run over thousands of rows every frame, so the common all-ASCII case
/// compares in place without allocating; other text falls back to full Unicode
/// lowercasing.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle.as_bytes());
        return n.len() <= h.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::contains_ci;

    #[test]
    fn matches_ignoring_case() {
        assert!(contains_ci("ActivityManager", "activityMAN"));
        assert!(contains_ci("anything", ""));
        assert!(contains_ci("", ""));
        assert!(!contains_ci("", "a"));
        assert!(!contains_ci("abc", "abcd"));
        assert!(!contains_ci("Activity", "tag"));
    }

    #[test]
    fn handles_non_ascii_text() {
        assert!(contains_ci("Ärger im Büro", "ÄRGER"));
        assert!(contains_ci("Ärger im Büro", "büro"));
        assert!(contains_ci("plain", "LAI"));
        assert!(!contains_ci("Ärger", "arger"));
    }
}
