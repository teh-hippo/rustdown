#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FenceState {
    marker: u8,
    marker_len: usize,
}

#[inline]
pub fn consume_fence_delimiter(line: &str, state: &mut Option<FenceState>) -> bool {
    let Some((marker, marker_len, rest)) = parse_fence_marker(line) else {
        return false;
    };

    match state {
        Some(open)
            if open.marker == marker && marker_len >= open.marker_len && rest.trim().is_empty() =>
        {
            *state = None;
            true
        }
        Some(_) => false,
        None => {
            // Per CommonMark §4.5: backtick fences must not have backticks
            // in the info string.  Tilde fences have no such restriction.
            if marker == b'`' && rest.contains('`') {
                return false;
            }
            *state = Some(FenceState { marker, marker_len });
            true
        }
    }
}

fn parse_fence_marker(line: &str) -> Option<(u8, usize, &str)> {
    // CommonMark §4.5: code fences may be indented 0-3 spaces only.
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if indent > 3 || line.as_bytes()[..indent].iter().any(|&b| b != b' ') {
        return None;
    }
    let first = *trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let marker_len = trimmed.bytes().take_while(|byte| *byte == first).count();
    (marker_len >= 3).then_some((first, marker_len, &trimmed[marker_len..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consume_fence_delimiter_supports_backtick_fences_with_info() {
        let mut state = None;
        assert!(consume_fence_delimiter("```azurecli", &mut state));
        assert!(state.is_some());
        assert!(!consume_fence_delimiter("az aks list", &mut state));
        assert!(state.is_some());
        assert!(consume_fence_delimiter("```", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn consume_fence_delimiter_supports_tilde_fences() {
        let mut state = None;
        assert!(consume_fence_delimiter("~~~bash", &mut state));
        assert!(state.is_some());
        assert!(!consume_fence_delimiter("~~~~not-a-close", &mut state));
        assert!(state.is_some());
        assert!(consume_fence_delimiter("~~~~", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn consume_fence_delimiter_requires_matching_marker_and_length_to_close() {
        let mut state = None;
        assert!(consume_fence_delimiter("~~~~", &mut state));
        assert!(!consume_fence_delimiter("```", &mut state));
        assert!(state.is_some());
        assert!(!consume_fence_delimiter("~~~", &mut state));
        assert!(state.is_some());
        assert!(consume_fence_delimiter("~~~~", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn consume_fence_delimiter_ignores_non_fence_lines() {
        let mut state = None;
        assert!(!consume_fence_delimiter("`inline`", &mut state));
        assert!(!consume_fence_delimiter("~~", &mut state));
        assert!(!consume_fence_delimiter("plain text", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn backtick_fence_with_backtick_in_info_string_rejected() {
        // CommonMark §4.5: backtick fence info strings must not contain backticks.
        let mut state = None;
        assert!(!consume_fence_delimiter("```foo`bar", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn tilde_fence_with_backtick_in_info_string_allowed() {
        // Tilde fences have no restriction on backticks in the info string.
        let mut state = None;
        assert!(consume_fence_delimiter("~~~foo`bar", &mut state));
        assert!(state.is_some());
    }

    // ── Indentation-limit tests (CommonMark §4.5) ───────────────────

    #[test]
    fn fence_three_space_indent_accepted() {
        let mut state = None;
        assert!(consume_fence_delimiter("   ```rust", &mut state));
        assert!(state.is_some());
        assert!(consume_fence_delimiter("   ```", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn fence_four_space_indent_rejected() {
        // 4+ spaces makes it an indented code block, not a fence.
        let mut state = None;
        assert!(!consume_fence_delimiter("    ```rust", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn fence_tab_indent_rejected() {
        let mut state = None;
        assert!(!consume_fence_delimiter("\t```rust", &mut state));
        assert!(state.is_none());
    }

    #[test]
    fn close_fence_four_space_indent_rejected() {
        let mut state = None;
        assert!(consume_fence_delimiter("```rust", &mut state));
        assert!(state.is_some());
        // Closing fence with 4-space indent is not valid.
        assert!(!consume_fence_delimiter("    ```", &mut state));
        assert!(state.is_some());
        // Normal close works.
        assert!(consume_fence_delimiter("```", &mut state));
        assert!(state.is_none());
    }
}
