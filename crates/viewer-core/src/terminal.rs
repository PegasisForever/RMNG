//! Terminal pieces shared by both viewer front-ends: the colour scheme and the
//! escape-sequence encoders. Pure logic — the GTK viewer draws with cairo and the native macOS
//! viewer with AppKit, but the palette a cell resolves to and the bytes a key or a mouse click
//! puts on the wire must not differ between them.
//!
//! Extracted verbatim from the GTK viewer's `terminal.rs`, which now imports it.

use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};

pub type Rgb3 = (f64, f64, f64);

/// A terminal color scheme: default fg/bg, selection highlight, and the 16 ANSI colors.
#[derive(Clone, Copy)]
pub struct Theme {
    pub fg: Rgb3,
    pub bg: Rgb3,
    pub sel: Rgb3,
    pub ansi: [Rgb3; 16],
}

/// 8-bit RGB → normalized.
pub fn n(r: u8, g: u8, b: u8) -> Rgb3 {
    (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
}

/// GNOME/Ptyxis dark palette. Background is Ptyxis's neutral `#1c1c1f` (NOT the aubergine it
/// shows on Ubuntu), and `Color0` (ANSI black) is neutralized from Ptyxis's `#241f31` aubergine
/// to a plain dark so no app can paint that purple as a background.
pub fn dark_theme() -> Theme {
    Theme {
        fg: n(0xff, 0xff, 0xff),
        bg: n(0x1c, 0x1c, 0x1f),
        sel: n(0x2b, 0x47, 0x66),
        ansi: [
            n(0x26, 0x26, 0x2b), // Color0: neutralized from Ptyxis aubergine #241f31
            n(0xc0, 0x1c, 0x28),
            n(0x2e, 0xc2, 0x7e),
            n(0xf5, 0xc2, 0x11),
            n(0x1e, 0x78, 0xe4),
            n(0x98, 0x41, 0xbb),
            n(0x0a, 0xb9, 0xdc),
            n(0xc0, 0xbf, 0xbc),
            n(0x5e, 0x5c, 0x64),
            n(0xed, 0x33, 0x3b),
            n(0x57, 0xe3, 0x89),
            n(0xf8, 0xe4, 0x5c),
            n(0x51, 0xa1, 0xff),
            n(0xc0, 0x61, 0xcb),
            n(0x4f, 0xd2, 0xfd),
            n(0xf6, 0xf5, 0xf4),
        ],
    }
}

/// GNOME/Ptyxis light palette (white background, dark foreground).
pub fn light_theme() -> Theme {
    Theme {
        fg: n(0x1d, 0x1d, 0x20),
        bg: n(0xff, 0xff, 0xff),
        sel: n(0xcf, 0xe1, 0xfa),
        ansi: [
            n(0x1d, 0x1d, 0x20),
            n(0xc0, 0x1c, 0x28),
            n(0x26, 0xa2, 0x69),
            n(0xa2, 0x73, 0x4c),
            n(0x12, 0x48, 0x8b),
            n(0xa3, 0x47, 0xba),
            n(0x2a, 0xa1, 0xb3),
            n(0xcf, 0xcf, 0xcf),
            n(0x5d, 0x5d, 0x5d),
            n(0xf6, 0x61, 0x51),
            n(0x33, 0xd1, 0x7a),
            n(0xe9, 0xad, 0x0c),
            n(0x2a, 0x7b, 0xde),
            n(0xc0, 0x61, 0xcb),
            n(0x33, 0xc7, 0xde),
            n(0xff, 0xff, 0xff),
        ],
    }
}

/// Whether the resolved GTK theme is dark, judged by the luminance of its default foreground
/// color (light text ⇒ dark theme). This reflects the theme GTK actually applied from the


pub fn rgb_f(rgb: Rgb) -> Rgb3 {
    (rgb.r as f64 / 255.0, rgb.g as f64 / 255.0, rgb.b as f64 / 255.0)
}

/// Resolve an alacritty cell color to normalized RGB, honoring the app's palette when it set one
/// and falling back to the current theme / the built-in xterm palette otherwise.
pub fn resolve(color: AnsiColor, palette: &alacritty_terminal::term::color::Colors, theme: &Theme) -> Rgb3 {
    match color {
        AnsiColor::Spec(rgb) => rgb_f(rgb),
        AnsiColor::Named(named) => {
            palette[named].map(rgb_f).unwrap_or_else(|| named_default(named, theme))
        }
        AnsiColor::Indexed(i) => {
            palette[i as usize].map(rgb_f).unwrap_or_else(|| indexed_default(i, theme))
        }
    }
}

/// Promote a base ANSI color (0-7, or the named equivalents) to its bright variant (8-15), for
/// the traditional bold-is-bright rendering. Truecolor, already-bright, and 256-cube colors pass
/// through unchanged.
pub fn bold_bright(c: AnsiColor) -> AnsiColor {
    use NamedColor as N;
    match c {
        AnsiColor::Named(N::Black) => AnsiColor::Named(N::BrightBlack),
        AnsiColor::Named(N::Red) => AnsiColor::Named(N::BrightRed),
        AnsiColor::Named(N::Green) => AnsiColor::Named(N::BrightGreen),
        AnsiColor::Named(N::Yellow) => AnsiColor::Named(N::BrightYellow),
        AnsiColor::Named(N::Blue) => AnsiColor::Named(N::BrightBlue),
        AnsiColor::Named(N::Magenta) => AnsiColor::Named(N::BrightMagenta),
        AnsiColor::Named(N::Cyan) => AnsiColor::Named(N::BrightCyan),
        AnsiColor::Named(N::White) => AnsiColor::Named(N::BrightWhite),
        AnsiColor::Named(N::Foreground) => AnsiColor::Named(N::BrightForeground),
        AnsiColor::Indexed(i) if i < 8 => AnsiColor::Indexed(i + 8),
        other => other,
    }
}

pub fn named_default(n: NamedColor, theme: &Theme) -> Rgb3 {
    use NamedColor::*;
    let idx: usize = match n {
        Black => 0,
        Red => 1,
        Green => 2,
        Yellow => 3,
        Blue => 4,
        Magenta => 5,
        Cyan => 6,
        White => 7,
        BrightBlack => 8,
        BrightRed => 9,
        BrightGreen => 10,
        BrightYellow => 11,
        BrightBlue => 12,
        BrightMagenta => 13,
        BrightCyan => 14,
        BrightWhite => 15,
        Foreground | BrightForeground => return theme.fg,
        Background => return theme.bg,
        DimForeground => return (theme.fg.0 * 0.66, theme.fg.1 * 0.66, theme.fg.2 * 0.66),
        _ => return theme.fg,
    };
    theme.ansi[idx]
}

/// The xterm 256-color palette → normalized RGB. 0-15 come from the theme's ANSI colors; the
/// 6×6×6 cube and grayscale ramp are absolute (scheme-independent).
pub fn indexed_default(idx: u8, theme: &Theme) -> Rgb3 {
    match idx {
        0..=15 => theme.ansi[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let conv = |v: u8| -> f64 {
                if v == 0 { 0.0 } else { (55 + v * 40) as f64 / 255.0 }
            };
            (conv(i / 36), conv((i % 36) / 6), conv(i % 6))
        }
        232..=255 => {
            let v = (8 + (idx - 232) * 10) as f64 / 255.0;
            (v, v, v)
        }
    }
}

// --- input encoding ---------------------------------------------------------------------

/// Arrow-key bytes, honoring application-cursor mode (SS3 vs CSI).
pub fn arrow(dir: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', dir]
    } else {
        vec![0x1b, b'[', dir]
    }
}

/// The base SGR/normal button code for a GTK button number (1/2/3 → 0/1/2).
pub fn base_button(button: u32) -> u8 {
    match button {
        2 => 1,
        3 => 2,
        _ => 0,
    }
}

/// Encode a mouse event: SGR (`ESC[<b;col;rowM/m`) when the app requested it, else normal X10
/// (`ESC[Mb col row`). `code` already includes wheel (64/65) and motion (+32) bits.
pub fn mouse_report(code: u8, col: usize, row: usize, pressed: bool, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        let m = if pressed { 'M' } else { 'm' };
        format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, m).into_bytes()
    } else {
        let cb = if pressed { code } else { 3 };
        let cx = (col as u16 + 1).min(223) as u8 + 32;
        let cy = (row as u16 + 1).min(223) as u8 + 32;
        vec![0x1b, b'[', b'M', 32u8.wrapping_add(cb), cx, cy]
    }
}
