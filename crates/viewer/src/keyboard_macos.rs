//! macOS physical-keyboard capture via a raw `NSEvent` local monitor.
//!
//! **Why bypass GTK's `EventControllerKey` on macOS.** GDK's macOS backend does *not*
//! map raw scancodes to key events like the X11/Wayland backends do. `keyDown:` calls
//! `interpretKeyEvents:`, routing every key through the Cocoa Text Input Context (the
//! IME / marked-text machinery). For text the input method commits *outside* the
//! synchronous `keyDown:` — dead keys, marked text, press-and-hold accent popovers, real
//! IMEs — GDK emits a **synthetic "null key"** `GdkKeyEvent` with `hardware_keycode == 0`
//! and `keyval == VoidSymbol` (`gdk/macos: _gdk_macos_surface_synthesize_null_key`). Our
//! Carbon-kVK→evdev table maps keycode `0` to `KEY_A` (A is the one letter whose kVK is
//! literally 0), so those synthetic markers were forwarded to the remote as phantom,
//! out-of-order `A` presses — the "type a, nothing; type u, get `ua`" bug.
//!
//! A remote-desktop / game viewer wants **physical** keys, not IME-composed text (the
//! remote runs its own keymap). So on macOS we read raw key events straight from
//! `NSEvent` — `keyCode` is the true Carbon virtual key, delivered immediately, in order,
//! never synthesized — exactly as `pointer_lock_macos.rs` reads raw mouse deltas.
//!
//! **Integration.** One app-global local monitor (there is a single remote keyboard).
//! While a monitor/video window is the key window (see [`note_window_active`]) the monitor
//! forwards physical presses/releases and **consumes** them (`return null`) so AppKit never
//! runs `interpretKeyEvents:` on them (no beep, no null-key synthesis). Modifier transitions
//! (`FlagsChanged`) are forwarded but **passed through** so GDK/GTK keep their own modifier
//! state — the local shortcuts (F11, Ctrl+Alt+G/P) are still recognised by the GTK key
//! handler in `main.rs`, which is why those keys are passed through here too. When a dialog /
//! the pre-connection window is key instead, every event is passed through untouched so GTK
//! text entries and buttons work normally.

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Write;
use std::net::TcpStream;
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};

use crate::kvk_evdev;

/// The viewer's input write half (port-1 socket); shared with the GTK thread.
/// Same alias as `main.rs`'s `Writer` / `pointer_lock_macos.rs`.
type Writer = Arc<Mutex<Option<TcpStream>>>;

// Carbon kVK codes of the keys the GTK handler owns as *local* shortcuts (see
// `install_keyboard` in main.rs). The monitor passes these through instead of forwarding
// them, so the remote never sees them — matching Linux, where F11 / Ctrl+Alt+G /
// Ctrl+Alt+P are never put on the wire.
const KVK_F11: u32 = 0x67;
const KVK_G: u32 = 0x05;
const KVK_P: u32 = 0x23;

/// evdev `KEY_CAPSLOCK`. macOS reports CapsLock as a lock *toggle* (`FlagsChanged`), not a
/// hold, so it can't be tracked in the held-set like the other modifiers.
const KEY_CAPSLOCK: u32 = 58;

/// App-global state, initialised once by [`install`]. `active_windows` counts how many
/// video/monitor windows are currently the key window (0 ⇒ a dialog / the pre-connection
/// window has focus, so keys stay local); `pressed` is the set of evdev keycodes currently
/// held on the remote (so a focus loss / panic can release them); `_monitor` keeps the
/// NSEvent monitor installed for the app lifetime.
struct Shared {
    active_windows: Arc<AtomicUsize>,
    pressed: Arc<Mutex<HashSet<u32>>>,
    writer: Writer,
    _monitor: Retained<AnyObject>,
}

thread_local! {
    // The GTK application is single-threaded; the NSEvent monitor block runs on that same
    // main thread. A thread-local avoids threading this singleton through every window's
    // `install_keyboard` call.
    static KB: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

/// Frame one input message to the server: `[0u8][u32be len][json]` (tag 0 = input).
/// One contiguous write, mirroring `send_tagged` in main.rs (TCP_NODELAY-friendly).
fn send_key(writer: &Writer, keycode: u32, pressed: bool) {
    let json = format!(r#"{{"kind":"key_code","keycode":{keycode},"pressed":{pressed}}}"#);
    let body = json.as_bytes();
    let mut frame = Vec::with_capacity(1 + 4 + body.len());
    frame.push(0u8);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    if let Some(g) = writer.lock().unwrap().as_mut() {
        let _ = g.write_all(&frame);
    }
}

/// IOKit `NX_DEVICE*` device-dependent modifier bits (the low 16 bits of
/// `NSEvent.modifierFlags`), keyed by the modifier's Carbon kVK. This is how the key's
/// actual up/down state is read from a `FlagsChanged` event itself (Chromium does the
/// same in `ui/events/cocoa`), rather than inferred from history.
fn device_flag_bit(kvk: u32) -> Option<usize> {
    Some(match kvk {
        0x3B => 0x0001, // kVK_Control       NX_DEVICELCTLKEYMASK
        0x38 => 0x0002, // kVK_Shift         NX_DEVICELSHIFTKEYMASK
        0x3C => 0x0004, // kVK_RightShift    NX_DEVICERSHIFTKEYMASK
        0x37 => 0x0008, // kVK_Command       NX_DEVICELCMDKEYMASK
        0x36 => 0x0010, // kVK_RightCommand  NX_DEVICERCMDKEYMASK
        0x3A => 0x0020, // kVK_Option        NX_DEVICELALTKEYMASK
        0x3D => 0x0040, // kVK_RightOption   NX_DEVICERALTKEYMASK
        0x3E => 0x2000, // kVK_RightControl  NX_DEVICERCTLKEYMASK
        _ => return None,
    })
}

/// Device-*independent* `NSEventModifierFlags` class bit for a modifier kVK. These high
/// bits (`Control` = 0x40000, etc.) are always present in `NSEvent.modifierFlags` — the
/// local-shortcut detection in `install` already relies on them — so they are the
/// reliable "is a key of this class down" signal. `keyCode` (kVK) already identifies the
/// specific left/right key, so the class flag is all we need for state.
fn modifier_class_flag(kvk: u32) -> Option<usize> {
    Some(match kvk {
        0x3B | 0x3E => NSEventModifierFlags::Control.0, // Control / RightControl
        0x38 | 0x3C => NSEventModifierFlags::Shift.0,   // Shift / RightShift
        0x37 | 0x36 => NSEventModifierFlags::Command.0, // Command / RightCommand
        0x3A | 0x3D => NSEventModifierFlags::Option.0,  // Option / RightOption
        _ => return None,
    })
}

/// The physical up/down state of modifier `kvk`, read from a `FlagsChanged` event's
/// `modifierFlags` (`mf`). `None` for non-modifier kVKs (fn/Globe, letters).
///
/// Primary signal is the device-independent class flag (guaranteed present). The
/// device-dependent per-key bit is used *only* to refine when the event actually carries
/// device bits (low word non-zero) — that disambiguates holding both the left and right
/// key of one modifier. This is robust whether or not the OS populates the device bits:
///   - class flag clear            → key is up (definitive)
///   - class set, no device bits   → this key is down (single-key case)
///   - class set, device bits set  → precise per-key bit
///
/// The old logic read `mf & device_bit` *alone*; when the device bits were not delivered
/// that was always 0, so every transition looked like a release and nothing was ever
/// forwarded — the remote's modifiers (e.g. a Control left stuck by a prior session)
/// never got their release and stayed down, so Tab/Space/Enter resolved as Ctrl+Tab etc.
fn modifier_now_down(mf: usize, kvk: u32) -> Option<bool> {
    let class_flag = modifier_class_flag(kvk)?;
    if mf & class_flag == 0 {
        return Some(false);
    }
    Some(match device_flag_bit(kvk) {
        Some(bit) if mf & 0xffff != 0 => mf & bit != 0,
        _ => true,
    })
}

/// Decide what to send for a modifier transition: `Some(pressed)` to forward, `None` to
/// drop. `now_down` is the key's real state from the event's device flag bit; the
/// held-set mirrors what the remote believes, so a transition that wouldn't change the
/// remote's view (a release we never sent a press for, a redundant press) is dropped.
///
/// Deriving press/release by *toggling* the held-set instead (the previous logic)
/// inverted the meaning whenever a modifier's press was never tracked — e.g. ⌘ held
/// across a Cmd+Tab INTO the viewer: its release after focus-gain became a phantom
/// press, leaving Super stuck down on the remote (Space → Super+Space input-source
/// switch, Enter → Super+Enter: neither types anything).
fn flags_transition(held: &mut HashSet<u32>, keycode: u32, now_down: bool) -> Option<bool> {
    if now_down {
        held.insert(keycode).then_some(true)
    } else {
        held.remove(&keycode).then_some(false)
    }
}

/// Install the app-global keyboard monitor. Call once, from `build_ui`, on the main thread.
pub fn install(writer: Writer) {
    let active_windows = Arc::new(AtomicUsize::new(0));
    let pressed: Arc<Mutex<HashSet<u32>>> = Arc::new(Mutex::new(HashSet::new()));

    let block = {
        let (active_windows, pressed, writer) = (active_windows.clone(), pressed.clone(), writer.clone());
        // The handler returns the event to let it continue down the responder chain, or
        // `null` to consume it. (Same convention as the pointer-lock monitor.)
        RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: the ObjC runtime passes a valid, non-null NSEvent for the masked types.
            let ev = unsafe { event.as_ref() };

            // Not looking at the remote (a dialog or the pre-connection window is key):
            // let AppKit + GTK handle the key so local text entries / buttons work.
            if active_windows.load(Ordering::Relaxed) == 0 {
                return event.as_ptr();
            }

            let etype = ev.r#type().0;
            let kc = ev.keyCode() as u32;
            let mf = ev.modifierFlags().0;
            let ctrl = mf & NSEventModifierFlags::Control.0 != 0;
            let opt = mf & NSEventModifierFlags::Option.0 != 0;
            let is_local_shortcut = kc == KVK_F11 || (ctrl && opt && (kc == KVK_G || kc == KVK_P));

            if etype == NSEventType::KeyDown.0 {
                // Local viewer shortcuts stay with the GTK handler: pass them through (with
                // modifiers intact) and don't forward them to the remote.
                if is_local_shortcut {
                    return event.as_ptr();
                }
                // A held key already autorepeats on the remote (Mutter re-injects the held
                // keycode); don't stack a second local repeat stream on top.
                if ev.isARepeat() {
                    return null_mut();
                }
                let keycode = kvk_evdev::translate(kc);
                tracing::debug!("key down: kVK={:#04x} evdev={}", kc, keycode);
                if keycode != 0 {
                    pressed.lock().unwrap().insert(keycode);
                    send_key(&writer, keycode, true);
                }
                // Consume: no macOS beep, and — critically — no `interpretKeyEvents:`, so
                // GDK never synthesizes the phantom keycode-0 null-key this module fixes.
                null_mut()
            } else if etype == NSEventType::KeyUp.0 {
                // Only release keys we actually forwarded a press for. This naturally passes
                // through shortcut keys (never in the held-set) and avoids phantom releases.
                let keycode = kvk_evdev::translate(kc);
                tracing::debug!("key up: kVK={:#04x} evdev={}", kc, keycode);
                if keycode != 0 && pressed.lock().unwrap().remove(&keycode) {
                    send_key(&writer, keycode, false);
                    return null_mut();
                }
                event.as_ptr()
            } else if etype == NSEventType::FlagsChanged.0 {
                // Modifier transition. Forward it, but PASS IT THROUGH (don't consume) so
                // GDK/GTK keep their modifier state in sync — the F11 / Ctrl+Alt+G/P
                // shortcuts are recognised by the GTK handler from that state.
                let keycode = kvk_evdev::translate(kc);
                if keycode == KEY_CAPSLOCK {
                    // Lock key: AppKit reports a state toggle, not a hold. Emit a tap so the
                    // remote toggles its own lock. Best-effort (verify on-device).
                    send_key(&writer, keycode, true);
                    send_key(&writer, keycode, false);
                } else if let Some(now_down) = modifier_now_down(mf, kc) {
                    // Press vs release comes from THIS event's flags (device-independent
                    // class flag, refined by the per-key device bit when present) — never
                    // from history, which goes stale across focus transitions (a modifier
                    // held during Cmd+Tab has its press delivered to the previous app; only
                    // its release reaches us, and it must read as an up, not a phantom down).
                    let decision = flags_transition(&mut pressed.lock().unwrap(), keycode, now_down);
                    tracing::debug!(
                        "flags: kVK={:#04x} evdev={} mf={:#010x} class_on={} low={:#06x} down={} -> {:?}",
                        kc, keycode, mf,
                        mf & modifier_class_flag(kc).unwrap_or(0) != 0,
                        mf & 0xffff, now_down, decision
                    );
                    if let Some(p) = decision {
                        send_key(&writer, keycode, p);
                    }
                }
                // kVKs with no modifier class (fn/Globe) are dropped: they carry no
                // remote-mappable state, and a blind toggle is what used to stick modifiers.
                event.as_ptr()
            } else {
                event.as_ptr()
            }
        })
    };

    let mask = NSEventMask::KeyDown | NSEventMask::KeyUp | NSEventMask::FlagsChanged;

    // SAFETY: called on the main thread; block is heap-allocated via RcBlock.
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
    let Some(monitor) = monitor else {
        tracing::warn!(
            "macOS keyboard: NSEvent local monitor install failed (runtime returned nil); \
             keyboard input will NOT reach the remote"
        );
        return;
    };

    KB.with(|k| *k.borrow_mut() = Some(Shared { active_windows, pressed, writer, _monitor: monitor }));
    tracing::info!("macOS keyboard monitor installed (physical keys → remote)");
}

/// Note a video/monitor window gaining or losing key-window status. Forwarding is enabled
/// while at least one is active; a reference count (not a flag) keeps it correct regardless
/// of the order in which one window's deactivate and another's activate notifications fire.
/// Driven from each window's `connect_is_active_notify` in main.rs.
pub fn note_window_active(active: bool) {
    KB.with(|k| {
        if let Some(s) = &*k.borrow() {
            let count = if active {
                s.active_windows.fetch_add(1, Ordering::Relaxed) + 1
            } else {
                // Saturating decrement: never underflow if notifications are unbalanced.
                s.active_windows
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| Some(c.saturating_sub(1)))
                    .map(|prev| prev.saturating_sub(1))
                    .unwrap_or(0)
            };
            tracing::debug!(
                "keyboard gate: active={active} count={count} (forwarding {})",
                if count > 0 { "ON" } else { "OFF" }
            );
        }
    });
}

/// Release every key currently held on the remote. Called on genuine focus loss and by the
/// Ctrl+Alt+G / Ctrl+Alt+P panic shortcuts (which also drop the leaked shortcut modifiers).
pub fn release_all() {
    KB.with(|k| {
        if let Some(s) = &*k.borrow() {
            let held: Vec<u32> = s.pressed.lock().unwrap().drain().collect();
            for keycode in held {
                send_key(&s.writer, keycode, false);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // evdev codes used below.
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_LEFTMETA: u32 = 125;
    const KEY_LEFTSHIFT: u32 = 42;

    // Carbon kVK codes (the input to modifier_now_down / modifier_class_flag).
    const KVK_CONTROL: u32 = 0x3B;
    const KVK_SHIFT: u32 = 0x38;
    const KVK_RIGHT_SHIFT: u32 = 0x3C;
    const KVK_COMMAND: u32 = 0x37;

    /// THE bug (Cmd+Tab into the viewer): ⌘'s press went to the previous app, so the
    /// first event we see is its release. That must be dropped — the old toggle logic
    /// turned it into a phantom press, sticking Super down on the remote (Space/Enter
    /// then resolve as Super+Space / Super+Enter and type nothing).
    #[test]
    fn spurious_release_is_dropped_not_inverted() {
        let mut held = HashSet::new();
        assert_eq!(flags_transition(&mut held, KEY_LEFTMETA, false), None);
        assert!(held.is_empty(), "an untracked release must not enter the held-set");
    }

    /// A modifier already physically down when the window gains focus: its eventual
    /// release is dropped (press was never forwarded), and the NEXT full press/release
    /// cycle forwards normally — the state self-corrects instead of staying inverted.
    #[test]
    fn state_self_corrects_after_focus_gain_with_modifier_held() {
        let mut held = HashSet::new();
        assert_eq!(flags_transition(&mut held, KEY_LEFTSHIFT, false), None);
        assert_eq!(flags_transition(&mut held, KEY_LEFTSHIFT, true), Some(true));
        assert_eq!(flags_transition(&mut held, KEY_LEFTSHIFT, false), Some(false));
        assert!(held.is_empty());
    }

    #[test]
    fn normal_press_release_cycle_forwards_both() {
        let mut held = HashSet::new();
        assert_eq!(flags_transition(&mut held, KEY_LEFTMETA, true), Some(true));
        assert!(held.contains(&KEY_LEFTMETA));
        assert_eq!(flags_transition(&mut held, KEY_LEFTMETA, false), Some(false));
        assert!(held.is_empty());
    }

    #[test]
    fn redundant_press_is_dropped() {
        let mut held = HashSet::new();
        assert_eq!(flags_transition(&mut held, KEY_LEFTMETA, true), Some(true));
        assert_eq!(flags_transition(&mut held, KEY_LEFTMETA, true), None);
        assert!(held.contains(&KEY_LEFTMETA));
    }

    /// The NX_DEVICE* bits for all eight modifier kVKs, per IOKit's IOLLEvent.h (same
    /// values Chromium's dom_code_data path uses). fn/Globe (0x3F) has no device bit.
    #[test]
    fn device_flag_bits() {
        assert_eq!(device_flag_bit(0x3B), Some(0x0001), "kVK_Control");
        assert_eq!(device_flag_bit(0x38), Some(0x0002), "kVK_Shift");
        assert_eq!(device_flag_bit(0x3C), Some(0x0004), "kVK_RightShift");
        assert_eq!(device_flag_bit(0x37), Some(0x0008), "kVK_Command");
        assert_eq!(device_flag_bit(0x36), Some(0x0010), "kVK_RightCommand");
        assert_eq!(device_flag_bit(0x3A), Some(0x0020), "kVK_Option");
        assert_eq!(device_flag_bit(0x3D), Some(0x0040), "kVK_RightOption");
        assert_eq!(device_flag_bit(0x3E), Some(0x2000), "kVK_RightControl");
        assert_eq!(device_flag_bit(0x3F), None, "fn/Globe has no device bit");
        assert_eq!(device_flag_bit(0x00), None, "non-modifier kVK has no device bit");
    }

    #[test]
    fn modifier_class_flags() {
        assert_eq!(modifier_class_flag(0x3B), Some(NSEventModifierFlags::Control.0));
        assert_eq!(modifier_class_flag(0x3E), Some(NSEventModifierFlags::Control.0));
        assert_eq!(modifier_class_flag(0x38), Some(NSEventModifierFlags::Shift.0));
        assert_eq!(modifier_class_flag(0x3C), Some(NSEventModifierFlags::Shift.0));
        assert_eq!(modifier_class_flag(0x37), Some(NSEventModifierFlags::Command.0));
        assert_eq!(modifier_class_flag(0x36), Some(NSEventModifierFlags::Command.0));
        assert_eq!(modifier_class_flag(0x3A), Some(NSEventModifierFlags::Option.0));
        assert_eq!(modifier_class_flag(0x3D), Some(NSEventModifierFlags::Option.0));
        assert_eq!(modifier_class_flag(0x3F), None, "fn/Globe");
        assert_eq!(modifier_class_flag(0x00), None, "non-modifier");
    }

    /// THE regression this fix targets: when macOS omits the device-dependent low bits,
    /// press/release must still be read from the device-independent class flag. The old
    /// `mf & device_bit` logic saw 0 here, read every transition as a release, and
    /// forwarded nothing — so a Control stuck on the remote never got its release.
    #[test]
    fn now_down_from_class_flag_when_device_bits_absent() {
        let ctrl = NSEventModifierFlags::Control.0; // class flag only, low word == 0
        assert_eq!(modifier_now_down(ctrl, KVK_CONTROL), Some(true), "press");
        assert_eq!(modifier_now_down(0, KVK_CONTROL), Some(false), "release");
    }

    /// When the OS reports device-dependent bits, use them to distinguish left from right
    /// so both-of-a-pair holds track independently.
    #[test]
    fn now_down_uses_device_bit_when_present() {
        let shift = NSEventModifierFlags::Shift.0;
        let l = 0x0002usize; // NX_DEVICELSHIFTKEYMASK
        let r = 0x0004usize; // NX_DEVICERSHIFTKEYMASK
        // Both shifts down.
        assert_eq!(modifier_now_down(shift | l | r, KVK_SHIFT), Some(true));
        assert_eq!(modifier_now_down(shift | l | r, KVK_RIGHT_SHIFT), Some(true));
        // Release right while left held: class still on, only left bit remains.
        assert_eq!(modifier_now_down(shift | l, KVK_RIGHT_SHIFT), Some(false));
        assert_eq!(modifier_now_down(shift | l, KVK_SHIFT), Some(true));
    }

    #[test]
    fn now_down_none_for_non_modifier() {
        assert_eq!(modifier_now_down(0, 0x00), None);
        assert_eq!(modifier_now_down(0, 0x3F), None, "fn/Globe");
    }

    /// End-to-end on the device-bits-absent path: a normal Control press then release
    /// forwards down then up (the bug: both were dropped, leaving Control stuck).
    #[test]
    fn device_bits_absent_forwards_full_cycle() {
        let ctrl = NSEventModifierFlags::Control.0;
        let mut held = HashSet::new();
        let down = modifier_now_down(ctrl, KVK_CONTROL).unwrap();
        assert_eq!(flags_transition(&mut held, KEY_LEFTCTRL, down), Some(true));
        let up = modifier_now_down(0, KVK_CONTROL).unwrap();
        assert_eq!(flags_transition(&mut held, KEY_LEFTCTRL, up), Some(false));
        assert!(held.is_empty());
    }

    /// Cmd+Tab INTO the viewer: ⌘'s press went to the previous app, so the first event we
    /// see is its release — class flag already clear → reads as up → dropped, not inverted
    /// into a phantom press.
    #[test]
    fn cmd_tab_spurious_release_reads_as_up_and_drops() {
        let mut held = HashSet::new();
        let up = modifier_now_down(0, KVK_COMMAND).unwrap();
        assert!(!up);
        assert_eq!(flags_transition(&mut held, KEY_LEFTMETA, up), None);
        assert!(held.is_empty());
    }
}
