//! The server-address dialog: the one piece of configuration the viewer can change at runtime.
//!
//! Monitor windows only exist once video flows, so with a wrong address there would otherwise be
//! nothing on screen to fix it with — the same reason the GTK viewer puts this on the startup
//! window. Saving persists to `~/.config/rmng-viewer/config.json` (shared with the GTK viewer)
//! and drops the current connection so the net thread reconnects to the new address.

use std::sync::Arc;

use objc2::MainThreadOnly;
use objc2_app_kit::{NSAlert, NSTextField};
use objc2_foundation::{ns_string, MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use crate::shared::Shared;

/// Show the modal dialog. Returns `true` if the address was changed.
pub fn show(mtm: MainThreadMarker, shared: &Arc<Shared>) -> bool {
    let current = shared.addr.lock().unwrap().clone();

    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Server address"));
    alert.setInformativeText(ns_string!("host:port of the control-server's video port."));
    alert.addButtonWithTitle(ns_string!("Save"));
    alert.addButtonWithTitle(ns_string!("Cancel"));

    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 24.0)),
    );
    field.setStringValue(&NSString::from_str(&current));
    alert.setAccessoryView(Some(&field));
    // Put the caret in the field rather than on the default button.
    alert.window().makeFirstResponder(Some(&field));

    // NSAlertFirstButtonReturn == 1000 (the first button added, i.e. Save).
    const NS_ALERT_FIRST_BUTTON_RETURN: isize = 1000;
    if alert.runModal() != NS_ALERT_FIRST_BUTTON_RETURN {
        return false;
    }
    let text = field.stringValue().to_string().trim().to_string();
    if !valid_addr(&text) {
        tracing::warn!("settings: {text:?} is not a valid host:port — keeping {current:?}");
        return false;
    }
    if text == current {
        return false;
    }
    *shared.addr.lock().unwrap() = text.clone();
    // Preserve every other persisted field (e.g. cmd_is_ctrl) rather than resetting it.
    let cfg = viewer_core::config::Config { server_addr: text.clone(), ..viewer_core::config::load() };
    if let Err(e) = viewer_core::config::save(&cfg) {
        tracing::warn!("settings: config save failed: {e}");
    }
    // Drop the live connection so the net thread's parked read returns and it reconnects.
    if let Some(s) = shared.writer.lock().unwrap().as_ref() {
        let _ = s.shutdown(std::net::Shutdown::Both);
    }
    tracing::info!("settings: server address set to {text}");
    true
}

/// Light `host:port` validation: a non-empty host and a port that parses as `u16`.
fn valid_addr(s: &str) -> bool {
    match s.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::valid_addr;

    #[test]
    fn accepts_host_port_and_rejects_junk() {
        assert!(valid_addr("100.99.199.38:9001"));
        assert!(valid_addr("localhost:1"));
        assert!(!valid_addr("no-port"));
        assert!(!valid_addr(":9001"), "empty host");
        assert!(!valid_addr("host:70000"), "port out of u16 range");
        assert!(!valid_addr("host:abc"));
    }
}
