use serde::Serialize;
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Serialize)]
pub struct OmarchyTheme {
    name: String,
    colors: BTreeMap<String, String>,
}

fn read_palette(state: &Path) -> Option<OmarchyTheme> {
    let name = std::fs::read_to_string(state.join("theme.name")).ok()?;
    let name = name.trim();
    if name.is_empty()
        || name.len() > 80
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return None;
    }
    let path = state.join("theme/colors.toml");
    if std::fs::metadata(&path).ok()?.len() > 65536 {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    let colors: BTreeMap<String, String> = raw
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let value = value.trim().trim_matches(['\'', '"']);
            (key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                && value.len() == 7
                && value.starts_with('#')
                && value[1..].bytes().all(|c| c.is_ascii_hexdigit()))
            .then(|| (key.to_owned(), value.to_owned()))
        })
        .collect();
    if ![
        "accent",
        "background",
        "foreground",
        "bright_foreground",
        "red",
        "yellow",
        "green",
    ]
    .iter()
    .all(|key| colors.contains_key(*key))
    {
        return None;
    }
    Some(OmarchyTheme {
        name: name.to_owned(),
        colors,
    })
}

#[tauri::command]
pub fn get_omarchy_theme(app: tauri::AppHandle) -> Option<OmarchyTheme> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    use tauri::Manager;
    read_palette(
        &app.path()
            .home_dir()
            .ok()?
            .join(".local/state/omarchy/current"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_palette_reloads_and_rejects_invalid_colors() {
        let root = std::env::temp_dir().join(format!("munki-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("theme")).unwrap();
        std::fs::write(root.join("theme.name"), "tokyo-night\n").unwrap();
        let colors = "accent = \"#7aa2f7\"\nbackground = \"#1a1b26\"\nforeground = \"#a9b1d6\"\nbright_foreground = \"#c0caf5\"\nred = \"#f7768e\"\nyellow = \"#e0af68\"\ngreen = \"#9ece6a\"";
        std::fs::write(root.join("theme/colors.toml"), colors).unwrap();
        assert_eq!(read_palette(&root).unwrap().colors["accent"], "#7aa2f7");
        std::fs::write(
            root.join("theme/colors.toml"),
            colors.replace("#7aa2f7", "#112233"),
        )
        .unwrap();
        assert_eq!(read_palette(&root).unwrap().colors["accent"], "#112233");
        std::fs::write(
            root.join("theme/colors.toml"),
            colors.replace("#7aa2f7", "red; background:url(x)"),
        )
        .unwrap();
        assert!(read_palette(&root).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
