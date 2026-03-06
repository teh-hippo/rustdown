use imara_diff::{Algorithm, Diff, InternedInput};

#[derive(Debug, PartialEq, Eq)]
pub enum Merge3Outcome {
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

/// Push the same tokens to two output buffers (used for non-conflicting regions).
fn push_both(a: &mut String, b: &mut String, tokens: &[&str]) {
    for tok in tokens {
        a.push_str(tok);
        b.push_str(tok);
    }
}

#[allow(clippy::too_many_lines)] // merge logic — linear flow with multiple phases
pub fn merge_three_way(base: &str, ours: &str, theirs: &str) -> Merge3Outcome {
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
        let cap = ours.len() + theirs.len() + 80;
        let mut conflict_marked = String::with_capacity(cap);
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
    let estimated_cap = base.len().max(ours.len()).max(theirs.len()) + 256;
    let mut ours_wins = String::with_capacity(estimated_cap);
    let mut conflict_marked = String::with_capacity(estimated_cap);
    let mut has_conflicts = false;

    loop {
        let next_ours = ours_edits.get(i_ours);
        let next_theirs = theirs_edits.get(i_theirs);

        let next_start = match (next_ours, next_theirs) {
            (Some(oe), Some(te)) => oe.base_start.min(te.base_start),
            (Some(oe), None) => oe.base_start,
            (None, Some(te)) => te.base_start,
            (None, None) => base_len,
        }
        .min(base_len);

        if pos < next_start {
            push_both(
                &mut ours_wins,
                &mut conflict_marked,
                &base_lines[pos..next_start],
            );
            pos = next_start;
        }

        let next_ours = ours_edits.get(i_ours);
        let next_theirs = theirs_edits.get(i_theirs);
        let (Some(oe), Some(te)) = (next_ours, next_theirs) else {
            if let Some(oe) = next_ours {
                push_both(&mut ours_wins, &mut conflict_marked, &oe.replacement);
                pos = oe.base_end;
                i_ours += 1;
                continue;
            }
            if let Some(te) = next_theirs {
                push_both(&mut ours_wins, &mut conflict_marked, &te.replacement);
                pos = te.base_end;
                i_theirs += 1;
                continue;
            }
            break;
        };

        if oe.base_start == pos && te.base_start == pos && edits_identical(oe, te) {
            push_both(&mut ours_wins, &mut conflict_marked, &oe.replacement);
            pos = oe.base_end;
            i_ours += 1;
            i_theirs += 1;
            continue;
        }

        if !edits_overlap(oe, te) {
            // Apply whichever edit starts first.
            if oe.base_start < te.base_start {
                push_both(&mut ours_wins, &mut conflict_marked, &oe.replacement);
                pos = oe.base_end;
                i_ours += 1;
            } else {
                push_both(&mut ours_wins, &mut conflict_marked, &te.replacement);
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
            #[allow(clippy::useless_let_if_seq)]
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
            push_both(&mut ours_wins, &mut conflict_marked, &[&ours_chunk]);
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
    let other_len = other_lines.len();

    diff.hunks()
        .map(|hunk| {
            let after_start = (hunk.after.start as usize).min(other_len);
            let after_end = (hunk.after.end as usize).min(other_len);
            Edit {
                base_start: hunk.before.start as usize,
                base_end: hunk.before.end as usize,
                replacement: other_lines[after_start..after_end].to_vec(),
            }
        })
        .collect()
}

#[allow(clippy::suspicious_operation_groupings)]
const fn edits_overlap(left: &Edit<'_>, right: &Edit<'_>) -> bool {
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

fn render_range_with_edits(base: &[&str], start: usize, end: usize, edits: &[Edit<'_>]) -> String {
    let base_len = base.len();
    let end = end.min(base_len);
    let mut out = String::new();
    let mut pos = start.min(end);
    for edit in edits {
        let edit_start = edit.base_start.min(base_len);
        if pos < edit_start {
            for tok in &base[pos..edit_start] {
                out.push_str(tok);
            }
        }
        for tok in &edit.replacement {
            out.push_str(tok);
        }
        pos = edit.base_end.min(base_len);
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

    fn assert_clean(base: &str, ours: &str, theirs: &str, expected: &str) {
        assert_eq!(
            merge_three_way(base, ours, theirs),
            Merge3Outcome::Clean(expected.to_owned())
        );
    }

    fn assert_conflict(base: &str, ours: &str, theirs: &str) -> (String, String) {
        match merge_three_way(base, ours, theirs) {
            Merge3Outcome::Conflicted {
                conflict_marked,
                ours_wins,
            } => (conflict_marked, ours_wins),
            Merge3Outcome::Clean(_) => panic!("Expected conflict"),
        }
    }

    fn edit<'a>(base_start: usize, base_end: usize, replacement: &[&'a str]) -> Edit<'a> {
        Edit {
            base_start,
            base_end,
            replacement: replacement.to_vec(),
        }
    }

    #[test]
    fn merge_three_way_clean_cases() {
        for (label, base, ours, theirs, expected) in [
            ("both same", "a\n", "b\n", "b\n", "b\n"),
            ("ours changed", "a\n", "b\n", "a\n", "b\n"),
            ("theirs changed", "a\n", "a\n", "c\n", "c\n"),
            ("identical", "same\n", "same\n", "same\n", "same\n"),
            (
                "disjoint regions",
                "line1\nline2\nline3\n",
                "LINE1\nline2\nline3\n",
                "line1\nline2\nLINE3\n",
                "LINE1\nline2\nLINE3\n",
            ),
            (
                "far apart",
                "a\nb\nc\nd\ne\n",
                "A\nb\nc\nd\ne\n",
                "a\nb\nc\nd\nE\n",
                "A\nb\nc\nd\nE\n",
            ),
            (
                "same edit both",
                "a\nb\nc\n",
                "a\nX\nc\n",
                "a\nX\nc\n",
                "a\nX\nc\n",
            ),
            ("all empty", "", "", "", ""),
            ("both same insert", "", "new\n", "new\n", "new\n"),
            ("both delete all", "old\n", "", "", ""),
            ("single line ours", "x\n", "y\n", "x\n", "y\n"),
            ("single line theirs", "x\n", "x\n", "z\n", "z\n"),
            ("no trailing nl", "a", "b", "a", "b"),
            ("only newlines", "\n\n\n", "\n\n\n", "\n\n\n", "\n\n\n"),
            (
                "ours deletes theirs modifies",
                "a\nb\nc\nd\n",
                "b\nc\nd\n",
                "a\nb\nc\nD\n",
                "b\nc\nD\n",
            ),
            (
                "insert different positions",
                "mid\n",
                "top\nmid\n",
                "mid\nbot\n",
                "top\nmid\nbot\n",
            ),
            (
                "adjacent edits",
                "a\nb\nc\n",
                "A\nb\nc\n",
                "a\nB\nc\n",
                "A\nB\nc\n",
            ),
            (
                "whitespace change",
                "line\n",
                "line \n",
                "line\n",
                "line \n",
            ),
            ("ours empty theirs keeps", "content\n", "", "content\n", ""),
            ("theirs empty ours keeps", "content\n", "content\n", "", ""),
        ] {
            assert_clean(base, ours, theirs, expected);
            let _ = label;
        }
    }

    #[test]
    fn merge_three_way_conflict_cases() {
        // Basic conflict.
        let (conflict, ours_wins) = assert_conflict("a\nb\n", "a\nO\n", "a\nT\n");
        assert!(conflict.contains("<<<<<<< ours"));
        assert!(conflict.contains("O\n"));
        assert!(conflict.contains("T\n"));
        assert!(conflict.contains(">>>>>>> theirs"));
        assert_eq!(ours_wins, "a\nO\n");

        // Empty base both insert.
        let (conflict, ours_wins) = assert_conflict("", "hello\n", "world\n");
        assert!(conflict.contains("<<<<<<< ours"));
        assert_eq!(ours_wins, "hello\n");

        // Completely different content.
        let (conflict, ours_wins) = assert_conflict("base\n", "alpha\n", "beta\n");
        assert!(conflict.contains("alpha\n"));
        assert!(conflict.contains("beta\n"));
        assert_eq!(ours_wins, "alpha\n");

        // Delete vs modify same line.
        let (conflict, _) = assert_conflict("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
        assert!(conflict.contains("<<<<<<< ours"));

        // Multiple non-adjacent conflicts.
        let (conflict, ours_wins) =
            assert_conflict("a\nb\nc\nd\ne\n", "A\nb\nc\nd\nE\n", "X\nb\nc\nd\nY\n");
        assert_eq!(conflict.matches("<<<<<<< ours").count(), 2);
        assert_eq!(ours_wins, "A\nb\nc\nd\nE\n");

        // Symmetry: both orderings produce conflict with swapped ours/theirs.
        let (c1, ow1) = assert_conflict("a\n", "X\n", "Y\n");
        let (c2, ow2) = assert_conflict("a\n", "Y\n", "X\n");
        assert_eq!(ow1, "X\n");
        assert_eq!(ow2, "Y\n");
        assert!(c1.contains("<<<<<<< ours"));
        assert!(c2.contains("<<<<<<< ours"));
    }

    #[test]
    fn diff_edits_detects_change_kinds() {
        for (base, current, base_start, base_end, replacement) in [
            ("a\nb\nc\n", "a\nX\nc\n", 1, 2, &["X\n"][..]),
            ("a\nc\n", "a\nb\nc\n", 1, 1, &["b\n"][..]),
            ("a\nb\nc\n", "a\nc\n", 1, 2, &[][..]),
        ] {
            let edits = diff_edits(base, current);
            assert_eq!(edits.len(), 1);
            assert_eq!(edits[0].base_start, base_start);
            assert_eq!(edits[0].base_end, base_end);
            assert_eq!(edits[0].replacement, replacement.to_vec());
        }
    }

    #[test]
    fn edits_overlap_parameterized() {
        for (label, a, b, expected) in [
            (
                "same point",
                edit(2, 2, &["x\n"]),
                edit(2, 2, &["y\n"]),
                true,
            ),
            (
                "disjoint",
                edit(0, 1, &["x\n"]),
                edit(2, 3, &["y\n"]),
                false,
            ),
            (
                "adjacent",
                edit(0, 2, &["x\n"]),
                edit(2, 4, &["y\n"]),
                false,
            ),
            (
                "overlapping",
                edit(0, 3, &["x\n"]),
                edit(2, 5, &["y\n"]),
                true,
            ),
            (
                "zero inside range",
                edit(1, 4, &["R\n"]),
                edit(2, 2, &["I\n"]),
                true,
            ),
        ] {
            assert_eq!(edits_overlap(&a, &b), expected, "{label}");
            assert_eq!(edits_overlap(&b, &a), expected, "{label} symmetric");
        }
    }

    #[test]
    fn edits_identical_and_ensure_newline() {
        let a = edit(1, 2, &["X\n"]);
        assert!(edits_identical(&a, &edit(1, 2, &["X\n"])));
        assert!(!edits_identical(&a, &edit(1, 2, &["Y\n"])));
        assert!(!edits_identical(&a, &edit(1, 3, &["X\n"])));

        for (input, expected) in [("hello", "hello\n"), ("hello\n", "hello\n"), ("", "")] {
            let mut buf = input.to_owned();
            ensure_newline(&mut buf);
            assert_eq!(buf, expected, "ensure_newline({input:?})");
        }
    }

    #[test]
    fn render_range_with_edits_all_operations() {
        let base = vec!["a\n", "b\n", "c\n", "d\n", "e\n"];
        // Replacement.
        assert_eq!(
            render_range_with_edits(&base, 0, 3, &[edit(1, 2, &["X\n"])]),
            "a\nX\nc\n"
        );
        // No edits.
        assert_eq!(render_range_with_edits(&base, 0, 3, &[]), "a\nb\nc\n");
        // Insertion.
        assert_eq!(
            render_range_with_edits(&base, 0, 3, &[edit(1, 1, &["NEW\n"])]),
            "a\nNEW\nb\nc\n"
        );
        // Deletion.
        assert_eq!(
            render_range_with_edits(&base, 0, 3, &[edit(1, 2, &[])]),
            "a\nc\n"
        );
        // Multiple edits.
        assert_eq!(
            render_range_with_edits(&base, 0, 5, &[edit(1, 2, &["B\n"]), edit(3, 4, &["D\n"])]),
            "a\nB\nc\nD\ne\n"
        );
        // Subrange.
        assert_eq!(render_range_with_edits(&base, 1, 3, &[]), "b\nc\n");
    }

    #[test]
    fn merge_large_identical_change() {
        use std::fmt::Write;
        // Both sides make the same large change → clean.
        for size in [100, 1000] {
            let mut base = String::new();
            let mut modified = String::new();
            for i in 0..size {
                let _ = writeln!(base, "line {i}");
                let _ = writeln!(modified, "CHANGED {i}");
            }
            assert_clean(&base, &modified, &modified, &modified);
        }
    }

    #[test]
    fn merge_max_lines_fallback_produces_whole_file_conflict() {
        // Documents exceeding MAX_MERGE_LINES should produce a whole-file conflict
        // without attempting line-level merge.
        use std::fmt::Write;
        let mut base = String::new();
        let mut ours = String::new();
        let mut theirs = String::new();
        for i in 0..20_001 {
            let _ = writeln!(base, "line {i}");
            let _ = writeln!(ours, "ours {i}");
            let _ = writeln!(theirs, "theirs {i}");
        }
        let (conflict, ours_wins) = assert_conflict(&base, &ours, &theirs);
        assert!(conflict.starts_with("<<<<<<< ours\n"));
        assert!(conflict.contains("=======\n"));
        assert!(conflict.ends_with(">>>>>>> theirs\n"));
        assert_eq!(ours_wins, ours);
    }

    #[test]
    fn merge_large_file_appended_falls_back_to_whole_file_conflict() {
        use std::fmt::Write;
        let mut base = String::new();
        for i in 0..25_000 {
            writeln!(base, "line {i}").unwrap_or_default();
        }
        let mut ours = base.clone();
        writeln!(ours, "our addition").unwrap_or_default();
        let mut theirs = base.clone();
        writeln!(theirs, "their addition").unwrap_or_default();
        let (conflict, ours_wins) = assert_conflict(&base, &ours, &theirs);
        assert!(conflict.contains("<<<<<<< ours"));
        assert!(conflict.contains(">>>>>>> theirs"));
        assert_eq!(ours_wins, ours);
    }

    #[test]
    fn merge_multiple_disjoint_and_truncated_edits() {
        // Disjoint edits merge cleanly.
        let base = "aaa\nbbb\nccc\nddd\neee\n";
        let ours = "AAA\nbbb\nccc\nddd\neee\n";
        let theirs = "aaa\nbbb\nccc\nddd\nEEE\n";
        match merge_three_way(base, ours, theirs) {
            Merge3Outcome::Clean(result) => {
                assert!(result.contains("AAA") && result.contains("EEE"));
            }
            Merge3Outcome::Conflicted { .. } => panic!("disjoint edits should merge cleanly"),
        }

        // Disk truncated to empty while user has additions.
        let base2 = "original content\n";
        let ours2 = "original content\nour addition\n";
        match merge_three_way(base2, ours2, "") {
            Merge3Outcome::Conflicted { ours_wins, .. } => {
                assert_eq!(ours_wins, ours2);
            }
            Merge3Outcome::Clean(result) => {
                assert!(result.contains("our addition"));
            }
        }
    }

    // ── Security / Fuzz Tests ────────────────────────────────────────

    #[test]
    fn fuzz_merge_adversarial_inputs() {
        // All three identical.
        assert_eq!(
            merge_three_way("same", "same", "same"),
            Merge3Outcome::Clean("same".to_owned())
        );

        // Various adversarial inputs: must not panic.
        let adversarial: Vec<(&str, &str, &str)> = vec![
            ("", "", ""),
            ("", "new", ""),
            ("", "", "new"),
            ("base", "", ""),
            ("a\0b", "a\0c", "a\0d"),
            ("a", "b", "c"),
            ("a", "a", "b"),
            ("a", "b", "a"),
            ("x", "y", "z"),
            ("\n\n\n", "\n\n\n\n", "\n\n"),
            // CRLF mixed endings.
            (
                "line1\r\nline2\r\nline3\r\n",
                "line1\nline2\nline3\n",
                "line1\r\nmodified\r\nline3\r\n",
            ),
            // Unicode boundaries.
            (
                "héllo wörld\ncafé résumé\n",
                "HÉLLO wörld\ncafé résumé\n",
                "héllo wörld\nCAFÉ résumé\n",
            ),
            // Pre-existing conflict markers.
            (
                "normal\n<<<<<<< ours\n=======\n>>>>>>> theirs\n",
                "changed\n<<<<<<< ours\n=======\n>>>>>>> theirs\n",
                "normal\n<<<<<<< ours\nmodified\n>>>>>>> theirs\n",
            ),
        ];
        for (base, ours, theirs) in &adversarial {
            let result = merge_three_way(base, ours, theirs);
            match result {
                Merge3Outcome::Clean(_) | Merge3Outcome::Conflicted { .. } => {}
            }
        }

        // Very long identical lines (deduplication stress).
        let long_line = "x".repeat(100_000) + "\n";
        let base = long_line.repeat(10);
        let _ = merge_three_way(&base, &base, &base);

        // Binary-like content.
        let bin_base = (0..256)
            .map(|b| (b % 128) as u8 as char)
            .collect::<String>();
        let bin_ours = bin_base.clone() + "ours";
        let bin_theirs = bin_base.clone() + "theirs";
        let result = merge_three_way(&bin_base, &bin_ours, &bin_theirs);
        match result {
            Merge3Outcome::Clean(text) => assert!(!text.is_empty()),
            Merge3Outcome::Conflicted {
                conflict_marked,
                ours_wins,
            } => {
                assert!(!conflict_marked.is_empty() && !ours_wins.is_empty());
            }
        }
    }

    // ── Chaos tests ──────────────────────────────────────────────────

    #[test]
    fn render_range_with_edits_edge_cases() {
        // Empty base.
        assert_eq!(render_range_with_edits(&[], 0, 0, &[]), "");
        // end > base.len() clamped.
        let base = vec!["a\n", "b\n"];
        assert_eq!(render_range_with_edits(&base, 0, 100, &[]), "a\nb\n");
        // start > end → empty.
        assert_eq!(render_range_with_edits(&base, 50, 100, &[]), "");
        // edit beyond base clamped.
        let edits = vec![Edit {
            base_start: 100,
            base_end: 200,
            replacement: vec!["x\n"],
        }];
        assert_eq!(render_range_with_edits(&["a\n"], 0, 1, &edits), "a\nx\n");
    }

    #[test]
    fn diff_edits_empty_inputs() {
        assert!(diff_edits("", "").is_empty());
        assert!(!diff_edits("", "hello\nworld\n").is_empty());
        assert!(!diff_edits("hello\nworld\n", "").is_empty());
    }
}
