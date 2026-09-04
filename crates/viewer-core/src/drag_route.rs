//! Where a button-drag has been pulled to: following a drag past its window's edge onto the
//! neighbouring monitor.
//!
//! A viewer shows one window per remote monitor, so the gap between two of its windows is a
//! seam the remote desktop does not actually have. Press a button in one window and drag past
//! its edge, and the toolkit's implicit pointer grab keeps delivering motion to the *origin*
//! window with coordinates that run off the end of that monitor's image. Clamping those to the
//! image — the obvious thing, and the right thing for ordinary motion — is exactly what pins a
//! dragged remote window at the seam. Lifting the overshoot into unified-desktop coordinates
//! and asking which monitor's rectangle contains the result is the whole trick.
//!
//! This is the same twenty lines' third home — the old `../gtk` client had it as
//! `screens::route_drag`, the GTK viewer ported it, and the native macOS viewer needed it
//! next — so it lives here and both front-ends call it rather than drifting apart. It is pure
//! geometry over the monitor rectangles the server already puts in every `ViewSpec`, with no
//! toolkit or framework in sight, which is what keeps `viewer-core` toolkit-free; the
//! precedent is [`crate::kvk_evdev`] and [`crate::kvk_modifiers`].

/// One monitor's place in the desktop layout (unified-desktop px), taken from the server's
/// configured layout — the same rectangle for every clone, so the routing does not shift under
/// the drag when the daemon's live report disagrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Resolve a drag at unclamped `origin`-local coords to the monitor it is really over.
///
/// `mx`/`my` are the drag's position in the origin monitor's image pixels **without any clamp**
/// — the overshoot past the edge that the implicit grab delivers is the entire signal here, so
/// a caller that clamps first has already thrown the answer away. Returns the target monitor's
/// id and the position in *its* image pixels.
///
/// Three outcomes:
///   - inside some monitor's rectangle → that monitor, coords rebased onto it (the origin
///     itself is just the case where the drag never left);
///   - dead space (a gap between monitors, or off the outside of the whole desktop) → the
///     origin with the coords pinned to its edge, so the drag stalls where it left rather than
///     jumping to whichever monitor happened to be nearest;
///   - `origin` not in the layout at all → `None`, and the caller sends nothing. That happens
///     for a window whose monitor the spec has just dropped; guessing a monitor there would
///     land the drag on someone else's desktop.
pub fn route_drag(layout: &[Screen], origin: u32, mx: f64, my: f64) -> Option<(u32, f64, f64)> {
    let o = layout.iter().find(|s| s.id == origin)?;
    // Origin-local → unified desktop, which is the only frame the monitors share.
    let ux = o.x as f64 + mx;
    let uy = o.y as f64 + my;
    for s in layout {
        let (sx, sy) = (s.x as f64, s.y as f64);
        if ux >= sx && ux < sx + s.w as f64 && uy >= sy && uy < sy + s.h as f64 {
            return Some((s.id, ux - sx, uy - sy));
        }
    }
    // `w - 1` rather than `w`: the far column of pixels is `w - 1`, and `w` is the first
    // coordinate belonging to whatever is on the other side of the seam.
    let lx = mx.clamp(0.0, o.w.saturating_sub(1) as f64);
    let ly = my.clamp(0.0, o.h.saturating_sub(1) as f64);
    Some((origin, lx, ly))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three 1920x1080 monitors in a row, the middle one being the drag origin in most tests.
    fn row() -> Vec<Screen> {
        vec![
            Screen { id: 0, x: 0, y: 0, w: 1920, h: 1080 },
            Screen { id: 1, x: 1920, y: 0, w: 1920, h: 1080 },
            Screen { id: 2, x: 3840, y: 0, w: 1920, h: 1080 },
        ]
    }

    /// A drag that never leaves its own window must stay put, coords untouched — the common
    /// case, and the one a routing bug would break for every user rather than just multi-head.
    #[test]
    fn a_drag_inside_the_origin_stays_on_the_origin() {
        let l = row();
        assert_eq!(route_drag(&l, 1, 10.0, 20.0), Some((1, 10.0, 20.0)));
        assert_eq!(route_drag(&l, 1, 1919.0, 1079.0), Some((1, 1919.0, 1079.0)));
        assert_eq!(route_drag(&l, 0, 0.0, 0.0), Some((0, 0.0, 0.0)));
    }

    /// Past the right edge: the overshoot becomes an offset into the right-hand neighbour.
    #[test]
    fn dragging_off_the_right_edge_crosses_to_the_neighbour() {
        let l = row();
        // One pixel past the origin's last column is the neighbour's first.
        assert_eq!(route_drag(&l, 1, 1920.0, 500.0), Some((2, 0.0, 500.0)));
        assert_eq!(route_drag(&l, 1, 2020.5, 500.0), Some((2, 100.5, 500.0)));
        // And two monitors over, from monitor 0 straight across 1 into 2.
        assert_eq!(route_drag(&l, 0, 3900.0, 7.0), Some((2, 60.0, 7.0)));
    }

    /// Past the left edge: negative origin-local coords land on the left-hand neighbour.
    #[test]
    fn dragging_off_the_left_edge_crosses_to_the_neighbour() {
        let l = row();
        assert_eq!(route_drag(&l, 1, -1.0, 300.0), Some((0, 1919.0, 300.0)));
        assert_eq!(route_drag(&l, 1, -1920.0, 300.0), Some((0, 0.0, 300.0)));
        assert_eq!(route_drag(&l, 2, -50.0, 300.0), Some((1, 1870.0, 300.0)));
    }

    /// A stacked layout routes vertically by exactly the same rule.
    #[test]
    fn dragging_off_the_top_and_bottom_edges_crosses_the_stack() {
        let l = vec![
            Screen { id: 0, x: 0, y: 0, w: 1920, h: 1080 },
            Screen { id: 1, x: 0, y: 1080, w: 1920, h: 1080 },
        ];
        // Down off the top monitor.
        assert_eq!(route_drag(&l, 0, 400.0, 1080.0), Some((1, 400.0, 0.0)));
        assert_eq!(route_drag(&l, 0, 400.0, 1200.0), Some((1, 400.0, 120.0)));
        // Up off the bottom one.
        assert_eq!(route_drag(&l, 1, 400.0, -1.0), Some((0, 400.0, 1079.0)));
        assert_eq!(route_drag(&l, 1, 400.0, -1080.0), Some((0, 400.0, 0.0)));
    }

    /// A monitor offset on the other axis only takes the drag where it really overlaps: leaving
    /// monitor 0's right edge high up is dead space, lower down it is the neighbour.
    #[test]
    fn an_offset_neighbour_is_only_entered_where_it_overlaps() {
        let l = vec![
            Screen { id: 0, x: 0, y: 0, w: 1920, h: 1080 },
            // Sitting 600px lower, so only the origin's bottom 480px face it.
            Screen { id: 1, x: 1920, y: 600, w: 1920, h: 1080 },
        ];
        assert_eq!(route_drag(&l, 0, 1930.0, 700.0), Some((1, 10.0, 100.0)));
        // Above monitor 1's top: nothing there, so pinned to the origin's edge.
        assert_eq!(route_drag(&l, 0, 1930.0, 100.0), Some((0, 1919.0, 100.0)));
    }

    /// A gap between two monitors is not a desktop: the drag pins to the edge it left rather
    /// than teleporting across the void to the far monitor.
    #[test]
    fn dead_space_between_monitors_pins_to_the_origin_edge() {
        let l = vec![
            Screen { id: 0, x: 0, y: 0, w: 1920, h: 1080 },
            // 1000px of nothing before the next monitor starts.
            Screen { id: 1, x: 2920, y: 0, w: 1920, h: 1080 },
        ];
        assert_eq!(route_drag(&l, 0, 1920.0, 400.0), Some((0, 1919.0, 400.0)));
        assert_eq!(route_drag(&l, 0, 2919.0, 400.0), Some((0, 1919.0, 400.0)));
        // One more pixel and it is a real monitor again.
        assert_eq!(route_drag(&l, 0, 2920.0, 400.0), Some((1, 0.0, 400.0)));
    }

    /// Off the outside of the whole desktop — past the last monitor, or above the top row —
    /// pins to the origin's edge on both axes at once.
    #[test]
    fn leaving_the_desktop_entirely_pins_to_the_origin_edge() {
        let l = row();
        assert_eq!(route_drag(&l, 2, 5000.0, 500.0), Some((2, 1919.0, 500.0)));
        assert_eq!(route_drag(&l, 0, -100.0, -100.0), Some((0, 0.0, 0.0)));
        // Both axes out at once: the corner.
        assert_eq!(route_drag(&l, 2, 9999.0, 9999.0), Some((2, 1919.0, 1079.0)));
    }

    /// An origin that is not in the layout has no frame to lift the coords out of, so there is
    /// no honest answer — say so instead of routing the drag onto an arbitrary desktop.
    #[test]
    fn an_unknown_origin_routes_nowhere() {
        assert_eq!(route_drag(&row(), 7, 10.0, 10.0), None);
        assert_eq!(route_drag(&[], 0, 0.0, 0.0), None);
    }

    /// Monitors of different sizes rebase correctly, and a zero-sized one (a spec that has not
    /// filled in geometry yet) must not panic or produce a negative clamp bound.
    #[test]
    fn odd_geometry_is_handled_without_panicking() {
        let l = vec![
            Screen { id: 0, x: 0, y: 0, w: 3840, h: 2160 },
            Screen { id: 1, x: 3840, y: 0, w: 1280, h: 720 },
            Screen { id: 2, x: 5120, y: 0, w: 0, h: 0 },
        ];
        assert_eq!(route_drag(&l, 0, 3900.0, 60.0), Some((1, 60.0, 60.0)));
        // A zero-sized monitor contains nothing, so a drag from it can only pin to 0,0.
        assert_eq!(route_drag(&l, 2, 5.0, 5.0), Some((2, 0.0, 0.0)));
        // ...but a drag *out* of it still routes, because its origin still has a position.
        assert_eq!(route_drag(&l, 2, -20.0, 60.0), Some((1, 1260.0, 60.0)));
    }
}
