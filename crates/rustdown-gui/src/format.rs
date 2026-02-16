#![forbid(unsafe_code)]

use std::{borrow::Cow, fs, path::Path};

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
    let mut in_fence = false;
    let mut segments = normalized.split('\n').peekable();
    while let Some(line) = segments.next() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if options.trim_trailing_whitespace && !in_fence {
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
