//! Client-local shortcuts: ordered settings overrides above Linux defaults.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A parsed client binding, ready for matching against key events.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub keys: Vec<String>,
    pub action: String,
    pub args: Option<serde_json::Value>,
}

impl Binding {
    pub fn new(
        keys: impl IntoIterator<Item = impl Into<String>>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            keys: keys.into_iter().map(Into::into).collect(),
            action: action.into(),
            args: None,
        }
    }

    fn args(mut self, args: serde_json::Value) -> Self {
        self.args = Some(args);
        self
    }
}

pub fn defaults() -> Vec<Binding> {
    let mut bindings = Vec::new();
    for (key, action) in [
        ("ctrl+k", "session.switch"),
        ("ctrl+shift+p", "client.commands"),
        ("ctrl+,", "client.settings"),
        ("ctrl+shift+b", "client.sidebar.toggle"),
        ("ctrl+shift+n", "session.new"),
        ("ctrl+shift+t", "client.tab.new"),
        ("ctrl+shift+w", "pane.close"),
        ("ctrl+shift+enter", "pane.zoom"),
        ("ctrl+shift+space", "pane.actions"),
        ("ctrl+shift+c", "pane.copy"),
        ("ctrl+shift+v", "pane.paste"),
        ("ctrl+shift+r", "pane.reconnect"),
        ("ctrl+pageup", "client.tab.previous"),
        ("ctrl+pagedown", "client.tab.next"),
        ("ctrl+=", "client.zoom.in"),
        ("ctrl+shift+=", "client.zoom.in"),
        ("ctrl+-", "client.zoom.out"),
        ("ctrl+0", "client.zoom.reset"),
        ("ctrl+[NumpadAdd]", "client.zoom.in"),
        ("ctrl+[NumpadSubtract]", "client.zoom.out"),
        ("ctrl+[Numpad0]", "client.zoom.reset"),
    ] {
        bindings.push(Binding::new([key], action));
    }
    for (key, direction) in [("ctrl+shift+d", "right"), ("ctrl+alt+d", "down")] {
        bindings.push(Binding::new([key], "pane.split").args(json!({"direction":direction})));
    }
    for direction in ["left", "right", "up", "down"] {
        bindings.push(
            Binding::new([format!("ctrl+alt+{direction}")], "pane.focus")
                .args(json!({"direction":direction})),
        );
    }
    for (key, lines) in [("shift+pageup", -20), ("shift+pagedown", 20)] {
        bindings.push(Binding::new([key], "pane.scroll").args(json!({"lines":lines})));
    }
    bindings
}

/// Write the complete default list into new client settings so it is easy to edit.
pub fn default_keybindings() -> Vec<Keybinding> {
    defaults()
        .into_iter()
        .map(|binding| Keybinding {
            key: binding.keys.join(">"),
            action: Some(binding.action),
            args: binding.args,
        })
        .collect()
}

/// Entries apply in file order. A null action removes only this exact shortcut/sequence.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keybinding {
    pub key: String,
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// Canonical spelling makes modifier order, aliases and case irrelevant for replacement.
pub fn normalize(key: &str) -> Result<Vec<String>> {
    key.split('>')
        .map(|chord| {
            let parts: Vec<_> = chord
                .trim()
                .split('+')
                .map(|p| p.trim().to_lowercase())
                .collect();
            let key = parts.last().expect("split always has an element");
            let mut modifiers = [false; 4];
            for modifier in &parts[..parts.len() - 1] {
                let index = match modifier.as_str() {
                    "ctrl" | "control" => 0,
                    "alt" | "opt" | "option" => 1,
                    "shift" => 2,
                    "cmd" | "command" | "super" | "meta" | "win" => 3,
                    _ => bail!("unknown modifier {modifier:?}"),
                };
                ensure!(!modifiers[index], "duplicate modifier {modifier:?}");
                modifiers[index] = true;
            }
            let physical = key.starts_with('[')
                && key.ends_with(']')
                && key.len() > 2
                && key[1..key.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric());
            let named = matches!(
                key.as_str(),
                "enter"
                    | "escape"
                    | "tab"
                    | "space"
                    | "backspace"
                    | "delete"
                    | "left"
                    | "right"
                    | "up"
                    | "down"
                    | "home"
                    | "end"
                    | "pageup"
                    | "pagedown"
            ) || key
                .strip_prefix('f')
                .and_then(|n| n.parse::<u8>().ok())
                .is_some_and(|n| (1..=24).contains(&n));
            ensure!(
                physical
                    || named
                    || (key.chars().count() == 1 && !key.chars().any(char::is_control)),
                "unknown key {key:?}"
            );
            let mut out = String::new();
            for (enabled, name) in modifiers.into_iter().zip(["ctrl", "alt", "shift", "super"]) {
                if enabled {
                    out.push_str(name);
                    out.push('+');
                }
            }
            out.push_str(key);
            Ok(out)
        })
        .collect()
}

pub fn resolve(overrides: &[Keybinding]) -> Result<Vec<Binding>> {
    let mut bindings = defaults();
    for b in &mut bindings {
        b.keys = normalize(&b.keys.join(">"))?;
    }
    let actions: Vec<_> = defaults()
        .into_iter()
        .map(|b| b.action)
        .chain(std::iter::once("pane.send_key".into()))
        .collect();
    for (index, entry) in overrides.iter().enumerate() {
        let keys = normalize(&entry.key)
            .map_err(|error| anyhow::anyhow!("keybindings[{index}] ({}): {error}", entry.key))?;
        if let Some(action) = &entry.action {
            ensure!(
                actions.contains(action),
                "keybindings[{index}]: unknown client action {action:?}"
            );
            let args = entry.args.clone().unwrap_or_else(|| json!({}));
            ensure!(
                args.is_object(),
                "keybindings[{index}]: args must be an object"
            );
            match action.as_str() {
                "pane.split" => ensure!(
                    matches!(args["direction"].as_str(), Some("right" | "down")),
                    "pane.split requires direction: right or down"
                ),
                "pane.focus" => ensure!(
                    matches!(
                        args["direction"].as_str(),
                        Some("right" | "down" | "left" | "up")
                    ),
                    "pane.focus requires direction"
                ),
                "pane.scroll" => ensure!(
                    args["lines"]
                        .as_i64()
                        .and_then(|n| isize::try_from(n).ok())
                        .is_some(),
                    "pane.scroll requires integer lines"
                ),
                "pane.send_key" => {
                    let key = args["key"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("pane.send_key requires key"))?;
                    ensure!(
                        normalize(key)?.len() == 1,
                        "pane.send_key expects one chord"
                    );
                }
                _ => {}
            }
        } else {
            ensure!(entry.args.is_none(), "an unbinding cannot have args");
        }
        bindings.retain(|b| b.keys != keys);
        if let Some(action) = &entry.action {
            let mut binding = Binding::new(keys, action);
            binding.args = entry.args.clone();
            bindings.insert(0, binding);
        }
    }
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entries(value: serde_json::Value) -> Vec<Keybinding> {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn aliases_replace_defaults_and_null_unbinds_exact_sequences() {
        let overrides = entries(json!([
            {"key":"Control+K", "action":"client.sidebar.toggle"},
            {"key":"ctrl+b>c", "action":"client.tab.new"},
            {"key":"ctrl+b>z", "action":"pane.zoom"},
            {"key":"ctrl+b>c", "action":null},
            {"key":"ctrl+b", "action":null}
        ]));
        let bindings = resolve(&overrides).unwrap();
        assert_eq!(bindings.iter().filter(|b| b.keys == ["ctrl+k"]).count(), 1);
        assert_eq!(
            bindings
                .iter()
                .find(|b| b.keys == ["ctrl+k"])
                .unwrap()
                .action,
            "client.sidebar.toggle"
        );
        assert!(!bindings.iter().any(|b| b.keys == ["ctrl+b", "c"]));
        assert!(bindings.iter().any(|b| b.keys == ["ctrl+b", "z"]));
        assert_eq!(
            normalize("Win+SHIFT+D").unwrap(),
            normalize("shift+cmd+d").unwrap()
        );
    }
    #[test]
    fn invalid_configuration_is_rejected() {
        for value in [
            json!([{"key":"ctrl++", "action":"pane.zoom"}]),
            json!([{"key":"ctrl+b>", "action":"pane.zoom"}]),
            json!([{"key":"ctrl+k", "action":"missing"}]),
            json!([{"key":"ctrl+d", "action":"pane.split", "args":{"direction":"diagonal"}}]),
            json!([{"key":"ctrl+k", "action":null, "args":{}}]),
        ] {
            assert!(resolve(&entries(value)).is_err());
        }
        assert!(resolve(&[]).is_ok());
    }
}
