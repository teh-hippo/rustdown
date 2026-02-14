#![forbid(unsafe_code)]

//! Shared logic for `rustdown` (GUI + CLI).

pub mod markdown;

#[cfg(test)]
mod tests {
    #[test]
    fn plain_text_basic() {
        let md = "# Title\n\nHello **world**.\n\n- a\n- b\n";
        let got = super::markdown::plain_text(md);
        assert_eq!(got.trim_end(), "Title\nHello world.\n- a\n- b");
    }

    #[test]
    fn plain_text_code_block() {
        let md = "```rs\nlet x = 1;\n```\n";
        let got = super::markdown::plain_text(md);
        assert_eq!(got.trim_end(), "let x = 1;");
    }

    #[test]
    fn plain_text_rule() {
        let md = "a\n\n---\n\nb\n";
        let got = super::markdown::plain_text(md);
        assert_eq!(got.trim_end(), "a\n---\nb");
    }
}
