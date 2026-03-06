#![forbid(unsafe_code)]

use std::{borrow::Cow, fs, path::Path};

use crate::markdown_fence::{FenceState, consume_fence_delimiter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndOfLine {
    Lf,
    CrLf,
}

#[derive(Clone, Copy, Debug)]
pub struct FormatOptions {
    pub trim_trailing_whitespace: bool,
    pub insert_final_newline: bool,
    pub end_of_line: Option<EndOfLine>,
}

const DEFAULT_OPTIONS: FormatOptions = FormatOptions {
    trim_trailing_whitespace: true,
    insert_final_newline: true,
    end_of_line: None,
};

#[must_use]
pub fn format_markdown(source: &str, options: FormatOptions) -> String {
    let eol = match options.end_of_line {
        Some(EndOfLine::CrLf) => "\r\n",
        #[allow(clippy::match_same_arms)]
        Some(EndOfLine::Lf) => "\n",
        None if source.contains("\r\n") => "\r\n",
        None => "\n",
    };
    let normalized = if memchr::memchr(b'\r', source.as_bytes()).is_some() {
        // Single-pass normalization: \r\n → \n, lone \r → \n.
        // Uses memchr to find each CR quickly, then copies contiguous byte
        // ranges to preserve multi-byte UTF-8 sequences.
        let mut result = String::with_capacity(source.len());
        let bytes = source.as_bytes();
        let mut start = 0;
        for cr_pos in memchr::memchr_iter(b'\r', bytes) {
            result.push_str(&source[start..cr_pos]);
            result.push('\n');
            start = if bytes.get(cr_pos + 1) == Some(&b'\n') {
                cr_pos + 2
            } else {
                cr_pos + 1
            };
        }
        result.push_str(&source[start..]);
        Cow::Owned(result)
    } else {
        Cow::Borrowed(source)
    };
    // Pre-allocate conservatively: CRLF output may grow by up to 1 byte per line.
    let extra = if eol.len() > 1 {
        normalized.len() / 40
    } else {
        2
    };
    let mut out = String::with_capacity(normalized.len() + extra);
    let mut in_fence: Option<FenceState> = None;
    let mut segments = normalized.split('\n').peekable();
    while let Some(line) = segments.next() {
        let is_fence_delimiter = consume_fence_delimiter(line, &mut in_fence);
        if options.trim_trailing_whitespace && in_fence.is_none() && !is_fence_delimiter {
            let hard_break = line.ends_with("  ");
            out.push_str(line.trim_end_matches([' ', '\t']));
            if hard_break {
                out.push_str("  ");
            }
        } else {
            out.push_str(line);
        }

        if segments.peek().is_some() {
            out.push_str(eol);
        }
    }
    if options.insert_final_newline && !out.ends_with(eol) {
        out.push_str(eol);
    }
    out
}

#[must_use]
pub fn options_for_path(path: Option<&Path>) -> FormatOptions {
    let mut opts = DEFAULT_OPTIONS;
    let Some(path) = path else {
        return opts;
    };
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let Some(mut dir) = path.parent() else {
        return opts;
    };

    let (mut trim, mut insert, mut eol) = (None, None, None);
    loop {
        if let Ok(contents) = fs::read_to_string(dir.join(".editorconfig")) {
            let overrides = editorconfig_overrides(contents.as_str(), file);
            trim = trim.or(overrides.trim);
            insert = insert.or(overrides.insert);
            eol = eol.or(overrides.eol);
            if overrides.root {
                break;
            }
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent;
    }

    if let Some(v) = trim {
        opts.trim_trailing_whitespace = v;
    }
    if let Some(v) = insert {
        opts.insert_final_newline = v;
    }
    opts.end_of_line = eol;
    opts
}

#[derive(Default)]
struct Overrides {
    root: bool,
    trim: Option<bool>,
    insert: Option<bool>,
    eol: Option<EndOfLine>,
}

fn editorconfig_overrides(contents: &str, file: &str) -> Overrides {
    let mut overrides = Overrides::default();
    let mut section_matches = false;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(pat) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section_matches = section_match(pat, file);
            continue;
        }
        let Some((key, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.eq_ignore_ascii_case("root") {
            overrides.root = value.eq_ignore_ascii_case("true");
            continue;
        }
        if !section_matches {
            continue;
        }
        match key {
            key if key.eq_ignore_ascii_case("trim_trailing_whitespace") => {
                overrides.trim = parse_bool(value);
            }
            key if key.eq_ignore_ascii_case("insert_final_newline") => {
                overrides.insert = parse_bool(value);
            }
            key if key.eq_ignore_ascii_case("end_of_line") => overrides.eol = parse_eol(value),
            _ => {}
        }
    }
    overrides
}

const fn parse_bool(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn parse_eol(value: &str) -> Option<EndOfLine> {
    value
        .eq_ignore_ascii_case("lf")
        .then_some(EndOfLine::Lf)
        .or_else(|| {
            value
                .eq_ignore_ascii_case("crlf")
                .then_some(EndOfLine::CrLf)
        })
}

fn section_match(pattern: &str, file: &str) -> bool {
    let pattern = pattern.trim();
    let ext_matches = || {
        let path = Path::new(file);
        path.extension().is_some_and(|ext| {
            ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
        })
    };
    pattern == "*.{md,markdown}" && ext_matches() || glob_match(pattern, file)
}

fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" || pattern == text {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }
    let mut text = text;
    let mut parts = pattern.split('*');
    let Some(start) = parts.next() else {
        return false;
    };
    let Some(end) = parts.next_back() else {
        return false;
    };
    if !pattern.starts_with('*') {
        let Some(rest) = text.strip_prefix(start) else {
            return false;
        };
        text = rest;
    }
    if !pattern.ends_with('*') {
        let Some(rest) = text.strip_suffix(end) else {
            return false;
        };
        text = rest;
    }
    parts.filter(|part| !part.is_empty()).all(|part| {
        text.find(part)
            .map(|i| text = &text[i + part.len()..])
            .is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        process,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir_path(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("rustdown-{label}-{}-{stamp}", process::id()))
    }

    fn write_text(path: &Path, contents: &str) {
        assert!(fs::write(path, contents).is_ok());
    }

    #[test]
    fn format_markdown_covers_whitespace_fence_and_eol_behaviors() {
        let explicit_lf = FormatOptions {
            trim_trailing_whitespace: false,
            insert_final_newline: false,
            end_of_line: Some(EndOfLine::Lf),
        };
        for (source, options, expected) in [
            (
                "plain \nhard break  \n```\ncode   \n```\n",
                DEFAULT_OPTIONS,
                "plain\nhard break  \n```\ncode   \n```\n",
            ),
            (
                "~~~azurecli\naz aks list   \n~~~\n",
                DEFAULT_OPTIONS,
                "~~~azurecli\naz aks list   \n~~~\n",
            ),
            ("a\r\nb\rc", explicit_lf, "a\nb\nc"),
            ("a\r\nb", DEFAULT_OPTIONS, "a\r\nb\r\n"),
        ] {
            assert_eq!(format_markdown(source, options), expected);
        }
    }

    #[test]
    fn options_for_path_editorconfig_resolution() {
        // Nearest editorconfig with root=true stops upward search.
        let root = temp_dir_path("editorconfig-root");
        let nested = root.join("nested");
        assert!(fs::create_dir_all(&nested).is_ok());
        write_text(
            &root.join(".editorconfig"),
            "[*.md]\ntrim_trailing_whitespace = true\ninsert_final_newline = false\nend_of_line = lf\n",
        );
        write_text(
            &nested.join(".editorconfig"),
            "root = true\n[*.md]\ntrim_trailing_whitespace = false\nend_of_line = crlf\n",
        );
        let file = nested.join("note.md");
        write_text(&file, "# note");
        let options = options_for_path(Some(&file));
        assert!(!options.trim_trailing_whitespace);
        assert!(options.insert_final_newline);
        assert_eq!(options.end_of_line, Some(EndOfLine::CrLf));
        let _ = fs::remove_dir_all(&root);

        // Braced markdown pattern.
        let root = temp_dir_path("editorconfig-pattern");
        assert!(fs::create_dir_all(&root).is_ok());
        write_text(
            &root.join(".editorconfig"),
            "[*.{md,markdown}]\ninsert_final_newline = false\n",
        );
        let file = root.join("readme.markdown");
        write_text(&file, "content");
        assert!(!options_for_path(Some(&file)).insert_final_newline);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn glob_match_parameterized() {
        for (label, pattern, input, expected) in [
            ("wildcard", "*", "anything", true),
            ("exact match", "exact", "exact", true),
            ("exact miss", "exact", "other", false),
            ("suffix *.md", "*.md", "README.md", true),
            ("suffix miss", "*.md", "README.txt", false),
            ("prefix", "test*", "testing", true),
            ("prefix miss", "test*", "other", false),
            ("middle", "a*c", "abc", true),
            ("middle long", "a*c", "aXYZc", true),
            ("middle miss", "a*c", "aXYZd", false),
            ("no wildcard miss", "abc", "def", false),
            ("multi star", "a*b*c", "aXbYc", true),
            ("multi star miss", "a*b*c", "aXbYd", false),
            ("star both ends", "*test*", "prefix-test-suffix", true),
            ("empty both", "", "", true),
            ("star short", "a*", "a", true),
        ] {
            assert_eq!(glob_match(pattern, input), expected, "{label}");
        }
    }

    #[test]
    fn parse_helpers_and_section_matching() {
        // parse_bool.
        for (input, expected) in [
            ("true", Some(true)),
            ("false", Some(false)),
            ("TRUE", Some(true)),
            ("False", Some(false)),
            ("", None),
            ("yes", None),
        ] {
            assert_eq!(parse_bool(input), expected, "parse_bool({input:?})");
        }
        // parse_eol.
        for (input, expected) in [
            ("lf", Some(EndOfLine::Lf)),
            ("crlf", Some(EndOfLine::CrLf)),
            ("LF", Some(EndOfLine::Lf)),
            ("CRLF", Some(EndOfLine::CrLf)),
            ("cr", None),
            ("", None),
        ] {
            assert_eq!(parse_eol(input), expected, "parse_eol({input:?})");
        }
        // options_for_path defaults.
        let opts = options_for_path(None);
        assert!(opts.trim_trailing_whitespace && opts.insert_final_newline);
        assert_eq!(opts.end_of_line, None);

        // section_match.
        assert!(section_match("*.md", "test.md"));
        assert!(section_match("*.{md,markdown}", "test.MD"));
        assert!(section_match("*.{md,markdown}", "test.markdown"));
        assert!(!section_match("*.txt", "test.md"));

        // editorconfig_overrides.
        let ov = editorconfig_overrides("[*.md]\ntrim_trailing_whitespace = false\n", "notes.md");
        assert_eq!(ov.trim, Some(false));
        let ov2 = editorconfig_overrides("[*.md]\ntrim_trailing_whitespace = false\n", "notes.txt");
        assert_eq!(ov2.trim, None);
    }

    // ── CRLF normalization edge cases ───────────────────────────────

    #[test]
    fn format_crlf_normalization_cases() {
        let opts = FormatOptions {
            trim_trailing_whitespace: false,
            insert_final_newline: false,
            end_of_line: Some(EndOfLine::Lf),
        };
        let cases = [
            // (input, expected, description)
            ("\r\n\r\n\r\n", "\n\n\n", "CRLF-only"),
            ("a\rb\rc", "a\nb\nc", "CR-only"),
            ("a\r\nb\rc\nd", "a\nb\nc\nd", "mixed CR/CRLF/LF"),
            ("café\r\nwörld\r\n", "café\nwörld\n", "unicode with CRLF"),
            ("日本語\r\n中文\r\n", "日本語\n中文\n", "CJK with CRLF"),
            ("🦀\r\n🎉\r\n", "🦀\n🎉\n", "emoji with CRLF"),
            ("über\rcool", "über\ncool", "unicode with lone CR"),
            ("hello\r", "hello\n", "trailing CR at EOF"),
            ("\r", "\n", "single CR"),
            // Multibyte round-trip cases (merged from fuzz_format_crlf_preserves_multibyte).
            ("héllo\r\nwörld\r\n", "héllo\nwörld\n", "latin diacritics"),
            (
                "café\r\nnaïve\r\nrésumé\r\n",
                "café\nnaïve\nrésumé\n",
                "french diacritics",
            ),
            ("Ω≈ç√∫\r\n≤≥÷\r\n", "Ω≈ç√∫\n≤≥÷\n", "math symbols"),
        ];
        for (input, expected, desc) in cases {
            let result = format_markdown(input, opts);
            assert_eq!(result, expected, "{desc}");
            assert!(!result.contains('\r'), "leftover CR for: {desc}");
        }
    }

    #[test]
    fn format_empty_final_newline_and_mixed_eol() {
        let opts_no_nl = FormatOptions {
            trim_trailing_whitespace: true,
            insert_final_newline: false,
            end_of_line: Some(EndOfLine::Lf),
        };
        assert_eq!(format_markdown("", opts_no_nl), "");
        // Hard break preserved with no final newline.
        assert_eq!(format_markdown("hello  ", opts_no_nl), "hello  ");

        let opts_nl = FormatOptions {
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            end_of_line: Some(EndOfLine::Lf),
        };
        assert_eq!(format_markdown("", opts_nl), "\n");

        // CRLF end_of_line.
        let opts_crlf = FormatOptions {
            trim_trailing_whitespace: false,
            insert_final_newline: true,
            end_of_line: Some(EndOfLine::CrLf),
        };
        assert_eq!(format_markdown("a\nb", opts_crlf), "a\r\nb\r\n");

        // Mixed CRLF/CR/LF in a single document.
        let opts_lf = FormatOptions {
            trim_trailing_whitespace: false,
            insert_final_newline: false,
            end_of_line: Some(EndOfLine::Lf),
        };
        assert_eq!(
            format_markdown("line1\r\nline2\rline3\n", opts_lf),
            "line1\nline2\nline3\n"
        );
    }

    #[test]
    fn format_preservation_and_no_op_cases() {
        let opts = FormatOptions {
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            end_of_line: Some(EndOfLine::Lf),
        };
        for (label, source, expected) in [
            (
                "already formatted",
                "# Hello\n\nParagraph text.\n",
                "# Hello\n\nParagraph text.\n",
            ),
            (
                "preserves code block spaces",
                "```\n  code  \n```\n",
                "```\n  code  \n```\n",
            ),
            ("preserves hard break", "line  \nnext\n", "line  \nnext\n"),
            (
                "crlf normalization only",
                "hello\r\nworld\r\n",
                "hello\nworld\n",
            ),
            (
                "fence at eof gets newline",
                "text\n```\ncode\n```",
                "text\n```\ncode\n```\n",
            ),
        ] {
            assert_eq!(format_markdown(source, opts), expected, "{label}");
        }
        // 4-space-indented pseudo-fence: trailing whitespace still trimmed.
        assert_eq!(
            format_markdown("    ```\ntrailing tab\t\n    ```\n", DEFAULT_OPTIONS),
            "    ```\ntrailing tab\n    ```\n",
            "indented pseudo-fence"
        );
    }

    // ── Security / Fuzz Tests ────────────────────────────────────────

    #[test]
    fn fuzz_format_adversarial_inputs() {
        let cases = [
            "",                                        // empty
            "\0\0\0",                                  // null bytes
            "\r",                                      // lone CR
            "\r\n",                                    // CRLF only
            "\r\r\r",                                  // multiple CRs
            "\n\n\n\n\n",                              // only newlines
            "\r\n\r\n\r\n",                            // only CRLFs
            "\r\n\r\r\n\n",                            // mixed line endings
            "\t\t\t\t",                                // only tabs
            " \t \t \t ",                              // mixed whitespace
            "```\n\r\n\r```",                          // fence with mixed endings
            &"x".repeat(1_000_000),                    // 1MB single line
            &("line\n".repeat(100_000)),               // 100K lines
            &"\r\n".repeat(50_000),                    // 50K empty CRLF lines
            "\u{FEFF}# BOM heading\n",                 // byte-order mark
            "日本語テスト\t \n中文测试  \n한국어\t\n", // CJK with trailing whitespace
            // Fence boundary cases.
            "```\ncode\n```\n",
            "````\ncode\n````\n",
            "~~~\ncode\n~~~\n",
            "```rust\nfn main() {}\n```\n",
            "```\n```\n```\n```\n",      // rapid open/close
            "```\nunclosed fence\n",     // unclosed
            "   ```\n   code\n   ```\n", // indented fence
        ];
        for input in &cases {
            let result = format_markdown(input, DEFAULT_OPTIONS);
            let _ = result.len();
        }
    }

    // ── Diagnostic: format round-trip on bundled documents ──

    #[test]
    fn diag_format_round_trip_demo_md() {
        let demo = include_str!("bundled/demo.md");
        let opts = FormatOptions {
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            end_of_line: Some(EndOfLine::Lf),
        };
        let first = format_markdown(demo, opts);
        let second = format_markdown(&first, opts);
        assert_eq!(
            first, second,
            "format_markdown should be idempotent on demo.md"
        );
    }

    #[test]
    fn diag_format_round_trip_verification_md() {
        let verif = include_str!("bundled/verification.md");
        let opts = FormatOptions {
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            end_of_line: Some(EndOfLine::Lf),
        };
        let first = format_markdown(verif, opts);
        let second = format_markdown(&first, opts);
        assert_eq!(
            first, second,
            "format_markdown should be idempotent on verification.md"
        );
    }

    #[test]
    fn diag_format_preserves_tables() {
        let table =
            "| Left | Center | Right |\n|:-----|:------:|------:|\n| a    | b      |     c |\n";
        let result = format_markdown(table, DEFAULT_OPTIONS);
        // Table alignment markers must survive
        assert!(
            result.contains(":--"),
            "table alignment markers lost: {result:?}"
        );
        assert!(
            result.contains("--:"),
            "right-align marker lost: {result:?}"
        );
        // Pipes must survive
        assert_eq!(
            result.matches('|').count(),
            table.matches('|').count(),
            "pipe count changed"
        );
    }

    #[test]
    fn diag_format_preserves_blockquotes() {
        let bq = "> Level one.\n>\n> > Level two.\n>\n> Back to one.\n";
        let result = format_markdown(bq, DEFAULT_OPTIONS);
        assert_eq!(
            result.matches('>').count(),
            bq.matches('>').count(),
            "blockquote markers changed"
        );
    }

    #[test]
    fn diag_format_preserves_nested_lists() {
        let list = "- Item one\n  - Nested A\n    - Deep nested\n  - Nested B\n- Item two\n";
        let result = format_markdown(list, DEFAULT_OPTIONS);
        assert_eq!(result, list, "nested list structure should be preserved");
    }

    #[test]
    fn diag_format_preserves_fenced_code_content() {
        // Content inside fences must be byte-for-byte preserved,
        // including trailing whitespace.
        let code = "```rust\nfn main() {   \n    println!(\"hello\");  \n}\n```\n";
        let result = format_markdown(code, DEFAULT_OPTIONS);
        assert!(
            result.contains("fn main() {   "),
            "code block trailing spaces stripped: {result:?}"
        );
        assert!(
            result.contains("    println!"),
            "code block indentation stripped: {result:?}"
        );
    }

    #[test]
    fn diag_format_hard_break_preserved_in_blockquote() {
        // Hard break (trailing two spaces) inside a blockquote line.
        let bq = "> First line  \n> Second line\n";
        let result = format_markdown(bq, DEFAULT_OPTIONS);
        // The hard break should be preserved after trimming.
        assert!(
            result.contains("line  \n"),
            "hard break inside blockquote lost: {result:?}"
        );
    }
}
