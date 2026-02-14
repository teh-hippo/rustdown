#![forbid(unsafe_code)]

//! Shared logic for `rustdown` (GUI + CLI).

pub mod markdown;

#[cfg(test)]
mod tests {
    #[test]
    fn plain_text_basic() {
        let md = "# Title\n\nHello **world**.\n\n- a\n- b\n";
        let got = super::markdown::plain_text(md);
        assert_eq!(got.trim_end(), "Title\nHello world.\na\nb");
    }
}
