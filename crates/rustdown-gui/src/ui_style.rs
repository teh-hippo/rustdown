use std::{fs, sync::Arc};

use eframe::egui;

const UI_FONT_NAME: &str = "rustdown-ui-font";

#[cfg(target_os = "linux")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
];
#[cfg(target_os = "linux")]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
    "/usr/share/fonts/truetype/unifont/unifont.ttf",
];
#[cfg(target_os = "macos")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
];
#[cfg(target_os = "macos")]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Supplemental/Symbol.ttf",
];
#[cfg(target_os = "windows")]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
];
#[cfg(target_os = "windows")]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\seguiemj.ttf",
    r"C:\Windows\Fonts\seguisym.ttf",
    r"C:\Windows\Fonts\arialuni.ttf",
];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const UI_FONT_CANDIDATE_PATHS: &[&str] = &[];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const UI_FONT_FALLBACK_PATHS: &[&str] = &[];

const DEFAULT_BODY_BUTTON_FONT_SIZE: f32 = 19.0;
const DEFAULT_MONOSPACE_FONT_SIZE: f32 = 18.0;
const DEFAULT_SMALL_FONT_SIZE: f32 = 13.0;

/// Load the primary system font and any available fallback fonts.
pub fn configure_fonts(ctx: &egui::Context) -> Result<(), String> {
    let primary_font_data = load_single_font()?;
    let primary_font_name = UI_FONT_NAME.to_owned();
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.clear();
    fonts.families.clear();
    fonts.font_data.insert(
        primary_font_name.clone(),
        Arc::new(egui::FontData::from_owned(primary_font_data)),
    );
    let mut proportional = vec![primary_font_name.clone()];
    let mut monospace = vec![primary_font_name];
    append_font_fallbacks(
        &mut fonts,
        &mut proportional,
        &mut monospace,
        UI_FONT_FALLBACK_PATHS,
    );
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
        // Visible column separators in markdown tables rendered by egui_commonmark.
        style.visuals.widgets.noninteractive.bg_stroke.width = 1.0;
    });
}

fn append_font_fallbacks(
    fonts: &mut egui::FontDefinitions,
    proportional: &mut Vec<String>,
    monospace: &mut Vec<String>,
    paths: &[&str],
) -> usize {
    let mut loaded = 0usize;
    for path in paths {
        let Ok(data) = fs::read(path) else {
            continue;
        };
        let name = format!("{UI_FONT_NAME}-fallback-{loaded}");
        fonts
            .font_data
            .insert(name.clone(), Arc::new(egui::FontData::from_owned(data)));
        proportional.push(name.clone());
        monospace.push(name);
        loaded += 1;
    }
    loaded
}

fn load_single_font() -> Result<Vec<u8>, String> {
    if let Ok(path) = std::env::var("RUSTDOWN_FONT_PATH") {
        if path.trim().is_empty() {
            return Err("RUSTDOWN_FONT_PATH is set but empty".to_owned());
        }
        return fs::read(&path).map_err(|err| {
            format!("Failed to read UI font from RUSTDOWN_FONT_PATH '{path}': {err}")
        });
    }

    for path in UI_FONT_CANDIDATE_PATHS {
        match fs::read(path) {
            Ok(data) => return Ok(data),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("Failed to read UI font at '{path}': {err}")),
        }
    }

    if UI_FONT_CANDIDATE_PATHS.is_empty() {
        return Err("No UI font candidates are configured for this platform".to_owned());
    }

    Err(format!(
        "No UI font files found. Tried: {}",
        UI_FONT_CANDIDATE_PATHS.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_style_applies_default_font_sizes() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let style = ctx.style();
        assert_eq!(
            style
                .text_styles
                .get(&egui::TextStyle::Body)
                .map(|font| font.size),
            Some(DEFAULT_BODY_BUTTON_FONT_SIZE)
        );
        assert_eq!(
            style
                .text_styles
                .get(&egui::TextStyle::Button)
                .map(|font| font.size),
            Some(DEFAULT_BODY_BUTTON_FONT_SIZE)
        );
        assert_eq!(
            style
                .text_styles
                .get(&egui::TextStyle::Monospace)
                .map(|font| font.size),
            Some(DEFAULT_MONOSPACE_FONT_SIZE)
        );
        assert_eq!(
            style
                .text_styles
                .get(&egui::TextStyle::Small)
                .map(|font| font.size),
            Some(DEFAULT_SMALL_FONT_SIZE)
        );
        assert_eq!(
            style
                .text_styles
                .get(&egui::TextStyle::Heading)
                .map(|font| font.size),
            Some(DEFAULT_BODY_BUTTON_FONT_SIZE * 2.0)
        );
    }

    #[test]
    fn append_font_fallbacks_loads_existing_files_only() {
        let mut fonts = egui::FontDefinitions::default();
        let mut proportional = Vec::new();
        let mut monospace = Vec::new();
        let loaded = append_font_fallbacks(
            &mut fonts,
            &mut proportional,
            &mut monospace,
            &["/nonexistent/font.ttf"],
        );
        assert_eq!(loaded, 0);
        assert!(proportional.is_empty());
    }

    #[test]
    fn append_font_fallbacks_empty_list() {
        let mut fonts = egui::FontDefinitions::default();
        let mut proportional = Vec::new();
        let mut monospace = Vec::new();
        let loaded = append_font_fallbacks(&mut fonts, &mut proportional, &mut monospace, &[]);
        assert_eq!(loaded, 0);
        assert!(proportional.is_empty());
        assert!(monospace.is_empty());
    }

    #[test]
    fn append_font_fallbacks_loads_real_file() {
        // Write a minimal file so we can test the load path, even though it
        // is not a valid font—egui only validates it lazily at render time.
        let dir = std::env::temp_dir().join("rustdown_font_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("fake.ttf");
        std::fs::write(&path, b"not-a-real-font-but-loads-ok").unwrap_or_else(|_| unreachable!());

        let path_str = path.to_str().unwrap_or_else(|| unreachable!());
        let mut fonts = egui::FontDefinitions::default();
        let mut proportional = Vec::new();
        let mut monospace = Vec::new();
        let loaded = append_font_fallbacks(
            &mut fonts,
            &mut proportional,
            &mut monospace,
            &["/nonexistent/font.ttf", path_str],
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(loaded, 1);
        assert_eq!(proportional.len(), 1);
        assert_eq!(monospace.len(), 1);
        assert!(fonts.font_data.contains_key(&proportional[0]));
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
    }

    #[test]
    fn default_font_size_constants_are_positive() {
        // Validate at runtime to catch accidental zero/negative constants.
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
        let body_size = style
            .text_styles
            .get(&egui::TextStyle::Body)
            .map_or(0.0, |f| f.size);
        let heading_size = style
            .text_styles
            .get(&egui::TextStyle::Heading)
            .map_or(0.0, |f| f.size);
        assert!(
            heading_size > body_size,
            "heading ({heading_size}) should be larger than body ({body_size})"
        );
    }
}
