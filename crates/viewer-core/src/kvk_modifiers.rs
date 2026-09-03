//! Modifier state read from a macOS `FlagsChanged` event, keyed by Carbon `kVK_*`.
//!
//! A `FlagsChanged` event says only "the modifier flags are now *this*"; it does not say
//! whether the key that caused it went down or up. Deriving that by *toggling* remembered
//! state goes wrong the moment an event is missed (a modifier held across a Cmd+Tab into the
//! viewer delivers only its release), so the state is read from the event's own flags instead.
//!
//! Both viewer front-ends need exactly this, on exactly these inputs — a raw
//! `NSEvent.modifierFlags` word and a `keyCode` — so it lives here rather than being written
//! twice and drifting. The precedent is [`crate::kvk_evdev`]: macOS-shaped, but pure integer
//! logic with no framework linkage, which is what keeps `viewer-core` toolkit-free. That is
//! also why the class bits below are spelled as literals rather than pulled from
//! `objc2_app_kit::NSEventModifierFlags` — each viewer carries a unit test asserting the two
//! agree, so a mismatch fails the build's tests instead of silently breaking modifiers.

/// Device-independent `NSEventModifierFlags` class bits (AppKit `NSEventModifierFlagShift`
/// and friends). These high bits are always present in `modifierFlags`.
pub const CLASS_SHIFT: usize = 1 << 17;
pub const CLASS_CONTROL: usize = 1 << 18;
pub const CLASS_OPTION: usize = 1 << 19;
pub const CLASS_COMMAND: usize = 1 << 20;

/// IOKit `NX_DEVICE*` device-dependent modifier bits (the low 16 bits of
/// `NSEvent.modifierFlags`), keyed by the modifier's Carbon kVK. These are what distinguish
/// the left key of a pair from the right one (Chromium's `ui/events/cocoa` reads them the
/// same way). `None` for kVKs that are not a left/right modifier.
pub fn device_flag_bit(kvk: u32) -> Option<usize> {
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

/// The device-*independent* class bit for a modifier kVK. Always present in `modifierFlags`,
/// so it is the reliable "is a key of this class down" signal; `keyCode` already says which of
/// the pair the event is about. `None` for kVKs that carry no modifier class (fn/Globe,
/// letters) — those have no remote-mappable state and must be dropped, not guessed at.
pub fn modifier_class_flag(kvk: u32) -> Option<usize> {
    Some(match kvk {
        0x3B | 0x3E => CLASS_CONTROL, // Control / RightControl
        0x38 | 0x3C => CLASS_SHIFT,   // Shift / RightShift
        0x37 | 0x36 => CLASS_COMMAND, // Command / RightCommand
        0x3A | 0x3D => CLASS_OPTION,  // Option / RightOption
        _ => return None,
    })
}

/// The physical up/down state of modifier `kvk`, read from a `FlagsChanged` event's
/// `modifierFlags` (`mf`). `None` for non-modifier kVKs (fn/Globe, letters).
///
/// Two tiers, because neither bit alone is sufficient:
///   - class flag clear            → key is up (definitive)
///   - class set, no device bits   → this key is down (single-key case)
///   - class set, device bits set  → the precise per-key bit decides
///
/// The class flag has to gate, because the device bits are not always delivered: reading
/// `mf & device_bit` *alone* saw 0 on those events, so every transition looked like a release,
/// nothing was ever forwarded, and a modifier already stuck on the remote never got its
/// release (Tab/Space/Enter then resolved as Ctrl+Tab and friends).
///
/// The device bit has to refine, because the class flag alone cannot tell the left key of a
/// pair from the right: hold LeftShift and tap RightShift, and on RightShift's release the
/// Shift class bit is still set by LeftShift — so the release reads as a press, is dropped as
/// redundant, and RightShift stays held on the remote.
pub fn modifier_now_down(mf: usize, kvk: u32) -> Option<bool> {
    let class_flag = modifier_class_flag(kvk)?;
    if mf & class_flag == 0 {
        return Some(false);
    }
    Some(match device_flag_bit(kvk) {
        Some(bit) if mf & 0xffff != 0 => mf & bit != 0,
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KVK_CONTROL: u32 = 0x3B;
    const KVK_SHIFT: u32 = 0x38;
    const KVK_RIGHT_SHIFT: u32 = 0x3C;

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
        assert_eq!(modifier_class_flag(0x3B), Some(CLASS_CONTROL));
        assert_eq!(modifier_class_flag(0x3E), Some(CLASS_CONTROL));
        assert_eq!(modifier_class_flag(0x38), Some(CLASS_SHIFT));
        assert_eq!(modifier_class_flag(0x3C), Some(CLASS_SHIFT));
        assert_eq!(modifier_class_flag(0x37), Some(CLASS_COMMAND));
        assert_eq!(modifier_class_flag(0x36), Some(CLASS_COMMAND));
        assert_eq!(modifier_class_flag(0x3A), Some(CLASS_OPTION));
        assert_eq!(modifier_class_flag(0x3D), Some(CLASS_OPTION));
        assert_eq!(modifier_class_flag(0x3F), None, "fn/Globe");
        assert_eq!(modifier_class_flag(0x00), None, "non-modifier");
    }

    /// When macOS omits the device-dependent low bits, press/release must still be read from
    /// the device-independent class flag, or nothing is ever forwarded and a stuck modifier
    /// never gets its release.
    #[test]
    fn now_down_from_class_flag_when_device_bits_absent() {
        assert_eq!(modifier_now_down(CLASS_CONTROL, KVK_CONTROL), Some(true), "press");
        assert_eq!(modifier_now_down(0, KVK_CONTROL), Some(false), "release");
    }

    /// Hold LeftShift, tap RightShift: on RightShift's release the Shift *class* bit is still
    /// set by LeftShift, so the class flag alone reads the release as a press and the remote
    /// keeps RightShift down forever. The device bit is what makes the two keys independent.
    #[test]
    fn left_and_right_of_one_modifier_track_independently() {
        let l = 0x0002usize; // NX_DEVICELSHIFTKEYMASK
        let r = 0x0004usize; // NX_DEVICERSHIFTKEYMASK
        // LeftShift down.
        assert_eq!(modifier_now_down(CLASS_SHIFT | l, KVK_SHIFT), Some(true));
        assert_eq!(modifier_now_down(CLASS_SHIFT | l, KVK_RIGHT_SHIFT), Some(false));
        // RightShift pressed while left is held.
        assert_eq!(modifier_now_down(CLASS_SHIFT | l | r, KVK_RIGHT_SHIFT), Some(true));
        assert_eq!(modifier_now_down(CLASS_SHIFT | l | r, KVK_SHIFT), Some(true));
        // RightShift released, left still down: THE regression — this must read as up.
        assert_eq!(modifier_now_down(CLASS_SHIFT | l, KVK_RIGHT_SHIFT), Some(false));
        assert_eq!(modifier_now_down(CLASS_SHIFT | l, KVK_SHIFT), Some(true));
    }

    #[test]
    fn now_down_none_for_non_modifier() {
        assert_eq!(modifier_now_down(0, 0x00), None);
        assert_eq!(modifier_now_down(0, 0x3F), None, "fn/Globe");
    }
}
