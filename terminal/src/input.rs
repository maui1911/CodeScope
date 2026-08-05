//! Convert gpui keystrokes to terminal escape sequences.
//!
//! Started as `vendor/gpui-terminal/src/input.rs` (MIT OR Apache-2.0,
//! Leonard Seibold) — the standard xterm-style table every gpui
//! terminal needs. The upstream table only covers *unmodified* named
//! keys, so Ctrl+Left, Shift+End and friends arrived at the PTY as a
//! bare Left / End. The modifier encoding below is the missing half of
//! that table (xterm's `CSI 1 ; <mod> <final>` / `CSI <n> ; <mod> ~`
//! forms), so shells and coding agents see the chord the user typed.

use alacritty_terminal::term::TermMode;
use gpui::{Keystroke, Modifiers};

/// xterm's modifier parameter: `1 + shift(1) + alt(2) + ctrl(4)`.
/// `None` when no modifier is held — the caller's cue to emit the short
/// unmodified form (`CSI A`, `SS3 P`, `CSI 5 ~`, …).
///
/// `Modifiers::platform` (Cmd / Win) is deliberately *not* folded into
/// xterm's meta bit: on macOS it drives the app-level chords, and no
/// TUI expects `CSI 1;9D` for Cmd+Left.
fn modifier_param(mods: &Modifiers) -> Option<u8> {
    let bits = u8::from(mods.shift) | (u8::from(mods.alt) << 1) | (u8::from(mods.control) << 2);
    (bits != 0).then_some(bits + 1)
}

/// `CSI 1 ; <mod> <final>` — the modified form of the cursor keys,
/// Home / End and F1-F4.
fn csi_modified(final_byte: u8, m: u8) -> Vec<u8> {
    format!("\x1b[1;{m}{}", final_byte as char).into_bytes()
}

/// Cursor keys: `SS3 <final>` in DECCKM application-cursor mode,
/// `CSI <final>` otherwise — but always the `CSI 1 ; <mod>` form once a
/// modifier is held, because xterm has no modified SS3 encoding.
fn cursor_key(final_byte: u8, m: Option<u8>, mode: TermMode) -> Vec<u8> {
    match m {
        Some(m) => csi_modified(final_byte, m),
        None if mode.contains(TermMode::APP_CURSOR) => vec![b'\x1b', b'O', final_byte],
        None => vec![b'\x1b', b'[', final_byte],
    }
}

/// F1-F4 keep their historic `SS3` form when unmodified and switch to
/// `CSI 1 ; <mod> <final>` with a modifier — same as xterm.
fn function_key(final_byte: u8, m: Option<u8>) -> Vec<u8> {
    match m {
        Some(m) => csi_modified(final_byte, m),
        None => vec![b'\x1b', b'O', final_byte],
    }
}

/// `CSI <num> ; <mod> ~` — the tilde-terminated keypad / F5-F12 block.
/// The modifier parameter is omitted entirely when nothing is held.
fn csi_tilde(num: u16, m: Option<u8>) -> Vec<u8> {
    match m {
        Some(m) => format!("\x1b[{num};{m}~").into_bytes(),
        None => format!("\x1b[{num}~").into_bytes(),
    }
}

/// Convert a gpui keystroke into bytes for the PTY. Returns `None` for
/// modifier-only or otherwise non-producing keystrokes.
pub fn keystroke_to_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let mods = &keystroke.modifiers;
    let m = modifier_param(mods);

    match keystroke.key.as_str() {
        "space" => {
            if mods.control {
                return Some(b"\x00".to_vec());
            }
            return Some(b" ".to_vec());
        }
        "enter" => {
            // Alt+Enter follows the generic meta rule (ESC prefix).
            // Agents that bind meta-Enter to "insert newline instead of
            // submit" — claude-code among them — need the prefix; plain
            // Enter must stay a bare CR.
            return Some(if mods.alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            });
        }
        "escape" => return Some(b"\x1b".to_vec()),
        "backspace" => {
            // xterm's split, and the one ConPTY reverses back into key
            // events for console-mode apps on Windows: Backspace is
            // DEL, Ctrl+Backspace is BS. claude-code maps BS to
            // delete-previous-word on Windows, and readline maps
            // meta-DEL to backward-kill-word — both were unreachable
            // while every shape collapsed to DEL.
            let base: &[u8] = if mods.control { b"\x08" } else { b"\x7f" };
            let mut out = Vec::with_capacity(2);
            if mods.alt {
                out.push(b'\x1b');
            }
            out.extend_from_slice(base);
            return Some(out);
        }
        "tab" => {
            if mods.shift {
                return Some(b"\x1b[Z".to_vec());
            }
            return Some(b"\t".to_vec());
        }

        "up" => return Some(cursor_key(b'A', m, mode)),
        "down" => return Some(cursor_key(b'B', m, mode)),
        "right" => return Some(cursor_key(b'C', m, mode)),
        "left" => return Some(cursor_key(b'D', m, mode)),

        // Home / End share the cursor-key modifier encoding but have no
        // application-mode variant worth honouring here.
        "home" => {
            return Some(match m {
                Some(m) => csi_modified(b'H', m),
                None => b"\x1b[H".to_vec(),
            });
        }
        "end" => {
            return Some(match m {
                Some(m) => csi_modified(b'F', m),
                None => b"\x1b[F".to_vec(),
            });
        }

        "pageup" => return Some(csi_tilde(5, m)),
        "pagedown" => return Some(csi_tilde(6, m)),
        "insert" => return Some(csi_tilde(2, m)),
        "delete" => return Some(csi_tilde(3, m)),

        "f1" => return Some(function_key(b'P', m)),
        "f2" => return Some(function_key(b'Q', m)),
        "f3" => return Some(function_key(b'R', m)),
        "f4" => return Some(function_key(b'S', m)),
        "f5" => return Some(csi_tilde(15, m)),
        "f6" => return Some(csi_tilde(17, m)),
        "f7" => return Some(csi_tilde(18, m)),
        "f8" => return Some(csi_tilde(19, m)),
        "f9" => return Some(csi_tilde(20, m)),
        "f10" => return Some(csi_tilde(21, m)),
        "f11" => return Some(csi_tilde(23, m)),
        "f12" => return Some(csi_tilde(24, m)),

        _ => {}
    }

    if mods.control {
        let key = keystroke.key.as_str();
        if key.len() == 1 {
            let ch = key.chars().next().unwrap();
            if ch.is_ascii_alphabetic() {
                let upper = ch.to_ascii_uppercase();
                let ctrl_char = (upper as u8) - b'@';
                return Some(vec![ctrl_char]);
            }
            match ch {
                '[' => return Some(b"\x1b".to_vec()),
                '\\' => return Some(b"\x1c".to_vec()),
                ']' => return Some(b"\x1d".to_vec()),
                '^' => return Some(b"\x1e".to_vec()),
                '_' => return Some(b"\x1f".to_vec()),
                '?' => return Some(b"\x7f".to_vec()),
                _ => {}
            }
        }
    }

    if mods.alt {
        let key = keystroke.key.as_str();
        if key.len() == 1 {
            let ch = key.chars().next().unwrap();
            if ch.is_ascii() {
                return Some(vec![b'\x1b', ch as u8]);
            }
        }
    }

    if let Some(key_char) = &keystroke.key_char
        && !mods.control
        && !mods.alt
    {
        return Some(key_char.as_bytes().to_vec());
    }

    let key = keystroke.key.as_str();
    if key.len() == 1 {
        let ch = key.chars().next().unwrap();
        if ch.is_ascii() && !mods.control {
            let ch = if mods.shift {
                ch.to_ascii_uppercase()
            } else {
                ch
            };
            return Some(vec![ch as u8]);
        }
        if !mods.control && !mods.alt {
            return Some(key.as_bytes().to_vec());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(key: &str, mods: Modifiers) -> Keystroke {
        Keystroke {
            modifiers: mods,
            key: key.to_string(),
            key_char: None,
        }
    }

    /// Bytes for `key` + `mods` with the terminal in normal (non-DECCKM)
    /// mode, rendered as a string so failures read like the escape
    /// sequence they are.
    fn seq(key: &str, mods: Modifiers) -> String {
        let bytes = keystroke_to_bytes(&keystroke(key, mods), TermMode::empty())
            .unwrap_or_else(|| panic!("{key} produced no bytes"));
        String::from_utf8(bytes).expect("sequence is valid utf-8")
    }

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Default::default()
        }
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Default::default()
        }
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Default::default()
        }
    }

    fn ctrl_shift() -> Modifiers {
        Modifiers {
            control: true,
            shift: true,
            ..Default::default()
        }
    }

    #[test]
    fn unmodified_cursor_keys_keep_their_short_form() {
        assert_eq!(seq("up", plain()), "\x1b[A");
        assert_eq!(seq("down", plain()), "\x1b[B");
        assert_eq!(seq("right", plain()), "\x1b[C");
        assert_eq!(seq("left", plain()), "\x1b[D");
    }

    #[test]
    fn application_cursor_mode_switches_to_ss3() {
        let bytes = keystroke_to_bytes(&keystroke("up", plain()), TermMode::APP_CURSOR).unwrap();
        assert_eq!(bytes, b"\x1bOA");
    }

    #[test]
    fn modified_cursor_keys_use_the_xterm_parameter_table() {
        // 1 + shift(1) + alt(2) + ctrl(4) — the table every shell and
        // TUI decodes. Ctrl+Left is backward-word, Shift+Left starts a
        // selection, Ctrl+Shift+Left selects a word.
        assert_eq!(seq("left", shift()), "\x1b[1;2D");
        assert_eq!(seq("left", alt()), "\x1b[1;3D");
        assert_eq!(seq("left", ctrl()), "\x1b[1;5D");
        assert_eq!(seq("left", ctrl_shift()), "\x1b[1;6D");
        assert_eq!(seq("right", ctrl()), "\x1b[1;5C");
        assert_eq!(seq("up", ctrl_shift()), "\x1b[1;6A");
    }

    #[test]
    fn modified_cursor_keys_never_use_ss3_even_in_application_mode() {
        // xterm has no modified SS3 form; sending `SS3 D` with a
        // modifier parameter is not a thing any parser accepts.
        let bytes = keystroke_to_bytes(&keystroke("left", ctrl()), TermMode::APP_CURSOR).unwrap();
        assert_eq!(bytes, b"\x1b[1;5D");
    }

    #[test]
    fn ctrl_backspace_sends_bs_and_plain_backspace_sends_del() {
        // The split that makes Ctrl+Backspace delete a word: DEL for
        // Backspace, BS for Ctrl+Backspace, meta-DEL for Alt+Backspace
        // (readline's backward-kill-word).
        assert_eq!(seq("backspace", plain()), "\x7f");
        assert_eq!(seq("backspace", ctrl()), "\x08");
        assert_eq!(seq("backspace", alt()), "\x1b\x7f");
        assert_eq!(
            seq(
                "backspace",
                Modifiers {
                    control: true,
                    alt: true,
                    ..Default::default()
                }
            ),
            "\x1b\x08"
        );
        // Shift+Backspace has no separate encoding — still DEL.
        assert_eq!(seq("backspace", shift()), "\x7f");
    }

    #[test]
    fn home_end_and_tilde_keys_carry_modifiers() {
        assert_eq!(seq("home", plain()), "\x1b[H");
        assert_eq!(seq("home", ctrl()), "\x1b[1;5H");
        assert_eq!(seq("end", shift()), "\x1b[1;2F");
        assert_eq!(seq("delete", plain()), "\x1b[3~");
        // Ctrl+Delete — delete-word-forward in readline / PSReadLine.
        assert_eq!(seq("delete", ctrl()), "\x1b[3;5~");
        assert_eq!(seq("pageup", shift()), "\x1b[5;2~");
    }

    #[test]
    fn function_keys_switch_from_ss3_to_csi_when_modified() {
        assert_eq!(seq("f1", plain()), "\x1bOP");
        assert_eq!(seq("f1", ctrl()), "\x1b[1;5P");
        assert_eq!(seq("f5", plain()), "\x1b[15~");
        assert_eq!(seq("f5", shift()), "\x1b[15;2~");
    }

    #[test]
    fn enter_stays_bare_cr_unless_alt_is_held() {
        assert_eq!(seq("enter", plain()), "\r");
        assert_eq!(seq("enter", shift()), "\r");
        assert_eq!(seq("enter", ctrl()), "\r");
        assert_eq!(seq("enter", alt()), "\x1b\r");
    }

    #[test]
    fn control_characters_and_shift_tab_are_unchanged() {
        // Regressions guard for the pre-existing table.
        assert_eq!(seq("c", ctrl()), "\x03");
        assert_eq!(seq("w", ctrl()), "\x17");
        assert_eq!(seq("space", ctrl()), "\0");
        assert_eq!(seq("tab", plain()), "\t");
        assert_eq!(seq("tab", shift()), "\x1b[Z");
        assert_eq!(seq("escape", plain()), "\x1b");
    }

    #[test]
    fn modifier_only_keystrokes_produce_nothing() {
        assert!(keystroke_to_bytes(&keystroke("shift", shift()), TermMode::empty()).is_none());
        assert!(keystroke_to_bytes(&keystroke("control", ctrl()), TermMode::empty()).is_none());
    }
}
