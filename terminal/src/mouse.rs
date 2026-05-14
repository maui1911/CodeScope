//! Mouse-event encoding for `MOUSE_REPORT_*` modes.
//!
//! When a TUI enables mouse reporting (`\x1b[?1000h` and friends),
//! the terminal must encode each click/drag/wheel event back as an
//! escape sequence the TUI can read from stdin. Without it, apps
//! like tmux, vim, htop, and lazygit can't handle clicks at all.
//!
//! Two encodings ship with most terminals:
//!
//! * **X10 / default** (`\x1b[M b cx cy`) — three bytes after the
//!   marker, each offset by 32. Capped at column / row 223 (the
//!   max ASCII-printable byte). What every old terminal grew up
//!   speaking.
//! * **SGR** (`\x1b[<b;cx;cy{M,m}`) — decimal numbers, no cap. The
//!   modern default — every TUI written this decade enables `?1006`
//!   alongside `?1000` to opt in.
//!
//! We emit SGR when `SGR_MOUSE` is set on the term, otherwise X10.
//! That covers ~100% of in-the-wild apps — UTF-8 (`?1005`) and URxvt
//! (`?1015`) extensions exist but nobody uses them in practice.

use alacritty_terminal::term::TermMode;

/// What kind of mouse event we're encoding. The naming mirrors gpui's
/// own mouse events — Press / Release / Motion / Wheel — but the wire
/// format borrows numbering from xterm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    /// Motion while a button is held (xterm calls this "any-event"
    /// reporting). Distinct from [`Press`] because the encoded
    /// button gets `+32` to mark it as motion.
    Motion,
    WheelUp,
    WheelDown,
}

/// Mouse button — translated to xterm's button id at encode time.
/// Other buttons (X1/X2 on Windows mice, browser back/forward) map to
/// xterm's 8/9/10 slots; we only handle Left/Middle/Right for now —
/// 95% of TUI bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Modifier keys held while the mouse event happened. xterm encodes
/// these as `+4` (shift), `+8` (alt), `+16` (ctrl) on top of the
/// button id.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// Returns `true` when the term has *any* mouse-reporting mode active
/// — the View can short-circuit selection / scrollback when this is
/// on and route the event through [`encode`] instead.
pub fn mouse_reporting_enabled(mode: TermMode) -> bool {
    mode.intersects(
        TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
    )
}

/// Returns `true` when the term reports mouse motion while a button
/// is held (DECSET ?1002). The View uses this to decide whether to
/// emit `Motion` events at all — most TUIs only enable click+release.
pub fn drag_reporting_enabled(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
}

/// Encode a mouse event into the bytes a TUI expects on stdin.
///
/// `col` and `row` are 0-based grid coordinates; the encoding
/// converts them to xterm's 1-based scheme. Returns `None` when the
/// event can't be represented (e.g. column > 223 in X10 mode — too
/// far right to encode in the 8-bit-per-axis format).
pub fn encode(
    mode: TermMode,
    kind: MouseEventKind,
    button: Option<MouseButton>,
    modifiers: Modifiers,
    col: usize,
    row: usize,
) -> Option<Vec<u8>> {
    let mut button_code = match (kind, button) {
        (MouseEventKind::Press, Some(MouseButton::Left)) => 0,
        (MouseEventKind::Press, Some(MouseButton::Middle)) => 1,
        (MouseEventKind::Press, Some(MouseButton::Right)) => 2,
        (MouseEventKind::Release, Some(_)) => {
            if mode.contains(TermMode::SGR_MOUSE) {
                // SGR: button id stays, the trailing `m` (lowercase)
                // signals release.
                match button.unwrap() {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                }
            } else {
                // X10: release always emits button id 3.
                3
            }
        }
        (MouseEventKind::Motion, Some(b)) => {
            // Motion = button id + 32 ("motion bit"). Per xterm
            // semantics this is "this cell got entered while
            // <button> is held".
            let base = match b {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
            };
            base + 32
        }
        (MouseEventKind::Motion, None) => {
            // Pointer motion with no buttons — only emitted under
            // `?1003h`. Encoded as button id 3 + 32 = 35.
            35
        }
        (MouseEventKind::WheelUp, _) => 64,
        (MouseEventKind::WheelDown, _) => 65,
        // Release without a button doesn't make sense; bail.
        (MouseEventKind::Release, None) => return None,
        (MouseEventKind::Press, None) => return None,
    };
    if modifiers.shift { button_code += 4; }
    if modifiers.alt { button_code += 8; }
    if modifiers.control { button_code += 16; }

    let cx = col + 1;
    let cy = row + 1;

    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if matches!(kind, MouseEventKind::Release) { 'm' } else { 'M' };
        Some(format!("\x1b[<{button_code};{cx};{cy}{suffix}").into_bytes())
    } else {
        // X10: each byte caps at 255 - 32 = 223. Anything past that
        // can't be encoded; the TUI just doesn't see those clicks.
        if cx > 223 || cy > 223 || button_code > 223 {
            return None;
        }
        let mut out = Vec::with_capacity(6);
        out.extend_from_slice(b"\x1b[M");
        out.push(32u8 + button_code as u8);
        out.push(32u8 + cx as u8);
        out.push(32u8 + cy as u8);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sgr() -> TermMode { TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE }
    fn x10() -> TermMode { TermMode::MOUSE_REPORT_CLICK }

    #[test]
    fn sgr_press_left() {
        let bytes = encode(sgr(), MouseEventKind::Press, Some(MouseButton::Left), Modifiers::default(), 10, 5).unwrap();
        assert_eq!(bytes, b"\x1b[<0;11;6M");
    }

    #[test]
    fn sgr_release_uses_lowercase_m() {
        let bytes = encode(sgr(), MouseEventKind::Release, Some(MouseButton::Left), Modifiers::default(), 0, 0).unwrap();
        assert_eq!(bytes, b"\x1b[<0;1;1m");
    }

    #[test]
    fn sgr_wheel_up() {
        let bytes = encode(sgr(), MouseEventKind::WheelUp, None, Modifiers::default(), 0, 0).unwrap();
        assert_eq!(bytes, b"\x1b[<64;1;1M");
    }

    #[test]
    fn sgr_motion_with_left_button() {
        let bytes = encode(sgr(), MouseEventKind::Motion, Some(MouseButton::Left), Modifiers::default(), 9, 4).unwrap();
        assert_eq!(bytes, b"\x1b[<32;10;5M");
    }

    #[test]
    fn sgr_modifiers_stack_on_button_code() {
        let mods = Modifiers { shift: true, alt: false, control: true };
        let bytes = encode(sgr(), MouseEventKind::Press, Some(MouseButton::Right), mods, 0, 0).unwrap();
        // 2 (right) + 4 (shift) + 16 (ctrl) = 22.
        assert_eq!(bytes, b"\x1b[<22;1;1M");
    }

    #[test]
    fn x10_press_left() {
        let bytes = encode(x10(), MouseEventKind::Press, Some(MouseButton::Left), Modifiers::default(), 10, 5).unwrap();
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 32 + 11, 32 + 6]);
    }

    #[test]
    fn x10_release_uses_button_id_3() {
        let bytes = encode(x10(), MouseEventKind::Release, Some(MouseButton::Left), Modifiers::default(), 0, 0).unwrap();
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32 + 3, 32 + 1, 32 + 1]);
    }

    #[test]
    fn x10_overflow_returns_none() {
        let bytes = encode(x10(), MouseEventKind::Press, Some(MouseButton::Left), Modifiers::default(), 300, 0);
        assert!(bytes.is_none());
    }

    #[test]
    fn reporting_enabled_combines_modes() {
        assert!(!mouse_reporting_enabled(TermMode::empty()));
        assert!(mouse_reporting_enabled(TermMode::MOUSE_REPORT_CLICK));
        assert!(mouse_reporting_enabled(TermMode::MOUSE_DRAG));
        assert!(mouse_reporting_enabled(TermMode::MOUSE_MOTION));
    }
}
