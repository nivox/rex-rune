//! Match client-local priority-ordered bindings. Prefixes are consumed locally.
use super::{input, workspace::Workspace};
use rex_linux::keymap::Binding;
use rune::Tx;
use rune::{Key, KeyEvent, PhysicalKeyCode};
use serde_json::{Value, json};

fn chord_matches(
    chord: &str,
    ev: &KeyEvent,
    physical: Option<PhysicalKeyCode>,
    base: Option<char>,
) -> bool {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut logo = false;
    let parts: Vec<_> = chord.split('+').collect();
    let Some(key) = parts.last() else {
        return false;
    };
    for modifier in &parts[..parts.len() - 1] {
        match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "opt" | "option" => alt = true,
            "shift" => shift = true,
            "cmd" | "command" | "super" | "meta" | "win" => logo = true,
            _ => return false,
        }
    }
    if (ctrl, alt, shift, logo)
        != (
            ev.mods.ctrl || ev.mods.cmd,
            ev.mods.alt,
            ev.mods.shift,
            ev.mods.logo,
        )
    {
        return false;
    }
    if key.starts_with('[') && key.ends_with(']') {
        return physical
            .is_some_and(|code| format!("{code:?}").eq_ignore_ascii_case(&key[1..key.len() - 1]));
    }
    let logical = match &ev.key {
        Key::Enter => "enter".into(),
        Key::Escape => "escape".into(),
        Key::Tab => "tab".into(),
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::Function(n) => format!("f{n}"),
        Key::Command(c) => base
            .unwrap_or(if *c == '+' { '=' } else { *c })
            .to_ascii_lowercase()
            .to_string(),
        Key::Text(t) => {
            base.map_or_else(|| t.to_lowercase(), |c| c.to_ascii_lowercase().to_string())
        }
        _ => return false,
    };
    key.eq_ignore_ascii_case(&logical) || (key.eq_ignore_ascii_case("space") && logical == " ")
}

#[derive(Default)]
pub struct Sequence {
    candidates: Vec<Binding>,
    depth: usize,
}

pub enum Match {
    None,
    Pending,
    Run(Binding, bool),
}
impl Sequence {
    pub fn clear(&mut self) {
        self.candidates.clear();
        self.depth = 0;
    }
    pub fn advance(
        &mut self,
        bindings: &[Binding],
        ev: &KeyEvent,
        physical: Option<PhysicalKeyCode>,
        base: Option<char>,
    ) -> Match {
        let depth = self.depth;
        if matches!(ev.key, Key::Modifier { .. }) {
            return Match::Pending;
        }
        if depth > 0 && ev.key == Key::Escape {
            self.clear();
            return Match::Pending;
        }
        let candidates = if depth == 0 {
            bindings
        } else {
            &self.candidates
        };
        let matches: Vec<_> = candidates
            .iter()
            .filter(|b| {
                b.keys
                    .get(depth)
                    .is_some_and(|k| chord_matches(k, ev, physical, base))
            })
            .cloned()
            .collect();
        let Some(first) = matches.first() else {
            self.clear();
            return Match::None;
        };
        if first.keys.len() == depth + 1 {
            let binding = first.clone();
            self.clear();
            return Match::Run(binding, depth > 0);
        }
        self.depth += 1;
        self.candidates = matches
            .into_iter()
            .filter(|b| b.keys.len() > depth + 1)
            .collect();
        Match::Pending
    }
}

pub fn bindings(s: &Workspace) -> Vec<Binding> {
    rex_linux::keymap::resolve(&s.settings.keybindings)
        .unwrap_or_else(|_| rex_linux::keymap::defaults())
}

pub fn on_key(s: &mut Workspace, ev: &KeyEvent, tx: &mut Tx, global: bool) -> bool {
    // Holding a prefix must not advance or leak the same key into the terminal.
    if tx.key_repeat()
        && tx
            .physical_key()
            .is_some_and(|c| s.consumed_keys.contains(&c))
    {
        return true;
    }
    let context = format!(
        "{}:{:?}:{:?}:{}:{}",
        s.active,
        s.model().selected,
        s.focused_block(),
        serde_json::to_string(&s.settings.keybindings).unwrap_or_default(),
        s.panel.is_some()
    );
    if s.binding_context.as_ref() != Some(&context) {
        s.sequence.clear();
        s.binding_context = Some(context);
    }
    let bindings = bindings(s);
    if global {
        let depth = s.sequence.depth;
        let candidates = if depth == 0 {
            &bindings
        } else {
            &s.sequence.candidates
        };
        let candidate = candidates.iter().find(|b| {
            b.keys
                .get(depth)
                .is_some_and(|k| chord_matches(k, ev, tx.physical_key(), tx.key_base()))
        });
        if !(depth > 0 && ev.key == Key::Escape)
            && !candidate.is_some_and(|b| b.action.starts_with("client.zoom."))
        {
            return false;
        }
    }
    let matched = s
        .sequence
        .advance(&bindings, ev, tx.physical_key(), tx.key_base());
    match matched {
        Match::None => false,
        Match::Pending => {
            if let Some(code) = tx.physical_key() {
                s.consumed_keys.insert(code);
            }
            true
        }
        Match::Run(binding, sequence) => {
            let suppress_repeat = sequence
                || !(binding.action.starts_with("client.zoom.")
                    || matches!(binding.action.as_str(), "pane.focus" | "pane.scroll"));
            perform(
                s,
                &binding.action,
                binding.args.unwrap_or_else(|| json!({})),
                tx,
            );
            if suppress_repeat && let Some(code) = tx.physical_key() {
                s.consumed_keys.insert(code);
            }
            true
        }
    }
}

pub fn perform(s: &mut Workspace, action: &str, args: Value, tx: &mut Tx) {
    use super::panel::Intent;
    use rex_linux::connection::Command;
    match action {
        "session.switch" => s.picker(true),
        "client.commands" => s.picker(false),
        "client.settings" => s.perform(Intent::Settings, tx),
        "client.sidebar.toggle" => s.perform(Intent::Sidebar, tx),
        "session.new" => s.perform(Intent::NewSession, tx),
        "client.tab.new" => s.perform(Intent::NewWindow, tx),
        "pane.close" => s.close_focused(tx),
        "pane.zoom" => s.perform(Intent::Zoom, tx),
        "pane.split" => match args["direction"].as_str() {
            Some("right") => s.perform(Intent::Split("horizontal"), tx),
            Some("down") => s.perform(Intent::Split("vertical"), tx),
            _ => s.notify("pane.split requires direction: right or down"),
        },
        "pane.focus" => {
            if matches!(
                args["direction"].as_str(),
                Some("left" | "right" | "up" | "down")
            ) {
                s.request("session.focus_direction", args);
            }
        }
        "client.tab.next" => s.request("session.focus_next_window", json!({})),
        "client.tab.previous" => s.request("session.focus_previous_window", json!({})),
        "pane.actions" => {
            if let Some(block) = s.focused_block() {
                s.pane_menu(block, None);
            }
        }
        "pane.copy" => {
            if let Some(text) = s.active_terminal().and_then(|t| t.copy_text()) {
                tx.copy(text);
            }
        }
        "pane.paste" => s.paste_clipboard(tx),
        "pane.reconnect" => {
            if let Some(term) = s.active_terminal_mut() {
                term.send(Command::Reconnect);
            }
        }
        "pane.scroll" => {
            if let Some(lines) = args["lines"].as_i64().and_then(|n| isize::try_from(n).ok())
                && let Some(term) = s.active_terminal_mut()
            {
                term.send(Command::Scroll(lines));
            }
        }
        "client.zoom.in" => s.change_ui_scale(1, tx),
        "client.zoom.out" => s.change_ui_scale(-1, tx),
        "client.zoom.reset" => s.change_ui_scale(0, tx),
        "pane.send_key" => {
            if let Some(key) = args["key"].as_str() {
                input::send_key(s, key);
            }
        }
        _ => s.notify(format!("Unknown client action: {action}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::Mods;
    fn event(key: &str, ctrl: bool) -> KeyEvent {
        KeyEvent {
            key: Key::Text(key.into()),
            mods: Mods {
                ctrl,
                ..Default::default()
            },
        }
    }
    #[test]
    fn sequences_complete_cancel_and_forward_only_the_unmatched_last_key() {
        let bindings = vec![
            Binding::new(["ctrl+b", "c", "s"], "session.switch"),
            Binding::new(["ctrl+b", "z"], "pane.zoom"),
        ];
        let mut seq = Sequence::default();
        assert!(matches!(
            seq.advance(&bindings, &event("b", true), None, None),
            Match::Pending
        ));
        assert!(matches!(
            seq.advance(&bindings, &event("c", false), None, None),
            Match::Pending
        ));
        let Match::Run(binding, sequence) = seq.advance(&bindings, &event("s", false), None, None)
        else {
            panic!("sequence did not complete");
        };
        assert_eq!(binding.action, "session.switch");
        assert!(sequence);
        seq.advance(&bindings, &event("b", true), None, None);
        assert!(matches!(
            seq.advance(
                &bindings,
                &KeyEvent {
                    key: Key::Escape,
                    mods: Mods::default()
                },
                None,
                None
            ),
            Match::Pending
        ));
        assert!(matches!(
            seq.advance(&bindings, &event("z", false), None, None),
            Match::None
        ));
        seq.advance(&bindings, &event("b", true), None, None);
        assert!(matches!(
            seq.advance(&bindings, &event("x", false), None, None),
            Match::None
        ));
        assert_eq!(seq.depth, 0);
    }
    #[test]
    fn priority_decides_between_immediate_actions_and_sequences() {
        let action = Binding::new(["ctrl+k"], "pane.zoom");
        let sequence = Binding::new(["ctrl+k", "s"], "session.switch");
        let mut seq = Sequence::default();
        assert!(matches!(
            seq.advance(
                &[action.clone(), sequence.clone()],
                &event("k", true),
                None,
                None
            ),
            Match::Run(_, _)
        ));
        assert!(matches!(
            seq.advance(&[sequence, action], &event("k", true), None, None),
            Match::Pending
        ));
    }
    #[test]
    fn logical_keys_follow_layout_and_physical_keys_ignore_it() {
        let ev = event("q", true);
        assert!(chord_matches(
            "CONTROL+a",
            &ev,
            Some(PhysicalKeyCode::KeyQ),
            Some('a')
        ));
        assert!(!chord_matches(
            "ctrl+q",
            &ev,
            Some(PhysicalKeyCode::KeyQ),
            Some('a')
        ));
        assert!(chord_matches(
            "ctrl+[KeyQ]",
            &ev,
            Some(PhysicalKeyCode::KeyQ),
            Some('a')
        ));
        assert!(!chord_matches("ctrl+shift+a", &ev, None, Some('a')));
        assert!(chord_matches(
            "ctrl+WIN+q",
            &KeyEvent {
                key: ev.key.clone(),
                mods: Mods {
                    ctrl: true,
                    logo: true,
                    ..Default::default()
                }
            },
            None,
            None
        ));
    }
    #[test]
    fn a_repeated_chord_completes_a_sequence() {
        let bindings = vec![Binding::new(["ctrl+b", "ctrl+b"], "pane.send_key")];
        let mut seq = Sequence::default();
        seq.advance(&bindings, &event("b", true), None, None);
        let Match::Run(binding, sequence) = seq.advance(&bindings, &event("b", true), None, None)
        else {
            panic!();
        };
        assert_eq!(binding.action, "pane.send_key");
        assert!(sequence);
        assert_eq!(seq.depth, 0);
    }
}
