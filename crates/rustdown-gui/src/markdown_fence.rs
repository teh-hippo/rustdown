#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FenceState {
    marker: u8,
    marker_len: usize,
}

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
    let trimmed = line.trim_start();
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
}
