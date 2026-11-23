//! Pure 2D window tiling geometry, fraction cycling, snap detection, and multi-monitor remapping.

/// Maximum per-edge pixel deviation tolerated when matching an existing window
/// rectangle against a canonical snap target (absorbs 1–2px DWM and DPI rounding).
pub const TOLERANCE: u32 = 2;

/// Number of canonical snap states tracked in [`snap_table`].
const SNAP_COUNT: usize = 20;

/// Percentage of screen width and height covered by [`Action::Almost`].
const ALMOST_PERCENT: i32 = 85;

/// Horizontal screen side for fractional and quarter tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HSide {
    Left,
    Right,
}

/// Vertical screen side for fractional and quarter tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VSide {
    Top,
    Bottom,
}

/// Axis-aligned rectangle in physical screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// Returns `true` if `self` and `other` match within [`TOLERANCE`] pixels on all 4 components.
    #[must_use]
    pub const fn approx_eq(self, other: Self) -> bool {
        self.x.abs_diff(other.x) <= TOLERANCE
            && self.y.abs_diff(other.y) <= TOLERANCE
            && self.width.abs_diff(other.width) <= TOLERANCE
            && self.height.abs_diff(other.height) <= TOLERANCE
    }
}

/// Window tiling actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Left half, cycling `1/2 -> 2/3 -> 1/3` on repeated presses.
    Left,
    /// Right half, cycling `1/2 -> 2/3 -> 1/3` on repeated presses.
    Right,
    /// Top half, cycling `1/2 -> 2/3 -> 1/3` on repeated presses.
    Top,
    /// Bottom half, cycling `1/2 -> 2/3 -> 1/3` on repeated presses.
    Bottom,
    /// Top-left quarter (`1/2` width by `1/2` height).
    TopLeft,
    /// Top-right quarter (`1/2` width by `1/2` height).
    TopRight,
    /// Bottom-left quarter (`1/2` width by `1/2` height).
    BottomLeft,
    /// Bottom-right quarter (`1/2` width by `1/2` height).
    BottomRight,
    /// Full work area with outer `gap`, toggling to edge-to-edge (`0px` gap) on repeat press.
    Maximize,
    /// Centered `85%` width by `85%` height.
    Almost,
    /// Centers the window preserving its current size, or snaps to the Center `1/3` column
    /// if the window is already centered.
    Center,
}

/// Computes the target [`Rect`] for `action` given the monitor `screen` work area,
/// the window's `current` visible bounds, and the physical `gap` in pixels.
#[must_use]
pub fn apply(action: Action, screen: Rect, current: Rect, gap: i32) -> Rect {
    let g = gap.max(0);
    let inner = inset(screen, g);
    match action {
        Action::Left => cycle_three(
            current,
            h_frac(inner, g, HSide::Left, 1, 2),
            h_frac(inner, g, HSide::Left, 2, 3),
            h_frac(inner, g, HSide::Left, 1, 3),
        ),
        Action::Right => cycle_three(
            current,
            h_frac(inner, g, HSide::Right, 1, 2),
            h_frac(inner, g, HSide::Right, 2, 3),
            h_frac(inner, g, HSide::Right, 1, 3),
        ),
        Action::Top => cycle_three(
            current,
            v_frac(inner, g, VSide::Top, 1, 2),
            v_frac(inner, g, VSide::Top, 2, 3),
            v_frac(inner, g, VSide::Top, 1, 3),
        ),
        Action::Bottom => cycle_three(
            current,
            v_frac(inner, g, VSide::Bottom, 1, 2),
            v_frac(inner, g, VSide::Bottom, 2, 3),
            v_frac(inner, g, VSide::Bottom, 1, 3),
        ),
        Action::TopLeft => quarter(inner, g, HSide::Left, VSide::Top),
        Action::TopRight => quarter(inner, g, HSide::Right, VSide::Top),
        Action::BottomLeft => quarter(inner, g, HSide::Left, VSide::Bottom),
        Action::BottomRight => quarter(inner, g, HSide::Right, VSide::Bottom),
        Action::Maximize => {
            if g > 0 && current.approx_eq(inner) {
                screen
            } else {
                inner
            }
        }
        Action::Almost => almost_maximize(screen, inner),
        Action::Center => {
            let preserved = center_preserve_size(screen, current, inner);
            let col_third = center_third(inner, g);
            if current.approx_eq(preserved) && !current.approx_eq(col_third) {
                col_third
            } else {
                preserved
            }
        }
    }
}

/// Returns `true` if `current` matches any canonical tiled position on `screen`.
#[must_use]
pub fn is_snapped(screen: Rect, current: Rect, gap: i32) -> bool {
    let g = gap.max(0);
    snap_table(screen, g).iter().any(|&target| current.approx_eq(target))
}

/// If `current` is snapped to a canonical tile on `src_screen` (with `src_gap`),
/// returns the identical canonical tile on `dst_screen` (with `dst_gap`).
///
/// Returns `None` if the window is in a free-floating position.
#[must_use]
pub fn remap_to_screen(
    current: Rect,
    src_screen: Rect,
    src_gap: i32,
    dst_screen: Rect,
    dst_gap: i32,
) -> Option<Rect> {
    let sg = src_gap.max(0);
    let dg = dst_gap.max(0);
    let idx = snap_table(src_screen, sg).iter().position(|&target| current.approx_eq(target))?;
    snap_table(dst_screen, dg).get(idx).copied()
}

/// Generates all 20 canonical snap rectangles for `screen` and physical gap `g`.
fn snap_table(screen: Rect, g: i32) -> [Rect; SNAP_COUNT] {
    let inner = inset(screen, g);
    [
        h_frac(inner, g, HSide::Left, 1, 2),
        h_frac(inner, g, HSide::Left, 2, 3),
        h_frac(inner, g, HSide::Left, 1, 3),
        h_frac(inner, g, HSide::Right, 1, 2),
        h_frac(inner, g, HSide::Right, 2, 3),
        h_frac(inner, g, HSide::Right, 1, 3),
        v_frac(inner, g, VSide::Top, 1, 2),
        v_frac(inner, g, VSide::Top, 2, 3),
        v_frac(inner, g, VSide::Top, 1, 3),
        v_frac(inner, g, VSide::Bottom, 1, 2),
        v_frac(inner, g, VSide::Bottom, 2, 3),
        v_frac(inner, g, VSide::Bottom, 1, 3),
        quarter(inner, g, HSide::Left, VSide::Top),
        quarter(inner, g, HSide::Right, VSide::Top),
        quarter(inner, g, HSide::Left, VSide::Bottom),
        quarter(inner, g, HSide::Right, VSide::Bottom),
        inner,
        screen,
        almost_maximize(screen, inner),
        center_third(inner, g),
    ]
}

/// Cycles `first -> second -> third -> first` based on `current`.
const fn cycle_three(current: Rect, first: Rect, second: Rect, third: Rect) -> Rect {
    if current.approx_eq(first) {
        second
    } else if current.approx_eq(second) {
        third
    } else {
        first
    }
}

/// Shrinks `r` by `gap` on all four sides, clamping width and height to at least `1`.
fn inset(r: Rect, gap: i32) -> Rect {
    Rect {
        x: r.x + gap,
        y: r.y + gap,
        width: (r.width - gap * 2).max(1),
        height: (r.height - gap * 2).max(1),
    }
}

/// Computes a left or right horizontal fraction `num / den`.
///
/// Right fractions are computed as the integer complement of the opposing `(den - num) / den`
/// left tile so adjacent tiles (e.g. `Left 2/3` + `Right 1/3`) always sum to `inner.width - gap`.
fn h_frac(inner: Rect, gap: i32, side: HSide, num: i32, den: i32) -> Rect {
    let avail = (inner.width - gap).max(1);
    match side {
        HSide::Left => {
            let w = ((avail * num) / den).max(1);
            Rect { x: inner.x, y: inner.y, width: w, height: inner.height }
        }
        HSide::Right => {
            let opp_left = (avail * (den - num)) / den;
            let w = (avail - opp_left).max(1);
            Rect { x: inner.x + opp_left + gap, y: inner.y, width: w, height: inner.height }
        }
    }
}

/// Computes a top or bottom vertical fraction `num / den`.
///
/// Bottom fractions are computed as the integer complement of the opposing `(den - num) / den`
/// top tile so adjacent vertical tiles always sum to `inner.height - gap`.
fn v_frac(inner: Rect, gap: i32, side: VSide, num: i32, den: i32) -> Rect {
    let avail = (inner.height - gap).max(1);
    match side {
        VSide::Top => {
            let h = ((avail * num) / den).max(1);
            Rect { x: inner.x, y: inner.y, width: inner.width, height: h }
        }
        VSide::Bottom => {
            let opp_top = (avail * (den - num)) / den;
            let h = (avail - opp_top).max(1);
            Rect { x: inner.x, y: inner.y + opp_top + gap, width: inner.width, height: h }
        }
    }
}

/// Computes one of the four screen quarters using complementary horizontal and vertical halves.
fn quarter(inner: Rect, gap: i32, h_side: HSide, v_side: VSide) -> Rect {
    let h = h_frac(inner, gap, h_side, 1, 2);
    let v = v_frac(inner, gap, v_side, 1, 2);
    Rect { x: h.x, y: v.y, width: h.width, height: v.height }
}

/// Computes the middle `1/3` column positioned with exact `gap` spacing between
/// `Left 1/3` and `Right 1/3` for seamless 3-column layouts (`[1/3 | 1/3 | 1/3]`).
fn center_third(inner: Rect, gap: i32) -> Rect {
    let left_col = h_frac(inner, gap, HSide::Left, 1, 3);
    let right_col = h_frac(inner, gap, HSide::Right, 1, 3);
    let start_x = left_col.x + left_col.width + gap;
    let end_x = right_col.x - gap;
    Rect { x: start_x, y: inner.y, width: (end_x - start_x).max(1), height: inner.height }
}

/// Computes a centered rectangle covering [`ALMOST_PERCENT`]% of the screen's width and height.
fn almost_maximize(screen: Rect, inner: Rect) -> Rect {
    let w = ((screen.width * ALMOST_PERCENT) / 100).clamp(1, inner.width);
    let h = ((screen.height * ALMOST_PERCENT) / 100).clamp(1, inner.height);
    Rect {
        x: screen.x + (screen.width - w) / 2,
        y: screen.y + (screen.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Centers `current` within `screen` while clamping its dimensions to `inner`.
fn center_preserve_size(screen: Rect, current: Rect, inner: Rect) -> Rect {
    let w = if current.width > 0 { current.width.clamp(1, inner.width) } else { inner.width };
    let h = if current.height > 0 { current.height.clamp(1, inner.height) } else { inner.height };
    Rect {
        x: screen.x + (screen.width - w) / 2,
        y: screen.y + (screen.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN_1080P: Rect = Rect { x: 0, y: 0, width: 1920, height: 1080 };
    const FLOATING: Rect = Rect { x: 140, y: 120, width: 900, height: 650 };

    #[test]
    fn left_and_right_cycle_through_halves_and_thirds() {
        let gap = 8;
        let l1 = apply(Action::Left, SCREEN_1080P, FLOATING, gap);
        let l2 = apply(Action::Left, SCREEN_1080P, l1, gap);
        let l3 = apply(Action::Left, SCREEN_1080P, l2, gap);
        let l4 = apply(Action::Left, SCREEN_1080P, l3, gap);
        assert_eq!(l1, l4);
        assert!(l2.width > l1.width);
        assert!(l3.width < l1.width);

        // Opposing 2/3 left and 1/3 right must have exact `gap` between them
        let r1 = apply(Action::Right, SCREEN_1080P, FLOATING, gap);
        let r2 = apply(Action::Right, SCREEN_1080P, r1, gap);
        let r3 = apply(Action::Right, SCREEN_1080P, r2, gap);
        assert_eq!(l2.x + l2.width + gap, r3.x);
        assert_eq!(r3.x + r3.width + gap, SCREEN_1080P.width);
    }

    #[test]
    fn top_and_bottom_cycle_through_halves_and_thirds_without_seams() {
        let odd_screen = Rect { x: 0, y: 0, width: 1920, height: 1051 };
        let gap = 9;
        let t1 = apply(Action::Top, odd_screen, FLOATING, gap);
        let t2 = apply(Action::Top, odd_screen, t1, gap);
        let t3 = apply(Action::Top, odd_screen, t2, gap);
        let b1 = apply(Action::Bottom, odd_screen, FLOATING, gap);
        let b2 = apply(Action::Bottom, odd_screen, b1, gap);
        let b3 = apply(Action::Bottom, odd_screen, b2, gap);

        // Top 1/2 + Bottom 1/2
        assert_eq!(t1.y + t1.height + gap, b1.y);
        assert_eq!(b1.y + b1.height + gap, odd_screen.height);
        // Top 2/3 + Bottom 1/3
        assert_eq!(t2.y + t2.height + gap, b3.y);
        assert_eq!(b3.y + b3.height + gap, odd_screen.height);
        // Top 1/3 + Bottom 2/3
        assert_eq!(t3.y + t3.height + gap, b2.y);
    }

    #[test]
    fn maximize_toggles_between_padded_and_zero_gap_fullscreen() {
        let gap = 8;
        let max_padded = apply(Action::Maximize, SCREEN_1080P, FLOATING, gap);
        assert_eq!(max_padded, Rect { x: 8, y: 8, width: 1904, height: 1064 });
        let max_full = apply(Action::Maximize, SCREEN_1080P, max_padded, gap);
        assert_eq!(max_full, SCREEN_1080P);
        let max_again = apply(Action::Maximize, SCREEN_1080P, max_full, gap);
        assert_eq!(max_again, max_padded);
    }

    #[test]
    fn center_cycles_from_preserved_size_to_center_third_column() {
        let gap = 8;
        let c1 = apply(Action::Center, SCREEN_1080P, FLOATING, gap);
        assert_eq!(c1.width, FLOATING.width);
        assert_eq!(c1.height, FLOATING.height);

        // Pressing Center again when already centered snaps to the middle 1/3 column
        let c2 = apply(Action::Center, SCREEN_1080P, c1, gap);
        let left_third = h_frac(inset(SCREEN_1080P, gap), gap, HSide::Left, 1, 3);
        let right_third = h_frac(inset(SCREEN_1080P, gap), gap, HSide::Right, 1, 3);
        assert_eq!(left_third.x + left_third.width + gap, c2.x);
        assert_eq!(c2.x + c2.width + gap, right_third.x);
        assert!(is_snapped(SCREEN_1080P, c2, gap));
    }

    #[test]
    fn remap_to_screen_preserves_exact_grid_tile_across_monitors() {
        let src = SCREEN_1080P;
        let dst = Rect { x: 1920, y: 0, width: 3840, height: 2160 };
        let src_gap = 8;
        let dst_gap = 12;

        // Window snapped to Left 2/3 on 1080p monitor
        let l1 = apply(Action::Left, src, FLOATING, src_gap);
        let l2_src = apply(Action::Left, src, l1, src_gap);
        let remapped = remap_to_screen(l2_src, src, src_gap, dst, dst_gap);
        let dst_l1 = apply(Action::Left, dst, FLOATING, dst_gap);
        let expected_dst_l2 = apply(Action::Left, dst, dst_l1, dst_gap);
        assert_eq!(remapped, Some(expected_dst_l2));
        assert!(remap_to_screen(FLOATING, src, src_gap, dst, dst_gap).is_none());
    }
}
