//! Win32 virtual-key → Linux evdev `KEY_*` translation, via the PS/2 **set-1 scancode**.
//!
//! On Windows, GDK reports `hardware_keycode` as the Win32 *virtual-key* code (the `wParam`
//! of `WM_KEYDOWN`). A VK is **not** a physical key: Windows derives it from the scancode
//! through the active keyboard layout, so the physical key left of `Z` is `VK_Z` on QWERTY
//! and `VK_W` on AZERTY. Sending a VK-derived evdev code would hand the remote GNOME session
//! the wrong *physical* key on every non-US layout — the remote applies its own layout on
//! top, so what the wire must carry is the position, not the letter.
//!
//! So this module goes back the way Windows came: `MapVirtualKeyW(vk, MAPVK_VK_TO_VSC_EX)`
//! inverts the layout mapping and yields the set-1 scancode (with `0xE0` in the high byte for
//! the extended block), which *is* a physical position. That scancode is then looked up in the
//! tables below. The Linux twin needs no such work — X11/Wayland hand GTK the evdev code
//! directly (`hardware_keycode = evdev + 8`); the macOS twin is [`viewer_core::kvk_evdev`], which
//! translates Carbon kVKs, also already physical.
//!
//! **Why the tables are written out rather than computed.** For the main block, set-1 scancode
//! and evdev code are numerically equal (`0x01 ESC` → `KEY_ESC 1`, … `0x58 F12` → `KEY_F12 88`)
//! — Linux inherited its keycode numbering from the XT keyboard. That identity is a fact worth
//! *asserting*, not relying on: it breaks at `0x54`/`0x55`, across the whole extended block, and
//! for every JIS and F13-F24 key. `base_identity_holds_where_it_should` pins it.
//!
//! **Sentinel:** value `0` means "no evdev equivalent" — the caller must skip the event rather
//! than send 0, exactly as with [`viewer_core::kvk_evdev`].
//!
//! **Known limitation — the numeric keypad's `Enter`.** GDK gives us a VK, and Windows uses the
//! same `VK_RETURN` for both `Enter` keys (they differ only by the extended bit in `lParam`,
//! which GDK does not forward). `MapVirtualKeyW` resolves the ambiguity toward the main
//! `Enter`, so keypad `Enter` arrives as `KEY_ENTER`, not `KEY_KPENTER`. Nothing on a normal
//! desktop distinguishes them. The fix, if one is ever needed, is the same raw-input path
//! `pointer_lock_win` already runs a message window for: `RAWKEYBOARD` carries `MakeCode` plus
//! `RI_KEY_E0`, i.e. the true scancode, and would feed `scancode_to_evdev` directly.

/// Sentinel: no evdev equivalent — caller must skip the event.
const U: u8 = 0;

/// Set-1 scancode (no `0xE0` prefix) → Linux evdev `KEY_*`, or 0 (sentinel / skip).
///
/// Array index = scancode `0x00-0x7F`. Values above `0x7F` never appear unprefixed in the
/// set-1 make-code space that `MAPVK_VK_TO_VSC_EX` produces.
#[rustfmt::skip]
pub static BASE_TO_EVDEV: [u8; 128] = [
    /*0x00*/   U, // (no key)
    /*0x01*/   1, // Escape              KEY_ESC
    /*0x02*/   2, // 1                   KEY_1
    /*0x03*/   3, // 2                   KEY_2
    /*0x04*/   4, // 3                   KEY_3
    /*0x05*/   5, // 4                   KEY_4
    /*0x06*/   6, // 5                   KEY_5
    /*0x07*/   7, // 6                   KEY_6
    /*0x08*/   8, // 7                   KEY_7
    /*0x09*/   9, // 8                   KEY_8
    /*0x0A*/  10, // 9                   KEY_9
    /*0x0B*/  11, // 0                   KEY_0
    /*0x0C*/  12, // -                   KEY_MINUS
    /*0x0D*/  13, // =                   KEY_EQUAL
    /*0x0E*/  14, // Backspace           KEY_BACKSPACE
    /*0x0F*/  15, // Tab                 KEY_TAB
    /*0x10*/  16, // Q                   KEY_Q
    /*0x11*/  17, // W                   KEY_W
    /*0x12*/  18, // E                   KEY_E
    /*0x13*/  19, // R                   KEY_R
    /*0x14*/  20, // T                   KEY_T
    /*0x15*/  21, // Y                   KEY_Y
    /*0x16*/  22, // U                   KEY_U
    /*0x17*/  23, // I                   KEY_I
    /*0x18*/  24, // O                   KEY_O
    /*0x19*/  25, // P                   KEY_P
    /*0x1A*/  26, // [                   KEY_LEFTBRACE
    /*0x1B*/  27, // ]                   KEY_RIGHTBRACE
    /*0x1C*/  28, // Enter               KEY_ENTER
    /*0x1D*/  29, // Left Ctrl           KEY_LEFTCTRL
    /*0x1E*/  30, // A                   KEY_A
    /*0x1F*/  31, // S                   KEY_S
    /*0x20*/  32, // D                   KEY_D
    /*0x21*/  33, // F                   KEY_F
    /*0x22*/  34, // G                   KEY_G
    /*0x23*/  35, // H                   KEY_H
    /*0x24*/  36, // J                   KEY_J
    /*0x25*/  37, // K                   KEY_K
    /*0x26*/  38, // L                   KEY_L
    /*0x27*/  39, // ;                   KEY_SEMICOLON
    /*0x28*/  40, // '                   KEY_APOSTROPHE
    /*0x29*/  41, // `                   KEY_GRAVE
    /*0x2A*/  42, // Left Shift          KEY_LEFTSHIFT
    /*0x2B*/  43, // \                   KEY_BACKSLASH
    /*0x2C*/  44, // Z                   KEY_Z
    /*0x2D*/  45, // X                   KEY_X
    /*0x2E*/  46, // C                   KEY_C
    /*0x2F*/  47, // V                   KEY_V
    /*0x30*/  48, // B                   KEY_B
    /*0x31*/  49, // N                   KEY_N
    /*0x32*/  50, // M                   KEY_M
    /*0x33*/  51, // ,                   KEY_COMMA
    /*0x34*/  52, // .                   KEY_DOT
    /*0x35*/  53, // /                   KEY_SLASH
    /*0x36*/  54, // Right Shift         KEY_RIGHTSHIFT
    /*0x37*/  55, // Keypad *            KEY_KPASTERISK
    /*0x38*/  56, // Left Alt            KEY_LEFTALT
    /*0x39*/  57, // Space               KEY_SPACE
    /*0x3A*/  58, // CapsLock            KEY_CAPSLOCK
    /*0x3B*/  59, // F1                  KEY_F1
    /*0x3C*/  60, // F2                  KEY_F2
    /*0x3D*/  61, // F3                  KEY_F3
    /*0x3E*/  62, // F4                  KEY_F4
    /*0x3F*/  63, // F5                  KEY_F5
    /*0x40*/  64, // F6                  KEY_F6
    /*0x41*/  65, // F7                  KEY_F7
    /*0x42*/  66, // F8                  KEY_F8
    /*0x43*/  67, // F9                  KEY_F9
    /*0x44*/  68, // F10                 KEY_F10
    /*0x45*/  69, // NumLock             KEY_NUMLOCK
    /*0x46*/  70, // ScrollLock          KEY_SCROLLLOCK
    /*0x47*/  71, // Keypad 7            KEY_KP7
    /*0x48*/  72, // Keypad 8            KEY_KP8
    /*0x49*/  73, // Keypad 9            KEY_KP9
    /*0x4A*/  74, // Keypad -            KEY_KPMINUS
    /*0x4B*/  75, // Keypad 4            KEY_KP4
    /*0x4C*/  76, // Keypad 5            KEY_KP5
    /*0x4D*/  77, // Keypad 6            KEY_KP6
    /*0x4E*/  78, // Keypad +            KEY_KPPLUS
    /*0x4F*/  79, // Keypad 1            KEY_KP1
    /*0x50*/  80, // Keypad 2            KEY_KP2
    /*0x51*/  81, // Keypad 3            KEY_KP3
    /*0x52*/  82, // Keypad 0            KEY_KP0
    /*0x53*/  83, // Keypad .            KEY_KPDOT
    /*0x54*/   U, // SysRq (Alt+PrintScreen only; PrintScreen proper is E0 37)
    /*0x55*/   U, // (unassigned)
    /*0x56*/  86, // ISO \ (102nd key)   KEY_102ND
    /*0x57*/  87, // F11                 KEY_F11
    /*0x58*/  88, // F12                 KEY_F12
    /*0x59*/ 117, // Keypad =            KEY_KPEQUAL
    /*0x5A*/   U, // (unassigned)
    /*0x5B*/   U, // (unassigned unprefixed; E0 5B is Left Meta)
    /*0x5C*/   U, // (unassigned unprefixed; E0 5C is Right Meta)
    /*0x5D*/   U, // (unassigned unprefixed; E0 5D is Menu)
    /*0x5E*/   U, // (unassigned)
    /*0x5F*/   U, // (unassigned)
    /*0x60*/   U, // (unassigned)
    /*0x61*/   U, // (unassigned)
    /*0x62*/   U, // (unassigned)
    /*0x63*/   U, // (unassigned)
    /*0x64*/ 183, // F13                 KEY_F13
    /*0x65*/ 184, // F14                 KEY_F14
    /*0x66*/ 185, // F15                 KEY_F15
    /*0x67*/ 186, // F16                 KEY_F16
    /*0x68*/ 187, // F17                 KEY_F17
    /*0x69*/ 188, // F18                 KEY_F18
    /*0x6A*/ 189, // F19                 KEY_F19
    /*0x6B*/ 190, // F20                 KEY_F20
    /*0x6C*/ 191, // F21                 KEY_F21
    /*0x6D*/ 192, // F22                 KEY_F22
    /*0x6E*/ 193, // F23                 KEY_F23
    /*0x6F*/   U, // (unassigned)
    /*0x70*/  93, // JIS Katakana/Hiragana  KEY_KATAKANAHIRAGANA
    /*0x71*/   U, // (unassigned)
    /*0x72*/   U, // (unassigned)
    /*0x73*/  89, // JIS \ / _ (RO)      KEY_RO
    /*0x74*/   U, // (unassigned)
    /*0x75*/   U, // (unassigned)
    /*0x76*/ 194, // F24                 KEY_F24
    /*0x77*/  91, // JIS Hiragana        KEY_HIRAGANA
    /*0x78*/  90, // JIS Katakana        KEY_KATAKANA
    /*0x79*/  92, // JIS Henkan          KEY_HENKAN
    /*0x7A*/   U, // (unassigned)
    /*0x7B*/  94, // JIS Muhenkan        KEY_MUHENKAN
    /*0x7C*/   U, // (unassigned)
    /*0x7D*/ 124, // JIS Yen             KEY_YEN
    /*0x7E*/  95, // JIS Keypad ,        KEY_KPJPCOMMA
    /*0x7F*/   U, // (unassigned)
];

/// `0xE0`-prefixed (extended) set-1 scancode → Linux evdev `KEY_*`, or 0 (sentinel / skip).
///
/// Array index = the byte **after** the `0xE0` prefix. This is where the navigation cluster,
/// the right-hand modifiers, and the ACPI/media keys live — none of which follow the
/// scancode/evdev identity that holds for the main block.
#[rustfmt::skip]
pub static EXT_TO_EVDEV: [u8; 128] = [
    /*0x00*/   U, /*0x01*/   U, /*0x02*/   U, /*0x03*/   U,
    /*0x04*/   U, /*0x05*/   U, /*0x06*/   U, /*0x07*/   U,
    /*0x08*/   U, /*0x09*/   U, /*0x0A*/   U, /*0x0B*/   U,
    /*0x0C*/   U, /*0x0D*/   U, /*0x0E*/   U, /*0x0F*/   U,
    /*0x10*/ 165, // Previous track      KEY_PREVIOUSSONG
    /*0x11*/   U, /*0x12*/   U, /*0x13*/   U, /*0x14*/   U,
    /*0x15*/   U, /*0x16*/   U, /*0x17*/   U, /*0x18*/   U,
    /*0x19*/ 163, // Next track          KEY_NEXTSONG
    /*0x1A*/   U, /*0x1B*/   U,
    /*0x1C*/  96, // Keypad Enter        KEY_KPENTER
    /*0x1D*/  97, // Right Ctrl          KEY_RIGHTCTRL
    /*0x1E*/   U, /*0x1F*/   U,
    /*0x20*/ 113, // Mute                KEY_MUTE
    /*0x21*/ 140, // Calculator          KEY_CALC
    /*0x22*/ 164, // Play/Pause          KEY_PLAYPAUSE
    /*0x23*/   U,
    /*0x24*/ 166, // Stop                KEY_STOPCD
    /*0x25*/   U, /*0x26*/   U, /*0x27*/   U, /*0x28*/   U,
    /*0x29*/   U, /*0x2A*/   U, /*0x2B*/   U, /*0x2C*/   U,
    /*0x2D*/   U,
    /*0x2E*/ 114, // Volume down         KEY_VOLUMEDOWN
    /*0x2F*/   U,
    /*0x30*/ 115, // Volume up           KEY_VOLUMEUP
    /*0x31*/   U,
    /*0x32*/ 172, // WWW home            KEY_HOMEPAGE
    /*0x33*/   U, /*0x34*/   U,
    /*0x35*/  98, // Keypad /            KEY_KPSLASH
    /*0x36*/   U,
    /*0x37*/  99, // PrintScreen         KEY_SYSRQ
    /*0x38*/ 100, // Right Alt           KEY_RIGHTALT
    /*0x39*/   U, /*0x3A*/   U, /*0x3B*/   U, /*0x3C*/   U,
    /*0x3D*/   U, /*0x3E*/   U, /*0x3F*/   U, /*0x40*/   U,
    /*0x41*/   U, /*0x42*/   U, /*0x43*/   U, /*0x44*/   U,
    /*0x45*/   U,
    /*0x46*/ 119, // Ctrl+Break          KEY_PAUSE
    /*0x47*/ 102, // Home                KEY_HOME
    /*0x48*/ 103, // Up                  KEY_UP
    /*0x49*/ 104, // PageUp              KEY_PAGEUP
    /*0x4A*/   U,
    /*0x4B*/ 105, // Left                KEY_LEFT
    /*0x4C*/   U,
    /*0x4D*/ 106, // Right               KEY_RIGHT
    /*0x4E*/   U,
    /*0x4F*/ 107, // End                 KEY_END
    /*0x50*/ 108, // Down                KEY_DOWN
    /*0x51*/ 109, // PageDown            KEY_PAGEDOWN
    /*0x52*/ 110, // Insert              KEY_INSERT
    /*0x53*/ 111, // Delete              KEY_DELETE
    /*0x54*/   U, /*0x55*/   U, /*0x56*/   U, /*0x57*/   U,
    /*0x58*/   U, /*0x59*/   U, /*0x5A*/   U,
    /*0x5B*/ 125, // Left Windows        KEY_LEFTMETA
    /*0x5C*/ 126, // Right Windows       KEY_RIGHTMETA
    /*0x5D*/ 127, // Menu / Apps         KEY_COMPOSE  (Linux calls the PC Menu key Compose)
    /*0x5E*/ 116, // Power               KEY_POWER
    /*0x5F*/ 142, // Sleep               KEY_SLEEP
    /*0x60*/   U, /*0x61*/   U, /*0x62*/   U,
    /*0x63*/ 143, // Wake                KEY_WAKEUP
    /*0x64*/   U,
    /*0x65*/ 217, // WWW search          KEY_SEARCH
    /*0x66*/ 156, // WWW favourites      KEY_BOOKMARKS
    /*0x67*/ 173, // WWW refresh         KEY_REFRESH
    /*0x68*/ 128, // WWW stop            KEY_STOP
    /*0x69*/ 159, // WWW forward         KEY_FORWARD
    /*0x6A*/ 158, // WWW back            KEY_BACK
    /*0x6B*/ 157, // My computer         KEY_COMPUTER
    /*0x6C*/ 155, // Mail                KEY_MAIL
    /*0x6D*/ 226, // Media select        KEY_MEDIA
    /*0x6E*/   U, /*0x6F*/   U, /*0x70*/   U, /*0x71*/   U,
    /*0x72*/   U, /*0x73*/   U, /*0x74*/   U, /*0x75*/   U,
    /*0x76*/   U, /*0x77*/   U, /*0x78*/   U, /*0x79*/   U,
    /*0x7A*/   U, /*0x7B*/   U, /*0x7C*/   U, /*0x7D*/   U,
    /*0x7E*/   U, /*0x7F*/   U,
];

/// Translate a set-1 scancode — as `MAPVK_VK_TO_VSC_EX` returns it, i.e. the make code in the
/// low byte and `0xE0` in the high byte for the extended block — to a Linux evdev `KEY_*`.
///
/// Returns `0` (sentinel) for anything with no evdev equivalent. Pure and layout-free, so it
/// is unit-testable without a Windows message pump.
#[inline]
pub fn scancode_to_evdev(scancode: u32) -> u32 {
    let low = (scancode & 0xFF) as usize;
    let table = match (scancode >> 8) & 0xFF {
        0x00 => &BASE_TO_EVDEV,
        0xE0 => &EXT_TO_EVDEV,
        // 0xE1 prefixes only Pause, which `translate` resolves from its VK before ever
        // reaching here (MapVirtualKeyW mis-reports it). Anything else is not a key.
        _ => return 0,
    };
    table.get(low).copied().unwrap_or(0) as u32
}

/// Translate a Win32 virtual-key code (GDK's `hardware_keycode` on Windows) to a Linux evdev
/// `KEY_*` code.
///
/// Returns `0` if the key has no evdev equivalent; callers must skip sending the event.
#[cfg(target_os = "windows")]
#[inline]
pub fn translate(vk: u32) -> u32 {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW};

    // Three keys `MapVirtualKeyW` cannot round-trip, so resolve them from the VK directly:
    //   - VK_PAUSE and VK_NUMLOCK both map to scancode 0x45 (Pause is really the E1-prefixed
    //     sequence `E1 1D 45`, which the API flattens), so the pair is indistinguishable.
    //   - VK_SNAPSHOT maps to the unprefixed 0x54 (the Alt+PrintScreen "SysRq" code) rather
    //     than the E0 37 that an unmodified PrintScreen actually sends.
    const VK_PAUSE: u32 = 0x13;
    const VK_SNAPSHOT: u32 = 0x2C;
    const VK_NUMLOCK: u32 = 0x90;
    match vk {
        VK_PAUSE => return 119,   // KEY_PAUSE
        VK_SNAPSHOT => return 99, // KEY_SYSRQ
        VK_NUMLOCK => return 69,  // KEY_NUMLOCK
        _ => {}
    }
    // SAFETY: MapVirtualKeyW is a pure lookup against the calling thread's active keyboard
    // layout. It takes no pointers and cannot fail destructively — an unmapped VK returns 0,
    // which `scancode_to_evdev` turns into the skip sentinel.
    scancode_to_evdev(unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC_EX) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the module: a physical position, not a letter. These are the
    /// scancodes an AZERTY keyboard reports for the keys labelled A/Z/M — the same positions a
    /// QWERTY keyboard labels Q/W/;. Both must send the *position*, so the remote's own layout
    /// decides the character.
    #[test]
    fn scancodes_are_positions_not_letters() {
        assert_eq!(scancode_to_evdev(0x10), 16, "position of QWERTY Q / AZERTY A → KEY_Q");
        assert_eq!(scancode_to_evdev(0x11), 17, "position of QWERTY W / AZERTY Z → KEY_W");
        assert_eq!(scancode_to_evdev(0x27), 39, "position of QWERTY ; / AZERTY M → KEY_SEMICOLON");
    }

    /// Linux inherited its keycode numbering from the XT keyboard, so across the main block the
    /// set-1 scancode and the evdev code are the same number. Assert it rather than rely on it:
    /// the table is hand-written and a typo here is a silently wrong key on the remote.
    #[test]
    fn base_identity_holds_where_it_should() {
        for sc in 0x01u32..=0x53 {
            assert_eq!(scancode_to_evdev(sc), sc, "set-1 {sc:#04x} should equal its evdev code");
        }
        // …and resumes after the two-code gap at 0x54/0x55.
        for sc in 0x56u32..=0x58 {
            assert_eq!(scancode_to_evdev(sc), sc, "set-1 {sc:#04x} should equal its evdev code");
        }
    }

    /// The gap is real: 0x54 is only ever produced by Alt+PrintScreen (evdev has no key there),
    /// and 0x55 is unassigned. Neither may be forwarded.
    #[test]
    fn the_identity_gap_is_sentineled() {
        assert_eq!(scancode_to_evdev(0x54), 0, "SysRq make code → sentinel");
        assert_eq!(scancode_to_evdev(0x55), 0, "unassigned → sentinel");
    }

    #[test]
    fn modifiers_keep_their_sides() {
        assert_eq!(scancode_to_evdev(0x1D), 29, "Left Ctrl   → KEY_LEFTCTRL");
        assert_eq!(scancode_to_evdev(0xE01D), 97, "Right Ctrl  → KEY_RIGHTCTRL");
        assert_eq!(scancode_to_evdev(0x2A), 42, "Left Shift  → KEY_LEFTSHIFT");
        assert_eq!(scancode_to_evdev(0x36), 54, "Right Shift → KEY_RIGHTSHIFT");
        assert_eq!(scancode_to_evdev(0x38), 56, "Left Alt    → KEY_LEFTALT");
        assert_eq!(scancode_to_evdev(0xE038), 100, "AltGr / Right Alt → KEY_RIGHTALT");
        assert_eq!(scancode_to_evdev(0xE05B), 125, "Left Windows  → KEY_LEFTMETA");
        assert_eq!(scancode_to_evdev(0xE05C), 126, "Right Windows → KEY_RIGHTMETA");
    }

    /// The navigation cluster is the extended twin of the keypad: same low byte, different key.
    /// Getting the prefix wrong would silently swap Home for Keypad-7 across the whole cluster.
    #[test]
    fn navigation_cluster_is_not_the_keypad() {
        assert_eq!(scancode_to_evdev(0x47), 71, "Keypad 7 → KEY_KP7");
        assert_eq!(scancode_to_evdev(0xE047), 102, "Home     → KEY_HOME");
        assert_eq!(scancode_to_evdev(0x48), 72, "Keypad 8 → KEY_KP8");
        assert_eq!(scancode_to_evdev(0xE048), 103, "Up       → KEY_UP");
        assert_eq!(scancode_to_evdev(0x4B), 75, "Keypad 4 → KEY_KP4");
        assert_eq!(scancode_to_evdev(0xE04B), 105, "Left     → KEY_LEFT");
        assert_eq!(scancode_to_evdev(0x4D), 77, "Keypad 6 → KEY_KP6");
        assert_eq!(scancode_to_evdev(0xE04D), 106, "Right    → KEY_RIGHT");
        assert_eq!(scancode_to_evdev(0x50), 80, "Keypad 2 → KEY_KP2");
        assert_eq!(scancode_to_evdev(0xE050), 108, "Down     → KEY_DOWN");
        assert_eq!(scancode_to_evdev(0x52), 82, "Keypad 0 → KEY_KP0");
        assert_eq!(scancode_to_evdev(0xE052), 110, "Insert   → KEY_INSERT");
        assert_eq!(scancode_to_evdev(0x53), 83, "Keypad . → KEY_KPDOT");
        assert_eq!(scancode_to_evdev(0xE053), 111, "Delete   → KEY_DELETE");
        // Shift+Insert / Ctrl+Insert are how paste works in a lot of remote software, so the
        // Insert mapping in particular has to be the real KEY_INSERT.
        assert_eq!(scancode_to_evdev(0xE01C), 96, "Keypad Enter → KEY_KPENTER");
    }

    /// F13-F24 and the JIS keys are exactly where the identity does *not* hold, so they are the
    /// entries most likely to rot. `viewer_core::kvk_evdev` maps the same evdev codes from the
    /// macOS side (for the native `viewer-macos` client) — these values must agree with it or the
    /// same physical key means two things per platform.
    #[test]
    fn high_function_keys_and_jis() {
        assert_eq!(scancode_to_evdev(0x57), 87, "F11 → KEY_F11");
        assert_eq!(scancode_to_evdev(0x58), 88, "F12 → KEY_F12");
        assert_eq!(scancode_to_evdev(0x64), 183, "F13 → KEY_F13");
        assert_eq!(scancode_to_evdev(0x67), 186, "F16 → KEY_F16 (matches kvk_evdev)");
        assert_eq!(scancode_to_evdev(0x6B), 190, "F20 → KEY_F20 (matches kvk_evdev)");
        assert_eq!(scancode_to_evdev(0x76), 194, "F24 → KEY_F24");
        assert_eq!(scancode_to_evdev(0x7D), 124, "JIS Yen → KEY_YEN (matches kvk_evdev)");
        assert_eq!(scancode_to_evdev(0x73), 89, "JIS RO  → KEY_RO  (matches kvk_evdev)");
        assert_eq!(scancode_to_evdev(0x7E), 95, "JIS Keypad , → KEY_KPJPCOMMA (matches kvk_evdev)");
        assert_eq!(scancode_to_evdev(0x56), 86, "ISO 102nd → KEY_102ND (matches kvk_evdev)");
    }

    /// A prefix we never emit, and an index past either table, must be skipped rather than
    /// wrap into some unrelated key.
    #[test]
    fn unknown_prefixes_and_out_of_range_are_sentineled() {
        assert_eq!(scancode_to_evdev(0xE100), 0, "E1 (Pause prefix) → sentinel; VK path handles it");
        assert_eq!(scancode_to_evdev(0xE145), 0, "E1 1D 45 flattened → sentinel");
        assert_eq!(scancode_to_evdev(0xFF00), 0, "unknown prefix → sentinel");
        assert_eq!(scancode_to_evdev(0x00), 0, "no key → sentinel");
        assert_eq!(scancode_to_evdev(0xE000), 0, "E0 with no make code → sentinel");
    }

    /// PrintScreen, ScrollLock and Pause are three different keys that all collide in the
    /// Windows API in some way; each has to land on its own evdev code.
    #[test]
    fn the_print_scroll_pause_trio_stays_distinct() {
        assert_eq!(scancode_to_evdev(0xE037), 99, "PrintScreen → KEY_SYSRQ");
        assert_eq!(scancode_to_evdev(0x46), 70, "ScrollLock  → KEY_SCROLLLOCK");
        assert_eq!(scancode_to_evdev(0xE046), 119, "Ctrl+Break  → KEY_PAUSE");
        assert_eq!(scancode_to_evdev(0x45), 69, "NumLock     → KEY_NUMLOCK");
    }
}
