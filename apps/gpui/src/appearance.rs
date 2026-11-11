use super::*;
use ely_core::theme::{Density, Palette};
pub fn bundled() -> Vec<Value> {
    let source = include_str!("../../desktop/src/themes/omarchy.ts");
    source
        .split_once("= ")
        .and_then(|(_, s)| s.split_once(" as const;"))
        .and_then(|(s, _)| serde_json::from_str(s).ok())
        .unwrap_or_default()
}
pub fn apply(settings: &Value, cx: &mut App) -> Result<()> {
    let id = settings["theme"].as_str().unwrap_or("auto");
    ely_desktop::stop_omarchy(cx);
    if id == "auto" || id == "omarchy" {
        if let Some(dir) = ely_desktop::omarchy_current_dir() {
            ely_desktop::follow_omarchy(dir, cx)?;
        } else {
            Theme::set_palette(Mode::Dark, None, cx);
            Theme::set_mode(Mode::Dark, cx);
        }
    } else if let Some(theme) = bundled().into_iter().find(|t| t["id"] == id) {
        let mode = if theme["mode"] == "light" {
            Mode::Light
        } else {
            Mode::Dark
        };
        let mut palette = if mode == Mode::Light {
            Palette::light(false)
        } else {
            Palette::dark(false)
        };
        let color = |name: &str, default: gpui::Hsla| -> gpui::Hsla {
            theme["colors"][name]
                .as_str()
                .and_then(|s| u32::from_str_radix(s.trim_start_matches('#'), 16).ok())
                .map(|v| gpui::rgb(v).into())
                .unwrap_or(default)
        };
        palette.bg = color("background", palette.bg);
        palette.surface = color("dark_background", palette.surface);
        palette.fg = color("foreground", palette.fg);
        palette.fg_muted = color("dark_foreground", palette.fg_muted);
        palette.fg_subtle = palette.fg_muted;
        palette.accent = color("accent", palette.accent);
        palette.hover = color("lighter_background", palette.hover);
        palette.active = color("selection", palette.active);
        Theme::set_palette(mode, Some(palette), cx);
        Theme::set_mode(mode, cx);
    } else {
        Theme::set_palette(Mode::Dark, None, cx);
        Theme::set_mode(Mode::Dark, cx);
    }
    Theme::update(cx, |t| {
        t.density = if settings["density"] == "compact" {
            Density::Compact
        } else {
            Density::Comfortable
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_themes_are_complete() {
        let themes = bundled();
        assert_eq!(themes.len(), 37);
        for theme in themes {
            assert!(theme["id"].is_string());
            for value in theme["colors"].as_object().unwrap().values() {
                assert!(
                    u32::from_str_radix(value.as_str().unwrap().trim_start_matches('#'), 16)
                        .is_ok()
                );
            }
        }
    }
}
