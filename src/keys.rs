//! Customisable single-key shortcuts, saved to the config folder. Cmd/Ctrl shortcuts (undo, copy…) stay fixed.
use eframe::egui::{InputState, Key};
use std::collections::HashMap;

/// (id, what it does, default key). A key is an egui key name, optionally prefixed with "Shift+".
pub const ACTIONS: &[(&str, &str, &str)] = &[
    ("rate0", "Clear stars", "0"),
    ("rate1", "1 star", "1"),
    ("rate2", "2 stars", "2"),
    ("rate3", "3 stars", "3"),
    ("rate4", "4 stars", "4"),
    ("rate5", "5 stars", "5"),
    ("pick", "Pick", "P"),
    ("reject", "Reject", "X"),
    ("unflag", "Clear flag", "U"),
    ("red", "Red label", "6"),
    ("yellow", "Yellow label", "7"),
    ("green", "Green label", "8"),
    ("blue", "Blue label", "9"),
    ("purple", "Purple label", "Shift+6"),
    ("next", "Next frame", "ArrowRight"),
    ("prev", "Previous frame", "ArrowLeft"),
    ("cull", "Culling view", "C"),
    ("compare", "Compare two (culling)", "Shift+C"),
    ("library", "Library", "G"),
    ("develop", "Develop", "D"),
    ("zoom", "1:1 zoom", "Z"),
    ("before", "Before / after", "Backslash"),
    ("clipping", "Show clipping", "J"),
    ("crop", "Crop tool", "R"),
    ("heal", "Heal tool", "Q"),
    ("gradient", "Linear mask", "M"),
    ("brush", "Brush mask", "K"),
    ("wb", "White balance picker", "W"),
    ("rot_left", "Rotate left", "OpenBracket"),
    ("rot_right", "Rotate right", "CloseBracket"),
    ("vcopy", "Virtual copy", "Shift+V"),
    ("snapshot", "New snapshot", "Shift+S"),
    ("focus", "Focus check (culling)", "F"),
];

pub struct Keymap {
    map: HashMap<String, String>,
}

fn path() -> Option<std::path::PathBuf> {
    Some(dirs_config()?.join("shortcuts.json"))
}

/// The per-user config folder (same place as presets).
pub fn dirs_config() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| {
        if cfg!(windows) { std::env::var_os("APPDATA").map(Into::into) } else { std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")) }
    })?;
    Some(base.join("ferrotype"))
}

impl Keymap {
    pub fn load() -> Self {
        let mut map: HashMap<String, String> = ACTIONS.iter().map(|(id, _, k)| (id.to_string(), k.to_string())).collect();
        if let Some(saved) = path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<HashMap<String, String>>(&t).ok()) {
            map.extend(saved.into_iter().filter(|(id, _)| ACTIONS.iter().any(|a| a.0 == id)));
        }
        Keymap { map }
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path().ok_or("no config folder")?;
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(p, serde_json::to_string_pretty(&self.map).unwrap()).map_err(|e| e.to_string())
    }

    pub fn key(&self, id: &str) -> &str {
        self.map.get(id).map(String::as_str).unwrap_or("")
    }

    /// Bind `combo` to `id`; any other action on the same key loses it.
    pub fn set(&mut self, id: &str, combo: String) {
        for v in self.map.values_mut() {
            if *v == combo {
                v.clear();
            }
        }
        self.map.insert(id.into(), combo);
    }

    pub fn reset(&mut self) {
        *self = Keymap { map: ACTIONS.iter().map(|(id, _, k)| (id.to_string(), k.to_string())).collect() };
    }

    pub fn pressed(&self, i: &InputState, id: &str) -> bool {
        let Some((shift, key)) = parse(self.key(id)) else { return false };
        // The modifiers of the key event itself: by the end of the frame Shift may already be released.
        i.events.iter().any(|e| matches!(e, eframe::egui::Event::Key { key: k, pressed: true, modifiers: m, .. }
            if *k == key && m.shift == shift && !m.command && !m.alt))
    }
}

fn parse(combo: &str) -> Option<(bool, Key)> {
    let (shift, name) = match combo.strip_prefix("Shift+") { Some(n) => (true, n), None => (false, combo) };
    Some((shift, Key::from_name(name)?))
}

/// The combo for a key press, for the shortcut editor.
pub fn combo(key: Key, shift: bool) -> String {
    format!("{}{}", if shift { "Shift+" } else { "" }, key.name())
}

/// Short display form: "Shift+[" rather than "Shift+OpenBracket".
pub fn display(combo: &str) -> String {
    let Some((shift, key)) = parse(combo) else { return "—".into() };
    let name = match key {
        Key::ArrowLeft => "Left",
        Key::ArrowRight => "Right",
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Backslash => "\\",
        k => k.name(),
    };
    format!("{}{name}", if shift { "Shift+" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_key_parses_and_none_clash() {
        let mut seen = std::collections::HashSet::new();
        for (id, _, k) in ACTIONS {
            assert!(parse(k).is_some(), "{id}: {k}");
            assert!(seen.insert(*k), "two actions on {k}");
        }
        let mut m = Keymap { map: Default::default() };
        m.reset();
        m.set("pick", "X".into());
        assert_eq!((m.key("pick"), m.key("reject")), ("X", ""));
    }
}
