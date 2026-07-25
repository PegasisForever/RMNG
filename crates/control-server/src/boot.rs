//! Late-boot sequencing for the control-server.
//!
//! GStreamer plugin scanning opens private pipes. Any child spawned while that
//! scan is in flight (cliproxy / smbd / sshd) can inherit a write end and prevent
//! EOF, hanging `media::init` forever and leaving ports 9000/9001/9005 unbound.
//! This helper encodes the required order: finish media init, then spawn
//! background supervisors, then start media listeners.

/// Run the post-setup late-boot sequence in the only safe order.
///
/// `init_media` must complete (including any GStreamer registry scan) before
/// `spawn_background` runs, because background tasks may fork children that
/// inherit open FDs. `spawn_media_listeners` receives the token from
/// `init_media` and starts video / forward / clone-socket listeners.
pub fn run_late_boot<T, FInit, FBg, FMedia>(
    init_media: FInit,
    spawn_background: FBg,
    spawn_media_listeners: FMedia,
) where
    FInit: FnOnce() -> T,
    FBg: FnOnce(),
    FMedia: FnOnce(T),
{
    let token = init_media();
    spawn_background();
    spawn_media_listeners(token);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn media_init_runs_before_background_and_listeners() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        let o3 = order.clone();

        run_late_boot(
            || {
                o1.lock().unwrap().push("init");
                "token"
            },
            || o2.lock().unwrap().push("background"),
            |token| {
                assert_eq!(token, "token");
                o3.lock().unwrap().push("listeners");
            },
        );

        assert_eq!(
            *order.lock().unwrap(),
            vec!["init", "background", "listeners"]
        );
    }
}
