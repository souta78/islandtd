//! Everything that touches the operating system.
//!
//! * `place` / `anchor_of` / `start_drag` / `request_focus`: window placement through winit's
//!   safe API (multi-monitor aware, DPI aware). Cross-platform.
//! * `windows`: startup entry, Task Scheduler wake and the reminder sound, done by running the
//!   built-in `reg.exe`, `schtasks.exe` and `rundll32.exe` with fixed arguments. No `unsafe`
//!   code anywhere in this project.

use slint::winit_030::{winit, WinitWindowAccessor};
use slint::{PhysicalPosition, Window};

// Compiled on every OS so the host build type-checks it; only called on Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod windows;

/// Enable/disable start-with-Windows. Off Windows this reports that it is unsupported.
pub fn windows_set_startup(enabled: bool, exe: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::set_startup(enabled, exe)
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, exe);
        Err("Start with Windows is only available on Windows.".to_string())
    }
}

/// Distance from the top edge of the screen, in logical pixels.
const TOP_MARGIN: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

fn rect_of(m: &winit::monitor::MonitorHandle) -> Rect {
    let (p, s) = (m.position(), m.size());
    Rect {
        x: p.x,
        y: p.y,
        w: s.width as i32,
        h: s.height as i32,
    }
}

/// Pure placement rule (unit-tested): where should the window's top-left go?
/// `anchor` is the remembered (centre-x, top-y) in physical pixels. It is honoured only if it
/// still lies on a connected monitor; otherwise the island returns to the top centre of the
/// primary monitor. The result is clamped so the window is never off-screen.
fn compute_position(
    monitors: &[Rect],
    primary: Option<Rect>,
    anchor: Option<(i32, i32)>,
    win_w: i32,
    margin_px: i32,
) -> Option<(i32, i32)> {
    let primary = primary.or_else(|| monitors.first().copied())?;
    let (cx, top, mon) = match anchor {
        Some((ax, ay)) => match monitors.iter().find(|m| m.contains(ax, ay)) {
            Some(m) => (ax, ay, *m),
            None => (primary.x + primary.w / 2, primary.y + margin_px, primary),
        },
        None => (primary.x + primary.w / 2, primary.y + margin_px, primary),
    };
    let x = (cx - win_w / 2).clamp(mon.x, (mon.x + mon.w - win_w).max(mon.x));
    let y = top.clamp(mon.y, (mon.y + mon.h - 40).max(mon.y));
    Some((x, y))
}

/// Move the window to its remembered or default position. Safe to call repeatedly; used at
/// startup, when expanding, and for "Reset position".
///
/// Returns `false` if the native window does not exist yet (Slint creates it lazily once the
/// event loop is running), so the caller can retry.
pub fn place(window: &Window, anchor: Option<(i32, i32)>) -> bool {
    let size = window.size();
    let info = window.with_winit_window(|w| {
        let monitors: Vec<Rect> = w.available_monitors().map(|m| rect_of(&m)).collect();
        let primary = w
            .primary_monitor()
            .or_else(|| w.current_monitor())
            .map(|m| rect_of(&m));
        let scale = w.scale_factor();
        (monitors, primary, scale)
    });
    let Some((monitors, primary, scale)) = info else {
        return false;
    };
    let margin = (TOP_MARGIN * scale).round() as i32;
    if let Some((x, y)) = compute_position(&monitors, primary, anchor, size.width as i32, margin) {
        window.set_position(PhysicalPosition::new(x, y));
    }
    true
}

/// The window's current anchor: (centre-x, top-y) in physical pixels.
pub fn anchor_of(window: &Window) -> (i32, i32) {
    let (p, s) = (window.position(), window.size());
    (p.x + s.width as i32 / 2, p.y)
}

/// Begin an OS-managed window drag (the island follows the pointer).
pub fn start_drag(window: &Window) {
    window.with_winit_window(|w| {
        let _ = w.drag_window();
    });
}

/// Ask the OS to focus the window. Only used when the user enabled "Reminders may take
/// keyboard focus"; otherwise reminders never steal focus.
pub fn request_focus(window: &Window) {
    window.with_winit_window(|w| w.focus_window());
}

#[cfg(test)]
mod tests {
    use super::*;

    const M1: Rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };
    const M2: Rect = Rect {
        x: 1920,
        y: 0,
        w: 2560,
        h: 1440,
    };
    const LEFT: Rect = Rect {
        x: -1280,
        y: 0,
        w: 1280,
        h: 1024,
    };

    #[test]
    fn defaults_to_top_centre_of_primary() {
        assert_eq!(
            compute_position(&[M1, M2], Some(M1), None, 424, 6),
            Some((748, 6))
        );
        // Primary at an offset (e.g. a secondary monitor positioned left of it).
        assert_eq!(
            compute_position(&[LEFT, M1], Some(M1), None, 424, 6),
            Some((748, 6))
        );
    }

    #[test]
    fn valid_anchor_on_any_monitor_is_kept() {
        assert_eq!(
            compute_position(&[M1, M2], Some(M1), Some((3200, 100)), 424, 6),
            Some((2988, 100))
        );
    }

    #[test]
    fn disconnected_monitor_falls_back_to_primary() {
        // Remembered a spot on monitor 2, which is now gone.
        assert_eq!(
            compute_position(&[M1], Some(M1), Some((3200, 100)), 424, 6),
            Some((748, 6))
        );
    }

    #[test]
    fn resolution_change_clamps_into_view() {
        // Was at x=1800 on 1920 wide; screen is now 1280 wide.
        let small = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 720,
        };
        let (x, y) = compute_position(&[small], Some(small), Some((1270, 710)), 424, 6).unwrap();
        assert!(x + 424 <= 1280 && x >= 0, "{x}");
        assert!(y <= 720 - 40, "{y}");
    }

    #[test]
    fn no_monitors_means_no_move() {
        assert_eq!(compute_position(&[], None, None, 424, 6), None);
    }
}
