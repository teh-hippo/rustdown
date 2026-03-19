use std::{fs, sync::Arc};

use eframe::egui;

// Bundled primary fonts (Source Pro family, OFL-licensed).
// See THIRD-PARTY-NOTICES.md for license details.
const BUNDLED_PROPORTIONAL: &str = "source-sans-3";
const BUNDLED_PROPORTIONAL_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceSans3-Regular.ttf");
const BUNDLED_PROPORTIONAL_BOLD: &str = "source-sans-3-bold";
const BUNDLED_PROPORTIONAL_BOLD_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceSans3-Bold.ttf");
const BUNDLED_MONOSPACE: &str = "source-code-pro";
const BUNDLED_MONOSPACE_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceCodePro-Regular.ttf");
const BUNDLED_MONOSPACE_BOLD: &str = "source-code-pro-bold";
const BUNDLED_MONOSPACE_BOLD_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceCodePro-Bold.ttf");

// Bundled symbol/emoji fallbacks shared by both families.
const BUNDLED_SYMBOL_FALLBACKS: &[(&str, &[u8])] = &[
    (
        "rustdown-symbola-subset",
        include_bytes!("../../../assets/fonts/rustdown-symbola-subset.ttf"),
    ),
    (
        "rustdown-noto-symbols2-subset",
        include_bytes!("../../../assets/fonts/rustdown-noto-symbols2-subset.ttf"),
    ),
];

// System font fallback paths for extended glyph coverage (proportional only).
#[cfg(target_os = "linux")]
const PROPORTIONAL_FALLBACK_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
    "/usr/share/fonts/truetype/unifont/unifont.ttf",
];
#[cfg(target_os = "macos")]
const PROPORTIONAL_FALLBACK_PATHS: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Supplemental/Symbol.ttf",
];
#[cfg(target_os = "windows")]
const PROPORTIONAL_FALLBACK_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
    r"C:\Windows\Fonts\seguiemj.ttf",
    r"C:\Windows\Fonts\seguisym.ttf",
    r"C:\Windows\Fonts\arialuni.ttf",
];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const PROPORTIONAL_FALLBACK_PATHS: &[&str] = &[];

const DEFAULT_BODY_BUTTON_FONT_SIZE: f32 = 19.0;
const DEFAULT_MONOSPACE_FONT_SIZE: f32 = 18.0;
const DEFAULT_SMALL_FONT_SIZE: f32 = 13.0;
const DEFAULT_SCROLL_ANIMATION_POINTS_PER_SECOND: f32 = 1150.0;

/// Load bundled fonts and configure font families.
///
/// The only failure path is a broken `RUSTDOWN_FONT_PATH` environment variable;
/// bundled fonts are always available since they are compiled into the binary.
pub fn configure_fonts(ctx: &egui::Context) -> Result<(), String> {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.clear();
    fonts.families.clear();

    // If RUSTDOWN_FONT_PATH is set, use it as the primary for both families.
    let env_override = load_env_font_override()?;

    // Insert bundled primary fonts (zero-copy from binary .rodata).
    insert_static(&mut fonts, BUNDLED_PROPORTIONAL, BUNDLED_PROPORTIONAL_DATA);
    insert_static(
        &mut fonts,
        BUNDLED_PROPORTIONAL_BOLD,
        BUNDLED_PROPORTIONAL_BOLD_DATA,
    );
    insert_static(&mut fonts, BUNDLED_MONOSPACE, BUNDLED_MONOSPACE_DATA);
    insert_static(
        &mut fonts,
        BUNDLED_MONOSPACE_BOLD,
        BUNDLED_MONOSPACE_BOLD_DATA,
    );

    // Build the proportional family chain.
    let mut proportional = Vec::new();
    if let Some((ref name, _)) = env_override {
        proportional.push(name.clone());
    }
    proportional.push(BUNDLED_PROPORTIONAL.to_owned());
    proportional.push(BUNDLED_PROPORTIONAL_BOLD.to_owned());

    // Build the monospace family chain.
    let mut monospace = Vec::new();
    if let Some((ref name, _)) = env_override {
        monospace.push(name.clone());
    }
    monospace.push(BUNDLED_MONOSPACE.to_owned());
    monospace.push(BUNDLED_MONOSPACE_BOLD.to_owned());

    // Insert env override font data if present.
    if let Some((name, data)) = env_override {
        fonts
            .font_data
            .insert(name, Arc::new(egui::FontData::from_owned(data)));
    }

    // Append bundled symbol fallbacks to both families.
    append_embedded_fallbacks(&mut fonts, &mut proportional, &mut monospace);

    // Append system font fallbacks for extended glyph coverage (proportional only).
    append_proportional_fallbacks(&mut fonts, &mut proportional);

    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, monospace);
    ctx.set_fonts(fonts);
    Ok(())
}

/// Apply the default text sizes and visual tweaks.
pub fn configure_style(ctx: &egui::Context) {
    ctx.style_mut(|style| {
        for text_style in [egui::TextStyle::Body, egui::TextStyle::Button] {
            if let Some(font_id) = style.text_styles.get_mut(&text_style) {
                font_id.size = DEFAULT_BODY_BUTTON_FONT_SIZE;
            }
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Heading) {
            // Set large enough to give egui_commonmark's heading scale factors
            // (which interpolate between Body and Heading sizes) visible
            // differentiation across all six heading levels.
            font_id.size = DEFAULT_BODY_BUTTON_FONT_SIZE * 2.0;
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Monospace) {
            font_id.size = DEFAULT_MONOSPACE_FONT_SIZE;
        }
        if let Some(font_id) = style.text_styles.get_mut(&egui::TextStyle::Small) {
            font_id.size = DEFAULT_SMALL_FONT_SIZE;
        }
        style.scroll_animation.points_per_second = DEFAULT_SCROLL_ANIMATION_POINTS_PER_SECOND;
        // Visible column separators in markdown tables rendered by egui_commonmark.
        style.visuals.widgets.noninteractive.bg_stroke.width = 1.0;
    });
}

fn insert_static(fonts: &mut egui::FontDefinitions, name: &str, data: &'static [u8]) {
    fonts
        .font_data
        .insert(name.to_owned(), Arc::new(egui::FontData::from_static(data)));
}

fn append_embedded_fallbacks(
    fonts: &mut egui::FontDefinitions,
    proportional: &mut Vec<String>,
    monospace: &mut Vec<String>,
) {
    for (name, data) in BUNDLED_SYMBOL_FALLBACKS {
        insert_static(fonts, name, data);
        proportional.push((*name).to_owned());
        monospace.push((*name).to_owned());
    }
}

fn append_proportional_fallbacks(
    fonts: &mut egui::FontDefinitions,
    proportional: &mut Vec<String>,
) {
    let mut loaded = 0usize;
    for path in PROPORTIONAL_FALLBACK_PATHS {
        let Ok(data) = fs::read(path) else {
            continue;
        };
        let name = format!("rustdown-fallback-{loaded}");
        fonts
            .font_data
            .insert(name.clone(), Arc::new(egui::FontData::from_owned(data)));
        proportional.push(name);
        loaded += 1;
    }
}

fn load_env_font_override() -> Result<Option<(String, Vec<u8>)>, String> {
    let path = match std::env::var("RUSTDOWN_FONT_PATH") {
        Ok(p) if !p.trim().is_empty() => p,
        Ok(_) => return Err("RUSTDOWN_FONT_PATH is set but empty".to_owned()),
        Err(_) => return Ok(None),
    };
    let data = fs::read(&path)
        .map_err(|err| format!("Failed to read font from RUSTDOWN_FONT_PATH '{path}': {err}"))?;
    Ok(Some(("rustdown-env-override".to_owned(), data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_style_applies_default_font_sizes() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let style = ctx.style();
        let expected: &[(egui::TextStyle, f32)] = &[
            (egui::TextStyle::Body, DEFAULT_BODY_BUTTON_FONT_SIZE),
            (egui::TextStyle::Button, DEFAULT_BODY_BUTTON_FONT_SIZE),
            (egui::TextStyle::Monospace, DEFAULT_MONOSPACE_FONT_SIZE),
            (egui::TextStyle::Small, DEFAULT_SMALL_FONT_SIZE),
            (
                egui::TextStyle::Heading,
                DEFAULT_BODY_BUTTON_FONT_SIZE * 2.0,
            ),
        ];
        for (text_style, size) in expected {
            assert_eq!(
                style.text_styles.get(text_style).map(|f| f.size),
                Some(*size),
                "{text_style:?} should have size {size}"
            );
        }
    }

    #[test]
    fn configure_style_sets_table_separator_stroke_width() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let style = ctx.style();
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(
                style.visuals.widgets.noninteractive.bg_stroke.width, 1.0,
                "table column separator stroke should be 1.0"
            );
        }
        assert!(
            (style.scroll_animation.points_per_second - DEFAULT_SCROLL_ANIMATION_POINTS_PER_SECOND)
                .abs()
                < f32::EPSILON,
            "scroll animation speed should be nudged up slightly"
        );
    }

    #[test]
    fn default_font_size_constants_are_positive() {
        let sizes = [
            DEFAULT_BODY_BUTTON_FONT_SIZE,
            DEFAULT_MONOSPACE_FONT_SIZE,
            DEFAULT_SMALL_FONT_SIZE,
        ];
        for size in sizes {
            assert!(size > 0.0, "font size {size} must be positive");
        }
    }

    #[test]
    fn heading_size_is_larger_than_body() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let style = ctx.style();
        let size_of = |ts: &egui::TextStyle| style.text_styles.get(ts).map_or(0.0, |f| f.size);
        assert!(size_of(&egui::TextStyle::Heading) > size_of(&egui::TextStyle::Body));
    }

    #[test]
    fn configure_fonts_succeeds_with_bundled_fonts() {
        let ctx = egui::Context::default();
        let result = configure_fonts(&ctx);
        assert!(
            result.is_ok(),
            "configure_fonts should always succeed with bundled fonts: {result:?}"
        );
    }

    #[test]
    fn proportional_and_monospace_use_distinct_primary_fonts() {
        let ctx = egui::Context::default();
        configure_fonts(&ctx).unwrap_or_else(|e| unreachable!("{e}"));
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx.fonts_mut(|fonts| {
            let families = &fonts.definitions().families;
            let proportional = families
                .get(&egui::FontFamily::Proportional)
                .unwrap_or_else(|| unreachable!());
            let monospace = families
                .get(&egui::FontFamily::Monospace)
                .unwrap_or_else(|| unreachable!());
            assert_eq!(proportional[0], BUNDLED_PROPORTIONAL);
            assert_eq!(monospace[0], BUNDLED_MONOSPACE);
            assert_ne!(
                proportional[0], monospace[0],
                "proportional and monospace should use different primary fonts"
            );
        });
    }

    #[test]
    fn bundled_fonts_cover_emoji_glyphs() {
        let ctx = egui::Context::default();
        configure_fonts(&ctx).unwrap_or_else(|e| unreachable!("{e}"));
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let body_font = egui::TextStyle::Body.resolve(&ctx.style());
        let has_glyphs = ctx.fonts_mut(|fonts| {
            let mut font = fonts.fonts.font(&body_font.family);
            font.has_glyphs("🔬🎉✨🟢")
        });
        assert!(
            has_glyphs,
            "bundled font fallbacks should cover the bundled-doc emoji set"
        );
    }

    #[test]
    fn monospace_chain_has_no_system_proportional_fonts() {
        let ctx = egui::Context::default();
        configure_fonts(&ctx).unwrap_or_else(|e| unreachable!("{e}"));
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx.fonts_mut(|fonts| {
            let monospace = fonts
                .definitions()
                .families
                .get(&egui::FontFamily::Monospace)
                .unwrap_or_else(|| unreachable!());
            for name in monospace {
                assert!(
                    !name.starts_with("rustdown-fallback-"),
                    "monospace chain should not contain system proportional fallbacks, found: {name}"
                );
            }
        });
    }

    #[test]
    #[allow(unsafe_code)]
    fn env_override_empty_is_error() {
        // SAFETY: test-only; env var manipulation is inherently racy but
        // acceptable in single-threaded test contexts.
        unsafe { std::env::set_var("RUSTDOWN_FONT_PATH", "") };
        let result = load_env_font_override();
        unsafe { std::env::remove_var("RUSTDOWN_FONT_PATH") };
        assert!(result.is_err());
    }

    #[test]
    #[allow(unsafe_code)]
    fn env_override_missing_is_none() {
        unsafe { std::env::remove_var("RUSTDOWN_FONT_PATH") };
        let result = load_env_font_override();
        assert!(matches!(result, Ok(None)));
    }
}
