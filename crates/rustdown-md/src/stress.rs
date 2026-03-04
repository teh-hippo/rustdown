#![forbid(unsafe_code)]
//! Stress-test data generators and performance benchmarks for `rustdown-md`.
//!
//! All functions here are `#[cfg(test)]` only.

use std::fmt::Write;

/// Build a large markdown document (~size KB) with mixed block types.
pub fn large_mixed_doc(target_kb: usize) -> String {
    let target_bytes = target_kb * 1024;
    let mut doc = String::with_capacity(target_bytes + 1024);
    let mut section = 0_u32;
    while doc.len() < target_bytes {
        section += 1;
        let _ = writeln!(doc, "# Section {section}\n");
        let _ = writeln!(
            doc,
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
             Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
             Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
             nisi ut aliquip ex ea commodo consequat.\n"
        );
        let _ = writeln!(doc, "## Subsection {section}.1\n");
        let _ = writeln!(
            doc,
            "Here is some **bold text** and *italic text* with `inline code` and \
             a [link](https://example.com). Also ~~strikethrough~~.\n"
        );
        let _ = writeln!(
            doc,
            "```rust\nfn example_{section}() {{\n    let x = 42;\n    println!(\"{{x}}\");\n}}\n```\n"
        );
        let _ = writeln!(
            doc,
            "- Item one with **formatting**\n- Item two with `code`\n- Item three\n  - Nested A\n  - Nested B\n"
        );
        let _ = writeln!(
            doc,
            "1. First ordered\n2. Second ordered\n3. Third ordered\n"
        );
        let _ = writeln!(
            doc,
            "> Blockquote with *emphasis* and **strong**.\n> Second line.\n"
        );
        let _ = writeln!(
            doc,
            "| Column A | Column B | Column C |\n|----------|----------|----------|\n| cell 1   | cell 2   | cell 3   |\n| cell 4   | cell 5   | cell 6   |\n"
        );
        let _ = writeln!(doc, "---\n");
    }
    doc
}

/// Build a document heavy on Unicode: CJK, emoji, combining marks, RTL, ZWJ sequences.
pub fn unicode_stress_doc(target_kb: usize) -> String {
    let target_bytes = target_kb * 1024;
    let mut doc = String::with_capacity(target_bytes + 1024);

    // CJK headings and paragraphs
    let _ = writeln!(doc, "# 日本語のテスト文書 🇯🇵\n");
    let _ = writeln!(
        doc,
        "これは日本語のテスト段落です。漢字、ひらがな、カタカナが混在しています。\n"
    );

    // Chinese
    let _ = writeln!(doc, "## 中文测试 🇨🇳\n");
    let _ = writeln!(doc, "这是一个中文测试段落。包含简体和繁體字符。\n");

    // Korean
    let _ = writeln!(doc, "## 한국어 테스트 🇰🇷\n");
    let _ = writeln!(
        doc,
        "한국어 테스트 단락입니다. 한글과 한자가 포함되어 있습니다.\n"
    );

    // Arabic (RTL)
    let _ = writeln!(doc, "## اختبار العربية 🇸🇦\n");
    let _ = writeln!(
        doc,
        "هذه فقرة اختبار باللغة العربية. النص من اليمين إلى اليسار.\n"
    );

    // Hebrew (RTL)
    let _ = writeln!(doc, "## בדיקת עברית 🇮🇱\n");
    let _ = writeln!(doc, "זהו פיסקת בדיקה בעברית. הטקסט מימין לשמאל.\n");

    // Emoji-heavy section
    let _ = writeln!(doc, "## Emoji Stress Test 🎉\n");
    let _ = writeln!(
        doc,
        "Flags: 🇦🇺 🇺🇸 🇬🇧 🇩🇪 🇫🇷 🇯🇵 🇰🇷 🇨🇳 🇧🇷 🇮🇳\n\
         ZWJ sequences: 👨‍👩‍👧‍👦 👩‍💻 🏳️‍🌈 👨‍🍳 👩‍🔬 🧑‍🤝‍🧑\n\
         Skin tones: 👋🏻 👋🏼 👋🏽 👋🏾 👋🏿\n\
         Misc: ♠️ ♥️ ♦️ ♣️ ⚡ 🔥 💀 ☠️ 🤖 👾\n"
    );

    // Combining marks and diacritics
    let _ = writeln!(doc, "## Combining Marks\n");
    let _ = writeln!(
        doc,
        "Zalgo: H̸̡̪̯ͨ͊̽̅̾̏E̵̡͐ C̸̨̣̣̩̓O̶̙̣̅M̵̨̆E̸̙͑S̷̰̽\n\
         Vietnamese: Việt Nam có rất nhiều dấu\n\
         Precomposed vs decomposed: é (precomposed) vs é (decomposed)\n\
         Thai: สวัสดีครับ/ค่ะ\n\
         Devanagari: नमस्ते दुनिया\n"
    );

    // Zero-width characters
    let _ = writeln!(doc, "## Zero-Width Characters\n");
    let _ = writeln!(
        doc,
        "ZWJ: a\u{200D}b\n\
         ZWNJ: a\u{200C}b\n\
         ZWSP: a\u{200B}b\n\
         Word joiner: a\u{2060}b\n\
         BOM: \u{FEFF}text after BOM\n\
         Soft hyphen: hyphe\u{00AD}nated\n"
    );

    // Mathematical symbols and special chars
    let _ = writeln!(doc, "## Math & Special Characters\n");
    let _ = writeln!(
        doc,
        "E = mc² (superscript via Unicode)\n\
         ∀x ∈ ℝ: x² ≥ 0\n\
         ∫₀^∞ e^(-x²) dx = √π/2\n\
         ℵ₀ < ℵ₁ (cardinal infinities)\n\
         ← → ↑ ↓ ↔ ⇐ ⇒ ⇑ ⇓ ⇔\n"
    );

    // Code blocks with unicode
    let _ = writeln!(
        doc,
        "```python\n# 日本語コメント\ndef greet(名前: str) -> str:\n    return f\"こんにちは、{{名前}}さん！\"\n```\n"
    );

    // Repeat to fill target size
    let base = doc.clone();
    let mut counter = 0_u32;
    while doc.len() < target_bytes {
        counter += 1;
        let _ = writeln!(doc, "### Repeat block {counter}\n");
        let end = base.len().min(target_bytes / 4);
        // Find a valid char boundary.
        let safe_end = base.floor_char_boundary(end);
        doc.push_str(&base[..safe_end]);
    }
    doc
}

/// Build a document with pathological patterns that stress parsers.
pub fn pathological_doc(target_kb: usize) -> String {
    let target_bytes = target_kb * 1024;
    let mut doc = String::with_capacity(target_bytes + 1024);

    // Deeply nested lists
    let _ = writeln!(doc, "# Deeply Nested Lists\n");
    for depth in 0..10 {
        let indent = "  ".repeat(depth);
        let _ = writeln!(doc, "{indent}- Level {depth} item");
    }
    doc.push('\n');

    // Long lines that need wrapping
    let _ = writeln!(doc, "# Very Long Lines\n");
    let long_word = "supercalifragilisticexpialidocious";
    for _ in 0..5 {
        for _ in 0..50 {
            doc.push_str(long_word);
            doc.push(' ');
        }
        doc.push('\n');
    }
    doc.push('\n');

    // Many small headings (stress nav outline)
    let _ = writeln!(doc, "# Heading Storm\n");
    for i in 0..200 {
        let level = (i % 6) + 1;
        let hashes = "#".repeat(level);
        let _ = writeln!(doc, "{hashes} Heading {i}\n");
        let _ = writeln!(doc, "Short paragraph.\n");
    }

    // Alternating inline formatting
    let _ = writeln!(doc, "# Inline Formatting Stress\n");
    for i in 0..100 {
        let _ = writeln!(
            doc,
            "Word{i} **bold{i}** *italic{i}* `code{i}` ~~strike{i}~~ \
             [link{i}](https://example.com/{i})"
        );
    }
    doc.push('\n');

    // Large table
    let _ = writeln!(doc, "# Large Table\n");
    let _ = write!(doc, "| ");
    for col in 0..10 {
        let _ = write!(doc, "Col {col} | ");
    }
    let _ = writeln!(doc);
    let _ = write!(doc, "| ");
    for _ in 0..10 {
        let _ = write!(doc, "--- | ");
    }
    let _ = writeln!(doc);
    for row in 0..50 {
        let _ = write!(doc, "| ");
        for col in 0..10 {
            let _ = write!(doc, "R{row}C{col} | ");
        }
        let _ = writeln!(doc);
    }
    doc.push('\n');

    // Many code blocks
    let _ = writeln!(doc, "# Code Block Storm\n");
    for i in 0..50 {
        let _ = writeln!(
            doc,
            "```\nCode block {i} with some content\nLine 2\nLine 3\n```\n"
        );
    }

    // Pad to target size
    while doc.len() < target_bytes {
        let _ = writeln!(
            doc,
            "Padding paragraph with **bold**, *italic*, and `code` to reach target size."
        );
    }
    doc
}

/// Single character documents and edge cases.
pub fn minimal_docs() -> Vec<(&'static str, String)> {
    vec![
        ("empty", String::new()),
        ("single_char", "x".to_owned()),
        ("single_newline", "\n".to_owned()),
        ("just_heading", "# H".to_owned()),
        ("heading_no_space", "#NotAHeading".to_owned()),
        ("only_whitespace", "   \n\n   \n".to_owned()),
        ("only_fence", "```\n```".to_owned()),
        ("unclosed_fence", "```\nno closing".to_owned()),
        ("bare_link", "https://example.com".to_owned()),
        ("bare_emoji", "🎉".to_owned()),
        ("null_byte_adjacent", "before\x00after".to_owned()),
        ("tab_heavy", "\t\t\t# Tabbed\n\t\tContent".to_owned()),
        ("crlf_endings", "Line1\r\nLine2\r\n# Head\r\n".to_owned()),
        ("mixed_endings", "Line1\nLine2\r\nLine3\rLine4".to_owned()),
        ("bom_prefix", "\u{FEFF}# BOM Heading".to_owned()),
    ]
}
