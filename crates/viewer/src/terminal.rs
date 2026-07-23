//! Cross-platform terminal tab view for headless clones.
//!
//! A [`TerminalView`] is a `gtk4::Notebook` with one [`TerminalTab`] per tmux session (plus a
//! tab-bar "+" that requests a new session). Each tab is a `DrawingArea` backed by a pure-Rust
//! [`vt100`] screen model: the control-server's raw PTY bytes are fed in via [`TerminalView::feed`]
//! and painted with cairo's monospace text API — no system terminal library, so it builds and
//! renders identically on Linux and macOS. Keystrokes are encoded to terminal input bytes and
//! handed back through the `on_input` callback; tab resizes go through `on_resize`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::cairo;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;

/// `vt100::Parser::set_size` is shadowed by a `gtk4::prelude` trait method of the same name in
/// this file's scope; call it from a child module that does not glob the GTK prelude.
mod vt_ext {
    pub fn set_size(p: &mut vt100::Parser, rows: u16, cols: u16) {
        p.screen_mut().set_size(rows, cols);
    }
}

/// Monospace point size for the cell grid.
const FONT_SIZE: f64 = 13.0;
/// Default grid until the widget is allocated and reports its real character dimensions.
const INIT_COLS: u16 = 80;
const INIT_ROWS: u16 = 24;
/// Scrollback the vt100 model retains (lines). The viewer shows the visible screen only.
const SCROLLBACK: usize = 1000;

/// Keystrokes/paste for a session (session name, bytes) → sent to the server.
pub type InputCb = Rc<dyn Fn(&str, Vec<u8>)>;
/// A tab resize (cols, rows) → the server resizes every session's PTY.
pub type ResizeCb = Rc<dyn Fn(u16, u16)>;
/// The tab-bar "+" → create a new tmux session.
pub type NewSessionCb = Rc<dyn Fn()>;

/// Callbacks a [`TerminalView`] uses to talk back to the control-server (via the port-1 writer).
#[derive(Clone)]
pub struct TermCallbacks {
    pub on_input: InputCb,
    pub on_resize: ResizeCb,
    pub on_new_session: NewSessionCb,
}

/// The notebook of terminal tabs shown on the viewer's primary window in headless mode.
pub struct TerminalView {
    notebook: gtk4::Notebook,
    cb: TermCallbacks,
    /// Session name → tab, in no particular order (the notebook holds the visual order).
    tabs: Rc<RefCell<HashMap<String, TerminalTab>>>,
}

impl TerminalView {
    pub fn new(cb: TermCallbacks) -> Self {
        let notebook = gtk4::Notebook::new();
        notebook.set_scrollable(true);
        notebook.set_hexpand(true);
        notebook.set_vexpand(true);

        // Tab-bar "+" button (end of the action area).
        let plus = gtk4::Button::from_icon_name("list-add-symbolic");
        plus.set_tooltip_text(Some("New tmux session"));
        plus.add_css_class("flat");
        {
            let on_new = cb.on_new_session.clone();
            plus.connect_clicked(move |_| (on_new)());
        }
        notebook.set_action_widget(&plus, gtk4::PackType::End);
        plus.set_visible(true);

        Self { notebook, cb, tabs: Rc::new(RefCell::new(HashMap::new())) }
    }

    /// The widget to embed as the primary window's child.
    pub fn widget(&self) -> &gtk4::Notebook {
        &self.notebook
    }

    /// Reconcile the open tabs to `sessions`: append tabs for new sessions (preserving existing
    /// ones and their scrollback), and remove tabs whose session vanished.
    pub fn set_sessions(&self, sessions: &[String]) {
        let mut tabs = self.tabs.borrow_mut();
        // Remove vanished sessions.
        let gone: Vec<String> =
            tabs.keys().filter(|k| !sessions.contains(k)).cloned().collect();
        for name in gone {
            if let Some(tab) = tabs.remove(&name) {
                if let Some(page) = self.notebook.page_num(&tab.area) {
                    self.notebook.remove_page(Some(page));
                }
            }
        }
        // Append new sessions in the server's order.
        for name in sessions {
            if tabs.contains_key(name) {
                continue;
            }
            let tab = TerminalTab::new(name.clone(), self.cb.clone());
            let label = gtk4::Label::new(Some(name));
            self.notebook.append_page(&tab.area, Some(&label));
            self.notebook.set_tab_reorderable(&tab.area, true);
            tabs.insert(name.clone(), tab);
        }
        // Make sure some tab is selected.
        if self.notebook.current_page().is_none() && self.notebook.n_pages() > 0 {
            self.notebook.set_current_page(Some(0));
        }
    }

    /// Feed raw PTY output bytes to the named session's screen model and repaint it.
    pub fn feed(&self, session: &str, data: &[u8]) {
        if let Some(tab) = self.tabs.borrow().get(session) {
            tab.parser.borrow_mut().process(data);
            tab.area.queue_draw();
        }
    }
}

/// One tmux session: a `DrawingArea` painting a [`vt100`] screen, with key + resize handling.
struct TerminalTab {
    area: gtk4::DrawingArea,
    parser: Rc<RefCell<vt100::Parser>>,
}

impl TerminalTab {
    fn new(session: String, cb: TermCallbacks) -> Self {
        let area = gtk4::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.set_can_focus(true);

        let parser = Rc::new(RefCell::new(vt100::Parser::new(INIT_ROWS, INIT_COLS, SCROLLBACK)));
        // Cell metrics (device px) measured from the monospace font; recomputed on the first draw
        // where a real cairo context exists.
        let cell = Rc::new(Cell::new(measure_cell()));

        // Paint the current screen.
        {
            let parser = parser.clone();
            let cell = cell.clone();
            area.set_draw_func(move |_area, cr, w, h| {
                let m = refine_cell(cr, &cell);
                draw_screen(cr, &parser.borrow(), m, w, h);
            });
        }

        // Resize: recompute grid from the allocation and tell the server (all PTYs share a size).
        {
            let parser = parser.clone();
            let cell = cell.clone();
            let on_resize = cb.on_resize.clone();
            area.connect_resize(move |_area, w, h| {
                let (cw, ch) = cell.get();
                if cw <= 0.0 || ch <= 0.0 {
                    return;
                }
                let cols = ((w as f64 / cw).floor() as i64).clamp(1, u16::MAX as i64) as u16;
                let rows = ((h as f64 / ch).floor() as i64).clamp(1, u16::MAX as i64) as u16;
                let (old_rows, old_cols) = parser.borrow().screen().size();
                if (rows, cols) != (old_rows, old_cols) {
                    vt_ext::set_size(&mut parser.borrow_mut(), rows, cols);
                    (on_resize)(cols, rows);
                }
            });
        }

        // Keyboard: encode to terminal input bytes and hand back to the server.
        {
            let keys = gtk4::EventControllerKey::new();
            let parser = parser.clone();
            let on_input = cb.on_input.clone();
            let session = session.clone();
            keys.connect_key_pressed(move |_c, keyval, _code, state| {
                let app_cursor = parser.borrow().screen().application_cursor();
                if let Some(bytes) = encode_key(keyval, state, app_cursor) {
                    (on_input)(&session, bytes);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            area.add_controller(keys);
        }

        // Click focuses the tab so it receives keys.
        {
            let click = gtk4::GestureClick::new();
            let area_w = area.downgrade();
            click.connect_pressed(move |_g, _n, _x, _y| {
                if let Some(a) = area_w.upgrade() {
                    a.grab_focus();
                }
            });
            area.add_controller(click);
        }

        Self { area, parser }
    }
}

/// Measure the monospace cell size (advance width, line height) in device pixels using a
/// throwaway image-surface cairo context. Refined once a real widget context is available.
fn measure_cell() -> (f64, f64) {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 8, 8).unwrap();
    let cr = cairo::Context::new(&surface).unwrap();
    cell_metrics(&cr)
}

/// Compute cell metrics for `cr`'s current target after selecting the monospace font.
fn cell_metrics(cr: &cairo::Context) -> (f64, f64) {
    cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(FONT_SIZE);
    let fe = cr.font_extents().unwrap_or_else(|_| panic!("font_extents"));
    let te = cr.text_extents("M").map(|t| t.x_advance()).unwrap_or(FONT_SIZE * 0.6);
    let w = if te > 0.0 { te } else { FONT_SIZE * 0.6 };
    let h = if fe.height() > 0.0 { fe.height() } else { FONT_SIZE * 1.3 };
    (w.ceil(), h.ceil())
}

/// Refine the cached cell metrics from a live draw context (font hinting can differ from the
/// throwaway surface). Returns the metrics to draw with.
fn refine_cell(cr: &cairo::Context, cell: &Rc<Cell<(f64, f64)>>) -> (f64, f64) {
    let m = cell_metrics(cr);
    cell.set(m);
    m
}

/// Paint the vt100 screen grid: backgrounds, glyphs, then the cursor.
fn draw_screen(cr: &cairo::Context, parser: &vt100::Parser, cell: (f64, f64), w: i32, h: i32) {
    let (cw, ch) = cell;
    let screen = parser.screen();
    let (rows, cols) = screen.size();

    // Clear to the default background.
    let (dr, dg, db) = DEFAULT_BG;
    cr.set_source_rgb(dr, dg, db);
    let _ = cr.paint();

    cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(FONT_SIZE);
    let ascent = cr.font_extents().map(|f| f.ascent()).unwrap_or(FONT_SIZE);

    let _ = (w, h);
    for row in 0..rows {
        for col in 0..cols {
            let Some(c) = screen.cell(row, col) else { continue };
            if c.is_wide_continuation() {
                continue;
            }
            let x = col as f64 * cw;
            let y = row as f64 * ch;
            let width_cells = if c.is_wide() { 2.0 } else { 1.0 };

            let mut fg = color_rgb(c.fgcolor(), DEFAULT_FG);
            let mut bg = color_rgb(c.bgcolor(), DEFAULT_BG);
            if c.inverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            if c.bold() {
                fg = brighten(fg);
            }

            // Background (only when it differs from the default, to reduce overdraw).
            if bg != DEFAULT_BG || c.inverse() {
                cr.set_source_rgb(bg.0, bg.1, bg.2);
                cr.rectangle(x, y, cw * width_cells, ch);
                let _ = cr.fill();
            }

            let contents = c.contents();
            if !contents.is_empty() {
                cr.select_font_face(
                    "monospace",
                    if c.italic() { cairo::FontSlant::Italic } else { cairo::FontSlant::Normal },
                    if c.bold() { cairo::FontWeight::Bold } else { cairo::FontWeight::Normal },
                );
                cr.set_font_size(FONT_SIZE);
                cr.set_source_rgb(fg.0, fg.1, fg.2);
                cr.move_to(x, y + ascent);
                let _ = cr.show_text(contents);
            }
            if c.underline() {
                cr.set_source_rgb(fg.0, fg.1, fg.2);
                cr.rectangle(x, y + ch - 1.0, cw * width_cells, 1.0);
                let _ = cr.fill();
            }
        }
    }

    // Cursor: a block at the cursor cell (unless hidden), drawn as an inverse over the glyph.
    if !screen.hide_cursor() {
        let (crow, ccol) = screen.cursor_position();
        let x = ccol as f64 * cw;
        let y = crow as f64 * ch;
        let (fr, fg_, fb) = DEFAULT_FG;
        cr.set_source_rgba(fr, fg_, fb, 0.75);
        cr.rectangle(x, y, cw, ch);
        let _ = cr.fill();
        if let Some(c) = screen.cell(crow, ccol) {
            let contents = c.contents();
            if !contents.is_empty() {
                let (br, bg_, bb) = DEFAULT_BG;
                cr.set_source_rgb(br, bg_, bb);
                cr.move_to(x, y + ascent);
                let _ = cr.show_text(contents);
            }
        }
    }
}

// --- colors ------------------------------------------------------------------------------

/// Default foreground/background (a soft light-on-dark terminal theme).
const DEFAULT_FG: (f64, f64, f64) = (0.85, 0.85, 0.86);
const DEFAULT_BG: (f64, f64, f64) = (0.11, 0.12, 0.13);

/// Map a vt100 color to normalized RGB, using `default` for [`vt100::Color::Default`].
fn color_rgb(color: vt100::Color, default: (f64, f64, f64)) -> (f64, f64, f64) {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => {
            let (r, g, b) = xterm256(i);
            (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
        }
        vt100::Color::Rgb(r, g, b) => (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0),
    }
}

/// Nudge a color brighter for bold text (keeps within [0,1]).
fn brighten((r, g, b): (f64, f64, f64)) -> (f64, f64, f64) {
    ((r + 0.15).min(1.0), (g + 0.15).min(1.0), (b + 0.15).min(1.0))
}

/// The standard xterm 256-color palette → 8-bit RGB.
fn xterm256(idx: u8) -> (u8, u8, u8) {
    // 0..15: the classic 16 ANSI colors.
    const BASE: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match idx {
        0..=15 => BASE[idx as usize],
        16..=231 => {
            // 6×6×6 color cube.
            let i = idx - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let conv = |v: u8| -> u8 {
                if v == 0 {
                    0
                } else {
                    55 + v * 40
                }
            };
            (conv(r), conv(g), conv(b))
        }
        232..=255 => {
            // 24-step grayscale ramp.
            let v = 8 + (idx - 232) * 10;
            (v, v, v)
        }
    }
}

// --- key encoding ------------------------------------------------------------------------

/// Encode a GTK key press into the byte sequence a terminal sends to its PTY. Returns `None`
/// for keys we don't handle (so GTK can process them, e.g. accelerators). Covers text, the
/// common control/navigation keys, Ctrl-letters, Alt (ESC-prefix), and function keys.
fn encode_key(
    keyval: gdk::Key,
    state: gdk::ModifierType,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    use gdk::Key;
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let alt = state.contains(gdk::ModifierType::ALT_MASK);

    // CSI/SS3 helpers for arrows: application-cursor mode swaps CSI (`ESC [`) for SS3 (`ESC O`).
    let cursor = |c: u8| -> Vec<u8> {
        if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };

    let named: Option<Vec<u8>> = match keyval {
        Key::Return | Key::KP_Enter => Some(vec![b'\r']),
        Key::BackSpace => Some(vec![0x7f]),
        Key::Tab => Some(vec![b'\t']),
        Key::ISO_Left_Tab => Some(vec![0x1b, b'[', b'Z']),
        Key::Escape => Some(vec![0x1b]),
        Key::Up => Some(cursor(b'A')),
        Key::Down => Some(cursor(b'B')),
        Key::Right => Some(cursor(b'C')),
        Key::Left => Some(cursor(b'D')),
        Key::Home => Some(cursor(b'H')),
        Key::End => Some(cursor(b'F')),
        Key::Insert => Some(b"\x1b[2~".to_vec()),
        Key::Delete => Some(b"\x1b[3~".to_vec()),
        Key::Page_Up => Some(b"\x1b[5~".to_vec()),
        Key::Page_Down => Some(b"\x1b[6~".to_vec()),
        Key::F1 => Some(b"\x1bOP".to_vec()),
        Key::F2 => Some(b"\x1bOQ".to_vec()),
        Key::F3 => Some(b"\x1bOR".to_vec()),
        Key::F4 => Some(b"\x1bOS".to_vec()),
        Key::F5 => Some(b"\x1b[15~".to_vec()),
        Key::F6 => Some(b"\x1b[17~".to_vec()),
        Key::F7 => Some(b"\x1b[18~".to_vec()),
        Key::F8 => Some(b"\x1b[19~".to_vec()),
        Key::F9 => Some(b"\x1b[20~".to_vec()),
        Key::F10 => Some(b"\x1b[21~".to_vec()),
        Key::F11 => Some(b"\x1b[23~".to_vec()),
        Key::F12 => Some(b"\x1b[24~".to_vec()),
        _ => None,
    };
    if let Some(mut bytes) = named {
        if alt {
            let mut v = vec![0x1b];
            v.append(&mut bytes);
            return Some(v);
        }
        return Some(bytes);
    }

    // Printable keys (keyval already reflects Shift, e.g. Shift+a → 'A').
    let ch = keyval.to_unicode()?;
    if ctrl {
        // Ctrl-letter and friends → the C0 control byte (a→0x01 … z→0x1a, @[\]^_ too).
        let up = ch.to_ascii_uppercase();
        let byte = match up {
            '@'..='_' => Some((up as u8) & 0x1f),
            ' ' => Some(0x00),
            '?' => Some(0x7f),
            _ => None,
        };
        if let Some(b) = byte {
            return Some(if alt { vec![0x1b, b] } else { vec![b] });
        }
    }
    // Non-control characters we can render as text. Skip pure control chars we didn't map.
    if (ch as u32) < 0x20 && ch != '\t' {
        return None;
    }
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    let mut out = Vec::with_capacity(s.len() + 1);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(s.as_bytes());
    Some(out)
}
