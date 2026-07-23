//! Cross-platform terminal tab view for headless clones, powered by the `alacritty_terminal`
//! engine and rendered on the GPU through GTK's scene graph.
//!
//! A [`TerminalView`] is a `gtk4::Notebook` with one [`TerminalTab`] per tmux session (plus a
//! tab-bar "+" that requests a new session). Each tab drives an `alacritty_terminal::Term` — a
//! full terminal state machine (grid, scrollback, selection, mouse/app modes) — fed the control-
//! server's raw PTY bytes via [`TerminalView::feed`]. Rendering is a custom `gtk4::Widget`
//! ([`TermArea`]) whose `snapshot()` emits **GSK render nodes**: glyphs via `append_layout` (GTK's
//! GPU glyph atlas) and backgrounds/cursor/selection via `append_color` — so text is drawn by the
//! GL/Vulkan renderer rather than rasterized on the CPU each frame. No system terminal library, so
//! it builds and runs identically on Linux and macOS.
//!
//! Interactions match a normal terminal: text **selection** (click-drag, word/line on
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
use gtk4::gdk;
use gtk4::gio;
use gtk4::glib;
use gtk4::graphene;
use gtk4::pango;
use gtk4::prelude::*;
use gtk4::subclass::prelude::ObjectSubclassIsExt;

/// Monospace pixel size for the cell grid.
const FONT_PX: f64 = 13.0;
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

/// Shared terminal handle used by both the render widget and the input controllers.
type SharedTerm = Rc<RefCell<Term<EventProxy>>>;
/// Shared cell metrics (px) and grid dimensions, written by the widget, read by controllers.
type SharedMetrics = Rc<Cell<(f64, f64)>>;
type SharedGrid = Rc<Cell<(usize, usize)>>;

// --- GPU render widget ------------------------------------------------------------------

mod imp {
    use super::*;
    use gtk4::subclass::prelude::*;

    /// Per-row text buffer built each frame: the row string plus a pango attribute list mapping
    /// byte ranges to foreground color + bold/italic/underline.
    struct RowBuf {
        text: String,
        attrs: pango::AttrList,
        cols: usize,
    }
    impl RowBuf {
        fn new() -> Self {
            Self { text: String::new(), attrs: pango::AttrList::new(), cols: 0 }
        }
        fn push_cell(&mut self, col: usize, ch: char, fg: (f64, f64, f64), flags: Flags, wide: bool) {
            while self.cols < col {
                self.text.push(' ');
                self.cols += 1;
            }
            let start = self.text.len() as u32;
            self.text.push(ch);
            let end = self.text.len() as u32;
            let (r, g, b) = pango16(fg);
            let mut a = pango::AttrColor::new_foreground(r, g, b);
            a.set_start_index(start);
            a.set_end_index(end);
            self.attrs.insert(a);
            let add = |mut attr: pango::Attribute| {
                attr.set_start_index(start);
                attr.set_end_index(end);
                self.attrs.insert(attr);
            };
            if flags.contains(Flags::BOLD) {
                add(pango::AttrInt::new_weight(pango::Weight::Bold).upcast());
            }
            if flags.contains(Flags::ITALIC) {
                add(pango::AttrInt::new_style(pango::Style::Italic).upcast());
            }
            if flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE | Flags::UNDERCURL) {
                add(pango::AttrInt::new_underline(pango::Underline::Single).upcast());
            }
            if flags.contains(Flags::STRIKEOUT) {
                add(pango::AttrInt::new_strikethrough(true).upcast());
            }
            self.cols += if wide { 2 } else { 1 };
        }
    }

    #[derive(Default)]
    pub struct TermArea {
        pub(super) term: RefCell<Option<SharedTerm>>,
        pub(super) on_resize: RefCell<Option<ResizeCb>>,
        pub(super) metrics: RefCell<Option<SharedMetrics>>,
        pub(super) grid: RefCell<Option<SharedGrid>>,
        pub(super) font: RefCell<Option<pango::FontDescription>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TermArea {
        const NAME: &'static str = "RmngTermArea";
        type Type = super::TermArea;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for TermArea {}

    impl WidgetImpl for TermArea {
        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            self.render(snapshot);
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.handle_size(width, height);
        }

        fn measure(&self, orientation: gtk4::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (cw, ch) = self.ensure_metrics();
            let nat = match orientation {
                gtk4::Orientation::Horizontal => (cw * INIT_COLS as f64) as i32,
                _ => (ch * INIT_ROWS as f64) as i32,
            };
            (1, nat.max(1), -1, -1)
        }
    }

    impl TermArea {
        /// Lazily build the monospace font + measure the cell size; publish it to the shared cell.
        fn ensure_metrics(&self) -> (f64, f64) {
            if self.font.borrow().is_none() {
                let mut fd = pango::FontDescription::new();
                fd.set_family("monospace");
                fd.set_absolute_size(FONT_PX * pango::SCALE as f64);
                let ctx = self.obj().pango_context();
                let m = ctx.metrics(Some(&fd), None);
                let cw = (m.approximate_char_width() as f64 / pango::SCALE as f64).max(1.0);
                let ch = ((m.ascent() + m.descent()) as f64 / pango::SCALE as f64).max(1.0);
                *self.font.borrow_mut() = Some(fd);
                if let Some(shared) = self.metrics.borrow().as_ref() {
                    shared.set((cw, ch));
                }
            }
            self.metrics.borrow().as_ref().map(|m| m.get()).unwrap_or((8.0, 16.0))
        }

        /// Recompute the grid from the allocation; resize the terminal + notify the server.
        fn handle_size(&self, w: i32, h: i32) {
            let (cw, ch) = self.ensure_metrics();
            if cw <= 0.0 || ch <= 0.0 {
                return;
            }
            let cols = ((w as f64 / cw).floor() as i64).clamp(1, u16::MAX as i64) as usize;
            let lines = ((h as f64 / ch).floor() as i64).clamp(1, u16::MAX as i64) as usize;
            if let Some(grid) = self.grid.borrow().as_ref() {
                if grid.get() == (cols, lines) {
                    return;
                }
                grid.set((cols, lines));
            }
            if let Some(term) = self.term.borrow().as_ref() {
                term.borrow_mut().resize(Dims { cols, lines });
            }
            if let Some(cb) = self.on_resize.borrow().as_ref() {
                cb(cols as u16, lines as u16);
            }
        }

        /// Paint the terminal: one `append_color` per non-default cell background, one
        /// `append_layout` (GPU text) per row, then the cursor.
        fn render(&self, snapshot: &gtk4::Snapshot) {
            let (cw, ch) = self.ensure_metrics();
            let term_rc = match self.term.borrow().as_ref() {
                Some(t) => t.clone(),
                None => return,
            };
            let term = term_rc.borrow();
            let obj = self.obj();
            let wpx = obj.width() as f32;
            let hpx = obj.height() as f32;
            let ctx = obj.pango_context();
            let font_b = self.font.borrow();
            let font = match font_b.as_ref() {
                Some(f) => f,
                None => return,
            };
            let (cwf, chf) = (cw as f32, ch as f32);
            let lines = self.grid.borrow().as_ref().map(|g| g.get().1).unwrap_or(INIT_ROWS);

            // Background fill.
            snapshot.append_color(&rgba(DEFAULT_BG), &graphene::Rect::new(0.0, 0.0, wpx, hpx));

            let content = term.renderable_content();
            let colors = content.colors;
            let offset = content.display_offset as i32;
            let selection = content.selection;
            let cursor = content.cursor;
            let cursor_on = cursor.shape != CursorShape::Hidden;

            let mut rows: Vec<RowBuf> = Vec::with_capacity(lines);
            rows.resize_with(lines, RowBuf::new);

            for indexed in content.display_iter {
                let point = indexed.point;
                let c = indexed.cell;
                let flags = c.flags;
                if flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let row = point.line.0 + offset;
                if row < 0 || row as usize >= lines {
                    continue;
                }
                let rowi = row as usize;
                let col = point.column.0;

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
                // Block cursor: invert this cell (fg glyph on the cursor color).
                if cursor_on
                    && point == cursor.point
                    && matches!(cursor.shape, CursorShape::Block | CursorShape::HollowBlock)
                {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let wide = flags.contains(Flags::WIDE_CHAR);
                if bg != DEFAULT_BG {
                    let w = cwf * if wide { 2.0 } else { 1.0 };
                    snapshot.append_color(
                        &rgba(bg),
                        &graphene::Rect::new(col as f32 * cwf, rowi as f32 * chf, w, chf),
                    );
                }
                let glyph = if flags.contains(Flags::HIDDEN) { ' ' } else { c.c };
                rows[rowi].push_cell(col, glyph, fg, flags, wide);
            }

            for (rowi, rb) in rows.iter().enumerate() {
                if rb.text.trim_end().is_empty() {
                    continue;
                }
                let layout = pango::Layout::new(&ctx);
                layout.set_font_description(Some(font));
                layout.set_text(&rb.text);
                layout.set_attributes(Some(&rb.attrs));
                snapshot.save();
                snapshot.translate(&graphene::Point::new(0.0, rowi as f32 * chf));
                snapshot.append_layout(&layout, &rgba(DEFAULT_FG));
                snapshot.restore();
            }

            // Beam / underline cursor (block is drawn via the inversion above).
            if cursor_on {
                let row = cursor.point.line.0 + offset;
                if row >= 0 && (row as usize) < lines {
                    let x = cursor.point.column.0 as f32 * cwf;
                    let y = row as f32 * chf;
                    match cursor.shape {
                        CursorShape::Beam => snapshot
                            .append_color(&rgba(DEFAULT_FG), &graphene::Rect::new(x, y, 2.0, chf)),
                        CursorShape::Underline => snapshot.append_color(
                            &rgba(DEFAULT_FG),
                            &graphene::Rect::new(x, y + chf - 2.0, cwf, 2.0),
                        ),
                        _ => {}
                    }
                }
            }
        }
    }
}

glib::wrapper! {
    /// GPU-rendered terminal surface for one session.
    pub struct TermArea(ObjectSubclass<imp::TermArea>) @extends gtk4::Widget;
}

impl TermArea {
    fn new() -> Self {
        glib::Object::new()
    }

    /// Wire the widget to its terminal, resize callback, and the shared metrics/grid cells the
    /// input controllers read.
    fn set_context(
        &self,
        term: SharedTerm,
        on_resize: ResizeCb,
        metrics: SharedMetrics,
        grid: SharedGrid,
    ) {
        let imp = self.imp();
        *imp.term.borrow_mut() = Some(term);
        *imp.on_resize.borrow_mut() = Some(on_resize);
        *imp.metrics.borrow_mut() = Some(metrics);
        *imp.grid.borrow_mut() = Some(grid);
    }
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
    area: TermArea,
    term: SharedTerm,
    parser: Rc<RefCell<Processor>>,
}

impl TerminalTab {
    fn new(session: String, cb: TermCallbacks) -> Self {
        let area = TermArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.set_can_focus(true);

        let display = gdk::Display::default();
        let clipboard = display.as_ref().map(|d| d.clipboard());
        let primary = display.as_ref().map(|d| d.primary_clipboard());

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
        let term: SharedTerm =
            Rc::new(RefCell::new(Term::new(TermConfig::default(), &init, proxy)));
        let parser = Rc::new(RefCell::new(Processor::new()));
        let metrics: SharedMetrics = Rc::new(Cell::new((8.0, 16.0)));
        let grid: SharedGrid = Rc::new(Cell::new((INIT_COLS, INIT_ROWS)));
        let drag = Rc::new(Cell::new(Drag::None));
        let last_pos = Rc::new(Cell::new((0usize, 0usize)));

        area.set_context(term.clone(), cb.on_resize.clone(), metrics.clone(), grid.clone());

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
                if ctrl && shift && matches!(keyval, gdk::Key::v | gdk::Key::V) {
                    if let Some(clip) = &clipboard {
                        paste_from(clip, &term, &session, &on_input);
                    }
                    return glib::Propagation::Stop;
                }
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
                    term.borrow_mut().scroll_display(Scroll::Bottom);
                    (on_input)(&session, bytes);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            area.add_controller(keys);
        }

        // Mouse buttons.
        {
            let click = gtk4::GestureClick::new();
            click.set_button(0);
            let p_term = term.clone();
            let p_metrics = metrics.clone();
            let p_grid = grid.clone();
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
                let shift = g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
                let mode = *p_term.borrow().mode();
                let mouse_on = mode.intersects(TermMode::MOUSE_MODE) && !shift;
                let (pt, side, col, row) =
                    locate(x, y, p_metrics.get(), p_grid.get(), &p_term);
                p_last.set((col, row));

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
            let rel_metrics = metrics.clone();
            let rel_grid = grid.clone();
            click.connect_released(move |_g, _n, x, y| {
                let (_pt, _side, col, row) =
                    locate(x, y, rel_metrics.get(), rel_grid.get(), &rel_term);
                match rel_drag.get() {
                    Drag::Reporting(code) => {
                        let mode = *rel_term.borrow().mode();
                        (rel_input)(&rel_session, mouse_report(code, col, row, false, mode));
                    }
                    Drag::Selecting => {
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

        // Pointer motion.
        {
            let motion = gtk4::EventControllerMotion::new();
            let term = term.clone();
            let metrics = metrics.clone();
            let grid = grid.clone();
            let drag = drag.clone();
            let last_pos = last_pos.clone();
            let on_input = cb.on_input.clone();
            let session = session.clone();
            let area_w = area.downgrade();
            motion.connect_motion(move |_m, x, y| {
                let (pt, side, col, row) = locate(x, y, metrics.get(), grid.get(), &term);
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
                            (on_input)(&session, mouse_report(code + 32, col, row, true, mode));
                        }
                    }
                    Drag::None => {}
                }
            });
            area.add_controller(motion);
        }

        // Scroll wheel.
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
                    let (col, row) = last_pos.get();
                    let code = if up { 64 } else { 65 };
                    for _ in 0..(steps / 3).max(1) {
                        (on_input)(&session, mouse_report(code, col, row, true, mode));
                    }
                } else if mode.contains(TermMode::ALT_SCREEN) && !shift {
                    let app = mode.contains(TermMode::APP_CURSOR);
                    let key = if up { arrow(b'A', app) } else { arrow(b'B', app) };
                    for _ in 0..steps {
                        (on_input)(&session, key.clone());
                    }
                } else {
                    term.borrow_mut()
                        .scroll_display(Scroll::Delta(if up { steps } else { -steps }));
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
    term: &SharedTerm,
    session: &str,
    on_input: &InputCb,
) {
    let bracketed = term.borrow().mode().contains(TermMode::BRACKETED_PASTE);
    let session = session.to_string();
    let on_input = on_input.clone();
    clipboard.read_text_async(gio::Cancellable::NONE, move |res| {
        if let Ok(Some(text)) = res {
            let text = text.replace('\n', "\r");
            let payload = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
            (on_input)(&session, payload.into_bytes());
        }
    });
}

// --- colors -----------------------------------------------------------------------------

/// Default foreground/background (a soft light-on-dark terminal theme).
const DEFAULT_FG: (f64, f64, f64) = (0.85, 0.85, 0.86);
const DEFAULT_BG: (f64, f64, f64) = (0.11, 0.12, 0.13);
/// Selection highlight background.
const SELECTION_BG: (f64, f64, f64) = (0.22, 0.35, 0.55);

fn rgba(c: (f64, f64, f64)) -> gdk::RGBA {
    gdk::RGBA::new(c.0 as f32, c.1 as f32, c.2 as f32, 1.0)
}

fn pango16(c: (f64, f64, f64)) -> (u16, u16, u16) {
    (
        (c.0.clamp(0.0, 1.0) * 65535.0) as u16,
        (c.1.clamp(0.0, 1.0) * 65535.0) as u16,
        (c.2.clamp(0.0, 1.0) * 65535.0) as u16,
    )
}

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
    grid: (usize, usize),
    term: &SharedTerm,
) -> (Point, Side, usize, usize) {
    let (cw, ch) = cell;
    let (cols, lines) = grid;
    let colf = (x / cw).max(0.0);
    let col = (colf.floor() as usize).min(cols.saturating_sub(1));
    let row = ((y / ch).floor() as i64).clamp(0, lines.saturating_sub(1) as i64) as usize;
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
