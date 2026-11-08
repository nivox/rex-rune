//! Keyboard: workspace shortcuts first, then every other key to the focused pane's PTY.
//! Terminals never take Rune focus. With nothing focused Rune offers a key to the window's
//! default button, then to `on_key`, then to Tab traversal, Space/Return presses and
//! scrolling. So no default button may exist outside an open modal (the main view has none),
//! and `on_key` takes every key it forwards.
use super::{
    keys::{self, Action},
    panel::{Intent, PanelKind},
    workspace::Workspace,
};
use rex_linux::connection::Command;
use rune::{Ime, Key, KeyEvent, PhysicalKeyCode as Code, Tx};

fn ctrl(ev: &KeyEvent) -> bool {
    ev.mods.ctrl || ev.mods.cmd
}

fn space(ev: &KeyEvent, tx: &Tx) -> bool {
    tx.physical_key() == Some(Code::Space)
        || ev.is_command(' ')
        || matches!(&ev.key, Key::Text(t) if t == " ")
}

/// Zoom bindings are also available in forms and popups.
pub fn zoom_key(s: &mut Workspace, ev: &KeyEvent, tx: &mut Tx) -> bool {
    super::bindings::on_key(s, ev, tx, true)
}

/// Keys a list popup (switcher, Commands, host step, pane menu) takes, from its field or root.
pub fn list_key(s: &mut Workspace, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if zoom_key(s, ev, tx) {
        return true;
    }
    if s.panel.as_ref().is_some_and(|p| p.rename.is_some()) {
        return rename_key(s, ev, tx);
    }
    match ev.key {
        Key::Up | Key::Down => {
            s.navigate_panel(if ev.key == Key::Up { -1 } else { 1 });
            tx.instant();
        }
        Key::Enter => s.submit(tx),
        Key::Escape => s.cancel_panel(tx),
        Key::Function(2) => {
            let selected = s
                .panel
                .as_ref()
                .filter(|p| matches!(p.kind, PanelKind::Picker(true)));
            if let Some(Intent::Select(host, session)) =
                selected.and_then(|p| s.entries().get(p.selected).map(|e| e.action.clone()))
            {
                s.start_rename(host, session);
            }
        }
        // Lists have one control; Tab must not walk focus out to the window behind them.
        Key::Tab => {}
        _ if ctrl(ev)
            && ev.mods.shift
            && space(ev, tx)
            && s.panel
                .as_ref()
                .is_some_and(|p| matches!(p.kind, PanelKind::Pane)) =>
        {
            s.close_panel(tx)
        }
        _ => return false,
    }
    true
}

/// Keys the switcher row being renamed takes: Enter saves, Esc goes back to browsing.
pub fn rename_key(s: &mut Workspace, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match ev.key {
        Key::Enter => s.finish_rename(true),
        Key::Escape => s.finish_rename(false),
        Key::Tab | Key::Up | Key::Down => {}
        _ => return zoom_key(s, ev, tx),
    }
    true
}

pub fn on_key(s: &mut Workspace, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let code = tx.physical_key();
    if s.panel.is_some() && zoom_key(s, ev, tx) {
        return true;
    }
    if let Some(panel) = &s.panel {
        if panel.is_list() && tx.focused().is_none() {
            return list_key(s, ev, tx);
        }
        if ev.key == Key::Escape && tx.focused().is_none() {
            s.cancel_panel(tx);
            return true;
        }
        return false;
    }
    if super::bindings::on_key(s, ev, tx, false) {
        return true;
    }
    if tx.focused().is_some() {
        // A control the pointer focused (rare: controls blur after their press) keeps its key.
        tx.blur();
    }
    let action = if tx.key_repeat() {
        Action::Repeat
    } else {
        Action::Press
    };
    let base = tx.key_base();
    let Some(term) = s.active_terminal_mut() else {
        return true;
    };
    if !term.status().live {
        return true;
    }
    term.clear_selection();
    if let Some(payload) = keys::payload(ev, code, base, action) {
        term.send(Command::RawKey(payload));
        if let Some(code) = code {
            s.forwarded.insert(code);
        }
    }
    true
}

/// A release reaches the PTY only when its press did: keys a shortcut, popup or field took
/// keep their releases local, even when the popup closed in between.
pub fn on_key_up(s: &mut Workspace, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let Some(code) = tx.physical_key() else {
        return false;
    };
    s.consumed_keys.remove(&code);
    if !s.forwarded.remove(&code) {
        return s.panel.is_none();
    }
    let code = Some(code);
    let base = tx.key_base();
    if let Some(term) = s.active_terminal_mut()
        && term.status().live
        && let Some(payload) = keys::payload(ev, code, base, Action::Release)
    {
        term.send(Command::RawKey(payload));
    }
    true
}

pub fn on_ime(s: &mut Workspace, ime: &Ime, _: &mut Tx) -> bool {
    let Some(term) = s.active_terminal_mut() else {
        return false;
    };
    match ime {
        Ime::Preedit(text, _) => term.preedit = text.clone(),
        Ime::Commit(text) => {
            term.preedit.clear();
            if term.status().live && !text.is_empty() {
                term.send(Command::Paste(text.clone()));
            }
        }
        _ => {}
    }
    true
}

/// Send a synthetic key straight to the block, bypassing Lua bindings and local shortcuts.
pub fn send_key(s: &mut Workspace, chord: &str) {
    let mut mods = rune::Mods::default();
    let parts: Vec<_> = chord.split('+').collect();
    let Some(key) = parts.last() else {
        return;
    };
    for modifier in &parts[..parts.len() - 1] {
        match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "opt" | "option" => mods.alt = true,
            "shift" => mods.shift = true,
            "cmd" | "command" | "super" | "meta" | "win" => mods.logo = true,
            _ => {
                s.notify("Invalid pane.send_key modifier");
                return;
            }
        }
    }
    let key = match key.to_ascii_lowercase().as_str() {
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Text(" ".into()),
        k if k.starts_with('f') && k[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) => {
            Key::Function(k[1..].parse().unwrap())
        }
        k if k.chars().count() == 1 => Key::Text(k.into()),
        _ => {
            s.notify("Invalid pane.send_key key");
            return;
        }
    };
    let base = match &key {
        Key::Text(t) => t.chars().next(),
        _ => None,
    };
    if let Some(term) = s.active_terminal_mut().filter(|t| t.status().live) {
        for action in [Action::Press, Action::Release] {
            if let Some(mut payload) = keys::payload(
                &KeyEvent {
                    key: key.clone(),
                    mods,
                },
                None,
                base,
                action,
            ) {
                if mods.shift
                    && !mods.ctrl
                    && !mods.logo
                    && payload["action"] != "release"
                    && let Some(base) = base
                {
                    let shifted = "`1234567890-=[]\\;',./"
                        .chars()
                        .zip("~!@#$%^&*()_+{}|:\"<>?".chars())
                        .find_map(|(plain, shifted)| (plain == base).then_some(shifted))
                        .unwrap_or_else(|| base.to_ascii_uppercase());
                    payload["text"] = serde_json::json!(shifted.to_string());
                }
                term.send(Command::RawKey(payload));
            }
        }
    }
}
