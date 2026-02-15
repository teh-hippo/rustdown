#![forbid(unsafe_code)]

use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndOfLine {
    Lf,
    CrLf,
}

impl EndOfLine {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            EndOfLine::Lf => "\n",
            EndOfLine::CrLf => "\r\n",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FormatOptions {
    pub(crate) trim_trailing_whitespace: bool,
    pub(crate) insert_final_newline: bool,
    pub(crate) end_of_line: Option<EndOfLine>,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            end_of_line: None, // preserve existing
        }
    }
}

pub(crate) fn format_markdown(source: &str, options: FormatOptions) -> String {
    let eol = options
        .end_of_line
        .unwrap_or_else(|| detect_end_of_line(source));
    let normalized = normalize_line_endings(source);

    // Preserve the number of newline *events* (split() keeps trailing empty segments).
    let mut out = String::with_capacity(normalized.len() + 2);
    let mut in_fence = false;

    let mut segments = normalized.split('\n').peekable();
    while let Some(line) = segments.next() {
        let trimmed_start = line.trim_start();
        if trimmed_start.starts_with("```") {
            in_fence = !in_fence;
        }

        if options.trim_trailing_whitespace && !in_fence {
            let hard_break = line
                .as_bytes()
                .iter()
                .rev()
                .take_while(|b| **b == b' ')
                .count()
                >= 2;
            let trimmed = line.trim_end_matches([' ', '\t']);
            out.push_str(trimmed);
            if hard_break {
                out.push_str("  ");
            }
        } else {
            out.push_str(line);
        }

        if segments.peek().is_some() {
            out.push_str(eol.as_str());
        }
    }

    if options.insert_final_newline && !out.ends_with(eol.as_str()) {
        out.push_str(eol.as_str());
    }

    out
}

pub(crate) fn options_for_path(path: Option<&Path>) -> FormatOptions {
    let mut options = FormatOptions::default();

    let Some(path) = path else {
        return options;
    };

    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let Some(mut dir) = path.parent() else {
        return options;
    };

    let mut parsed = Vec::new();
    loop {
        let cfg_path = dir.join(".editorconfig");
        if let Ok(contents) = fs::read_to_string(&cfg_path) {
            let cfg = parse_editorconfig(&contents);
            let root = cfg.root;
            parsed.push(cfg);
            if root {
                break;
            }
        }

        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent;
    }

    for cfg in parsed.iter().rev() {
        cfg.apply(file_name, &mut options);
    }

    options
}

fn detect_end_of_line(source: &str) -> EndOfLine {
    if source.contains("\r\n") {
        EndOfLine::CrLf
    } else {
        EndOfLine::Lf
    }
}

fn normalize_line_endings(source: &str) -> String {
    if !source.contains('\r') {
        return source.to_owned();
    }

    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                let _ = chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
struct EditorConfig {
    root: bool,
    sections: Vec<Section>,
}

impl EditorConfig {
    fn apply(&self, file_name: &str, options: &mut FormatOptions) {
        for section in &self.sections {
            if !section.matches(file_name) {
                continue;
            }

            if let Some(value) = section.trim_trailing_whitespace {
                options.trim_trailing_whitespace = value;
            }
            if let Some(value) = section.insert_final_newline {
                options.insert_final_newline = value;
            }
            if let Some(value) = section.end_of_line {
                options.end_of_line = Some(value);
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Section {
    patterns: Vec<String>,
    trim_trailing_whitespace: Option<bool>,
    insert_final_newline: Option<bool>,
    end_of_line: Option<EndOfLine>,
}

impl Section {
    fn matches(&self, file_name: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| glob_match(pattern.as_str(), file_name))
    }
}

fn parse_editorconfig(contents: &str) -> EditorConfig {
    let mut cfg = EditorConfig::default();
    let mut current: Option<Section> = None;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            if let Some(section) = current.take() {
                cfg.sections.push(section);
            }

            let inner = line[1..line.len() - 1].trim();
            current = Some(Section {
                patterns: expand_patterns(inner),
                trim_trailing_whitespace: None,
                insert_final_newline: None,
                end_of_line: None,
            });
            continue;
        }

        let Some((key, value)) = split_key_value(line) else {
            continue;
        };

        if key.eq_ignore_ascii_case("root") {
            cfg.root = parse_bool(value).unwrap_or(false);
            continue;
        }

        let Some(section) = current.as_mut() else {
            continue;
        };

        if key.eq_ignore_ascii_case("trim_trailing_whitespace") {
            section.trim_trailing_whitespace = parse_bool(value);
        } else if key.eq_ignore_ascii_case("insert_final_newline") {
            section.insert_final_newline = parse_bool(value);
        } else if key.eq_ignore_ascii_case("end_of_line") {
            section.end_of_line = parse_eol(value);
        }
    }

    if let Some(section) = current.take() {
        cfg.sections.push(section);
    }

    cfg
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let idx = line.find('=').or_else(|| line.find(':'))?;
    let (key, rest) = line.split_at(idx);
    let value = rest.get(1..)?;
    Some((key.trim(), value.trim()))
}

fn parse_bool(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn parse_eol(value: &str) -> Option<EndOfLine> {
    if value.eq_ignore_ascii_case("lf") {
        Some(EndOfLine::Lf)
    } else if value.eq_ignore_ascii_case("crlf") {
        Some(EndOfLine::CrLf)
    } else {
        None
    }
}

fn expand_patterns(raw: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    for part in split_on_commas(raw) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        patterns.extend(expand_braces(part));
    }
    patterns
}

fn split_on_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (idx, ch) in s.char_indices() {
        match ch {
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&s[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }

    out.push(&s[start..]);
    out
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(close) = pattern[open + 1..].find('}') else {
        return vec![pattern.to_owned()];
    };
    let close = open + 1 + close;

    let prefix = &pattern[..open];
    let suffix = pattern.get(close + 1..).unwrap_or_default();
    let inner = &pattern[open + 1..close];

    let mut out = Vec::new();
    for alt in inner.split(',') {
        let alt = alt.trim();
        if alt.is_empty() {
            continue;
        }
        out.push(format!("{prefix}{alt}{suffix}"));
    }

    if out.is_empty() {
        vec![pattern.to_owned()]
    } else {
        out
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if !pattern.contains('*') {
        return pattern == text;
    }

    let start_anchor = !pattern.starts_with('*');
    let end_anchor = !pattern.ends_with('*');

    let segments: Vec<&str> = pattern.split('*').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return true;
    }

    let mut idx = 0usize;
    for (seg_idx, seg) in segments.iter().enumerate() {
        let is_first = seg_idx == 0;
        let is_last = seg_idx + 1 == segments.len();

        if is_first && start_anchor {
            if !text.starts_with(seg) {
                return false;
            }
            idx = seg.len();
            continue;
        }

        if is_last && end_anchor {
            if seg.len() > text.len() {
                return false;
            }
            let end_pos = text.len() - seg.len();
            if end_pos < idx {
                return false;
            }
            return &text[end_pos..] == *seg;
        }

        let Some(found) = text.get(idx..).and_then(|rest| rest.find(seg)) else {
            return false;
        };
        idx = idx.saturating_add(found).saturating_add(seg.len());
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn glob_match_basics() {
        assert!(glob_match("*", "a.md"));
        assert!(glob_match("*.md", "a.md"));
        assert!(!glob_match("*.md", "a.rs"));
        assert!(glob_match("foo*", "foobar"));
        assert!(glob_match("*bar", "foobar"));
        assert!(glob_match("f*bar", "foobar"));
    }

    #[test]
    fn format_keeps_markdown_hardbreak() {
        let input = "a  \n";
        let got = format_markdown(
            input,
            FormatOptions {
                trim_trailing_whitespace: true,
                insert_final_newline: true,
                end_of_line: Some(EndOfLine::Lf),
            },
        );
        assert_eq!(got, input);
    }

    #[test]
    fn format_does_not_trim_in_fenced_code() {
        let input = "```rs\nlet x = 1;   \n```\n";
        let got = format_markdown(
            input,
            FormatOptions {
                trim_trailing_whitespace: true,
                insert_final_newline: true,
                end_of_line: Some(EndOfLine::Lf),
            },
        );
        assert_eq!(got, input);
    }

    #[test]
    fn format_converts_end_of_line() {
        let input = "a\nb\n";
        let got = format_markdown(
            input,
            FormatOptions {
                trim_trailing_whitespace: true,
                insert_final_newline: true,
                end_of_line: Some(EndOfLine::CrLf),
            },
        );
        assert_eq!(got, "a\r\nb\r\n");
    }

    #[test]
    fn editorconfig_section_applies() {
        let cfg = parse_editorconfig(
            r#"
root = true

[*]
trim_trailing_whitespace = false

[*.md]
insert_final_newline = false
end_of_line = crlf
"#,
        );

        let mut opts = FormatOptions::default();
        cfg.apply("note.md", &mut opts);
        assert!(!opts.trim_trailing_whitespace);
        assert!(!opts.insert_final_newline);
        assert_eq!(opts.end_of_line, Some(EndOfLine::CrLf));
    }

    #[test]
    fn editorconfig_brace_pattern_applies() {
        let cfg = parse_editorconfig(
            r#"
[*.{md,markdown}]
insert_final_newline = false
"#,
        );

        let mut opts = FormatOptions::default();
        cfg.apply("note.md", &mut opts);
        assert!(!opts.insert_final_newline);
    }

    #[test]
    fn options_for_path_merges_parent_and_child_editorconfig()
    -> Result<(), Box<dyn std::error::Error>> {
        let base = std::env::temp_dir();
        let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos(),
            Err(_) => 0,
        };

        let root: PathBuf = base.join(format!("rustdown-editorconfig-{nanos}"));
        let a = root.join("a");
        let b = a.join("b");
        fs::create_dir_all(&b)?;

        fs::write(
            a.join(".editorconfig"),
            r#"
root = true

[*]
trim_trailing_whitespace = false
"#,
        )?;
        fs::write(
            b.join(".editorconfig"),
            r#"
[*.md]
insert_final_newline = false
"#,
        )?;

        let file = b.join("note.md");
        fs::write(&file, "hello\n")?;

        let opts = options_for_path(Some(&file));
        assert!(!opts.trim_trailing_whitespace);
        assert!(!opts.insert_final_newline);

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
