//! Windows pointer lock: `ClipCursor` to a 1×1 rectangle + a Raw Input mouse stream.
//!
//! Implements the same public surface as `pointer_lock.rs` (Wayland), using:
//! - `ClipCursor` onto a 1×1 rectangle at the cursor's current position, which pins the
//!   visible cursor in place — the Win32 equivalent of a Wayland locked pointer.
//! - `WM_INPUT` from `RegisterRawInputDevices` (HID generic-desktop / mouse) for unbounded
//!   relative deltas. Raw Input reports the device's own motion **before** the pointer
//!   acceleration curve and before the clip rectangle clamps it, which is exactly what a
//!   grabbed remote app (a game) needs — `WM_MOUSEMOVE` would report nothing at all once the
//!   cursor is pinned.
//! - `NULL` back to `ClipCursor` on release.
//!
//! Delta delivery mirrors the Wayland module exactly:
//! - Wire framing: `[0u8][u32be len][JSON]`
//! - JSON: `{"kind":"pointer_relative","dx":…,"dy":…}`
//!
//! **The local cursor is not hidden here.** The GTK tick in `main.rs` already swaps the video
//! widget's cursor to `none` whenever `is_engaged()` is true, and restores the remote's own
//! cursor texture when it is not. A `ShowCursor(FALSE)` on top would fight GDK's per-window
//! `WM_SETCURSOR` handling and leak a hidden cursor on any path that skips `release`.
//!
//! **Why a message-only window on its own thread.** `WM_INPUT` needs an `HWND` to be delivered
//! to. Taking GTK's own window would mean subclassing GDK's window procedure — chaining
//! `CallWindowProcW`, and unpicking it again on every window teardown (this viewer creates and
//! destroys a window per remote monitor on every layout change). A private `HWND_MESSAGE`
//! window owned by a dedicated thread has none of that coupling: it outlives every GTK window,
//! GDK never sees it, and its `GetMessageW` loop is independent of GTK's main loop. `engage`
//! and `release` stay callable from the GTK thread, as their call sites (signal handlers and
//! the reconcile tick) require.
//!
//! **`RIDEV_INPUTSINK`** makes the registration deliver raw input even while our message-only
//! window is not the foreground window — which it never is, since it is not a real window.

use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use gtk4::gdk;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ClipCursor, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetTimer, SetWindowLongPtrW, TranslateMessage, WM_CLOSE, WM_DESTROY, WM_INPUT, WM_TIMER,
    WNDCLASSW,
};

/// Deliver raw input even when `hwndTarget` is not the foreground window. Our target is a
/// message-only window, which can never be foreground, so this flag is mandatory.
const RIDEV_INPUTSINK: u32 = 0x0000_0100;
/// Stop delivering raw input for this usage page/usage pair. `hwndTarget` must be `NULL`.
const RIDEV_REMOVE: u32 = 0x0000_0001;
/// `GetRawInputData` command: fetch the raw input payload (header + data).
const RID_INPUT: u32 = 0x1000_0003;
/// `RAWINPUTHEADER::dwType` for a mouse packet.
const RIM_TYPEMOUSE: u32 = 0;
/// HID usage page "generic desktop controls", usage "mouse".
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;
/// `RAWMOUSE::usFlags`: `lLastX`/`lLastY` are absolute coordinates, not deltas. Set by
/// absolute-positioning devices — drawing tablets, touch digitisers, and the virtual mouse an
/// RDP or VM console injects. Their motion has to be differenced rather than accumulated.
const MOUSE_MOVE_ABSOLUTE: u16 = 0x01;
/// `SetTimer` id for the clip-rectangle refresh (see [`Ctx::reassert_clip`]).
const TIMER_REASSERT_CLIP: usize = 1;
/// How often to re-apply the clip rectangle while engaged, in milliseconds.
const REASSERT_MS: u32 = 250;
/// Sentinel for "no previous absolute sample yet" in [`Ctx::last_abs_x`] / `last_abs_y`.
const NO_SAMPLE: i32 = i32::MIN;

/// The viewer's input write half (port-1 socket); shared with the GTK thread.
type Writer = Arc<Mutex<Option<TcpStream>>>;

/// Frame one input message to the server: `[0u8][u32be len][json]` (tag 0 = input).
/// Mirrors `pointer_lock.rs` (Wayland) byte-for-byte — same framing, same JSON shape.
fn send_relative(writer: &Writer, dx: f64, dy: f64) {
    let json = format!(r#"{{"kind":"pointer_relative","dx":{dx},"dy":{dy}}}"#);
    if let Some(g) = writer.lock().unwrap().as_mut() {
        let hdr = (json.len() as u32).to_be_bytes();
        let _ = g
            .write_all(&[0u8])
            .and_then(|_| g.write_all(&hdr))
            .and_then(|_| g.write_all(json.as_bytes()));
    }
}

/// State shared between the GTK thread (which flips `engaged` and records where the cursor was
/// pinned) and the raw-input thread (whose window procedure reads both).
///
/// Reached from the window procedure through `GWLP_USERDATA`, and kept alive for the process by
/// the `Arc` the [`PointerLock`] holds — the window procedure only ever borrows it.
struct Ctx {
    writer: Writer,
    /// Whether deltas should be forwarded. Read on every `WM_INPUT`; the raw-input
    /// registration itself stays up for the process's life so engaging costs no syscalls.
    engaged: AtomicBool,
    /// Where the cursor was pinned, so [`Ctx::reassert_clip`] can restore the clip rectangle
    /// after Windows drops it.
    pin_x: AtomicI32,
    pin_y: AtomicI32,
    /// Previous sample from an absolute-positioning device, or [`NO_SAMPLE`]. Only touched
    /// from the raw-input thread.
    last_abs_x: AtomicI32,
    last_abs_y: AtomicI32,
}

impl Ctx {
    /// Pin the cursor by clipping it to a 1×1 rectangle at `(x, y)`.
    fn apply_clip(x: i32, y: i32) -> bool {
        let rect = RECT { left: x, top: y, right: x + 1, bottom: y + 1 };
        // SAFETY: `rect` is a valid, initialised RECT living for the duration of the call.
        unsafe { ClipCursor(&rect) != 0 }
    }

    /// Re-apply the clip rectangle. Windows silently releases a cursor clip on events the
    /// process does not see — another application taking the foreground, a session
    /// lock/unlock, the secure-attention sequence. Without this the viewer would still believe
    /// it held the pointer while the cursor had quietly escaped: motion would be delivered as
    /// relative deltas to the remote *and* move the local cursor across the desktop.
    fn reassert_clip(&self) {
        if self.engaged.load(Ordering::Relaxed) {
            Self::apply_clip(self.pin_x.load(Ordering::Relaxed), self.pin_y.load(Ordering::Relaxed));
        }
    }

    /// Turn one raw mouse packet into a relative delta and forward it.
    fn on_raw_mouse(&self, flags: u16, last_x: i32, last_y: i32) {
        let (dx, dy) = if flags & MOUSE_MOVE_ABSOLUTE != 0 {
            // Absolute device: the payload is a position, so difference consecutive samples.
            // The first sample after engaging establishes the origin and yields no motion —
            // otherwise the remote would receive one enormous jump from the origin.
            let (px, py) =
                (self.last_abs_x.swap(last_x, Ordering::Relaxed), self.last_abs_y.swap(last_y, Ordering::Relaxed));
            if px == NO_SAMPLE || py == NO_SAMPLE {
                return;
            }
            (last_x - px, last_y - py)
        } else {
            (last_x, last_y)
        };
        if dx != 0 || dy != 0 {
            send_relative(&self.writer, f64::from(dx), f64::from(dy));
        }
    }
}

/// Window procedure for the message-only raw-input window.
///
/// # Safety
/// Called by the Win32 message pump with the arguments it documents. The `Ctx` pointer stashed
/// in `GWLP_USERDATA` is set once, immediately after `CreateWindowExW` and before the message
/// loop starts, and the `Arc` it came from outlives the window (the [`PointerLock`] holds a
/// clone and destroys the window in `Drop`), so dereferencing it here is sound.
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctx = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Ctx;
    if !ctx.is_null() {
        let ctx = unsafe { &*ctx };
        match msg {
            WM_INPUT => {
                // Not named `raw`: in edition 2024 `&raw` is the raw-borrow prefix, so
                // `&raw ...` at a use site is at best confusing and at worst ambiguous.
                let mut packet: RAWINPUT = unsafe { std::mem::zeroed() };
                let mut size = std::mem::size_of::<RAWINPUT>() as u32;
                // SAFETY: `lparam` is the HRAWINPUT the message carries; `packet`/`size` are a
                // correctly sized, writable output buffer and its length in bytes.
                let read = unsafe {
                    GetRawInputData(
                        lparam as HRAWINPUT,
                        RID_INPUT,
                        (&mut packet as *mut RAWINPUT).cast(),
                        &mut size,
                        std::mem::size_of::<RAWINPUTHEADER>() as u32,
                    )
                };
                // `GetRawInputData` returns (u32)-1 on failure and the byte count otherwise.
                if read != u32::MAX && packet.header.dwType == RIM_TYPEMOUSE {
                    // Read the packet unconditionally — an absolute device must keep its
                    // origin tracked even while released, or the first delta after engaging
                    // would be a jump from wherever the pointer was when the lock last ended.
                    // SAFETY: the union's `mouse` arm is the live one when dwType is
                    // RIM_TYPEMOUSE, which was just checked.
                    let mouse = unsafe { packet.data.mouse };
                    if ctx.engaged.load(Ordering::Relaxed) {
                        ctx.on_raw_mouse(mouse.usFlags, mouse.lLastX, mouse.lLastY);
                    } else {
                        ctx.last_abs_x.store(NO_SAMPLE, Ordering::Relaxed);
                        ctx.last_abs_y.store(NO_SAMPLE, Ordering::Relaxed);
                    }
                }
                // WM_INPUT must still reach DefWindowProcW so the system can clean the packet up.
            }
            WM_TIMER if wparam == TIMER_REASSERT_CLIP => {
                ctx.reassert_clip();
                return 0;
            }
            _ => {}
        }
    }
    match msg {
        WM_CLOSE => {
            // SAFETY: `hwnd` is this window, still valid inside its own window procedure.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's GetMessageW loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: forwarding the message the pump handed us, unmodified.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// `GetWindowLongPtrW`/`SetWindowLongPtrW` index for the per-window user pointer.
const GWLP_USERDATA: i32 = -21;

pub struct PointerLock {
    ctx: Arc<Ctx>,
    /// The message-only window, as a plain integer so this struct stays `Send`-agnostic —
    /// `HWND` is a raw pointer. Only used to post `WM_CLOSE` from `Drop`.
    hwnd: usize,
}

impl PointerLock {
    /// Set up the Windows pointer lock: start the raw-input thread and register for mouse
    /// packets. Returns `None` when `RMNG_NO_POINTER_LOCK=1`, or when the window or the raw
    /// input registration cannot be created — in which case the viewer keeps its ordinary
    /// absolute-pointer behaviour, exactly as on a compositor without the Wayland protocols.
    pub fn new(_display: &gdk::Display, writer: Writer) -> Option<Self> {
        if std::env::var_os("RMNG_NO_POINTER_LOCK").is_some() {
            return None;
        }
        let ctx = Arc::new(Ctx {
            writer,
            engaged: AtomicBool::new(false),
            pin_x: AtomicI32::new(0),
            pin_y: AtomicI32::new(0),
            last_abs_x: AtomicI32::new(NO_SAMPLE),
            last_abs_y: AtomicI32::new(NO_SAMPLE),
        });

        // The window must be created on the thread that pumps its messages, so the thread
        // reports back whether it got one (and which) before `new` decides to succeed.
        let (tx, rx) = std::sync::mpsc::channel::<Option<usize>>();
        let thread_ctx = ctx.clone();
        std::thread::Builder::new()
            .name("rmng-win-rawptr".into())
            .spawn(move || raw_input_thread(thread_ctx, tx))
            .ok()?;

        let hwnd = match rx.recv() {
            Ok(Some(h)) => h,
            _ => {
                tracing::warn!(
                    "Windows pointer lock: raw-input window setup failed; pointer lock disabled \
                     (the viewer keeps absolute-pointer behaviour)"
                );
                return None;
            }
        };
        tracing::info!("Windows pointer lock ready (Ctrl+Alt+G toggles, Ctrl+Alt+P releases)");
        Some(PointerLock { ctx, hwnd })
    }

    pub fn is_engaged(&self) -> bool {
        self.ctx.engaged.load(Ordering::Relaxed)
    }

    /// Pin the cursor where it stands and start forwarding relative deltas.
    ///
    /// `surface` is unused: the clip rectangle is a screen-space, process-wide resource, and
    /// raw input is delivered to our own window regardless of which GTK window has focus. The
    /// argument stays for parity with the Wayland twin, whose constraint is per-surface.
    pub fn engage(&self, _surface: &gdk::Surface) {
        if self.ctx.engaged.load(Ordering::Relaxed) {
            return;
        }
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: `pt` is a valid, writable POINT.
        if unsafe { GetCursorPos(&mut pt) } == 0 {
            tracing::warn!("Windows pointer lock: GetCursorPos failed; pointer lock NOT engaged");
            return;
        }
        if !Ctx::apply_clip(pt.x, pt.y) {
            // ClipCursor fails when the process does not own the foreground window. Engaging
            // anyway would forward deltas while the cursor still roamed the local desktop.
            tracing::warn!(
                "Windows pointer lock: ClipCursor failed (not the foreground process?); \
                 pointer lock NOT engaged"
            );
            return;
        }
        self.ctx.pin_x.store(pt.x, Ordering::Relaxed);
        self.ctx.pin_y.store(pt.y, Ordering::Relaxed);
        self.ctx.last_abs_x.store(NO_SAMPLE, Ordering::Relaxed);
        self.ctx.last_abs_y.store(NO_SAMPLE, Ordering::Relaxed);
        self.ctx.engaged.store(true, Ordering::Relaxed);
        tracing::info!("Windows pointer lock engaged");
    }

    /// Release the cursor. Idempotent when not engaged.
    pub fn release(&self) {
        if !self.ctx.engaged.swap(false, Ordering::Relaxed) {
            return;
        }
        // SAFETY: a null rectangle is the documented "remove the clip" argument.
        if unsafe { ClipCursor(std::ptr::null()) } == 0 {
            tracing::warn!("Windows pointer lock: ClipCursor(NULL) failed; cursor may stay pinned");
        }
        tracing::info!("Windows pointer lock released");
    }
}

impl Drop for PointerLock {
    fn drop(&mut self) {
        self.release();
        // SAFETY: posting to a window this type created; the raw-input thread destroys it and
        // ends its own message loop. A stale HWND makes PostMessageW fail harmlessly.
        unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
    }
}

/// Body of the raw-input thread: create the message-only window, register for mouse packets,
/// then pump messages until the window is destroyed. Reports the window handle (or `None` on
/// failure) back to [`PointerLock::new`] before entering the loop.
fn raw_input_thread(ctx: Arc<Ctx>, tx: std::sync::mpsc::Sender<Option<usize>>) {
    // UTF-16, NUL-terminated: every `*W` Win32 entry point wants a wide string.
    let class_name: Vec<u16> = "RmngViewerRawInput\0".encode_utf16().collect();

    // SAFETY: the class name outlives the RegisterClassW call and every window created from
    // it (the window is destroyed before this function returns). A duplicate registration
    // returns 0, which the CreateWindowExW below then reports as a failure.
    let atom = unsafe {
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.lpszClassName = class_name.as_ptr();
        RegisterClassW(&wc)
    };
    if atom == 0 {
        let _ = tx.send(None);
        return;
    }

    // HWND_MESSAGE: a message-only window. It is never displayed, never enumerated, and never
    // becomes foreground — it exists purely as a WM_INPUT delivery target.
    // SAFETY: the class was just registered; all other arguments are the documented "no style,
    // no size, no parent, no menu, no instance, no creation parameter" nulls.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let _ = tx.send(None);
        return;
    }

    // Publish the context before any message can reach the window procedure: nothing has
    // pumped this thread's queue yet, so no WM_INPUT can have been dispatched.
    // SAFETY: `hwnd` is this thread's window; the Arc outlives it (see `wnd_proc`'s safety note).
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::as_ptr(&ctx) as isize) };

    let rid = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    // SAFETY: one correctly sized RAWINPUTDEVICE, and its size in bytes.
    let registered = unsafe {
        RegisterRawInputDevices(&rid, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) != 0
    };
    if !registered {
        // SAFETY: destroying the window this thread just created.
        unsafe { DestroyWindow(hwnd) };
        let _ = tx.send(None);
        return;
    }

    // Refresh the clip rectangle periodically — see `Ctx::reassert_clip` for why it can vanish.
    // SAFETY: `hwnd` is valid; a NULL callback routes the tick to the window procedure as
    // WM_TIMER, which is what `wnd_proc` handles.
    unsafe { SetTimer(hwnd, TIMER_REASSERT_CLIP, REASSERT_MS, None) };

    let _ = tx.send(Some(hwnd as usize));

    // SAFETY: standard Win32 message pump. GetMessageW returns 0 at WM_QUIT (posted by
    // WM_DESTROY) and -1 on error; both end the loop.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let got = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if got <= 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Stop raw-input delivery before the window goes away. `RIDEV_REMOVE` requires a null
    // target; leaving a registration pointing at a destroyed window is what makes a later
    // re-registration in the same process fail.
    let rid_remove = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: std::ptr::null_mut(),
    };
    // SAFETY: same shape as the registration above.
    unsafe { RegisterRawInputDevices(&rid_remove, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) };
}
