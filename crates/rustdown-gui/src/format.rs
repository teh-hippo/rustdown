#![forbid(unsafe_code)]

use std::{fs, path::Path};

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

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            trim_trailing_whitespace: true,
            insert_final_newline: true,
            end_of_line: None,
        }
    }
}

pub(crate) fn format_markdown(source: &str, options: FormatOptions) -> String {
    let eol = match options.end_of_line {
        Some(EndOfLine::CrLf) => "\r\n",
        Some(EndOfLine::Lf) => "\n",
        None if source.contains("\r\n") => "\r\n",
        None => "\n",
    };
    let normalized = if source.contains('\r') {
        source.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        source.to_owned()
    };

    let mut out = String::with_capacity(normalized.len() + 2);
    let mut in_fence = false;

    let mut segments = normalized.split('\n').peekable();
    while let Some(line) = segments.next() {
        if line.trim_start().starts_with("```") {
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
            out.push_str(eol);
        }
    }

    if options.insert_final_newline && !out.ends_with(eol) {
        out.push_str(eol);
    }

    out
}

pub(crate) fn options_for_path(path: Option<&Path>) -> FormatOptions {
    let mut opts = FormatOptions::default();
    let Some(path) = path else {
        return opts;
    };

    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let Some(mut dir) = path.parent() else {
        return opts;
    };

    let mut picked = Overrides::default();

    loop {
        let cfg_path = dir.join(".editorconfig");
        if let Ok(contents) = fs::read_to_string(&cfg_path) {
            let (root, overrides) = parse_editorconfig_overrides(&contents, file_name);
            picked.fill_missing(overrides);

            if root {
                break;
            }
        }

        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent;
    }

    if let Some(value) = picked.trim_trailing_whitespace {
        opts.trim_trailing_whitespace = value;
    }
    if let Some(value) = picked.insert_final_newline {
        opts.insert_final_newline = value;
    }
    opts.end_of_line = picked.end_of_line;

    opts
}

#[derive(Default, Clone, Copy)]
struct Overrides {
    trim_trailing_whitespace: Option<bool>,
    insert_final_newline: Option<bool>,
    end_of_line: Option<EndOfLine>,
}

impl Overrides {
    fn fill_missing(&mut self, other: Self) {
        if self.trim_trailing_whitespace.is_none() {
            self.trim_trailing_whitespace = other.trim_trailing_whitespace;
        }
        if self.insert_final_newline.is_none() {
            self.insert_final_newline = other.insert_final_newline;
        }
        if self.end_of_line.is_none() {
            self.end_of_line = other.end_of_line;
        }
    }
}

fn parse_editorconfig_overrides(contents: &str, file_name: &str) -> (bool, Overrides) {
    let mut root = false;
    let mut overrides = Overrides::default();
    let mut section_matches = false;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if let Some(inner) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section_matches = patterns_match(inner.trim(), file_name);
            continue;
        }

        let Some((key, value)) = split_key_value(line) else {
            continue;
        };

        if key.eq_ignore_ascii_case("root") {
            root = parse_bool(value).unwrap_or(false);
            continue;
        }

        if !section_matches {
            continue;
        }

        if key.eq_ignore_ascii_case("trim_trailing_whitespace") {
            overrides.trim_trailing_whitespace = parse_bool(value);
        } else if key.eq_ignore_ascii_case("insert_final_newline") {
            overrides.insert_final_newline = parse_bool(value);
        } else if key.eq_ignore_ascii_case("end_of_line") {
            overrides.end_of_line = parse_eol(value);
        }
    }

    (root, overrides)
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let idx = line.find('=').or_else(|| line.find(':'))?;
    let (key, rest) = line.split_at(idx);
    Some((key.trim(), rest.get(1..)?.trim()))
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

fn patterns_match(raw: &str, file_name: &str) -> bool {
    let raw = raw.trim();
    if raw.contains('{') {
        brace_pattern_match(raw, file_name)
    } else {
        raw.split(',').any(|p| glob_match(p.trim(), file_name))
    }
}

fn brace_pattern_match(pattern: &str, file_name: &str) -> bool {
    let Some(open) = pattern.find('{') else {
        return glob_match(pattern, file_name);
    };
    let Some(close_rel) = pattern[open + 1..].find('}') else {
        return glob_match(pattern, file_name);
    };

    let close = open + 1 + close_rel;
    let prefix = &pattern[..open];
    let suffix = pattern.get(close + 1..).unwrap_or_default();
    pattern[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|alt| !alt.is_empty())
        .any(|alt| {
            let mut expanded = String::with_capacity(prefix.len() + alt.len() + suffix.len());
            expanded.push_str(prefix);
            expanded.push_str(alt);
            expanded.push_str(suffix);
            glob_match(expanded.as_str(), file_name)
        })
}

fn glob_match(pattern: &str, mut text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }

    let mut parts = pattern.split('*');
    let start = parts.next().unwrap_or_default();
    let end = parts.next_back().unwrap_or_default();

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

    for seg in parts {
        if seg.is_empty() {
            continue;
        }
        let Some(found) = text.find(seg) else {
            return false;
        };
        text = &text[found + seg.len()..];
    }

    true
}
