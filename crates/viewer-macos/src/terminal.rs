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
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSPasteboard, NSPasteboardTypeString, NSSegmentedControl, NSSegmentSwitchTracking,
    NSStrikethroughStyleAttributeName, NSUnderlineStyleAttributeName, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString,
};

use viewer_core::terminal::{arrow, base_button, bold_bright, dark_theme, mouse_report, resolve, Rgb3, Theme};

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

/// The terminal's event sink. `PtyWrite` is the emulator answering a query the remote app sent —
/// Primary/Secondary DA, DSR/CPR, DECRQM, XTVERSION, the OSC 10/11 colour reports — and those
/// replies have to reach the PTY or the asker just times out: tmux probes its outer terminal on
/// attach, ncurses uses `u7`/`u9`, and prompt frameworks measure themselves with CPR. OSC 52
/// clipboard-stores are honoured by writing the general pasteboard, the same path ⌘C takes.
/// Everything else (title, bell) has no surface here and is dropped.
///
/// One proxy per session, carrying its own name, so a reply is routed to the session that asked
/// even when the user has since switched tabs.
struct EventProxy {
    session: String,
    on_input: InputCb,
    /// Injected rather than called directly, so the routing is testable without a pasteboard.
    set_clipboard: Rc<dyn Fn(String)>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(text) => (self.on_input)(&self.session, text.into_bytes()),
            AlacEvent::ClipboardStore(_, text) => (self.set_clipboard)(text),
            other => tracing::trace!("terminal event ignored: {other:?}"),
        }
    }
}

/// One tmux session's screen.
struct Session {
    term: Term<EventProxy>,
    parser: Processor,
}

impl Session {
    fn new(name: &str, dims: Dims, on_input: InputCb) -> Self {
        let proxy = EventProxy {
            session: name.to_string(),
            on_input,
            set_clipboard: Rc::new(|text: String| set_pasteboard_string(&text)),
        };
        Session {
            term: Term::new(TermConfig::default(), &dims, proxy),
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
    /// Set by the "+" tab: focus whichever session appears next, since the server creates it
    /// asynchronously and only tells us via the next `ViewSpec`.
    focus_new: Cell<bool>,
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
    /// Sub-notch scroll delta not yet turned into a notch (see [`scroll_notches`]).
    scroll_rem: Cell<f64>,
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

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &objc2_app_kit::NSEvent) {
            self.mouse(event, true);
        }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &objc2_app_kit::NSEvent) {
            self.mouse(event, false);
        }
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &objc2_app_kit::NSEvent) {
            self.mouse(event, true);
        }
        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &objc2_app_kit::NSEvent) {
            self.mouse(event, false);
        }
        // AppKit routes every button past left and right through `otherMouse*`; without these the
        // middle button never reaches an app that asked for mouse reports.
        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &objc2_app_kit::NSEvent) {
            self.mouse(event, true);
        }
        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &objc2_app_kit::NSEvent) {
            self.mouse(event, false);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &objc2_app_kit::NSEvent) {
            let Some(state) = self.state() else { return };
            let Some(key) = state.active_session() else { return };
            let (point, side, _, _) = self.locate(&state, event);
            let mut terms = state.terms.borrow_mut();
            let Some(sess) = terms.get_mut(&key) else { return };
            // An app that grabbed the mouse gets the motion; otherwise we are extending a
            // selection. Shift overrides the grab here too, or the selection a Shift-click just
            // started could never grow past its first cell.
            let shift = event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Shift);
            let grabbed = sess.term.mode().intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION);
            if grabbed && !shift {
                return;
            }
            if let Some(sel) = sess.term.selection.as_mut() {
                sel.update(point, side);
                drop(terms);
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &objc2_app_kit::NSEvent) {
            let Some(state) = self.state() else { return };
            let Some(key) = state.active_session() else { return };
            let dy = event.scrollingDeltaY();
            if dy == 0.0 {
                return;
            }
            let (_, ch) = state.metrics.get();
            // Everything below counts in notches, because that is the unit the two consumers
            // disagree on: an app in mouse-reporting mode wants one report per notch, while
            // scrollback and the alternate screen move three lines for that same turn. Deriving
            // both from one count is what keeps a tmux pane scrolling at the GTK viewer's speed.
            let precise = event.hasPreciseScrollingDeltas();
            let notches = scroll_notches(&self.ivars().scroll_rem, precise, dy, ch);
            if notches == 0 {
                return;
            }
            let lines = notches * LINES_PER_NOTCH;
            // `locate` reads the term map itself, so it has to run *before* we borrow that map —
            // doing it inside the borrow is a `BorrowMutError` and, inside an ObjC method, an
            // abort. The mouse handlers above take the same order for the same reason; the cost on
            // a plain scrollback tick is one point conversion.
            let (_, _, col, row) = self.locate(&state, event);
            let shift = event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Shift);
            let mut terms = state.terms.borrow_mut();
            let Some(sess) = terms.get_mut(&key) else { return };
            let mode = *sess.term.mode();
            // Shift always means "scroll my scrollback", overriding whatever the app asked for —
            // otherwise a tmux pane with `mouse on` leaves no way to look back.
            if mode.intersects(TermMode::MOUSE_MODE) && !shift {
                // The app wants wheel events itself (codes 64/65), not scrollback. One report per
                // notch: the app decides for itself how far a notch scrolls its pane, so sending
                // three would scroll it three times as far as every other terminal does.
                drop(terms);
                let code = if notches > 0 { 64 } else { 65 };
                for _ in 0..notches.abs() {
                    state.send(mouse_report(code, col, row, true, mode));
                }
                return;
            }
            if mode.contains(TermMode::ALT_SCREEN) && !shift {
                // The alternate screen has no scrollback of ours to move, so the wheel is
                // synthesized as arrow keys — that is what makes `less` and `man` scroll.
                drop(terms);
                let app = mode.contains(TermMode::APP_CURSOR);
                let seq = if lines > 0 { arrow(b'A', app) } else { arrow(b'B', app) };
                for _ in 0..lines.abs() {
                    state.send(seq.clone());
                }
                return;
            }
            sess.term.scroll_display(Scroll::Delta(lines));
            drop(terms);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &objc2_app_kit::NSEvent) {
            let Some(state) = self.state() else { return };
            let mods = event.modifierFlags();
            let cmd = mods.contains(objc2_app_kit::NSEventModifierFlags::Command);
            let ctrl = mods.contains(objc2_app_kit::NSEventModifierFlags::Control);
            let alt = mods.contains(objc2_app_kit::NSEventModifierFlags::Option);
            let shift = mods.contains(objc2_app_kit::NSEventModifierFlags::Shift);
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
            let Some(key) = state.active_session() else { return };

            // Shift + the paging keys drive our scrollback instead of reaching the app, which is
            // the only way to read history that a full-screen app would otherwise swallow.
            if shift {
                let scroll = match kvk {
                    0x74 => Some(Scroll::PageUp),
                    0x79 => Some(Scroll::PageDown),
                    0x73 => Some(Scroll::Top),
                    0x77 => Some(Scroll::Bottom),
                    _ => None,
                };
                if let Some(s) = scroll {
                    if let Some(sess) = state.terms.borrow_mut().get_mut(&key) {
                        sess.term.scroll_display(s);
                    }
                    self.setNeedsDisplay(true);
                    return;
                }
            }

            let app_cursor = {
                let terms = state.terms.borrow();
                terms.get(&key).map(|t| t.term.mode().contains(TermMode::APP_CURSOR)).unwrap_or(false)
            };
            let chars = event.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
            if let Some(bytes) = encode_key(kvk, &chars, ctrl, alt, shift, app_cursor) {
                // Typing means the user is done reading history, so snap back to the prompt —
                // otherwise the keystroke goes to a screen they cannot see.
                let scrolled = {
                    let mut terms = state.terms.borrow_mut();
                    terms.get_mut(&key).is_some_and(|sess| {
                        let was = sess.term.grid().display_offset() != 0;
                        sess.term.scroll_display(Scroll::Bottom);
                        was
                    })
                };
                if scrolled {
                    self.setNeedsDisplay(true);
                }
                state.send(bytes);
            }
        }
    }
);

impl GridView {
    fn state(&self) -> Option<Rc<State>> {
        self.ivars().state.borrow().clone()
    }

    /// Map a mouse event to a grid `Point` + `Side`, plus the on-screen (col, row). The view is
    /// flipped, so `locationInWindow` converts to a top-left origin directly.
    fn locate(
        &self,
        state: &Rc<State>,
        event: &objc2_app_kit::NSEvent,
    ) -> (Point, Side, usize, usize) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        let (cw, ch) = state.metrics.get();
        let (cols, lines) = state.grid.get();
        // Undo the inset applied when rendering, so clicks land on the cell they look like.
        let pad = cw * PAD_CELLS;
        let colf = ((p.x - pad) / cw).max(0.0);
        let col = (colf.floor() as usize).min(cols.saturating_sub(1));
        let row = (((p.y - pad) / ch).floor() as i64).clamp(0, lines.saturating_sub(1) as i64) as usize;
        let side = if colf.fract() < 0.5 { Side::Left } else { Side::Right };
        let offset = state
            .terms
            .borrow()
            .get(&state.active_session().unwrap_or_default())
            .map(|s| s.term.grid().display_offset() as i32)
            .unwrap_or(0);
        (Point::new(Line(row as i32 - offset), Column(col)), side, col, row)
    }

    /// A button press or release: mouse reporting when the app asked for it, else selection.
    fn mouse(&self, event: &objc2_app_kit::NSEvent, pressed: bool) {
        let Some(state) = self.state() else { return };
        let Some(key) = state.active_session() else { return };
        let Some(code) = xterm_button(event.buttonNumber()) else { return };
        let shift = event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Shift);
        let clicks = event.clickCount();
        let (point, side, col, row) = self.locate(&state, event);
        let mut terms = state.terms.borrow_mut();
        let Some(sess) = terms.get_mut(&key) else { return };
        let mode = *sess.term.mode();
        // Shift overrides the app's mouse grab, so text stays selectable inside a tmux pane
        // running `mouse on` — without it there is no way to copy anything out of one.
        if mode.intersects(TermMode::MOUSE_MODE) && !shift {
            drop(terms);
            state.send(mouse_report(code, col, row, pressed, mode));
            return;
        }
        // Only the left button starts a selection: a right- or middle-click otherwise collapses
        // the selection the user just made. The GTK viewer pastes PRIMARY on a middle-click, but
        // macOS has no PRIMARY selection, so the middle button here only ever reports.
        if pressed && code == 0 {
            // Click count picks the granularity, the way every terminal does: double for a word,
            // triple for the line. The drag handler extends whichever we started.
            let ty = match clicks {
                2 => SelectionType::Semantic,
                n if n >= 3 => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            sess.term.selection = Some(Selection::new(ty, point, side));
        }
        drop(terms);
        if pressed {
            self.window().map(|w| w.makeFirstResponder(Some(self)));
        }
        self.setNeedsDisplay(true);
    }

    fn paste(&self) {
        let Some(state) = self.state() else { return };
        let pb = NSPasteboard::generalPasteboard();
        let Some(text) = pb.stringForType(unsafe { NSPasteboardTypeString }) else { return };
        // A terminal's Return is CR; pasting the LF the pasteboard actually holds leaves readline
        // and every line-based shell waiting for an end of line that never comes. Normalizing
        // outside the bracketed wrapper matters too — bracketed paste changes how the app reads
        // the bytes, not which byte ends a line.
        let s = text.to_string().replace('\n', "\r");
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
        set_pasteboard_string(&text);
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
            // The run key has to carry every flag `draw_text` acts on, or a decorated cell would
            // be merged into an undecorated run and lose its decoration.
            let style = flags & (TEXT_STYLE_FLAGS | Flags::BOLD | Flags::ITALIC);
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

/// Put `text` on the general pasteboard — the single write path for both ⌘C and the emulator's
/// OSC 52 clipboard-store. [`crate::clipboard`] owns the pasteboard ⇄ server bridge and writes it
/// the same way; it will notice the resulting `changeCount` bump on its next tick and offer the
/// text to the server, exactly as it does for a ⌘C, so a remote OSC 52 propagates like a local
/// copy instead of stopping at this viewer.
fn set_pasteboard_string(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    // SAFETY: `NSPasteboardTypeString` is the documented AppKit pasteboard-type global.
    let ty = unsafe { NSPasteboardTypeString };
    pb.setString_forType(&NSString::from_str(text), ty);
}

/// Fill `rect` with `color`.
fn fill_rect(rect: NSRect, color: Rgb3) {
    ns_color(color).setFill();
    NSBezierPath::fillRect(rect);
}

/// The cell flags `draw_text` turns into text attributes. AppKit draws one underline style, so
/// the double and curly variants fold into a plain underline — the same simplification the GTK
/// viewer makes, and far better than the nothing they render as otherwise.
const TEXT_STYLE_FLAGS: Flags = Flags::UNDERLINE
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::STRIKEOUT);

/// Draw `text` with the terminal font at `origin` (top-left, the view being flipped).
fn draw_text(text: &str, origin: NSPoint, font: &NSFont, fg: Rgb3, flags: Flags) {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let _ = mtm;
    let mut keys: Vec<&objc2_foundation::NSString> = Vec::with_capacity(4);
    let mut vals: Vec<&AnyObject> = Vec::with_capacity(4);
    let color = ns_color(fg);
    // SAFETY: these are the documented AppKit attribute-name globals.
    unsafe {
        keys.push(NSFontAttributeName);
        vals.push(&*(font as *const NSFont as *const AnyObject));
        keys.push(NSForegroundColorAttributeName);
        vals.push(&*(&*color as *const NSColor as *const AnyObject));
    }
    let single = NSNumber::new_i32(1);
    if flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE | Flags::UNDERCURL) {
        // SAFETY: the documented AppKit attribute-name global.
        unsafe {
            keys.push(NSUnderlineStyleAttributeName);
            vals.push(&*(&*single as *const NSNumber as *const AnyObject));
        }
    }
    if flags.contains(Flags::STRIKEOUT) {
        // SAFETY: the documented AppKit attribute-name global.
        unsafe {
            keys.push(NSStrikethroughStyleAttributeName);
            vals.push(&*(&*single as *const NSNumber as *const AnyObject));
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

/// The xterm button code for an `NSEvent.buttonNumber`, or `None` for a button the terminal has
/// nothing to say about.
///
/// AppKit numbers buttons 0=left, **1=right**, 2=middle, while xterm's low two bits run 0=left,
/// 1=middle, 2=right — the two disagree on exactly right vs middle, so feeding a button number
/// straight to [`base_button`] reports a right-click as a middle-click. [`crate::window`]'s
/// `evdev_button` translates the same AppKit numbering for the video plane; going through
/// `base_button` here keeps the xterm codes themselves in `viewer-core`.
fn xterm_button(ns_button: isize) -> Option<u8> {
    let gtk = match ns_button {
        0 => 1, // left
        1 => 3, // right
        2 => 2, // middle
        // Back/forward have no xterm encoding; reporting them as a left click would be worse
        // than dropping them.
        _ => return None,
    };
    Some(base_button(gtk))
}

/// Lines one wheel notch moves — the step terminals have used for as long as wheels have
/// existed, and the one the GTK viewer sends.
const LINES_PER_NOTCH: i32 = 3;

/// Turn one scroll event into whole wheel notches, carrying the rest in `rem`.
///
/// A wheel (`precise` false) reports a notch at a time, so only its direction matters. A trackpad
/// reports points, and a slow two-finger drag is a small fraction of a notch per event, so the
/// delta is scaled to notches and truncated toward zero with the remainder kept — rounding each
/// event on its own would leave that drag frozen. Scaling by the notch rather than the cell costs
/// the drag no distance, since the caller multiplies back by [`LINES_PER_NOTCH`]; it only makes
/// the finger travel a notch before anything moves. [`crate::window`]'s `wheel_notches` carries
/// the same way for the video plane, on its own remainder.
fn scroll_notches(rem: &Cell<f64>, precise: bool, dy: f64, cell_h: f64) -> i32 {
    if !precise {
        return dy.signum() as i32;
    }
    let acc = rem.get() + dy / (cell_h * f64::from(LINES_PER_NOTCH));
    let notches = acc.trunc() as i32;
    rem.set(acc - f64::from(notches));
    notches
}

/// Encode a macOS key press into terminal input bytes. `kvk` is the Carbon virtual key and
/// `chars` the characters ignoring modifiers; `None` means "not a key the terminal sends".
fn encode_key(
    kvk: u32,
    chars: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let named: Option<Vec<u8>> = match kvk {
        0x24 | 0x4C => Some(vec![b'\r']),          // Return / KeypadEnter
        0x33 => Some(vec![0x7f]),                  // Delete (backspace)
        // Shift+Tab is back-tab (CBT), which is how readline and every TUI walk a completion
        // list or a field order backwards; plain TAB would just complete forwards again.
        0x30 if shift => Some(b"\x1b[Z".to_vec()), // Shift+Tab
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
            focus_new: Cell::new(false),
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
                .set_ivars(GridIvars {
                    state: RefCell::new(Some(state.clone())),
                    ..GridIvars::default()
                });
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
        let previous = self.state.sessions.borrow().clone();
        if previous == sessions {
            return;
        }
        {
            let mut terms = self.state.terms.borrow_mut();
            terms.retain(|k, _| sessions.contains(k));
            let dims = self.dims();
            let on_input = self.state.cb.on_input.clone();
            for s in sessions {
                terms
                    .entry(s.clone())
                    .or_insert_with(|| Session::new(s, dims, on_input.clone()));
            }
        }
        // The "+" asked for a session and one has appeared: focus it, the way every tab UI does.
        let appeared = sessions.iter().position(|s| !previous.contains(s));
        *self.state.sessions.borrow_mut() = sessions.to_vec();
        match appeared {
            Some(i) if self.state.focus_new.replace(false) => self.state.active.set(i),
            _ if self.state.active.get() >= sessions.len() => {
                self.state.active.set(sessions.len().saturating_sub(1))
            }
            _ => {}
        }
        // Re-announce our grid size for the new session list. A session that existed before we
        // attached is proxied only once the server has a client for it, which happens after our
        // first resize went out — so that resize landed on nothing and the session kept the width
        // it was created at, and its output arrived too wide and got clipped. Clearing `last_sent`
        // forces the debounced send to repeat rather than dedupe itself away.
        self.state.last_sent.set((0, 0));
        self.state.resize_at.set(Some(Instant::now()));
        tracing::info!(
            "terminal: {} session(s) {:?}, active {}",
            sessions.len(),
            sessions,
            self.state.active.get()
        );
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
                    tracing::info!("terminal: reporting grid {}x{}", size.0, size.1);
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
            // back where it was so the strip doesn't look like it jumped until the session lands.
            self.rebuild_tabs();
            self.state.focus_new.set(true);
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
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use alacritty_terminal::event::{Event as AlacEvent, EventListener};
    use alacritty_terminal::term::ClipboardType;

    use super::{encode_key, scroll_notches, xterm_button, EventProxy, LINES_PER_NOTCH};

    /// What the proxy's sinks saw: PTY writes as (session, bytes), and clipboard texts.
    #[derive(Default)]
    struct Recorder {
        pty: RefCell<Vec<(String, Vec<u8>)>>,
        clip: RefCell<Vec<String>>,
    }

    /// A proxy whose sinks record instead of reaching the socket or the pasteboard.
    fn recording_proxy() -> (EventProxy, Rc<Recorder>) {
        let rec = Rc::new(Recorder::default());
        let proxy = EventProxy {
            session: "work".to_string(),
            on_input: {
                let rec = rec.clone();
                Rc::new(move |s: &str, b: Vec<u8>| rec.pty.borrow_mut().push((s.to_string(), b)))
            },
            set_clipboard: {
                let rec = rec.clone();
                Rc::new(move |t: String| rec.clip.borrow_mut().push(t))
            },
        };
        (proxy, rec)
    }

    /// The emulator answers Device Attributes, DSR/CPR, DECRQM and the colour queries with a
    /// `PtyWrite`. Dropping those leaves tmux (which probes its outer terminal on attach) and
    /// ncurses (`u7`/`u9`) waiting for a reply that never arrives, so the reply must reach the
    /// PTY — tagged with the session that asked, not whichever tab happens to be in front.
    #[test]
    fn pty_writes_reach_the_asking_session() {
        let (proxy, rec) = recording_proxy();
        proxy.send_event(AlacEvent::PtyWrite("\x1b[?6c".to_string()));
        assert_eq!(rec.pty.borrow().as_slice(), [("work".to_string(), b"\x1b[?6c".to_vec())]);
    }

    #[test]
    fn clipboard_stores_are_honoured_and_other_events_dropped() {
        let (proxy, rec) = recording_proxy();
        proxy.send_event(AlacEvent::ClipboardStore(ClipboardType::Clipboard, "copied".to_string()));
        proxy.send_event(AlacEvent::Bell);
        proxy.send_event(AlacEvent::Title("ignored".to_string()));
        assert_eq!(rec.clip.borrow().as_slice(), ["copied".to_string()]);
        assert!(rec.pty.borrow().is_empty(), "only PtyWrite goes to the PTY");
    }

    #[test]
    fn named_keys_encode_to_their_sequences() {
        assert_eq!(encode_key(0x24, "\r", false, false, false, false), Some(vec![b'\r']), "Return");
        assert_eq!(encode_key(0x33, "", false, false, false, false), Some(vec![0x7f]), "Backspace is DEL");
        assert_eq!(encode_key(0x35, "", false, false, false, false), Some(vec![0x1b]), "Escape");
    }

    /// Application-cursor mode swaps CSI for SS3 — getting this wrong breaks arrow keys in vim
    /// and every full-screen TUI.
    #[test]
    fn arrows_follow_application_cursor_mode() {
        assert_eq!(encode_key(0x7E, "", false, false, false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(0x7E, "", false, false, false, true), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn control_letters_become_control_codes() {
        assert_eq!(encode_key(0x08, "c", true, false, false, false), Some(vec![0x03]), "Ctrl+C");
        assert_eq!(encode_key(0x00, "a", true, false, false, false), Some(vec![0x01]), "Ctrl+A");
        assert_eq!(
            encode_key(0x31, " ", true, false, false, false),
            Some(vec![0x00]),
            "Ctrl+Space is NUL"
        );
    }

    /// Alt/Meta prefixes with ESC, both for plain characters and for named sequences.
    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(encode_key(0x00, "a", false, true, false, false), Some(vec![0x1b, b'a']));
        assert_eq!(encode_key(0x7E, "", false, true, false, false), Some(b"\x1b\x1b[A".to_vec()));
    }

    #[test]
    fn plain_text_passes_through_as_utf8() {
        assert_eq!(encode_key(0x00, "a", false, false, false, false), Some(vec![b'a']));
        assert_eq!(encode_key(0x00, "é", false, false, false, false), Some("é".as_bytes().to_vec()));
    }

    /// A bare modifier press produces no characters and must send nothing.
    #[test]
    fn keys_with_no_characters_send_nothing() {
        assert_eq!(encode_key(0x3B, "", false, false, false, false), None);
    }

    /// Shift+Tab is back-tab, not another forward tab: sending TAB makes a completion menu or a
    /// field order walk the wrong way, with no way to go back.
    #[test]
    fn shift_tab_is_back_tab() {
        assert_eq!(encode_key(0x30, "\t", false, false, false, false), Some(vec![b'\t']), "Tab");
        assert_eq!(encode_key(0x30, "\t", false, false, true, false), Some(b"\x1b[Z".to_vec()));
        // Alt still prefixes the back-tab sequence, like every other named key.
        assert_eq!(encode_key(0x30, "\t", false, true, true, false), Some(b"\x1b\x1b[Z".to_vec()));
    }

    /// AppKit's button numbering (0=left, 1=right, 2=middle) disagrees with xterm's
    /// (0=left, 1=middle, 2=right) on exactly the pair that matters, and the mismatch used to
    /// report every right-click as a middle-click — which pastes, in an app that honours it.
    #[test]
    fn appkit_buttons_map_to_their_xterm_codes() {
        assert_eq!(xterm_button(0), Some(0), "left");
        assert_eq!(xterm_button(1), Some(2), "right is xterm 2, not 1");
        assert_eq!(xterm_button(2), Some(1), "middle is xterm 1, not 2");
        assert_eq!(xterm_button(3), None, "back/forward have no xterm encoding");
    }

    /// Cell height used by the scroll tests; one notch is `CH * LINES_PER_NOTCH` points of travel.
    const CH: f64 = 20.0;

    /// One turn of a real wheel is one notch, whichever way it goes — so a mouse-reporting app
    /// gets one report and our scrollback moves three lines. Sending three reports for the notch
    /// scrolled a tmux pane three times as far here as under the GTK viewer.
    #[test]
    fn one_wheel_notch_is_one_report_and_three_lines() {
        let rem = Cell::new(0.0);
        let notches = scroll_notches(&rem, false, 10.0, CH);
        assert_eq!(notches, 1, "one report per notch, not one per line");
        assert_eq!(notches * LINES_PER_NOTCH, 3, "but three lines of scrollback for it");
        assert_eq!(scroll_notches(&rem, false, -10.0, CH), -1, "and the same downwards");
        // A wheel carries nothing between events: its magnitude never reaches the remainder.
        assert_eq!(rem.get(), 0.0);
    }

    /// A flick of the trackpad covering several notches forwards all of them at once, rather than
    /// dribbling them out one event at a time.
    #[test]
    fn a_fast_trackpad_flick_forwards_every_notch_it_covered() {
        let rem = Cell::new(0.0);
        let far = 3.5 * CH * f64::from(LINES_PER_NOTCH);
        assert_eq!(scroll_notches(&rem, true, far, CH), 3);
        let half = 0.5 * CH * f64::from(LINES_PER_NOTCH);
        assert_eq!(scroll_notches(&rem, true, half, CH), 1, "the carried half completes a notch");
    }

    /// A trackpad reports a small fraction of a notch per event; rounding each one alone would
    /// leave a slow two-finger drag frozen, so the remainder has to carry — and it must not fire
    /// early either, or scrolling in mouse-reporting mode outruns the wheel again.
    #[test]
    fn sub_notch_trackpad_deltas_accumulate_into_exactly_one_notch() {
        let rem = Cell::new(0.0);
        let nudge = 0.4 * CH * f64::from(LINES_PER_NOTCH); // two fifths of a notch per event
        assert_eq!(scroll_notches(&rem, true, nudge, CH), 0, "a nudge alone moves nothing");
        assert_eq!(scroll_notches(&rem, true, nudge, CH), 0);
        assert_eq!(scroll_notches(&rem, true, nudge, CH), 1, "three nudges make one notch");
        assert_eq!(scroll_notches(&rem, true, nudge, CH), 0, "and the fourth starts over");
        // Truncation is toward zero, so a drag the other way loses nothing either.
        let rem = Cell::new(0.0);
        assert_eq!(scroll_notches(&rem, true, -nudge, CH), 0);
        assert_eq!(scroll_notches(&rem, true, -nudge, CH), 0);
        assert_eq!(scroll_notches(&rem, true, -nudge, CH), -1);
    }

    /// Reversing mid-drag cancels what the fingers had banked, instead of the leftover from one
    /// direction firing a notch the other way.
    #[test]
    fn a_trackpad_drag_that_reverses_cancels_its_carried_fraction() {
        let rem = Cell::new(0.0);
        let half = 0.5 * CH * f64::from(LINES_PER_NOTCH);
        assert_eq!(scroll_notches(&rem, true, half, CH), 0);
        assert_eq!(scroll_notches(&rem, true, -half, CH), 0);
        assert_eq!(rem.get(), 0.0, "the fingers ended where they started");
    }
}
