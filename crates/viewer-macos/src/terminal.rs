//! The headless-clone terminal: one tab per tmux session, each driving an
//! `alacritty_terminal::Term` fed with the server's raw PTY bytes and drawn with AppKit text.
//!
//! The GTK viewer draws the same grid with cairo; the colour scheme and the escape-sequence
//! encoders live in [`viewer_core::terminal`] so the two front-ends cannot drift. What differs is
//! only the painting (`NSAttributedString` here, pango there) and the key source (`NSEvent`'s
//! `keyCode` here, GDK keyvals there).
//!
//! The view is `isFlipped`, so its origin is top-left and row 0 is at the top — the natural
//! orientation for a grid, and what `drawAtPoint:` then expects.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSPasteboard, NSPasteboardTypeString, NSSegmentedControl, NSSegmentSwitchTracking,
    NSUnderlineStyleAttributeName, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString,
};

use viewer_core::terminal::{arrow, bold_bright, dark_theme, resolve, Rgb3, Theme};

/// Extra leading, as a multiple of the font's natural line height.
const LINE_HEIGHT: f64 = 1.1;
/// Inset around the grid, in cells.
const PAD_CELLS: f64 = 0.5;
const INIT_COLS: usize = 80;
const INIT_ROWS: usize = 24;
/// Height of the tab strip, in points.
const TAB_H: f64 = 28.0;
/// A resize storm (live window drag) would otherwise send one `TermResize` per frame.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(90);

pub type InputCb = Rc<dyn Fn(&str, Vec<u8>)>;
pub type ResizeCb = Rc<dyn Fn(u16, u16)>;
pub type NewSessionCb = Rc<dyn Fn()>;

pub struct TermCallbacks {
    pub on_input: InputCb,
    pub on_resize: ResizeCb,
    pub on_new_session: NewSessionCb,
}

/// Grid size for alacritty's `Dimensions`.
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

/// `Term` needs an event sink; nothing it emits requires action here (no PTY to drive, no
/// title/bell surface), so events are dropped deliberately rather than silently.
struct EventProxy;

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        tracing::trace!("terminal event ignored: {event:?}");
    }
}

/// One tmux session's screen.
struct Session {
    term: Term<EventProxy>,
    parser: Processor,
}

impl Session {
    fn new(dims: Dims) -> Self {
        Session {
            term: Term::new(TermConfig::default(), &dims, EventProxy),
            parser: Processor::new(),
        }
    }
}

/// Everything the view and its callbacks share. Main-thread only.
struct State {
    sessions: RefCell<Vec<String>>,
    active: Cell<usize>,
    terms: RefCell<HashMap<String, Session>>,
    cb: TermCallbacks,
    theme: Theme,
    /// (cell width, cell height) in points.
    metrics: Cell<(f64, f64)>,
    /// Current grid size, and the last size actually reported to the server.
    grid: Cell<(usize, usize)>,
    last_sent: Cell<(u16, u16)>,
    resize_at: Cell<Option<Instant>>,
}

impl State {
    fn active_session(&self) -> Option<String> {
        self.sessions.borrow().get(self.active.get()).cloned()
    }

    /// Send bytes as input for the active session.
    fn send(&self, bytes: Vec<u8>) {
        if let Some(s) = self.active_session() {
            (self.cb.on_input)(&s, bytes);
        }
    }
}

// ── the grid view ───────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GridIvars {
    state: RefCell<Option<Rc<State>>>,
}

define_class!(
    // SAFETY: NSView imposes no subclassing requirements beyond main-thread use; no Drop conflict.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = GridIvars]
    #[name = "RmngTermGridView"]
    pub struct GridView;

    impl GridView {
        /// Top-left origin: row 0 at the top, and `drawAtPoint:` measured from the top.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.paint();
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &objc2_app_kit::NSEvent) {
            let Some(state) = self.state() else { return };
            let mods = event.modifierFlags();
            let cmd = mods.contains(objc2_app_kit::NSEventModifierFlags::Command);
            let ctrl = mods.contains(objc2_app_kit::NSEventModifierFlags::Control);
            let alt = mods.contains(objc2_app_kit::NSEventModifierFlags::Option);
            let kvk = event.keyCode() as u32;

            // ⌘V pastes; ⌘C copies the selection when there is one. These are Mac chords, so they
            // are handled here rather than the GTK viewer's Ctrl+Shift pair.
            if cmd && kvk == 0x09 {
                self.paste();
                return;
            }
            if cmd && kvk == 0x08 {
                self.copy_selection();
                return;
            }
            if cmd {
                return; // other ⌘ chords belong to the app, not the remote shell
            }

            let app_cursor = {
                let terms = state.terms.borrow();
                state
                    .active_session()
                    .and_then(|s| terms.get(&s).map(|t| t.term.mode().contains(TermMode::APP_CURSOR)))
                    .unwrap_or(false)
            };
            let chars = event.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
            if let Some(bytes) = encode_key(kvk, &chars, ctrl, alt, app_cursor) {
                state.send(bytes);
            }
        }
    }
);

impl GridView {
    fn state(&self) -> Option<Rc<State>> {
        self.ivars().state.borrow().clone()
    }

    fn paste(&self) {
        let Some(state) = self.state() else { return };
        let pb = NSPasteboard::generalPasteboard();
        let Some(text) = pb.stringForType(unsafe { NSPasteboardTypeString }) else { return };
        let s = text.to_string();
        let bracketed = {
            let terms = state.terms.borrow();
            state
                .active_session()
                .and_then(|k| terms.get(&k).map(|t| t.term.mode().contains(TermMode::BRACKETED_PASTE)))
                .unwrap_or(false)
        };
        // Bracketed paste tells the shell these bytes are pasted, not typed, so a multi-line
        // paste doesn't execute line by line.
        let mut bytes = Vec::with_capacity(s.len() + 12);
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(s.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        state.send(bytes);
    }

    fn copy_selection(&self) {
        let Some(state) = self.state() else { return };
        let terms = state.terms.borrow();
        let Some(key) = state.active_session() else { return };
        let Some(sess) = terms.get(&key) else { return };
        let Some(text) = sess.term.selection_to_string() else { return };
        if text.is_empty() {
            return;
        }
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        pb.setString_forType(&NSString::from_str(&text), unsafe { NSPasteboardTypeString });
    }

    /// Paint the active session's grid.
    fn paint(&self) {
        let Some(state) = self.state() else { return };
        let theme = state.theme;
        let bounds = self.bounds();

        fill_rect(bounds, theme.bg);

        let terms = state.terms.borrow();
        let Some(key) = state.active_session() else { return };
        let Some(sess) = terms.get(&key) else { return };

        let (cw, ch) = state.metrics.get();
        let (cols, lines) = state.grid.get();
        let pad = cw * PAD_CELLS;
        // Extra leading is split above and below the glyph so text sits centred in its cell while
        // backgrounds and the cursor still fill the whole cell.
        let text_pad = (ch - ch / LINE_HEIGHT) / 2.0;

        let content = sess.term.renderable_content();
        let colors = content.colors;
        let offset = content.display_offset as i32;
        let selection = content.selection;
        let cursor = content.cursor;
        let cursor_on = cursor.shape != CursorShape::Hidden;

        // Pass 1: backgrounds, coalescing horizontal runs of one colour into a single rect.
        // Pass 2: glyphs, coalescing runs that share a colour and flags into one attributed
        // string. A solid line becomes one fill and one draw rather than `cols` of each.
        let mut bg_run: Option<(usize, usize, usize, Rgb3)> = None;
        let mut text_run: Option<(usize, usize, String, Rgb3, Flags)> = None;
        let flush_bg = |run: Option<(usize, usize, usize, Rgb3)>| {
            if let Some((r, c0, c1, color)) = run {
                let rect = NSRect::new(
                    NSPoint::new(pad + c0 as f64 * cw, pad + r as f64 * ch),
                    NSSize::new((c1 - c0) as f64 * cw, ch),
                );
                fill_rect(rect, color);
            }
        };
        let font = self.font();
        let flush_text = |run: Option<(usize, usize, String, Rgb3, Flags)>| {
            if let Some((r, c0, s, fg, flags)) = run {
                if !s.trim().is_empty() {
                    draw_text(
                        &s,
                        NSPoint::new(pad + c0 as f64 * cw, pad + r as f64 * ch + text_pad),
                        &font,
                        fg,
                        flags,
                    );
                }
            }
        };

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let flags = cell.flags;
            if flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let row = point.line.0 + offset;
            if row < 0 || row as usize >= lines {
                continue;
            }
            let (rowi, col) = (row as usize, point.column.0);
            if col >= cols {
                continue;
            }

            // Bold promotes the 8 base ANSI colours to their bright variants — the "bold is
            // bright" behaviour TUIs like htop rely on.
            let fg_src = if flags.contains(Flags::BOLD) { bold_bright(cell.fg) } else { cell.fg };
            let mut fg = resolve(fg_src, colors, &theme);
            let mut bg = resolve(cell.bg, colors, &theme);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::DIM) {
                fg = (fg.0 * 0.66, fg.1 * 0.66, fg.2 * 0.66);
            }
            if selection.is_some_and(|r| r.contains(point)) {
                bg = theme.sel;
            }
            if cursor_on
                && point == cursor.point
                && matches!(cursor.shape, CursorShape::Block | CursorShape::HollowBlock)
            {
                std::mem::swap(&mut fg, &mut bg);
            }
            let span = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

            // Background run.
            if bg == theme.bg {
                flush_bg(bg_run.take());
            } else {
                match bg_run {
                    Some((r, c0, c1, c)) if r == rowi && c1 == col && c == bg => {
                        bg_run = Some((r, c0, col + span, c));
                    }
                    other => {
                        flush_bg(other);
                        bg_run = Some((rowi, col, col + span, bg));
                    }
                }
            }

            // Glyph run.
            let glyph = if flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
            let style = flags & (Flags::UNDERLINE | Flags::BOLD | Flags::ITALIC);
            match text_run.take() {
                Some((r, c0, mut s, f, fl))
                    if r == rowi && f == fg && fl == style && c0 + s.chars().count() == col =>
                {
                    s.push(glyph);
                    text_run = Some((r, c0, s, f, fl));
                }
                other => {
                    flush_text(other);
                    text_run = Some((rowi, col, glyph.to_string(), fg, style));
                }
            }
        }
        flush_bg(bg_run.take());
        flush_text(text_run.take());

        // A bar / underline cursor is drawn on top; a block cursor was handled by the swap above.
        if cursor_on && !matches!(cursor.shape, CursorShape::Block | CursorShape::HollowBlock) {
            let row = cursor.point.line.0 + offset;
            if row >= 0 && (row as usize) < lines {
                let (x, y) = (pad + cursor.point.column.0 as f64 * cw, pad + row as f64 * ch);
                let rect = match cursor.shape {
                    CursorShape::Underline => {
                        NSRect::new(NSPoint::new(x, y + ch - 2.0), NSSize::new(cw, 2.0))
                    }
                    _ => NSRect::new(NSPoint::new(x, y), NSSize::new(2.0, ch)),
                };
                fill_rect(rect, theme.fg);
            }
        }
    }

    fn font(&self) -> Retained<NSFont> {
        terminal_font()
    }
}

/// Fill `rect` with `color`.
fn fill_rect(rect: NSRect, color: Rgb3) {
    ns_color(color).setFill();
    NSBezierPath::fillRect(rect);
}

/// Draw `text` with the terminal font at `origin` (top-left, the view being flipped).
fn draw_text(text: &str, origin: NSPoint, font: &NSFont, fg: Rgb3, flags: Flags) {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let _ = mtm;
    let mut keys: Vec<&objc2_foundation::NSString> = Vec::with_capacity(3);
    let mut vals: Vec<&AnyObject> = Vec::with_capacity(3);
    let color = ns_color(fg);
    // SAFETY: these are the documented AppKit attribute-name globals.
    unsafe {
        keys.push(NSFontAttributeName);
        vals.push(&*(font as *const NSFont as *const AnyObject));
        keys.push(NSForegroundColorAttributeName);
        vals.push(&*(&*color as *const NSColor as *const AnyObject));
    }
    let underline = NSNumber::new_i32(1);
    if flags.contains(Flags::UNDERLINE) {
        unsafe {
            keys.push(NSUnderlineStyleAttributeName);
            vals.push(&*(&*underline as *const NSNumber as *const AnyObject));
        }
    }
    let attrs = NSDictionary::from_slices(&keys, &vals);
    let s = NSString::from_str(text);
    // SAFETY: `initWithString:attributes:` takes a string and an attribute dictionary, which is
    // exactly what we built; the result is a normal attributed string.
    let attr: Retained<NSAttributedString> =
        unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &s, Some(&attrs)) };
    // SAFETY: `drawAtPoint:` is AppKit's NSStringDrawing category on NSAttributedString; it is
    // valid inside a `drawRect:` where a graphics context is current, which is our only caller.
    let _: () = unsafe { msg_send![&*attr, drawAtPoint: origin] };
}

fn ns_color(c: Rgb3) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.0, c.1, c.2, 1.0)
}

/// Menlo is the terminal face every Mac has; fall back to the system monospace if it is missing.
fn terminal_font() -> Retained<NSFont> {
    NSFont::fontWithName_size(&NSString::from_str("Menlo"), 12.0)
        .or_else(|| NSFont::userFixedPitchFontOfSize(12.0))
        .unwrap_or_else(|| NSFont::systemFontOfSize(12.0))
}

/// Cell metrics for the terminal font: a monospace advance and the leaded line height.
fn cell_metrics() -> (f64, f64) {
    let font = terminal_font();
    let cw = font.maximumAdvancement().width.max(1.0);
    let ch = ((font.ascender() - font.descender() + font.leading()) * LINE_HEIGHT).max(1.0);
    (cw, ch)
}

/// Encode a macOS key press into terminal input bytes. `kvk` is the Carbon virtual key and
/// `chars` the characters ignoring modifiers; `None` means "not a key the terminal sends".
fn encode_key(kvk: u32, chars: &str, ctrl: bool, alt: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let named: Option<Vec<u8>> = match kvk {
        0x24 | 0x4C => Some(vec![b'\r']),          // Return / KeypadEnter
        0x33 => Some(vec![0x7f]),                  // Delete (backspace)
        0x30 => Some(vec![b'\t']),                 // Tab
        0x35 => Some(vec![0x1b]),                  // Escape
        0x7E => Some(arrow(b'A', app_cursor)),     // Up
        0x7D => Some(arrow(b'B', app_cursor)),     // Down
        0x7C => Some(arrow(b'C', app_cursor)),     // Right
        0x7B => Some(arrow(b'D', app_cursor)),     // Left
        0x73 => Some(arrow(b'H', app_cursor)),     // Home
        0x77 => Some(arrow(b'F', app_cursor)),     // End
        0x72 => Some(b"\x1b[2~".to_vec()),         // Insert / Help
        0x75 => Some(b"\x1b[3~".to_vec()),         // Forward delete
        0x74 => Some(b"\x1b[5~".to_vec()),         // Page Up
        0x79 => Some(b"\x1b[6~".to_vec()),         // Page Down
        0x7A => Some(b"\x1bOP".to_vec()),          // F1
        0x78 => Some(b"\x1bOQ".to_vec()),          // F2
        0x63 => Some(b"\x1bOR".to_vec()),          // F3
        0x76 => Some(b"\x1bOS".to_vec()),          // F4
        0x60 => Some(b"\x1b[15~".to_vec()),        // F5
        0x61 => Some(b"\x1b[17~".to_vec()),        // F6
        0x62 => Some(b"\x1b[18~".to_vec()),        // F7
        0x64 => Some(b"\x1b[19~".to_vec()),        // F8
        0x65 => Some(b"\x1b[20~".to_vec()),        // F9
        0x6D => Some(b"\x1b[21~".to_vec()),        // F10
        0x67 => Some(b"\x1b[23~".to_vec()),        // F11
        0x6F => Some(b"\x1b[24~".to_vec()),        // F12
        _ => None,
    };
    if let Some(mut bytes) = named {
        // Alt/Meta prefixes the sequence with ESC, the usual xterm convention.
        if alt {
            let mut v = vec![0x1b];
            v.append(&mut bytes);
            return Some(v);
        }
        return Some(bytes);
    }

    let ch = chars.chars().next()?;
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

// ── the public view ─────────────────────────────────────────────────────────────────────────

/// The terminal view: a tab strip over the grid. Lives on the main window while a headless clone
/// is selected.
pub struct TerminalView {
    container: Retained<NSView>,
    grid: Retained<GridView>,
    tabs: Retained<NSSegmentedControl>,
    state: Rc<State>,
}

impl TerminalView {
    pub fn new(mtm: MainThreadMarker, cb: TermCallbacks, frame: NSRect) -> Self {
        let metrics = cell_metrics();
        let state = Rc::new(State {
            sessions: RefCell::new(Vec::new()),
            active: Cell::new(0),
            terms: RefCell::new(HashMap::new()),
            cb,
            theme: dark_theme(),
            metrics: Cell::new(metrics),
            grid: Cell::new((INIT_COLS, INIT_ROWS)),
            last_sent: Cell::new((0, 0)),
            resize_at: Cell::new(None),
        });

        let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
        container.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );

        let tabs = NSSegmentedControl::initWithFrame(
            NSSegmentedControl::alloc(mtm),
            NSRect::new(
                NSPoint::new(4.0, frame.size.height - TAB_H),
                NSSize::new(frame.size.width - 8.0, TAB_H - 4.0),
            ),
        );
        tabs.setSegmentCount(0);
        tabs.setTrackingMode(NSSegmentSwitchTracking::SelectOne);
        tabs.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin,
        );

        let grid_frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(frame.size.width, frame.size.height - TAB_H),
        );
        let grid = {
            let this = GridView::alloc(mtm)
                .set_ivars(GridIvars { state: RefCell::new(Some(state.clone())) });
            let this: Retained<GridView> = unsafe { msg_send![super(this), initWithFrame: grid_frame] };
            this
        };
        grid.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );

        container.addSubview(&grid);
        container.addSubview(&tabs);
        TerminalView { container, grid, tabs, state }
    }

    pub fn view(&self) -> &NSView {
        &self.container
    }

    pub fn grid_view(&self) -> &GridView {
        &self.grid
    }

    /// Replace the session list, keeping the screens of sessions that are still present so a
    /// re-send of the same list doesn't wipe scrollback.
    pub fn set_sessions(&self, sessions: &[String]) {
        if *self.state.sessions.borrow() == sessions {
            return;
        }
        {
            let mut terms = self.state.terms.borrow_mut();
            terms.retain(|k, _| sessions.contains(k));
            let dims = self.dims();
            for s in sessions {
                terms.entry(s.clone()).or_insert_with(|| Session::new(dims));
            }
        }
        *self.state.sessions.borrow_mut() = sessions.to_vec();
        if self.state.active.get() >= sessions.len() {
            self.state.active.set(sessions.len().saturating_sub(1));
        }
        self.rebuild_tabs();
        self.grid.setNeedsDisplay(true);
    }

    /// Feed raw PTY bytes into one session's screen.
    pub fn feed(&self, session: &str, data: &[u8]) {
        let mut terms = self.state.terms.borrow_mut();
        let Some(sess) = terms.get_mut(session) else { return };
        let Session { term, parser } = sess;
        parser.advance(term, data);
        drop(terms);
        if self.state.active_session().as_deref() == Some(session) {
            self.grid.setNeedsDisplay(true);
        }
    }

    /// Recompute the grid from the view size and report it when it settles (debounced, because a
    /// live window drag would otherwise send a resize per frame).
    pub fn tick(&self) {
        let bounds = self.grid.bounds();
        let (cw, ch) = self.state.metrics.get();
        let pad = cw * PAD_CELLS;
        let cols = (((bounds.size.width - 2.0 * pad) / cw).floor() as usize).max(1);
        let lines = (((bounds.size.height - 2.0 * pad) / ch).floor() as usize).max(1);
        if (cols, lines) != self.state.grid.get() {
            self.state.grid.set((cols, lines));
            let dims = Dims { cols, lines };
            for sess in self.state.terms.borrow_mut().values_mut() {
                sess.term.resize(dims);
            }
            self.state.resize_at.set(Some(Instant::now()));
            self.grid.setNeedsDisplay(true);
        }
        if let Some(at) = self.state.resize_at.get() {
            if at.elapsed() >= RESIZE_DEBOUNCE {
                self.state.resize_at.set(None);
                let (cols, lines) = self.state.grid.get();
                let size = (cols as u16, lines as u16);
                if size != self.state.last_sent.get() {
                    self.state.last_sent.set(size);
                    (self.state.cb.on_resize)(size.0, size.1);
                }
            }
        }
    }

    /// Called when the tab strip selection changes, including the trailing "+".
    pub fn on_tab_clicked(&self) {
        let sel = self.tabs.selectedSegment();
        let n = self.state.sessions.borrow().len();
        if sel < 0 {
            return;
        }
        if sel as usize >= n {
            // The trailing "+" segment: ask the server for a new session, and put the selection
            // back where it was so the strip doesn't look like it jumped.
            self.rebuild_tabs();
            (self.state.cb.on_new_session)();
            return;
        }
        self.state.active.set(sel as usize);
        self.grid.setNeedsDisplay(true);
    }

    fn dims(&self) -> Dims {
        let (cols, lines) = self.state.grid.get();
        Dims { cols, lines }
    }

    fn rebuild_tabs(&self) {
        let sessions = self.state.sessions.borrow();
        let count = sessions.len() + 1; // + the trailing "+"
        self.tabs.setSegmentCount(count as isize);
        for (i, s) in sessions.iter().enumerate() {
            self.tabs.setLabel_forSegment(&NSString::from_str(s), i as isize);
        }
        self.tabs.setLabel_forSegment(&NSString::from_str("+"), (count - 1) as isize);
        let active = self.state.active.get().min(sessions.len().saturating_sub(1));
        self.tabs.setSelectedSegment(active as isize);
    }

    /// The tab strip, so the caller can wire its target/action.
    pub fn tabs(&self) -> &NSSegmentedControl {
        &self.tabs
    }
}

#[cfg(test)]
mod tests {
    use super::encode_key;

    #[test]
    fn named_keys_encode_to_their_sequences() {
        assert_eq!(encode_key(0x24, "\r", false, false, false), Some(vec![b'\r']), "Return");
        assert_eq!(encode_key(0x33, "", false, false, false), Some(vec![0x7f]), "Backspace is DEL");
        assert_eq!(encode_key(0x35, "", false, false, false), Some(vec![0x1b]), "Escape");
    }

    /// Application-cursor mode swaps CSI for SS3 — getting this wrong breaks arrow keys in vim
    /// and every full-screen TUI.
    #[test]
    fn arrows_follow_application_cursor_mode() {
        assert_eq!(encode_key(0x7E, "", false, false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(0x7E, "", false, false, true), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn control_letters_become_control_codes() {
        assert_eq!(encode_key(0x08, "c", true, false, false), Some(vec![0x03]), "Ctrl+C");
        assert_eq!(encode_key(0x00, "a", true, false, false), Some(vec![0x01]), "Ctrl+A");
        assert_eq!(encode_key(0x31, " ", true, false, false), Some(vec![0x00]), "Ctrl+Space is NUL");
    }

    /// Alt/Meta prefixes with ESC, both for plain characters and for named sequences.
    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(encode_key(0x00, "a", false, true, false), Some(vec![0x1b, b'a']));
        assert_eq!(encode_key(0x7E, "", false, true, false), Some(b"\x1b\x1b[A".to_vec()));
    }

    #[test]
    fn plain_text_passes_through_as_utf8() {
        assert_eq!(encode_key(0x00, "a", false, false, false), Some(vec![b'a']));
        assert_eq!(encode_key(0x00, "é", false, false, false), Some("é".as_bytes().to_vec()));
    }

    /// A bare modifier press produces no characters and must send nothing.
    #[test]
    fn keys_with_no_characters_send_nothing() {
        assert_eq!(encode_key(0x3B, "", false, false, false), None);
    }
}
