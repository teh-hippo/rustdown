#![forbid(unsafe_code)]

use std::{borrow::Cow, fs, path::Path};

use crate::markdown_fence::{FenceState, consume_fence_delimiter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndOfLine {
    Lf,
    CrLf,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FormatOptions {
    pub(crate) trim_trailing_whitespace: bool,
    pub(crate) insert_final_newline: bool,
    pub(crate) end_of_line: Option<EndOfLine>,
}

const DEFAULT_OPTIONS: FormatOptions = FormatOptions {
    trim_trailing_whitespace: true,
    insert_final_newline: true,
    end_of_line: None,
};

#[must_use]
pub(crate) fn format_markdown(source: &str, options: FormatOptions) -> String {
    let eol = match options.end_of_line {
        Some(EndOfLine::CrLf) => "\r\n",
        Some(EndOfLine::Lf) => "\n",
        None if source.contains("\r\n") => "\r\n",
        None => "\n",
    };
    let normalized = if source.contains('\r') {
        Cow::Owned(source.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(source)
    };
    let mut out = String::with_capacity(normalized.len() + 2);
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
pub(crate) fn options_for_path(path: Option<&Path>) -> FormatOptions {
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
                overrides.trim = parse_bool(value)
            }
            key if key.eq_ignore_ascii_case("insert_final_newline") => {
                overrides.insert = parse_bool(value)
            }
            key if key.eq_ignore_ascii_case("end_of_line") => overrides.eol = parse_eol(value),
            _ => {}
        };
    }
    overrides
}

fn parse_bool(value: &str) -> Option<bool> {
    value
        .eq_ignore_ascii_case("true")
        .then_some(true)
        .or_else(|| value.eq_ignore_ascii_case("false").then_some(false))
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
    pattern == "*.{md,markdown}" && (file.ends_with(".md") || file.ends_with(".markdown"))
        || glob_match(pattern, file)
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
    fn format_markdown_trims_outside_fences_and_keeps_hard_breaks() {
        let source = "plain \nhard break  \n```\ncode   \n```\n";
        let formatted = format_markdown(source, DEFAULT_OPTIONS);
        assert_eq!(formatted, "plain\nhard break  \n```\ncode   \n```\n");
    }

    #[test]
    fn format_markdown_preserves_tilde_fence_content_whitespace() {
        let source = "~~~azurecli\naz aks list   \n~~~\n";
        let formatted = format_markdown(source, DEFAULT_OPTIONS);
        assert_eq!(formatted, source);
    }

    #[test]
    fn format_markdown_normalizes_cr_and_respects_explicit_lf() {
        let options = FormatOptions {
            trim_trailing_whitespace: false,
            insert_final_newline: false,
            end_of_line: Some(EndOfLine::Lf),
        };
        let formatted = format_markdown("a\r\nb\rc", options);
        assert_eq!(formatted, "a\nb\nc");
    }

    #[test]
    fn format_markdown_preserves_crlf_when_detected() {
        let formatted = format_markdown("a\r\nb", DEFAULT_OPTIONS);
        assert_eq!(formatted, "a\r\nb\r\n");
    }

    #[test]
    fn options_for_path_prefers_nearest_editorconfig_and_stops_at_root() {
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
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn options_for_path_supports_braced_markdown_pattern() {
        let root = temp_dir_path("editorconfig-pattern");
        assert!(fs::create_dir_all(&root).is_ok());
        write_text(
            &root.join(".editorconfig"),
            "[*.{md,markdown}]\ninsert_final_newline = false\n",
        );
        let file = root.join("readme.markdown");
        write_text(&file, "content");

        let options = options_for_path(Some(&file));

        assert!(!options.insert_final_newline);
        let _ = fs::remove_dir_all(root);
    }
}
