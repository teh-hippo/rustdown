use std::borrow::Cow;

#[derive(Default)]
pub struct SearchState {
    pub visible: bool,
    pub replace_mode: bool,
    pub query: String,
    pub replacement: String,
    pub last_replace_count: Option<usize>,
    /// Cached match-count state (private — only accessed via `match_count()`).
    match_count_query: String,
    match_count_seq: u64,
    match_count: usize,
}

impl SearchState {
    /// Create a `SearchState` pre-populated with the given query.
    pub fn with_query(query: &str) -> Self {
        Self {
            query: query.to_owned(),
            ..Self::default()
        }
    }

    pub fn match_count(&mut self, haystack: &str, haystack_seq: u64) -> usize {
        if self.match_count_seq == haystack_seq && self.match_count_query == self.query {
            return self.match_count;
        }

        let count = find_match_count(haystack, self.query.as_str());
        self.match_count_query.clear();
        self.match_count_query.push_str(self.query.as_str());
        self.match_count_seq = haystack_seq;
        self.match_count = count;
        count
    }
}

#[must_use]
pub fn find_match_count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    if needle.len() == 1 {
        return memchr::memchr_iter(needle.as_bytes()[0], haystack.as_bytes()).count();
    }
    memchr::memmem::find_iter(haystack.as_bytes(), needle.as_bytes()).count()
}

#[must_use]
pub fn replace_all_occurrences<'a>(
    haystack: &'a str,
    needle: &str,
    replacement: &str,
) -> (Cow<'a, str>, usize) {
    if needle.is_empty() || needle == replacement {
        return (Cow::Borrowed(haystack), 0);
    }

    let matches = find_match_count(haystack, needle);
    if matches == 0 {
        return (Cow::Borrowed(haystack), 0);
    }

    (Cow::Owned(haystack.replace(needle, replacement)), matches)
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    // ── find_match_count ────────────────────────────────────────────

    #[test]
    fn count_basic_multiple_matches() {
        assert_eq!(find_match_count("abcabc", "abc"), 2);
    }

    #[test]
    fn count_single_char_needle() {
        assert_eq!(find_match_count("banana", "a"), 3);
    }

    #[test]
    fn count_no_matches() {
        assert_eq!(find_match_count("hello world", "xyz"), 0);
    }

    #[test]
    fn count_empty_needle_returns_zero() {
        assert_eq!(find_match_count("hello", ""), 0);
    }

    #[test]
    fn count_empty_haystack_returns_zero() {
        assert_eq!(find_match_count("", "abc"), 0);
    }

    #[test]
    fn count_both_empty_returns_zero() {
        assert_eq!(find_match_count("", ""), 0);
    }

    #[test]
    fn count_case_sensitive() {
        assert_eq!(find_match_count("AaAaA", "a"), 2);
        assert_eq!(find_match_count("AaAaA", "A"), 3);
    }

    #[test]
    fn count_non_overlapping() {
        // "aaa" searched for "aa" — non-overlapping gives 1
        assert_eq!(find_match_count("aaa", "aa"), 1);
        assert_eq!(find_match_count("aaaa", "aa"), 2);
    }

    #[test]
    fn count_unicode_content() {
        assert_eq!(find_match_count("café café café", "café"), 3);
        assert_eq!(find_match_count("日本語日本語", "日本語"), 2);
    }

    #[test]
    fn count_needle_longer_than_haystack() {
        assert_eq!(find_match_count("ab", "abcdef"), 0);
    }

    #[test]
    fn count_full_haystack_match() {
        assert_eq!(find_match_count("exact", "exact"), 1);
    }

    // ── replace_all_occurrences ─────────────────────────────────────

    #[test]
    fn replace_basic() {
        let (result, count) = replace_all_occurrences("foo bar foo", "foo", "baz");
        assert_eq!(count, 2);
        assert_eq!(result, "baz bar baz");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn replace_with_empty_string_deletes() {
        let (result, count) = replace_all_occurrences("aXbXc", "X", "");
        assert_eq!(count, 2);
        assert_eq!(result, "abc");
    }

    #[test]
    fn replace_no_matches_returns_borrowed() {
        let (result, count) = replace_all_occurrences("hello", "xyz", "!!!");
        assert_eq!(count, 0);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn replace_empty_needle_returns_borrowed() {
        let (result, count) = replace_all_occurrences("hello", "", "x");
        assert_eq!(count, 0);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn replace_needle_equals_replacement_returns_borrowed() {
        let (result, count) = replace_all_occurrences("hello", "ll", "ll");
        assert_eq!(count, 0);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn replace_unicode() {
        let (result, count) = replace_all_occurrences("café latte café", "café", "tea");
        assert_eq!(count, 2);
        assert_eq!(result, "tea latte tea");
    }

    #[test]
    fn replace_with_longer_replacement() {
        let (result, count) = replace_all_occurrences("a-b-c", "-", "---");
        assert_eq!(count, 2);
        assert_eq!(result, "a---b---c");
    }

    // ── SearchState::match_count (caching) ──────────────────────────

    #[test]
    fn state_match_count_basic() {
        let mut state = SearchState::with_query("ab");
        let count = state.match_count("ab ab ab", 1);
        assert_eq!(count, 3);
    }

    #[test]
    fn state_cache_returns_same_result() {
        let mut state = SearchState::with_query("x");
        let first = state.match_count("x x x", 1);
        // Same seq + same query → cached path
        let second = state.match_count("x x x", 1);
        assert_eq!(first, second);
        assert_eq!(first, 3);
    }

    #[test]
    fn state_cache_invalidated_by_new_seq() {
        let mut state = SearchState::with_query("a");
        assert_eq!(state.match_count("aaa", 1), 3);
        // Different seq forces recomputation with new haystack
        assert_eq!(state.match_count("aa", 2), 2);
    }

    #[test]
    fn state_cache_invalidated_by_query_change() {
        let mut state = SearchState::with_query("a");
        assert_eq!(state.match_count("abc", 1), 1);
        // Change the query field directly
        state.query = "b".to_owned();
        assert_eq!(state.match_count("abc", 1), 1);
    }

    #[test]
    fn state_empty_query_returns_zero() {
        let mut state = SearchState::default();
        assert_eq!(state.match_count("anything", 1), 0);
    }

    #[test]
    fn state_with_query_sets_field() {
        let state = SearchState::with_query("hello");
        assert_eq!(state.query, "hello");
        assert!(!state.visible);
        assert!(!state.replace_mode);
    }
}
