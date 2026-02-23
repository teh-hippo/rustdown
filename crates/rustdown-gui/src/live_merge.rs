use imara_diff::{Algorithm, Diff, InternedInput};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Merge3Outcome {
    Clean(String),
    Conflicted {
        conflict_marked: String,
        ours_wins: String,
    },
}

#[derive(Clone, Debug)]
struct Edit<'a> {
    base_start: usize,
    base_end: usize,
    replacement: Vec<&'a str>,
}

pub(crate) fn merge_three_way(base: &str, ours: &str, theirs: &str) -> Merge3Outcome {
    if ours == theirs {
        return Merge3Outcome::Clean(ours.to_owned());
    }
    if base == ours {
        return Merge3Outcome::Clean(theirs.to_owned());
    }
    if base == theirs {
        return Merge3Outcome::Clean(ours.to_owned());
    }

    let base_lines: Vec<&str> = imara_diff::sources::lines(base).collect();

    const MAX_MERGE_LINES: usize = 20_000;
    let max_lines = base_lines
        .len()
        .max(imara_diff::sources::lines(ours).count())
        .max(imara_diff::sources::lines(theirs).count());
    if max_lines > MAX_MERGE_LINES {
        let mut conflict_marked = String::new();
        conflict_marked.push_str("<<<<<<< ours\n");
        conflict_marked.push_str(ours);
        ensure_newline(&mut conflict_marked);
        conflict_marked.push_str("=======\n");
        conflict_marked.push_str(theirs);
        ensure_newline(&mut conflict_marked);
        conflict_marked.push_str(">>>>>>> theirs\n");

        return Merge3Outcome::Conflicted {
            conflict_marked,
            ours_wins: ours.to_owned(),
        };
    }

    let ours_edits = diff_edits(base, ours);
    let theirs_edits = diff_edits(base, theirs);

    let base_len = base_lines.len();
    let mut pos = 0usize;
    let mut i_ours = 0usize;
    let mut i_theirs = 0usize;
    let mut ours_wins = String::new();
    let mut conflict_marked = String::new();
    let mut has_conflicts = false;

    loop {
        let next_ours = ours_edits.get(i_ours);
        let next_theirs = theirs_edits.get(i_theirs);

        let next_start = match (next_ours, next_theirs) {
            (Some(oe), Some(te)) => oe.base_start.min(te.base_start),
            (Some(oe), None) => oe.base_start,
            (None, Some(te)) => te.base_start,
            (None, None) => base_len,
        };

        if pos < next_start {
            for tok in &base_lines[pos..next_start] {
                ours_wins.push_str(tok);
                conflict_marked.push_str(tok);
            }
            pos = next_start;
        }

        let next_ours = ours_edits.get(i_ours);
        let next_theirs = theirs_edits.get(i_theirs);
        let (Some(oe), Some(te)) = (next_ours, next_theirs) else {
            if let Some(oe) = next_ours {
                for tok in &oe.replacement {
                    ours_wins.push_str(tok);
                    conflict_marked.push_str(tok);
                }
                pos = oe.base_end;
                i_ours += 1;
                continue;
            }
            if let Some(te) = next_theirs {
                for tok in &te.replacement {
                    ours_wins.push_str(tok);
                    conflict_marked.push_str(tok);
                }
                pos = te.base_end;
                i_theirs += 1;
                continue;
            }
            break;
        };

        if oe.base_start == pos && te.base_start == pos && edits_identical(oe, te) {
            for tok in &oe.replacement {
                ours_wins.push_str(tok);
                conflict_marked.push_str(tok);
            }
            pos = oe.base_end;
            i_ours += 1;
            i_theirs += 1;
            continue;
        }

        if !edits_overlap(oe, te) {
            // Apply whichever edit starts first.
            if oe.base_start < te.base_start {
                for tok in &oe.replacement {
                    ours_wins.push_str(tok);
                    conflict_marked.push_str(tok);
                }
                pos = oe.base_end;
                i_ours += 1;
            } else {
                for tok in &te.replacement {
                    ours_wins.push_str(tok);
                    conflict_marked.push_str(tok);
                }
                pos = te.base_end;
                i_theirs += 1;
            }
            continue;
        }

        // Collect a minimal overlapping group.
        let conflict_start = pos;
        let ours_group_start = i_ours;
        let theirs_group_start = i_theirs;

        let mut group_end = conflict_start;
        if oe.base_start == conflict_start {
            group_end = group_end.max(oe.base_end);
            i_ours += 1;
        }
        if te.base_start == conflict_start {
            group_end = group_end.max(te.base_end);
            i_theirs += 1;
        }

        loop {
            let mut progressed = false;
            if let Some(next) = ours_edits.get(i_ours)
                && next.base_start < group_end
            {
                group_end = group_end.max(next.base_end);
                i_ours += 1;
                progressed = true;
            }
            if let Some(next) = theirs_edits.get(i_theirs)
                && next.base_start < group_end
            {
                group_end = group_end.max(next.base_end);
                i_theirs += 1;
                progressed = true;
            }
            if !progressed {
                break;
            }
        }

        let ours_chunk = render_range_with_edits(
            &base_lines,
            conflict_start,
            group_end,
            &ours_edits[ours_group_start..i_ours],
        );
        let theirs_chunk = render_range_with_edits(
            &base_lines,
            conflict_start,
            group_end,
            &theirs_edits[theirs_group_start..i_theirs],
        );

        if ours_chunk == theirs_chunk {
            ours_wins.push_str(&ours_chunk);
            conflict_marked.push_str(&ours_chunk);
        } else {
            has_conflicts = true;
            ours_wins.push_str(&ours_chunk);

            ensure_newline(&mut conflict_marked);
            conflict_marked.push_str("<<<<<<< ours\n");
            conflict_marked.push_str(&ours_chunk);
            ensure_newline(&mut conflict_marked);
            conflict_marked.push_str("=======\n");
            conflict_marked.push_str(&theirs_chunk);
            ensure_newline(&mut conflict_marked);
            conflict_marked.push_str(">>>>>>> theirs\n");
        }

        pos = group_end;
    }

    if has_conflicts {
        Merge3Outcome::Conflicted {
            conflict_marked,
            ours_wins,
        }
    } else {
        Merge3Outcome::Clean(ours_wins)
    }
}

fn diff_edits<'a>(base: &'a str, other: &'a str) -> Vec<Edit<'a>> {
    let input = InternedInput::new(base, other);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);

    let other_lines: Vec<&'a str> = imara_diff::sources::lines(other).collect();

    diff.hunks()
        .map(|hunk| Edit {
            base_start: hunk.before.start as usize,
            base_end: hunk.before.end as usize,
            replacement: other_lines[hunk.after.start as usize..hunk.after.end as usize].to_vec(),
        })
        .collect()
}

fn edits_overlap(left: &Edit<'_>, right: &Edit<'_>) -> bool {
    if left.base_start == left.base_end && right.base_start == right.base_end {
        return left.base_start == right.base_start;
    }
    if left.base_start == left.base_end {
        return right.base_start <= left.base_start && left.base_start < right.base_end;
    }
    if right.base_start == right.base_end {
        return left.base_start <= right.base_start && right.base_start < left.base_end;
    }

    left.base_start < right.base_end && right.base_start < left.base_end
}

fn edits_identical(left: &Edit<'_>, right: &Edit<'_>) -> bool {
    left.base_start == right.base_start
        && left.base_end == right.base_end
        && left.replacement == right.replacement
}

fn render_range_with_edits<'a>(
    base: &[&str],
    start: usize,
    end: usize,
    edits: &[Edit<'a>],
) -> String {
    let mut out = String::new();
    let mut pos = start;
    for edit in edits {
        if pos < edit.base_start {
            for tok in &base[pos..edit.base_start] {
                out.push_str(tok);
            }
        }
        for tok in &edit.replacement {
            out.push_str(tok);
        }
        pos = edit.base_end;
    }
    if pos < end {
        for tok in &base[pos..end] {
            out.push_str(tok);
        }
    }
    out
}

fn ensure_newline(buf: &mut String) {
    if !buf.is_empty() && !buf.ends_with('\n') {
        buf.push('\n');
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn identical_ours_and_theirs_returns_clean() {
        let result = merge_three_way("a\n", "b\n", "b\n");
        assert_eq!(result, Merge3Outcome::Clean("b\n".to_owned()));
    }

    #[test]
    fn only_ours_changed_returns_ours() {
        let result = merge_three_way("a\n", "b\n", "a\n");
        assert_eq!(result, Merge3Outcome::Clean("b\n".to_owned()));
    }

    #[test]
    fn only_theirs_changed_returns_theirs() {
        let result = merge_three_way("a\n", "a\n", "c\n");
        assert_eq!(result, Merge3Outcome::Clean("c\n".to_owned()));
    }

    #[test]
    fn non_overlapping_edits_merge_cleanly() {
        let base = "line1\nline2\nline3\n";
        let ours = "LINE1\nline2\nline3\n";
        let theirs = "line1\nline2\nLINE3\n";
        let result = merge_three_way(base, ours, theirs);
        assert_eq!(
            result,
            Merge3Outcome::Clean("LINE1\nline2\nLINE3\n".to_owned())
        );
    }

    #[test]
    fn overlapping_edits_produce_conflict() {
        let base = "a\nb\n";
        let ours = "a\nO\n";
        let theirs = "a\nT\n";
        let result = merge_three_way(base, ours, theirs);
        match result {
            Merge3Outcome::Conflicted {
                conflict_marked,
                ours_wins,
            } => {
                assert!(conflict_marked.contains("<<<<<<< ours"));
                assert!(conflict_marked.contains("O\n"));
                assert!(conflict_marked.contains("T\n"));
                assert!(conflict_marked.contains(">>>>>>> theirs"));
                assert_eq!(ours_wins, "a\nO\n");
            }
            Merge3Outcome::Clean(_) => panic!("Expected conflict"),
        }
    }

    #[test]
    fn identical_overlapping_edits_are_not_conflicts() {
        let base = "a\nb\nc\n";
        let ours = "a\nX\nc\n";
        let theirs = "a\nX\nc\n";
        let result = merge_three_way(base, ours, theirs);
        assert_eq!(result, Merge3Outcome::Clean("a\nX\nc\n".to_owned()));
    }

    #[test]
    fn empty_base_with_different_additions_conflicts() {
        let result = merge_three_way("", "hello\n", "world\n");
        match result {
            Merge3Outcome::Conflicted {
                conflict_marked, ..
            } => {
                assert!(conflict_marked.contains("<<<<<<< ours"));
            }
            Merge3Outcome::Clean(_) => panic!("Expected conflict"),
        }
    }

    #[test]
    fn all_identical_returns_clean() {
        let text = "same\n";
        let result = merge_three_way(text, text, text);
        assert_eq!(result, Merge3Outcome::Clean(text.to_owned()));
    }

    #[test]
    fn multi_line_non_overlapping_merge() {
        let base = "a\nb\nc\nd\ne\n";
        let ours = "A\nb\nc\nd\ne\n";
        let theirs = "a\nb\nc\nd\nE\n";
        let result = merge_three_way(base, ours, theirs);
        assert_eq!(result, Merge3Outcome::Clean("A\nb\nc\nd\nE\n".to_owned()));
    }

    #[test]
    fn diff_edits_detects_single_line_change() {
        let edits = diff_edits("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].base_start, 1);
        assert_eq!(edits[0].base_end, 2);
        assert_eq!(edits[0].replacement, vec!["X\n"]);
    }

    #[test]
    fn diff_edits_detects_insertion() {
        let edits = diff_edits("a\nc\n", "a\nb\nc\n");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].base_start, 1);
        assert_eq!(edits[0].base_end, 1);
        assert_eq!(edits[0].replacement, vec!["b\n"]);
    }

    #[test]
    fn diff_edits_detects_deletion() {
        let edits = diff_edits("a\nb\nc\n", "a\nc\n");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].base_start, 1);
        assert_eq!(edits[0].base_end, 2);
        assert!(edits[0].replacement.is_empty());
    }

    #[test]
    fn edits_overlap_pure_insertions_at_same_point() {
        let a = Edit {
            base_start: 2,
            base_end: 2,
            replacement: vec!["x\n"],
        };
        let b = Edit {
            base_start: 2,
            base_end: 2,
            replacement: vec!["y\n"],
        };
        assert!(edits_overlap(&a, &b));
    }

    #[test]
    fn edits_overlap_non_overlapping_ranges() {
        let a = Edit {
            base_start: 0,
            base_end: 1,
            replacement: vec!["x\n"],
        };
        let b = Edit {
            base_start: 2,
            base_end: 3,
            replacement: vec!["y\n"],
        };
        assert!(!edits_overlap(&a, &b));
    }

    #[test]
    fn edits_overlap_adjacent_ranges_do_not_overlap() {
        let a = Edit {
            base_start: 0,
            base_end: 2,
            replacement: vec!["x\n"],
        };
        let b = Edit {
            base_start: 2,
            base_end: 4,
            replacement: vec!["y\n"],
        };
        assert!(!edits_overlap(&a, &b));
    }

    #[test]
    fn edits_overlap_partial_overlap() {
        let a = Edit {
            base_start: 0,
            base_end: 3,
            replacement: vec!["x\n"],
        };
        let b = Edit {
            base_start: 2,
            base_end: 5,
            replacement: vec!["y\n"],
        };
        assert!(edits_overlap(&a, &b));
    }
}
