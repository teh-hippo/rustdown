use std::borrow::Cow;

#[derive(Default)]
pub(crate) struct SearchState {
    pub(crate) visible: bool,
    pub(crate) replace_mode: bool,
    pub(crate) query: String,
    pub(crate) replacement: String,
    pub(crate) last_replace_count: Option<usize>,
    pub(crate) match_count_query: String,
    pub(crate) match_count_seq: u64,
    pub(crate) match_count: usize,
}

impl SearchState {
    pub(crate) fn match_count(&mut self, haystack: &str, haystack_seq: u64) -> usize {
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
pub(crate) fn find_match_count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    if needle.len() == 1 {
        return memchr::memchr_iter(needle.as_bytes()[0], haystack.as_bytes()).count();
    }
    memchr::memmem::find_iter(haystack.as_bytes(), needle.as_bytes()).count()
}

#[must_use]
pub(crate) fn replace_all_occurrences<'a>(
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
