//! The clipboard bridge: `NSPasteboard` ⇄ the broker's rich + lazy offer/request/data protocol.
//!
//! Bytes move only on paste. Outbound, a local copy is noticed by polling `changeCount` (AppKit
//! has no change notification) and advertised as an offer of the types the pasteboard holds.
//! Inbound, an offer is answered with a request per MIME chosen by [`pick_mimes`], and the bytes
//! that come back are written to the pasteboard. `applying` suppresses the echo so mirroring a
//! remote copy doesn't immediately re-offer it back.
//!
//! MIME ⇄ `NSPasteboardType` is a small fixed table: text/plain ⇄ `NSPasteboardTypeString`,
//! text/html ⇄ `NSPasteboardTypeHTML`, image/png ⇄ `NSPasteboardTypePNG`, image/tiff ⇄
//! `NSPasteboardTypeTIFF`. Anything else is passed through under its own name, which round-trips
//! between two RMNG viewers and is simply not offered by other Mac apps.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardType, NSPasteboardTypeHTML, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSCopying, NSData, NSString};
use wire::socket::{ClipboardData, ClipboardMsg, ClipboardOffer, ClipboardRequest};

use crate::shared::{is_text_mime, pick_mimes, send_tagged, Shared};

/// Pasteboard state carried across ticks.
pub struct Clipboard {
    pasteboard: Retained<NSPasteboard>,
    /// Last `changeCount` we have seen; a bump means someone copied locally.
    last_change: isize,
    /// Set while we are writing a remote offer into the pasteboard, so the resulting change is
    /// not re-offered back to the server.
    applying: bool,
    /// Monotonic serial for our own offers.
    serial: u64,
    /// The remote offer being collected, and the bytes gathered for it so far.
    cur_serial: u64,
    collected: Vec<(String, Vec<u8>)>,
}

impl Clipboard {
    pub fn new(mtm: objc2_foundation::MainThreadMarker) -> Self {
        let _ = mtm; // NSPasteboard is not main-thread-only, but we only use it from there.
        let pasteboard = NSPasteboard::generalPasteboard();
        let last_change = pasteboard.changeCount();
        Clipboard {
            pasteboard,
            last_change,
            applying: false,
            serial: 1,
            cur_serial: 0,
            collected: Vec::new(),
        }
    }

    /// One housekeeping step: drain inbound clipboard messages, then notice a local copy.
    pub fn tick(&mut self, shared: &Arc<Shared>) {
        let msgs: Vec<ClipboardMsg> = shared.clip_inbox.lock().unwrap().drain(..).collect();
        for msg in msgs {
            self.handle(shared, msg);
        }
        self.poll_local(shared);
    }

    fn handle(&mut self, shared: &Arc<Shared>, msg: ClipboardMsg) {
        match msg {
            ClipboardMsg::Offer(o) => {
                self.cur_serial = o.serial;
                self.collected.clear();
                let wanted = pick_mimes(&o.mime_types);
                tracing::debug!(target: "clip",
                    "remote offer serial={} mimes={:?} -> requesting {wanted:?}", o.serial, o.mime_types);
                for mime_type in wanted {
                    let req = ClipboardRequest { serial: o.serial, mime_type };
                    if let Ok(json) = serde_json::to_string(&ClipboardMsg::Request(req)) {
                        send_tagged(&shared.writer, 1, &json);
                    }
                }
            }
            ClipboardMsg::Data(d) => {
                if d.serial != self.cur_serial {
                    tracing::debug!(target: "clip",
                        "dropping stale data serial={} mime={} (current {})", d.serial, d.mime_type, self.cur_serial);
                    return;
                }
                if d.bytes.is_empty() {
                    tracing::warn!(target: "clip",
                        "empty data for mime={} serial={} (remote read failed?)", d.mime_type, d.serial);
                    return;
                }
                tracing::debug!(target: "clip",
                    "data serial={} mime={} ({} bytes)", d.serial, d.mime_type, d.bytes.len());
                self.collected.retain(|(m, _)| m != &d.mime_type);
                self.collected.push((d.mime_type, d.bytes));
                self.write_collected();
            }
            ClipboardMsg::Request(r) => self.serve(shared, r),
        }
    }

    /// Write every collected MIME onto the pasteboard in one declaration, so both the rich type
    /// and plain text are pasteable.
    fn write_collected(&mut self) {
        self.applying = true;
        self.pasteboard.clearContents();
        for (mime, bytes) in &self.collected {
            let ty = pasteboard_type(mime);
            if is_text_mime(mime) {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    self.pasteboard.setString_forType(&NSString::from_str(text), &ty);
                    continue;
                }
            }
            let data = NSData::with_bytes(bytes);
            self.pasteboard.setData_forType(Some(&data), &ty);
        }
        // Adopt the resulting change count so poll_local doesn't read our own write as a copy.
        self.last_change = self.pasteboard.changeCount();
        self.applying = false;
        tracing::debug!(target: "clip", "local pasteboard set: {:?}",
            self.collected.iter().map(|(m, b)| format!("{m} ({}B)", b.len())).collect::<Vec<_>>());
    }

    /// Answer a remote request from the local pasteboard.
    fn serve(&self, shared: &Arc<Shared>, r: ClipboardRequest) {
        let ty = pasteboard_type(&r.mime_type);
        let bytes = if is_text_mime(&r.mime_type) {
            self.pasteboard.stringForType(&ty)
                .map(|s| s.to_string().into_bytes())
                .unwrap_or_default()
        } else {
            self.pasteboard.dataForType(&ty).map(|d| d.to_vec()).unwrap_or_default()
        };
        tracing::debug!(target: "clip",
            "serving serial={} mime={} ({} bytes)", r.serial, r.mime_type, bytes.len());
        let data = ClipboardData { serial: r.serial, mime_type: r.mime_type, bytes };
        if let Ok(json) = serde_json::to_string(&ClipboardMsg::Data(data)) {
            send_tagged(&shared.writer, 1, &json);
        }
    }

    /// Notice a local copy and advertise it (types only — bytes go out on request).
    fn poll_local(&mut self, shared: &Arc<Shared>) {
        if self.applying {
            return;
        }
        let now = self.pasteboard.changeCount();
        if now == self.last_change {
            return;
        }
        self.last_change = now;
        let Some(types) = self.pasteboard.types() else { return };
        // Dedupe: AppKit lists both the modern UTI and its legacy alias (NSStringPboardType),
        // which map to the same MIME, and offering a name twice just costs a round trip.
        let mut mimes: Vec<String> = Vec::new();
        for t in types.iter() {
            if let Some(m) = mime_for_type(&t) {
                if !mimes.contains(&m) {
                    mimes.push(m);
                }
            }
        }
        if mimes.is_empty() {
            return;
        }
        tracing::debug!(target: "clip", "local copy: offering {mimes:?}");
        self.serial += 1;
        let offer = ClipboardOffer { serial: self.serial, mime_types: mimes };
        if let Ok(json) = serde_json::to_string(&ClipboardMsg::Offer(offer)) {
            send_tagged(&shared.writer, 1, &json);
        }
    }
}

/// MIME → `NSPasteboardType`; unknown types pass through under their own name.
fn pasteboard_type(mime: &str) -> Retained<NSPasteboardType> {
    unsafe {
        if is_text_mime(mime) {
            NSPasteboardTypeString.copy()
        } else if mime == "text/html" {
            NSPasteboardTypeHTML.copy()
        } else if mime.starts_with("image/png") {
            NSPasteboardTypePNG.copy()
        } else if mime.starts_with("image/tiff") {
            NSPasteboardTypeTIFF.copy()
        } else {
            NSString::from_str(mime)
        }
    }
}

/// `NSPasteboardType` → the MIME we advertise for it. `None` for Apple-internal types that have
/// no useful cross-platform equivalent (they would only produce failed requests).
fn mime_for_type(ty: &NSPasteboardType) -> Option<String> {
    let name = ty.to_string();
    let known = unsafe {
        [
            (NSPasteboardTypeString.to_string(), "text/plain;charset=utf-8"),
            (NSPasteboardTypeHTML.to_string(), "text/html"),
            (NSPasteboardTypePNG.to_string(), "image/png"),
            (NSPasteboardTypeTIFF.to_string(), "image/tiff"),
        ]
    };
    for (k, mime) in known {
        if name == k {
            return Some(mime.to_string());
        }
    }
    // Apple-internal, opaque, or legacy-alias types: advertising these to a Linux clone only
    // produces requests for a MIME it cannot use.
    if name.starts_with("com.apple.") || name.starts_with("dyn.") || name.ends_with("PboardType") {
        return None;
    }
    Some(name)
}
