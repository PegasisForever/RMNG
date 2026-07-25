//! Carbon `kVK_*` → Linux evdev `KEY_*` translation table.
//!
//! On macOS, GDK reports `hardware_keycode` as the Carbon virtual key code
//! (0x00-0x7F).  The wire protocol expects Linux evdev keycodes
//! (`input-event-codes.h KEY_*`).  This module provides a single 128-entry
//! lookup table that maps kVK indices to evdev values.
//!
//! **Derivation source:** Chromium `ui/events/keycodes/dom/dom_code_data.inc`.
//! Each row in that file ties one USB HID usage code, one Linux evdev code, one
//! XKB code (= evdev + 8), and one macOS kVK code together.  Where the physical
//! USB position differs from the key's *label* on Apple keyboards (F13/F14/F15
//! occupy the PrintScreen/ScrollLock/Pause USB slots), the USB-position evdev
//! code is used — matching what Chromium itself sends.
//!
//! **Sentinel:** value `0` means the kVK has no evdev equivalent (unassigned
//! slot, the `fn` modifier which is not a transmittable key, or a JIS key whose
//! mapping is uncertain).  Callers must skip the event rather than send 0.
//!
//! **Uncertain / sentineled keys** (see `UNMAPPED` entries below):
//! - `0x34` — unassigned in Apple's kVK table
//! - `0x3F` `kVK_Function` — the `fn` modifier; not a Linux key event
//! - `0x42`, `0x44`, `0x46`, `0x4D` — gaps in the keypad layout
//! - `0x66` `kVK_JIS_Eisu` — uncertain evdev mapping (DOM code "Lang2" vs
//!   "NonConvert" varies across sources); sentineled until verified on hardware
//! - `0x68` `kVK_JIS_Kana` — similarly uncertain ("Lang1" / "KatakanaHiragana")
//! - `0x6C`, `0x70`, `0x7F` — unassigned

/// Sentinel: no evdev equivalent — caller must skip the event.
const U: u8 = 0;

/// Carbon kVK index → Linux evdev `KEY_*` code, or 0 (sentinel / skip).
///
/// Array index = kVK code (0x00-0x7F).  All 128 entries are present;
/// out-of-range kVK values are rejected by `translate`.
///
/// Evdev values sourced from Chromium `dom_code_data.inc` rows.
/// Every value fits in `u8` (max used: 190 = `KEY_F20`).
#[rustfmt::skip]
pub static KVK_TO_EVDEV: [u8; 128] = [
    //  kVK   label                   evdev  KEY_*
    /*0x00*/ 30, // kVK_ANSI_A             KEY_A
    /*0x01*/ 31, // kVK_ANSI_S             KEY_S
    /*0x02*/ 32, // kVK_ANSI_D             KEY_D
    /*0x03*/ 33, // kVK_ANSI_F             KEY_F
    /*0x04*/ 35, // kVK_ANSI_H             KEY_H
    /*0x05*/ 34, // kVK_ANSI_G             KEY_G
    /*0x06*/ 44, // kVK_ANSI_Z             KEY_Z
    /*0x07*/ 45, // kVK_ANSI_X             KEY_X
    /*0x08*/ 46, // kVK_ANSI_C             KEY_C
    /*0x09*/ 47, // kVK_ANSI_V             KEY_V
    /*0x0A*/ 86, // kVK_ISO_Section        KEY_102ND  (ISO keyboards)
    /*0x0B*/ 48, // kVK_ANSI_B             KEY_B
    /*0x0C*/ 16, // kVK_ANSI_Q             KEY_Q
    /*0x0D*/ 17, // kVK_ANSI_W             KEY_W
    /*0x0E*/ 18, // kVK_ANSI_E             KEY_E
    /*0x0F*/ 19, // kVK_ANSI_R             KEY_R
    /*0x10*/ 21, // kVK_ANSI_Y             KEY_Y
    /*0x11*/ 20, // kVK_ANSI_T             KEY_T
    /*0x12*/  2, // kVK_ANSI_1             KEY_1
    /*0x13*/  3, // kVK_ANSI_2             KEY_2
    /*0x14*/  4, // kVK_ANSI_3             KEY_3
    /*0x15*/  5, // kVK_ANSI_4             KEY_4
    /*0x16*/  7, // kVK_ANSI_6             KEY_6
    /*0x17*/  6, // kVK_ANSI_5             KEY_5
    /*0x18*/ 13, // kVK_ANSI_Equal         KEY_EQUAL
    /*0x19*/ 10, // kVK_ANSI_9             KEY_9
    /*0x1A*/  8, // kVK_ANSI_7             KEY_7
    /*0x1B*/ 12, // kVK_ANSI_Minus         KEY_MINUS
    /*0x1C*/  9, // kVK_ANSI_8             KEY_8
    /*0x1D*/ 11, // kVK_ANSI_0             KEY_0
    /*0x1E*/ 27, // kVK_ANSI_RightBracket  KEY_RIGHTBRACE
    /*0x1F*/ 24, // kVK_ANSI_O             KEY_O
    /*0x20*/ 22, // kVK_ANSI_U             KEY_U
    /*0x21*/ 26, // kVK_ANSI_LeftBracket   KEY_LEFTBRACE
    /*0x22*/ 23, // kVK_ANSI_I             KEY_I
    /*0x23*/ 25, // kVK_ANSI_P             KEY_P
    /*0x24*/ 28, // kVK_Return             KEY_ENTER
    /*0x25*/ 38, // kVK_ANSI_L             KEY_L
    /*0x26*/ 36, // kVK_ANSI_J             KEY_J
    /*0x27*/ 40, // kVK_ANSI_Quote         KEY_APOSTROPHE
    /*0x28*/ 37, // kVK_ANSI_K             KEY_K
    /*0x29*/ 39, // kVK_ANSI_Semicolon     KEY_SEMICOLON
    /*0x2A*/ 43, // kVK_ANSI_Backslash     KEY_BACKSLASH
    /*0x2B*/ 51, // kVK_ANSI_Comma         KEY_COMMA
    /*0x2C*/ 53, // kVK_ANSI_Slash         KEY_SLASH
    /*0x2D*/ 49, // kVK_ANSI_N             KEY_N
    /*0x2E*/ 50, // kVK_ANSI_M             KEY_M
    /*0x2F*/ 52, // kVK_ANSI_Period        KEY_DOT
    /*0x30*/ 15, // kVK_Tab                KEY_TAB
    /*0x31*/ 57, // kVK_Space              KEY_SPACE
    /*0x32*/ 41, // kVK_ANSI_Grave         KEY_GRAVE
    /*0x33*/ 14, // kVK_Delete (Backspace) KEY_BACKSPACE
    /*0x34*/  U, // (unassigned)
    /*0x35*/  1, // kVK_Escape             KEY_ESC
    /*0x36*/126, // kVK_RightCommand       KEY_RIGHTMETA
    /*0x37*/125, // kVK_Command            KEY_LEFTMETA
    /*0x38*/ 42, // kVK_Shift              KEY_LEFTSHIFT
    /*0x39*/ 58, // kVK_CapsLock           KEY_CAPSLOCK
    /*0x3A*/ 56, // kVK_Option             KEY_LEFTALT
    /*0x3B*/ 29, // kVK_Control            KEY_LEFTCTRL
    /*0x3C*/ 54, // kVK_RightShift         KEY_RIGHTSHIFT
    /*0x3D*/100, // kVK_RightOption        KEY_RIGHTALT
    /*0x3E*/ 97, // kVK_RightControl       KEY_RIGHTCTRL
    /*0x3F*/  U, // kVK_Function           (fn modifier — not transmittable)
    /*0x40*/187, // kVK_F17                KEY_F17
    /*0x41*/ 83, // kVK_ANSI_KeypadDecimal KEY_KPDOT
    /*0x42*/  U, // (unassigned)
    /*0x43*/ 55, // kVK_ANSI_KeypadMultiply KEY_KPASTERISK
    /*0x44*/  U, // (unassigned)
    /*0x45*/ 78, // kVK_ANSI_KeypadPlus    KEY_KPPLUS
    /*0x46*/  U, // (unassigned)
    /*0x47*/ 69, // kVK_ANSI_KeypadClear   KEY_NUMLOCK  (NumLock physical position)
    /*0x48*/115, // kVK_VolumeUp           KEY_VOLUMEUP
    /*0x49*/114, // kVK_VolumeDown         KEY_VOLUMEDOWN
    /*0x4A*/113, // kVK_Mute               KEY_MUTE
    /*0x4B*/ 98, // kVK_ANSI_KeypadDivide  KEY_KPSLASH
    /*0x4C*/ 96, // kVK_ANSI_KeypadEnter   KEY_KPENTER
    /*0x4D*/  U, // (unassigned)
    /*0x4E*/ 74, // kVK_ANSI_KeypadMinus   KEY_KPMINUS
    /*0x4F*/188, // kVK_F18                KEY_F18
    /*0x50*/189, // kVK_F19                KEY_F19
    /*0x51*/117, // kVK_ANSI_KeypadEquals  KEY_KPEQUAL
    /*0x52*/ 82, // kVK_ANSI_Keypad0       KEY_KP0
    /*0x53*/ 79, // kVK_ANSI_Keypad1       KEY_KP1
    /*0x54*/ 80, // kVK_ANSI_Keypad2       KEY_KP2
    /*0x55*/ 81, // kVK_ANSI_Keypad3       KEY_KP3
    /*0x56*/ 75, // kVK_ANSI_Keypad4       KEY_KP4
    /*0x57*/ 76, // kVK_ANSI_Keypad5       KEY_KP5
    /*0x58*/ 77, // kVK_ANSI_Keypad6       KEY_KP6
    /*0x59*/ 71, // kVK_ANSI_Keypad7       KEY_KP7
    /*0x5A*/190, // kVK_F20                KEY_F20
    /*0x5B*/ 72, // kVK_ANSI_Keypad8       KEY_KP8
    /*0x5C*/ 73, // kVK_ANSI_Keypad9       KEY_KP9
    /*0x5D*/124, // kVK_JIS_Yen            KEY_YEN
    /*0x5E*/121, // kVK_JIS_Underscore     KEY_RO
    /*0x5F*/ 95, // kVK_JIS_KeypadComma    KEY_KPJPCOMMA
    /*0x60*/ 63, // kVK_F5                 KEY_F5
    /*0x61*/ 64, // kVK_F6                 KEY_F6
    /*0x62*/ 65, // kVK_F7                 KEY_F7
    /*0x63*/ 61, // kVK_F3                 KEY_F3
    /*0x64*/ 66, // kVK_F8                 KEY_F8
    /*0x65*/ 67, // kVK_F9                 KEY_F9
    /*0x66*/  U, // kVK_JIS_Eisu           (uncertain: DOM "Lang2" vs "NonConvert")
    /*0x67*/ 87, // kVK_F11                KEY_F11
    /*0x68*/  U, // kVK_JIS_Kana           (uncertain: DOM "Lang1" vs "KatakanaHiragana")
    /*0x69*/ 99, // kVK_F13               KEY_SYSRQ   (USB PrintScreen physical pos)
    /*0x6A*/186, // kVK_F16                KEY_F16
    /*0x6B*/ 70, // kVK_F14               KEY_SCROLLLOCK (USB ScrollLock physical pos)
    /*0x6C*/  U, // (unassigned)
    /*0x6D*/ 68, // kVK_F10               KEY_F10
    /*0x6E*/127, // kVK_ContextualMenu    KEY_COMPOSE  (PC "Menu" key; Linux calls it Compose)
    /*0x6F*/ 88, // kVK_F12               KEY_F12
    /*0x70*/  U, // (unassigned)
    /*0x71*/119, // kVK_F15               KEY_PAUSE    (USB Pause physical pos)
    /*0x72*/110, // kVK_Help              KEY_INSERT   (Insert on any PC keyboard; Chromium agrees)
    /*0x73*/102, // kVK_Home              KEY_HOME
    /*0x74*/104, // kVK_PageUp            KEY_PAGEUP
    /*0x75*/111, // kVK_ForwardDelete     KEY_DELETE   (the Del key, not Backspace)
    /*0x76*/ 62, // kVK_F4               KEY_F4
    /*0x77*/107, // kVK_End              KEY_END
    /*0x78*/ 60, // kVK_F2              KEY_F2
    /*0x79*/109, // kVK_PageDown         KEY_PAGEDOWN
    /*0x7A*/ 59, // kVK_F1             KEY_F1
    /*0x7B*/105, // kVK_LeftArrow       KEY_LEFT
    /*0x7C*/106, // kVK_RightArrow      KEY_RIGHT
    /*0x7D*/108, // kVK_DownArrow       KEY_DOWN
    /*0x7E*/103, // kVK_UpArrow         KEY_UP
    /*0x7F*/  U, // (unassigned)
];

/// Translate a Carbon kVK `hardware_keycode` to a Linux evdev `KEY_*` code.
///
/// Returns `0` if the kVK is out of range or has no evdev equivalent (sentinel).
/// Callers must skip sending a key event when the return value is `0`.
#[inline]
pub fn translate(kv: u32) -> u32 {
    KVK_TO_EVDEV.get(kv as usize).copied().unwrap_or(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spot-checks against the invariants listed in the task brief.
    // These pin the table against accidental typos during future edits.

    #[test]
    fn letters() {
        assert_eq!(translate(0x00), 30, "kVK_ANSI_A → KEY_A");
        assert_eq!(translate(0x01), 31, "kVK_ANSI_S → KEY_S");
        assert_eq!(translate(0x06), 44, "kVK_ANSI_Z → KEY_Z");
        assert_eq!(translate(0x0C), 16, "kVK_ANSI_Q → KEY_Q");
        assert_eq!(translate(0x0D), 17, "kVK_ANSI_W → KEY_W");
        assert_eq!(translate(0x0E), 18, "kVK_ANSI_E → KEY_E");
        assert_eq!(translate(0x0F), 19, "kVK_ANSI_R → KEY_R");
        assert_eq!(translate(0x10), 21, "kVK_ANSI_Y → KEY_Y");
        assert_eq!(translate(0x11), 20, "kVK_ANSI_T → KEY_T");
        assert_eq!(translate(0x23), 25, "kVK_ANSI_P → KEY_P");
        assert_eq!(translate(0x2D), 49, "kVK_ANSI_N → KEY_N");
        assert_eq!(translate(0x2E), 50, "kVK_ANSI_M → KEY_M");
    }

    #[test]
    fn digits() {
        assert_eq!(translate(0x12),  2, "kVK_ANSI_1 → KEY_1");
        assert_eq!(translate(0x13),  3, "kVK_ANSI_2 → KEY_2");
        assert_eq!(translate(0x14),  4, "kVK_ANSI_3 → KEY_3");
        assert_eq!(translate(0x17),  6, "kVK_ANSI_5 → KEY_5");
        assert_eq!(translate(0x16),  7, "kVK_ANSI_6 → KEY_6");
        assert_eq!(translate(0x1A),  8, "kVK_ANSI_7 → KEY_7");
        assert_eq!(translate(0x1C),  9, "kVK_ANSI_8 → KEY_8");
        assert_eq!(translate(0x19), 10, "kVK_ANSI_9 → KEY_9");
        assert_eq!(translate(0x1D), 11, "kVK_ANSI_0 → KEY_0");
        assert_eq!(translate(0x15),  5, "kVK_ANSI_4 → KEY_4");
    }

    #[test]
    fn modifiers() {
        assert_eq!(translate(0x37), 125, "kVK_Command       → KEY_LEFTMETA");
        assert_eq!(translate(0x36), 126, "kVK_RightCommand  → KEY_RIGHTMETA");
        assert_eq!(translate(0x3A),  56, "kVK_Option        → KEY_LEFTALT");
        assert_eq!(translate(0x3D), 100, "kVK_RightOption   → KEY_RIGHTALT");
        assert_eq!(translate(0x3B),  29, "kVK_Control       → KEY_LEFTCTRL");
        assert_eq!(translate(0x3E),  97, "kVK_RightControl  → KEY_RIGHTCTRL");
        assert_eq!(translate(0x38),  42, "kVK_Shift         → KEY_LEFTSHIFT");
        assert_eq!(translate(0x3C),  54, "kVK_RightShift    → KEY_RIGHTSHIFT");
        assert_eq!(translate(0x39),  58, "kVK_CapsLock      → KEY_CAPSLOCK");
    }

    #[test]
    fn special_keys() {
        assert_eq!(translate(0x24), 28, "kVK_Return → KEY_ENTER");
        assert_eq!(translate(0x30), 15, "kVK_Tab    → KEY_TAB");
        assert_eq!(translate(0x31), 57, "kVK_Space  → KEY_SPACE");
        assert_eq!(translate(0x33), 14, "kVK_Delete (Backspace) → KEY_BACKSPACE");
        assert_eq!(translate(0x35),  1, "kVK_Escape → KEY_ESC");
        assert_eq!(translate(0x75),111, "kVK_ForwardDelete → KEY_DELETE");
    }

    #[test]
    fn arrows_and_navigation() {
        assert_eq!(translate(0x7B), 105, "kVK_LeftArrow  → KEY_LEFT");
        assert_eq!(translate(0x7C), 106, "kVK_RightArrow → KEY_RIGHT");
        assert_eq!(translate(0x7D), 108, "kVK_DownArrow  → KEY_DOWN");
        assert_eq!(translate(0x7E), 103, "kVK_UpArrow    → KEY_UP");
        assert_eq!(translate(0x73), 102, "kVK_Home       → KEY_HOME");
        assert_eq!(translate(0x74), 104, "kVK_PageUp     → KEY_PAGEUP");
        assert_eq!(translate(0x77), 107, "kVK_End        → KEY_END");
        assert_eq!(translate(0x79), 109, "kVK_PageDown   → KEY_PAGEDOWN");
    }

    #[test]
    fn punctuation() {
        assert_eq!(translate(0x1B), 12, "kVK_ANSI_Minus        → KEY_MINUS (12)");
        assert_eq!(translate(0x18), 13, "kVK_ANSI_Equal        → KEY_EQUAL (13)");
        assert_eq!(translate(0x21), 26, "kVK_ANSI_LeftBracket  → KEY_LEFTBRACE (26)");
        assert_eq!(translate(0x1E), 27, "kVK_ANSI_RightBracket → KEY_RIGHTBRACE (27)");
        assert_eq!(translate(0x29), 39, "kVK_ANSI_Semicolon    → KEY_SEMICOLON (39)");
        assert_eq!(translate(0x27), 40, "kVK_ANSI_Quote        → KEY_APOSTROPHE (40)");
        assert_eq!(translate(0x2A), 43, "kVK_ANSI_Backslash    → KEY_BACKSLASH (43)");
        assert_eq!(translate(0x2B), 51, "kVK_ANSI_Comma        → KEY_COMMA (51)");
        assert_eq!(translate(0x2F), 52, "kVK_ANSI_Period       → KEY_DOT (52)");
        assert_eq!(translate(0x2C), 53, "kVK_ANSI_Slash        → KEY_SLASH (53)");
        assert_eq!(translate(0x32), 41, "kVK_ANSI_Grave        → KEY_GRAVE (41)");
    }

    #[test]
    /// 0x72 is Help on a 1990s Apple keyboard and **Insert** on every PC keyboard attached to a
    /// Mac; Chromium's dom_code_data.inc maps kVK 0x72 → INSERT. KEY_HELP(138) made Shift+Insert
    /// and Ctrl+Insert unreachable, which is how paste works in a lot of remote software.
    #[test]
    fn help_key_is_insert() {
        assert_eq!(translate(0x72), 110, "kVK_Help/Insert → KEY_INSERT");
    }

    /// Apple's HIToolbox/Events.h defines kVK_ContextualMenu = 0x6E — the PC Menu key. Linux
    /// delivers that key as KEY_COMPOSE(127), not KEY_MENU(139).
    #[test]
    fn contextual_menu_is_compose() {
        assert_eq!(translate(0x6E), 127, "kVK_ContextualMenu → KEY_COMPOSE");
    }

    #[test]
    fn function_keys() {
        assert_eq!(translate(0x7A),  59, "kVK_F1  → KEY_F1");
        assert_eq!(translate(0x78),  60, "kVK_F2  → KEY_F2");
        assert_eq!(translate(0x63),  61, "kVK_F3  → KEY_F3");
        assert_eq!(translate(0x76),  62, "kVK_F4  → KEY_F4");
        assert_eq!(translate(0x60),  63, "kVK_F5  → KEY_F5");
        assert_eq!(translate(0x61),  64, "kVK_F6  → KEY_F6");
        assert_eq!(translate(0x62),  65, "kVK_F7  → KEY_F7");
        assert_eq!(translate(0x64),  66, "kVK_F8  → KEY_F8");
        assert_eq!(translate(0x65),  67, "kVK_F9  → KEY_F9");
        assert_eq!(translate(0x6D),  68, "kVK_F10 → KEY_F10");
        assert_eq!(translate(0x67),  87, "kVK_F11 → KEY_F11");
        assert_eq!(translate(0x6F),  88, "kVK_F12 → KEY_F12");
        // F13-F15 are in USB PrintScreen/ScrollLock/Pause physical positions per Chromium
        assert_eq!(translate(0x69),  99, "kVK_F13 → KEY_SYSRQ (PrintScreen physical)");
        assert_eq!(translate(0x6B),  70, "kVK_F14 → KEY_SCROLLLOCK");
        assert_eq!(translate(0x71), 119, "kVK_F15 → KEY_PAUSE");
        assert_eq!(translate(0x6A), 186, "kVK_F16 → KEY_F16");
        assert_eq!(translate(0x40), 187, "kVK_F17 → KEY_F17");
        assert_eq!(translate(0x4F), 188, "kVK_F18 → KEY_F18");
        assert_eq!(translate(0x50), 189, "kVK_F19 → KEY_F19");
        assert_eq!(translate(0x5A), 190, "kVK_F20 → KEY_F20");
    }

    #[test]
    fn keypad() {
        assert_eq!(translate(0x52),  82, "Keypad0 → KEY_KP0");
        assert_eq!(translate(0x53),  79, "Keypad1 → KEY_KP1");
        assert_eq!(translate(0x54),  80, "Keypad2 → KEY_KP2");
        assert_eq!(translate(0x55),  81, "Keypad3 → KEY_KP3");
        assert_eq!(translate(0x56),  75, "Keypad4 → KEY_KP4");
        assert_eq!(translate(0x57),  76, "Keypad5 → KEY_KP5");
        assert_eq!(translate(0x58),  77, "Keypad6 → KEY_KP6");
        assert_eq!(translate(0x59),  71, "Keypad7 → KEY_KP7");
        assert_eq!(translate(0x5B),  72, "Keypad8 → KEY_KP8");
        assert_eq!(translate(0x5C),  73, "Keypad9 → KEY_KP9");
        assert_eq!(translate(0x41),  83, "KeypadDecimal  → KEY_KPDOT");
        assert_eq!(translate(0x43),  55, "KeypadMultiply → KEY_KPASTERISK");
        assert_eq!(translate(0x45),  78, "KeypadPlus     → KEY_KPPLUS");
        assert_eq!(translate(0x47),  69, "KeypadClear    → KEY_NUMLOCK");
        assert_eq!(translate(0x4B),  98, "KeypadDivide   → KEY_KPSLASH");
        assert_eq!(translate(0x4C),  96, "KeypadEnter    → KEY_KPENTER");
        assert_eq!(translate(0x4E),  74, "KeypadMinus    → KEY_KPMINUS");
        assert_eq!(translate(0x51), 117, "KeypadEquals   → KEY_KPEQUAL");
    }

    #[test]
    fn jis_and_iso() {
        assert_eq!(translate(0x0A),  86, "kVK_ISO_Section      → KEY_102ND");
        assert_eq!(translate(0x5D), 124, "kVK_JIS_Yen          → KEY_YEN");
        assert_eq!(translate(0x5E), 121, "kVK_JIS_Underscore   → KEY_RO");
        assert_eq!(translate(0x5F),  95, "kVK_JIS_KeypadComma  → KEY_KPJPCOMMA");
    }

    #[test]
    fn sentinels() {
        // These must all return 0 so the caller drops the event.
        assert_eq!(translate(0x34),   0, "0x34 unassigned → sentinel");
        assert_eq!(translate(0x3F),   0, "kVK_Function → sentinel (not transmittable)");
        assert_eq!(translate(0x66),   0, "kVK_JIS_Eisu → sentinel (uncertain)");
        assert_eq!(translate(0x68),   0, "kVK_JIS_Kana → sentinel (uncertain)");
        assert_eq!(translate(0x7F),   0, "0x7F unassigned → sentinel");
        // Out-of-range kVK must also return sentinel.
        assert_eq!(translate(0x80),   0, "kVK 0x80 (out of range) → sentinel");
        assert_eq!(translate(0xFF),   0, "kVK 0xFF (out of range) → sentinel");
    }

    #[test]
    fn media_keys() {
        assert_eq!(translate(0x48), 115, "kVK_VolumeUp   → KEY_VOLUMEUP");
        assert_eq!(translate(0x49), 114, "kVK_VolumeDown → KEY_VOLUMEDOWN");
        assert_eq!(translate(0x4A), 113, "kVK_Mute       → KEY_MUTE");
    }
}
