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

    // Single-pass: find all occurrences and build the result in one scan.
    let needle_bytes = needle.as_bytes();
    let haystack_bytes = haystack.as_bytes();
    let mut iter = memchr::memmem::find_iter(haystack_bytes, needle_bytes);
    let Some(first) = iter.next() else {
        return (Cow::Borrowed(haystack), 0);
    };

    // Pre-allocate with a reasonable estimate.
    let estimated = if replacement.len() >= needle.len() {
        haystack.len() + (replacement.len() - needle.len()) * 4
    } else {
        haystack.len()
    };
    let mut result = String::with_capacity(estimated);
    let mut count = 1usize;
    let mut prev_end = 0;

    result.push_str(&haystack[prev_end..first]);
    result.push_str(replacement);
    prev_end = first + needle.len();

    for pos in iter {
        result.push_str(&haystack[prev_end..pos]);
        result.push_str(replacement);
        prev_end = pos + needle.len();
        count += 1;
    }

    result.push_str(&haystack[prev_end..]);
    (Cow::Owned(result), count)
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    // ── find_match_count ────────────────────────────────────────────

    #[test]
    fn count_match_cases() {
        let cases: &[(&str, &str, usize)] = &[
            ("abcabc", "abc", 2),          // basic multiple
            ("banana", "a", 3),            // single char
            ("hello world", "xyz", 0),     // no matches
            ("hello", "", 0),              // empty needle
            ("", "abc", 0),                // empty haystack
            ("", "", 0),                   // both empty
            ("AaAaA", "a", 2),             // case sensitive
            ("AaAaA", "A", 3),             // case sensitive caps
            ("aaa", "aa", 1),              // non-overlapping
            ("aaaa", "aa", 2),             // non-overlapping 2
            ("café café café", "café", 3), // unicode
            ("日本語日本語", "日本語", 2), // CJK
            ("ab", "abcdef", 0),           // needle > haystack
            ("exact", "exact", 1),         // full match
        ];
        for (haystack, needle, expected) in cases {
            assert_eq!(
                find_match_count(haystack, needle),
                *expected,
                "count({haystack:?}, {needle:?})"
            );
        }
    }

    // ── replace_all_occurrences ─────────────────────────────────────

    #[test]
    fn replace_cases() {
        // (haystack, needle, replacement, expected_count, expected_result, should_be_owned)
        let cases: &[(&str, &str, &str, usize, &str, bool)] = &[
            ("foo bar foo", "foo", "baz", 2, "baz bar baz", true),
            ("aXbXc", "X", "", 2, "abc", true),
            ("hello", "xyz", "!!!", 0, "hello", false),
            ("hello", "", "x", 0, "hello", false),
            ("hello", "ll", "ll", 0, "hello", false),
            ("café latte café", "café", "tea", 2, "tea latte tea", true),
            ("a-b-c", "-", "---", 2, "a---b---c", true),
            ("abc def abc", "abc", "xyz", 2, "xyz def xyz", true),
            ("hello world hello", "hello", "hi", 2, "hi world hi", true),
            ("", "abc", "xyz", 0, "", false),
        ];
        for (haystack, needle, repl, exp_count, exp_result, owned) in cases {
            let (result, count) = replace_all_occurrences(haystack, needle, repl);
            assert_eq!(
                count, *exp_count,
                "count for replace({haystack:?}, {needle:?}, {repl:?})"
            );
            assert_eq!(
                result, *exp_result,
                "result for replace({haystack:?}, {needle:?}, {repl:?})"
            );
            if *owned {
                assert!(matches!(result, Cow::Owned(_)), "should be Owned");
            } else {
                assert!(matches!(result, Cow::Borrowed(_)), "should be Borrowed");
            }
        }
    }

    #[test]
    fn count_literal_special_chars() {
        // Confirm that regex-special characters are treated as literal bytes.
        assert_eq!(find_match_count("a.b.c", "."), 2);
        assert_eq!(find_match_count("a*b*c", "*"), 2);
        assert_eq!(find_match_count("(a)(b)", "("), 2);
        assert_eq!(find_match_count("[x][y][x]", "[x]"), 2);
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
