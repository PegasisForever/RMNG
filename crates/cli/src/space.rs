//! The two spaces of an `rmng desktop` call, and the arithmetic between them.
//!
//! `--resolution` sets the size of the screenshot: the monitor is scaled down, keeping its
//! shape, until it fits inside `W×H`; a monitor that already fits is not scaled, and nothing
//! is scaled up. `--cursor-coordinate-space` sets the units X and Y are given in: `native` is
//! pixels of that screenshot, and `W×H` is a grid of `W×H` cells laid over the whole
//! screenshot (so `999x999` reads X and Y on a 0–999 scale, the way some vision models give
//! positions).
//!
//! The CLI does this arithmetic itself and hands the daemon only an exact screenshot size
//! that is no larger than the monitor, plus X and Y in that size. Every daemon version reads
//! those the same way, so a clone still running an older daemon behaves like a new one.

use std::fmt;
use std::str::FromStr;

/// `<W>x<H>`, or `native`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    Native,
    Size(u32, u32),
}

impl FromStr for Space {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("native") {
            return Ok(Space::Native);
        }
        let bad = || format!("expected <W>x<H> (like 1920x1080) or native, got '{s}'");
        let (w, h) = s.split_once(['x', 'X']).ok_or_else(bad)?;
        let w: u32 = w.trim().parse().map_err(|_| bad())?;
        let h: u32 = h.trim().parse().map_err(|_| bad())?;
        if w == 0 || h == 0 {
            return Err(format!("W and H must be more than 0, got '{s}'"));
        }
        Ok(Space::Size(w, h))
    }
}

impl fmt::Display for Space {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Space::Native => f.write_str("native"),
            Space::Size(w, h) => write!(f, "{w}x{h}"),
        }
    }
}

/// The screenshot size for a monitor of `(nw, nh)` under `--resolution res`: the monitor
/// scaled down to fit inside `res`, shape kept; unchanged when it already fits. A scaled size
/// is rounded down to even numbers, as the daemon's encoder needs.
pub fn shot_size((nw, nh): (u32, u32), res: Space) -> (u32, u32) {
    let Space::Size(w, h) = res else {
        return (nw, nh);
    };
    if nw <= w && nh <= h {
        return (nw, nh);
    }
    let scale = (w as f64 / nw as f64).min(h as f64 / nh as f64);
    let even = |v: f64| ((v.floor() as u32) & !1).max(2);
    (even(nw as f64 * scale), even(nh as f64 * scale))
}

/// The daemon's `resolution` argument for a screenshot of `shot` pixels on a monitor of
/// `native` pixels. `native` when they are the same, so an odd-sized monitor is not rounded.
pub fn daemon_resolution(native: (u32, u32), shot: (u32, u32)) -> String {
    if shot == native {
        "native".into()
    } else {
        format!("{}x{}", shot.0, shot.1)
    }
}

/// X and Y given in `space`, as pixels of a screenshot of `shot` pixels.
pub fn to_shot(x: f64, y: f64, space: Space, (sw, sh): (u32, u32)) -> (f64, f64) {
    match space {
        Space::Native => (x, y),
        Space::Size(w, h) => (x * sw as f64 / w as f64, y * sh as f64 / h as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes_and_native() {
        assert_eq!("1920x1080".parse(), Ok(Space::Size(1920, 1080)));
        assert_eq!(" 999X999 ".parse(), Ok(Space::Size(999, 999)));
        assert_eq!("Native".parse(), Ok(Space::Native));
        for bad in ["", "1920", "0x10", "10x0", "-1x5", "axb", "1080p"] {
            assert!(bad.parse::<Space>().is_err(), "{bad:?}");
        }
        assert_eq!(Space::Size(999, 999).to_string(), "999x999");
    }

    #[test]
    fn a_larger_screen_is_scaled_down_to_fit_keeping_its_shape() {
        let p1080 = Space::Size(1920, 1080);
        assert_eq!(shot_size((2560, 1440), p1080), (1920, 1080));
        assert_eq!(shot_size((3840, 2160), p1080), (1920, 1080));
        // Ultrawide: width is the limit.
        assert_eq!(shot_size((3440, 1440), p1080), (1920, 802));
        // 16:10: height is the limit.
        assert_eq!(shot_size((2560, 1600), p1080), (1728, 1080));
        // Portrait.
        assert_eq!(shot_size((1440, 2560), p1080), (606, 1080));
    }

    #[test]
    fn a_screen_that_fits_is_not_scaled() {
        let p1080 = Space::Size(1920, 1080);
        assert_eq!(shot_size((1920, 1080), p1080), (1920, 1080));
        assert_eq!(shot_size((1280, 720), p1080), (1280, 720));
        assert_eq!(shot_size((1365, 767), p1080), (1365, 767));
        assert_eq!(shot_size((3840, 2160), Space::Native), (3840, 2160));
        assert_eq!(daemon_resolution((1365, 767), (1365, 767)), "native");
        assert_eq!(daemon_resolution((2560, 1440), (1920, 1080)), "1920x1080");
    }

    #[test]
    fn a_grid_space_maps_across_the_whole_screenshot() {
        let shot = (1920, 1080);
        assert_eq!(to_shot(500.0, 300.0, Space::Native, shot), (500.0, 300.0));
        assert_eq!(to_shot(0.0, 0.0, Space::Size(999, 999), shot), (0.0, 0.0));
        let (x, y) = to_shot(999.0, 999.0, Space::Size(999, 999), shot);
        assert_eq!((x, y), (1920.0, 1080.0));
        let (x, y) = to_shot(500.0, 250.0, Space::Size(1000, 1000), shot);
        assert_eq!((x, y), (960.0, 270.0));
    }
}
