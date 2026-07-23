//! Cross-platform terminal tab view for headless clones, powered by the `alacritty_terminal`
//! engine.
//!
//! A [`TerminalView`] is a `gtk4::Notebook` with one [`TerminalTab`] per tmux session (plus a
//! tab-bar "+" that requests a new session). Each tab drives an `alacritty_terminal::Term` — a
//! full terminal state machine (grid, scrollback, selection, mouse/app modes) — fed the control-
//! server's raw PTY bytes via [`TerminalView::feed`]. We render its grid into a GTK `DrawingArea`
//! with cairo (no system terminal library, so it builds and runs identically on Linux and macOS),
//! and wire up the interactions of a normal terminal: text **selection** (click-drag, word/line on
//! double/triple click) with **copy** (Ctrl+Shift+C and the primary selection), **paste**
//! (Ctrl+Shift+V and middle-click, bracketed when requested), **mouse reporting** to the remote app
//! when it enables mouse mode, and **scrollback** (wheel / Shift+PageUp). Keystrokes and any bytes
//! the terminal itself needs to send (query replies, mouse reports) go back through `on_input`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor, Rgb};
use gtk4::cairo;
use gtk4::gdk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

/// Monospace point size for the cell grid.
const FONT_SIZE: f64 = 13.0;
/// Default grid until the widget is allocated and reports its real character dimensions.
const INIT_COLS: usize = 80;
const INIT_ROWS: usize = 24;

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

// --- alacritty glue ---------------------------------------------------------------------

/// Grid dimensions handed to `Term::new`/`resize`. Scrollback length is a separate `Config`
/// field, so `total_lines == screen_lines` here.
#[derive(Clone, Copy)]
struct Dims {
    cols: usize,
    lines: usize,
}
impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// The terminal's event sink: forwards the bytes the emulator needs to write back to the PTY
/// (cursor-position replies, mouse reports it generates, etc.) to the server, and honors OSC-52
/// clipboard-store requests by writing the system clipboard.
struct EventProxy {
    session: String,
    on_input: InputCb,
    set_clipboard: Rc<dyn Fn(String)>,
}
impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(text) => (self.on_input)(&self.session, text.into_bytes()),
            AlacEvent::ClipboardStore(_, text) => (self.set_clipboard)(text),
            _ => {}
        }
    }
}

/// What a press-drag is currently doing.
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    /// Extending a text selection.
    Selecting,
    /// Reporting mouse events to the remote app; the held button code (0/1/2).
    Reporting(u8),
}

// --- view -------------------------------------------------------------------------------

/// The notebook of terminal tabs shown on the viewer's primary window in headless mode.
pub struct TerminalView {
    notebook: gtk4::Notebook,
    cb: TermCallbacks,
    tabs: Rc<RefCell<HashMap<String, TerminalTab>>>,
}

impl TerminalView {
    pub fn new(cb: TermCallbacks) -> Self {
        let notebook = gtk4::Notebook::new();
        notebook.set_scrollable(true);
        notebook.set_hexpand(true);
        notebook.set_vexpand(true);

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
        let gone: Vec<String> = tabs.keys().filter(|k| !sessions.contains(k)).cloned().collect();
        for name in gone {
            if let Some(tab) = tabs.remove(&name) {
                if let Some(page) = self.notebook.page_num(&tab.area) {
                    self.notebook.remove_page(Some(page));
                }
            }
        }
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
        if self.notebook.current_page().is_none() && self.notebook.n_pages() > 0 {
            self.notebook.set_current_page(Some(0));
        }
    }

    /// Feed raw PTY output bytes to the named session's terminal and repaint it.
    pub fn feed(&self, session: &str, data: &[u8]) {
        if let Some(tab) = self.tabs.borrow().get(session) {
            let mut term = tab.term.borrow_mut();
            tab.parser.borrow_mut().advance(&mut *term, data);
            drop(term);
            tab.area.queue_draw();
        }
    }
}

// --- one session's tab ------------------------------------------------------------------

struct TerminalTab {
    area: gtk4::DrawingArea,
    term: Rc<RefCell<Term<EventProxy>>>,
    parser: Rc<RefCell<Processor>>,
}

impl TerminalTab {
    fn new(session: String, cb: TermCallbacks) -> Self {
        let area = gtk4::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.set_can_focus(true);

        // Clipboards from the default display (works before the widget is realized).
        let display = gdk::Display::default();
        let clipboard = display.as_ref().map(|d| d.clipboard());
        let primary = display.as_ref().map(|d| d.primary_clipboard());

        // Terminal engine. The event proxy forwards emulator-generated PTY writes to the server
        // and OSC-52 clipboard stores to the system clipboard.
        let proxy = EventProxy {
            session: session.clone(),
            on_input: cb.on_input.clone(),
            set_clipboard: {
                let clip = clipboard.clone();
                Rc::new(move |text: String| {
                    if let Some(c) = &clip {
                        c.set_text(&text);
                    }
                })
            },
        };
        let init = Dims { cols: INIT_COLS, lines: INIT_ROWS };
        let term = Rc::new(RefCell::new(Term::new(TermConfig::default(), &init, proxy)));
        let parser = Rc::new(RefCell::new(Processor::new()));
        let dims = Rc::new(Cell::new(init));
        let cell = Rc::new(Cell::new(measure_cell()));
        let drag = Rc::new(Cell::new(Drag::None));
        // Last pointer cell (col,row), used to position mouse-wheel reports.
        let last_pos = Rc::new(Cell::new((0usize, 0usize)));

        // Paint.
        {
            let term = term.clone();
            let cell = cell.clone();
            area.set_draw_func(move |_a, cr, w, h| {
                let m = refine_cell(cr, &cell);
                draw_content(cr, &term.borrow(), m, w, h);
            });
        }

        // Resize: recompute the grid from the allocation and tell the server (all PTYs share a size).
        {
            let term = term.clone();
            let dims = dims.clone();
            let cell = cell.clone();
            let on_resize = cb.on_resize.clone();
            area.connect_resize(move |_a, w, h| {
                let (cw, ch) = cell.get();
                if cw <= 0.0 || ch <= 0.0 {
                    return;
                }
                let cols = ((w as f64 / cw).floor() as i64).clamp(1, u16::MAX as i64) as usize;
                let lines = ((h as f64 / ch).floor() as i64).clamp(1, u16::MAX as i64) as usize;
                let cur = dims.get();
                if (cols, lines) != (cur.cols, cur.lines) {
                    let nd = Dims { cols, lines };
                    dims.set(nd);
                    term.borrow_mut().resize(nd);
                    (on_resize)(cols as u16, lines as u16);
                }
            });
        }

        // Keyboard.
        {
            let keys = gtk4::EventControllerKey::new();
            let term = term.clone();
            let on_input = cb.on_input.clone();
            let session = session.clone();
            let clipboard = clipboard.clone();
            let area_w = area.downgrade();
            keys.connect_key_pressed(move |_c, keyval, _code, state| {
                use gdk::ModifierType as M;
                let ctrl = state.contains(M::CONTROL_MASK);
                let shift = state.contains(M::SHIFT_MASK);
                // Copy: Ctrl+Shift+C.
                if ctrl && shift && matches!(keyval, gdk::Key::c | gdk::Key::C) {
                    if let (Some(clip), Some(text)) =
                        (&clipboard, term.borrow().selection_to_string())
                    {
                        if !text.is_empty() {
                            clip.set_text(&text);
                        }
                    }
                    return glib::Propagation::Stop;
                }
                // Paste: Ctrl+Shift+V.
                if ctrl && shift && matches!(keyval, gdk::Key::v | gdk::Key::V) {
                    if let Some(clip) = &clipboard {
                        paste_from(clip, &term, &session, &on_input);
                    }
                    return glib::Propagation::Stop;
                }
                // Scrollback: Shift+PageUp/PageDown/Home/End.
                if shift {
                    let scroll = match keyval {
                        gdk::Key::Page_Up => Some(Scroll::PageUp),
                        gdk::Key::Page_Down => Some(Scroll::PageDown),
                        gdk::Key::Home => Some(Scroll::Top),
                        gdk::Key::End => Some(Scroll::Bottom),
                        _ => None,
                    };
                    if let Some(s) = scroll {
                        term.borrow_mut().scroll_display(s);
                        if let Some(a) = area_w.upgrade() {
                            a.queue_draw();
                        }
                        return glib::Propagation::Stop;
                    }
                }
                let app_cursor = term.borrow().mode().contains(TermMode::APP_CURSOR);
                if let Some(bytes) = encode_key(keyval, state, app_cursor) {
                    // Any keypress snaps the view back to the prompt (like a normal terminal).
                    term.borrow_mut().scroll_display(Scroll::Bottom);
                    (on_input)(&session, bytes);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            area.add_controller(keys);
        }

        // Mouse buttons (press/release) — selection, copy-to-primary, paste, mouse reporting.
        {
            let click = gtk4::GestureClick::new();
            click.set_button(0); // all buttons
            let p_term = term.clone();
            let p_dims = dims.clone();
            let p_cell = cell.clone();
            let p_drag = drag.clone();
            let p_last = last_pos.clone();
            let p_input = cb.on_input.clone();
            let p_session = session.clone();
            let p_primary = primary.clone();
            let p_area = area.downgrade();
            click.connect_pressed(move |g, n_press, x, y| {
                if let Some(a) = p_area.upgrade() {
                    a.grab_focus();
                }
                let button = g.current_button();
                let state = g.current_event_state();
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let mode = *p_term.borrow().mode();
                let mouse_on = mode.intersects(TermMode::MOUSE_MODE) && !shift;
                let (pt, side, col, row) = locate(x, y, p_cell.get(), p_dims.get(), &p_term);
                p_last.set((col, row));

                // Middle-click paste (primary selection) when the app isn't grabbing the mouse.
                if button == 2 && !mouse_on {
                    if let Some(p) = &p_primary {
                        paste_from(p, &p_term, &p_session, &p_input);
                    }
                    return;
                }
                if mouse_on {
                    let code = base_button(button);
                    (p_input)(&p_session, mouse_report(code, col, row, true, mode));
                    p_drag.set(Drag::Reporting(code));
                    return;
                }
                // Text selection (left button). Double/triple click = word/line.
                if button == 1 {
                    let ty = match n_press {
                        2 => SelectionType::Semantic,
                        n if n >= 3 => SelectionType::Lines,
                        _ => SelectionType::Simple,
                    };
                    p_term.borrow_mut().selection = Some(Selection::new(ty, pt, side));
                    p_drag.set(Drag::Selecting);
                    if let Some(a) = p_area.upgrade() {
                        a.queue_draw();
                    }
                }
            });

            let rel_term = term.clone();
            let rel_drag = drag.clone();
            let rel_primary = primary.clone();
            let rel_input = cb.on_input.clone();
            let rel_session = session.clone();
            let rel_cell = cell.clone();
            let rel_dims = dims.clone();
            click.connect_released(move |_g, _n, x, y| {
                let (_pt, _side, col, row) = locate(x, y, rel_cell.get(), rel_dims.get(), &rel_term);
                match rel_drag.get() {
                    Drag::Reporting(code) => {
                        let mode = *rel_term.borrow().mode();
                        (rel_input)(&rel_session, mouse_report(code, col, row, false, mode));
                    }
                    Drag::Selecting => {
                        // Auto-copy the selection to the primary selection (middle-click paste).
                        if let (Some(p), Some(text)) =
                            (&rel_primary, rel_term.borrow().selection_to_string())
                        {
                            if !text.is_empty() {
                                p.set_text(&text);
                            }
                        }
                    }
                    Drag::None => {}
                }
                rel_drag.set(Drag::None);
            });
            area.add_controller(click);
        }

        // Pointer motion — extend a selection, or report motion to the app.
        {
            let motion = gtk4::EventControllerMotion::new();
            let term = term.clone();
            let dims = dims.clone();
            let cell = cell.clone();
            let drag = drag.clone();
            let last_pos = last_pos.clone();
            let on_input = cb.on_input.clone();
            let session = session.clone();
            let area_w = area.downgrade();
            motion.connect_motion(move |_m, x, y| {
                let (pt, side, col, row) = locate(x, y, cell.get(), dims.get(), &term);
                last_pos.set((col, row));
                match drag.get() {
                    Drag::Selecting => {
                        if let Some(sel) = term.borrow_mut().selection.as_mut() {
                            sel.update(pt, side);
                        }
                        if let Some(a) = area_w.upgrade() {
                            a.queue_draw();
                        }
                    }
                    Drag::Reporting(code) => {
                        let mode = *term.borrow().mode();
                        if mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG) {
                            // Motion report: base button + 32 (motion bit).
                            (on_input)(&session, mouse_report(code + 32, col, row, true, mode));
                        }
                    }
                    Drag::None => {}
                }
            });
            area.add_controller(motion);
        }

        // Scroll wheel — scrollback, or wheel reports / alt-screen arrow keys.
        {
            let scroll =
                gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::BOTH_AXES);
            let term = term.clone();
            let on_input = cb.on_input.clone();
            let session = session.clone();
            let last_pos = last_pos.clone();
            let area_w = area.downgrade();
            scroll.connect_scroll(move |c, _dx, dy| {
                if dy == 0.0 {
                    return glib::Propagation::Proceed;
                }
                let up = dy < 0.0;
                let steps = (dy.abs().ceil() as i32).max(1) * 3;
                let mode = *term.borrow().mode();
                let shift = c.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
                if mode.intersects(TermMode::MOUSE_MODE) && !shift {
                    // Wheel report: SGR/normal button 64 (up) / 65 (down).
                    let (col, row) = last_pos.get();
                    let code = if up { 64 } else { 65 };
                    for _ in 0..(steps / 3).max(1) {
                        (on_input)(&session, mouse_report(code, col, row, true, mode));
                    }
                } else if mode.contains(TermMode::ALT_SCREEN) && !shift {
                    // Alt-screen app (less/vim) with no mouse mode: translate to arrow keys.
                    let app = mode.contains(TermMode::APP_CURSOR);
                    let key = if up { arrow(b'A', app) } else { arrow(b'B', app) };
                    for _ in 0..steps {
                        (on_input)(&session, key.clone());
                    }
                } else {
                    // Scrollback.
                    term.borrow_mut().scroll_display(Scroll::Delta(if up { steps } else { -steps }));
                    if let Some(a) = area_w.upgrade() {
                        a.queue_draw();
                    }
                }
                glib::Propagation::Stop
            });
            area.add_controller(scroll);
        }

        Self { area, term, parser }
    }
}

/// Read the system clipboard and send it to the session as input (bracketed when the app asked).
fn paste_from(
    clipboard: &gdk::Clipboard,
    term: &Rc<RefCell<Term<EventProxy>>>,
    session: &str,
    on_input: &InputCb,
) {
    let bracketed = term.borrow().mode().contains(TermMode::BRACKETED_PASTE);
    let session = session.to_string();
    let on_input = on_input.clone();
    clipboard.read_text_async(gio::Cancellable::NONE, move |res| {
        if let Ok(Some(text)) = res {
            // Terminals expect CR for line breaks on paste.
            let text = text.replace('\n', "\r");
            let payload = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
            (on_input)(&session, payload.into_bytes());
        }
    });
}

// --- rendering --------------------------------------------------------------------------

/// Default foreground/background (a soft light-on-dark terminal theme).
const DEFAULT_FG: (f64, f64, f64) = (0.85, 0.85, 0.86);
const DEFAULT_BG: (f64, f64, f64) = (0.11, 0.12, 0.13);
/// Selection highlight background.
const SELECTION_BG: (f64, f64, f64) = (0.22, 0.35, 0.55);

fn measure_cell() -> (f64, f64) {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 8, 8).unwrap();
    let cr = cairo::Context::new(&surface).unwrap();
    cell_metrics(&cr)
}

fn cell_metrics(cr: &cairo::Context) -> (f64, f64) {
    cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(FONT_SIZE);
    let fe = cr.font_extents().map(|f| f.height()).unwrap_or(FONT_SIZE * 1.3);
    let te = cr.text_extents("M").map(|t| t.x_advance()).unwrap_or(FONT_SIZE * 0.6);
    let w = if te > 0.0 { te } else { FONT_SIZE * 0.6 };
    let h = if fe > 0.0 { fe } else { FONT_SIZE * 1.3 };
    (w.ceil(), h.ceil())
}

fn refine_cell(cr: &cairo::Context, cell: &Rc<Cell<(f64, f64)>>) -> (f64, f64) {
    let m = cell_metrics(cr);
    cell.set(m);
    m
}

/// Paint the terminal grid: backgrounds (incl. selection), glyphs, then the cursor.
fn draw_content(cr: &cairo::Context, term: &Term<EventProxy>, cell: (f64, f64), _w: i32, _h: i32) {
    let (cw, ch) = cell;
    let content = term.renderable_content();
    let colors = content.colors;
    let display_offset = content.display_offset as i32;
    let selection = content.selection;

    // Clear to the default background.
    cr.set_source_rgb(DEFAULT_BG.0, DEFAULT_BG.1, DEFAULT_BG.2);
    let _ = cr.paint();

    cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(FONT_SIZE);
    let ascent = cr.font_extents().map(|f| f.ascent()).unwrap_or(FONT_SIZE);

    for indexed in content.display_iter {
        let point = indexed.point;
        let c = indexed.cell;
        let flags = c.flags;
        if flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let row = point.line.0 + display_offset;
        if row < 0 {
            continue;
        }
        let x = point.column.0 as f64 * cw;
        let y = row as f64 * ch;
        let width_cells = if flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 };

        let mut fg = resolve(c.fg, colors, DEFAULT_FG);
        let mut bg = resolve(c.bg, colors, DEFAULT_BG);
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(Flags::DIM) {
            fg = (fg.0 * 0.66, fg.1 * 0.66, fg.2 * 0.66);
        }
        if selection.is_some_and(|r| r.contains(point)) {
            bg = SELECTION_BG;
        }

        if bg != DEFAULT_BG {
            cr.set_source_rgb(bg.0, bg.1, bg.2);
            cr.rectangle(x, y, cw * width_cells, ch);
            let _ = cr.fill();
        }

        if c.c != ' ' && !flags.contains(Flags::HIDDEN) {
            cr.select_font_face(
                "monospace",
                if flags.contains(Flags::ITALIC) {
                    cairo::FontSlant::Italic
                } else {
                    cairo::FontSlant::Normal
                },
                if flags.contains(Flags::BOLD) {
                    cairo::FontWeight::Bold
                } else {
                    cairo::FontWeight::Normal
                },
            );
            cr.set_font_size(FONT_SIZE);
            cr.set_source_rgb(fg.0, fg.1, fg.2);
            cr.move_to(x, y + ascent);
            let mut buf = [0u8; 4];
            let _ = cr.show_text(c.c.encode_utf8(&mut buf));
        }
        if flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE | Flags::UNDERCURL) {
            cr.set_source_rgb(fg.0, fg.1, fg.2);
            cr.rectangle(x, y + ch - 1.5, cw * width_cells, 1.0);
            let _ = cr.fill();
        }
        if flags.contains(Flags::STRIKEOUT) {
            cr.set_source_rgb(fg.0, fg.1, fg.2);
            cr.rectangle(x, y + ch / 2.0, cw * width_cells, 1.0);
            let _ = cr.fill();
        }
    }

    // Cursor (skip when scrolled away or hidden).
    let cur = content.cursor;
    if cur.shape != CursorShape::Hidden {
        let row = cur.point.line.0 + display_offset;
        if row >= 0 {
            let x = cur.point.column.0 as f64 * cw;
            let y = row as f64 * ch;
            cr.set_source_rgba(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2, 0.8);
            match cur.shape {
                CursorShape::Beam => {
                    cr.rectangle(x, y, 2.0, ch);
                    let _ = cr.fill();
                }
                CursorShape::Underline => {
                    cr.rectangle(x, y + ch - 2.0, cw, 2.0);
                    let _ = cr.fill();
                }
                CursorShape::HollowBlock => {
                    cr.set_line_width(1.0);
                    cr.rectangle(x + 0.5, y + 0.5, cw - 1.0, ch - 1.0);
                    let _ = cr.stroke();
                }
                _ => {
                    // Block: fill + redraw the glyph under it in the background color.
                    cr.rectangle(x, y, cw, ch);
                    let _ = cr.fill();
                    if let Some(g) = term.grid().display_iter().find(|i| i.point == cur.point) {
                        if g.cell.c != ' ' {
                            cr.set_source_rgb(DEFAULT_BG.0, DEFAULT_BG.1, DEFAULT_BG.2);
                            cr.move_to(x, y + ascent);
                            let mut buf = [0u8; 4];
                            let _ = cr.show_text(g.cell.c.encode_utf8(&mut buf));
                        }
                    }
                }
            }
        }
    }
}

// --- colors -----------------------------------------------------------------------------

fn rgb_f(rgb: Rgb) -> (f64, f64, f64) {
    (rgb.r as f64 / 255.0, rgb.g as f64 / 255.0, rgb.b as f64 / 255.0)
}

/// Resolve an alacritty cell color to normalized RGB, honoring the theme palette when set and
/// falling back to a built-in xterm palette / the given default otherwise.
fn resolve(
    color: AnsiColor,
    palette: &alacritty_terminal::term::color::Colors,
    default: (f64, f64, f64),
) -> (f64, f64, f64) {
    match color {
        AnsiColor::Spec(rgb) => rgb_f(rgb),
        AnsiColor::Named(named) => {
            palette[named].map(rgb_f).unwrap_or_else(|| named_default(named, default))
        }
        AnsiColor::Indexed(i) => palette[i as usize].map(rgb_f).unwrap_or_else(|| indexed_default(i)),
    }
}

fn named_default(n: NamedColor, fallback: (f64, f64, f64)) -> (f64, f64, f64) {
    use NamedColor::*;
    let idx: u8 = match n {
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
        Foreground | BrightForeground => return DEFAULT_FG,
        Background => return DEFAULT_BG,
        DimForeground => return (DEFAULT_FG.0 * 0.66, DEFAULT_FG.1 * 0.66, DEFAULT_FG.2 * 0.66),
        _ => return fallback,
    };
    indexed_default(idx)
}

/// The standard xterm 256-color palette → normalized RGB.
fn indexed_default(idx: u8) -> (f64, f64, f64) {
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
    let (r, g, b) = match idx {
        0..=15 => BASE[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let conv = |v: u8| -> u8 {
                if v == 0 {
                    0
                } else {
                    55 + v * 40
                }
            };
            (conv(i / 36), conv((i % 36) / 6), conv(i % 6))
        }
        232..=255 => {
            let v = 8 + (idx - 232) * 10;
            (v, v, v)
        }
    };
    (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
}

// --- input encoding ---------------------------------------------------------------------

/// Map a pixel position to a grid `Point` + `Side`, plus the on-screen (col,row).
fn locate(
    x: f64,
    y: f64,
    cell: (f64, f64),
    dims: Dims,
    term: &Rc<RefCell<Term<EventProxy>>>,
) -> (Point, Side, usize, usize) {
    let (cw, ch) = cell;
    let colf = (x / cw).max(0.0);
    let col = (colf.floor() as usize).min(dims.cols.saturating_sub(1));
    let row = ((y / ch).floor() as i64).clamp(0, dims.lines.saturating_sub(1) as i64) as usize;
    let side = if colf.fract() < 0.5 { Side::Left } else { Side::Right };
    let display_offset = term.borrow().grid().display_offset() as i32;
    let point = Point::new(Line(row as i32 - display_offset), Column(col));
    (point, side, col, row)
}

/// Arrow-key bytes, honoring application-cursor mode (SS3 vs CSI).
fn arrow(dir: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', dir]
    } else {
        vec![0x1b, b'[', dir]
    }
}

/// The base SGR/normal button code for a GTK button number (1/2/3 → 0/1/2).
fn base_button(button: u32) -> u8 {
    match button {
        2 => 1,
        3 => 2,
        _ => 0,
    }
}

/// Encode a mouse event: SGR (`ESC[<b;col;rowM/m`) when the app requested it, else normal X10
/// (`ESC[Mb col row`). `code` already includes wheel (64/65) and motion (+32) bits.
fn mouse_report(code: u8, col: usize, row: usize, pressed: bool, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        let m = if pressed { 'M' } else { 'm' };
        format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, m).into_bytes()
    } else {
        // Normal encoding: release is button 3; values are offset by 32 and clamped to 223.
        let cb = if pressed { code } else { 3 };
        let cx = (col as u16 + 1).min(223) as u8 + 32;
        let cy = (row as u16 + 1).min(223) as u8 + 32;
        vec![0x1b, b'[', b'M', 32u8.wrapping_add(cb), cx, cy]
    }
}

/// Encode a GTK key press into terminal input bytes. Returns `None` for keys we don't handle.
fn encode_key(keyval: gdk::Key, state: gdk::ModifierType, app_cursor: bool) -> Option<Vec<u8>> {
    use gdk::Key;
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let alt = state.contains(gdk::ModifierType::ALT_MASK);

    let named: Option<Vec<u8>> = match keyval {
        Key::Return | Key::KP_Enter => Some(vec![b'\r']),
        Key::BackSpace => Some(vec![0x7f]),
        Key::Tab => Some(vec![b'\t']),
        Key::ISO_Left_Tab => Some(b"\x1b[Z".to_vec()),
        Key::Escape => Some(vec![0x1b]),
        Key::Up => Some(arrow(b'A', app_cursor)),
        Key::Down => Some(arrow(b'B', app_cursor)),
        Key::Right => Some(arrow(b'C', app_cursor)),
        Key::Left => Some(arrow(b'D', app_cursor)),
        Key::Home => Some(arrow(b'H', app_cursor)),
        Key::End => Some(arrow(b'F', app_cursor)),
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

    let ch = keyval.to_unicode()?;
    if ctrl {
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
