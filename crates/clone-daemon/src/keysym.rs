//! X11 keysym lookup for the MCP `key`/`type` tools, injected via
//! `org.gnome.Mutter.RemoteDesktop.Session.NotifyKeyboardKeysym`.
//!
//! Mutter maps keysyms to keycodes internally (allocating a spare keycode if the
//! active layout has none), so no XKB handling is needed here — including shift
//! levels: sending keysym `A` produces an uppercase A without a synthetic Shift.
//! Ported from `../../computer-use/src/keysym.rs`.

use anyhow::{Result, bail};

/// Named keysyms accepted in key combos, xdotool-style. Case-sensitive, matching
/// X11 keysym names (`Return`, `Page_Up`, …) plus a few aliases.
fn named_keysym(name: &str) -> Option<u32> {
    Some(match name {
        "Return" | "Enter" => 0xff0d,
        "KP_Enter" => 0xff8d,
        "Tab" => 0xff09,
        "ISO_Left_Tab" => 0xfe20,
        "space" => 0x0020,
        "BackSpace" => 0xff08,
        "Delete" => 0xffff,
        "Insert" => 0xff63,
        "Escape" => 0xff1b,
        "Home" => 0xff50,
        "End" => 0xff57,
        "Left" => 0xff51,
        "Up" => 0xff52,
        "Right" => 0xff53,
        "Down" => 0xff54,
        "Page_Up" | "Prior" => 0xff55,
        "Page_Down" | "Next" => 0xff56,
        "Menu" => 0xff67,
        "Print" => 0xff61,
        "Pause" => 0xff13,
        "Scroll_Lock" => 0xff14,
        "Caps_Lock" => 0xffe5,
        "Num_Lock" => 0xff7f,
        "Control_L" => 0xffe3,
        "Control_R" => 0xffe4,
        "Shift_L" => 0xffe1,
        "Shift_R" => 0xffe2,
        "Alt_L" => 0xffe9,
        "Alt_R" => 0xffea,
        "Super_L" => 0xffeb,
        "Super_R" => 0xffec,
        "Meta_L" => 0xffe7,
        "F1" => 0xffbe,
        "F2" => 0xffbf,
        "F3" => 0xffc0,
        "F4" => 0xffc1,
        "F5" => 0xffc2,
        "F6" => 0xffc3,
        "F7" => 0xffc4,
        "F8" => 0xffc5,
        "F9" => 0xffc6,
        "F10" => 0xffc7,
        "F11" => 0xffc8,
        "F12" => 0xffc9,
        "XF86AudioRaiseVolume" => 0x1008ff13,
        "XF86AudioLowerVolume" => 0x1008ff11,
        "XF86AudioMute" => 0x1008ff12,
        _ => return None,
    })
}

/// Modifier aliases accepted in combos (matched case-insensitively).
fn modifier_keysym(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => 0xffe3,               // Control_L
        "shift" => 0xffe1,                          // Shift_L
        "alt" => 0xffe9,                            // Alt_L
        "super" | "meta" | "win" | "cmd" => 0xffeb, // Super_L
        _ => return None,
    })
}

/// Map a character to its X11 keysym. Latin-1 printables map to their codepoint;
/// everything else maps to `0x01000000 | codepoint`.
pub fn char_to_keysym(c: char) -> Option<u32> {
    match c {
        '\n' | '\r' => Some(0xff0d), // Return
        '\t' => Some(0xff09),        // Tab
        ' '..='~' => Some(c as u32),
        '\u{a0}'..='\u{ff}' => Some(c as u32),
        c if (c as u32) >= 0x100 => Some(0x0100_0000 | c as u32),
        _ => None,
    }
}

/// Parse an xdotool-style key combo (`ctrl+c`, `Return`, `alt+Tab`, `ctrl+shift+t`)
/// into an ordered list of keysyms: press in order, release in reverse.
pub fn parse_key_combo(combo: &str) -> Result<Vec<u32>> {
    let tokens: Vec<&str> = combo.split('+').map(str::trim).collect();
    if tokens.is_empty() || tokens.iter().any(|t| t.is_empty()) {
        bail!("invalid key combo: {combo:?}");
    }
    let mut keysyms = Vec::with_capacity(tokens.len());
    for (i, token) in tokens.iter().enumerate() {
        let is_last = i == tokens.len() - 1;
        let sym = if !is_last {
            modifier_keysym(token)
                .or_else(|| named_keysym(token))
                .ok_or_else(|| anyhow::anyhow!("unknown modifier {token:?} in combo {combo:?}"))?
        } else if let Some(sym) = named_keysym(token) {
            sym
        } else if let Some(sym) = modifier_keysym(token) {
            sym
        } else {
            let mut chars = token.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => char_to_keysym(c)
                    .ok_or_else(|| anyhow::anyhow!("cannot map character {c:?} to a keysym"))?,
                _ => bail!("unknown key {token:?} in combo {combo:?}"),
            }
        };
        keysyms.push(sym);
    }
    Ok(keysyms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_c() {
        assert_eq!(parse_key_combo("ctrl+c").unwrap(), vec![0xffe3, 'c' as u32]);
    }
    #[test]
    fn alt_tab() {
        assert_eq!(parse_key_combo("alt+Tab").unwrap(), vec![0xffe9, 0xff09]);
    }
    #[test]
    fn unicode() {
        assert_eq!(char_to_keysym('é'), Some(0xe9));
        assert_eq!(char_to_keysym('中'), Some(0x0100_0000 | '中' as u32));
    }
    #[test]
    fn invalid() {
        assert!(parse_key_combo("").is_err());
        assert!(parse_key_combo("ctrl+").is_err());
    }
}
