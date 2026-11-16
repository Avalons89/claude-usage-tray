//! Choices made from the tray menu, saved as JSON in the app's config folder.
//! Keys double as tray menu ids and as the field names the page reads.

use std::collections::BTreeMap;
use std::path::Path;

pub type Settings = BTreeMap<String, bool>;

/// (key, tray label, default) for the ticks under "Show on strip".
pub const STRIP: [(&str, &str, bool); 5] = [
    ("session", "Session", true),
    ("week", "Week", true),
    ("model_weeks", "Model weeks at 80% or more", true),
    ("today", "Today", true),
    ("burn", "Burn rate", true),
];
pub const ISLAND: (&str, &str, bool) = ("island", "Island style", false);
pub const KEEP_ON_TOP: (&str, &str, bool) = ("keep_on_top", "Keep on top", true);
pub const LOCKED: (&str, &str, bool) = ("locked", "Lock position", false);

pub fn load(path: &Path) -> Settings {
    parse(&std::fs::read_to_string(path).unwrap_or_default())
}

pub fn save(path: &Path, settings: &Settings) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn parse(saved: &str) -> Settings {
    let saved: Settings = serde_json::from_str(saved).unwrap_or_default();
    STRIP
        .iter()
        .chain([&ISLAND, &KEEP_ON_TOP, &LOCKED])
        .map(|&(key, _, default)| (key.to_string(), saved.get(key).copied().unwrap_or(default)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_choices_win_and_missing_ones_take_their_defaults() {
        let settings = parse(r#"{"burn": false, "locked": true, "retired_key": true}"#);
        assert!(!settings["burn"]);
        assert!(settings["locked"]);
        assert!(settings["session"]);
        assert!(settings["keep_on_top"]);
        assert!(!settings.contains_key("retired_key"));
    }

    #[test]
    fn an_unreadable_file_gives_the_defaults() {
        let settings = parse("not json");
        assert!(settings["model_weeks"]);
        assert!(!settings["island"]);
        assert!(!settings["locked"]);
    }
}
