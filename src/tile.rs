//
// Operates on physical pixel coordinates with a unified gap model: the outer margin
// around the screen edges equals the inner seam between adjacent tiled windows.
// Complement arithmetic guarantees that opposing tiles (`1/2 + 1/2` or `2/3 + 1/3`)
// sum to the exact monitor width with zero 1px rounding gaps on odd resolutions.

// Axis-aligned rectangle in physical screen coordinates (origin at top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

// Tiling layout actions.
//
// [`Action::Left`] and [`Action::Right`] cycle through `1/2 -> 2/3 -> 1/3` widths
// when invoked repeatedly on an already-snapped window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Maximize,
    Almost,
    Center,
}

// Maximum per-axis deviation (in physical pixels) when matching current window bounds
// against a layout preset. Absorbs ±1-2px rounding shifts from DWM extended frame
// bounds on fractional DPI scaling (125%, 150%, 175%).
const TOLERANCE: u32 = 2;

impl Rect {
    // Returns `true` if all four components are within [`TOLERANCE`] pixels of `other`.
    #[must_use]
    pub const fn approx_eq(self, other: Self) -> bool {
        self.x.abs_diff(other.x) <= TOLERANCE
            && self.y.abs_diff(other.y) <= TOLERANCE
            && self.width.abs_diff(other.width) <= TOLERANCE
            && self.height.abs_diff(other.height) <= TOLERANCE
    }
}

// Computes the target [`Rect`] for a window given a tiling [`Action`].
#[must_use]
pub fn apply(action: Action, screen: Rect, current: Rect, gap: i32) -> Rect {
    let g = gap.max(0);
    match action {
        Action::Left => cycle_h(screen, current, g, true),
        Action::Right => cycle_h(screen, current, g, false),
        Action::Top => v_split(screen, g, true),
        Action::Bottom => v_split(screen, g, false),
        Action::TopLeft => quarter(screen, g, true, true),
        Action::TopRight => quarter(screen, g, false, true),
        Action::BottomLeft => quarter(screen, g, true, false),
        Action::BottomRight => quarter(screen, g, false, false),
        Action::Maximize => inset(screen, g),
        Action::Almost => almost(screen, g),
        Action::Center => center(screen, current),
    }
}

// Returns `true` if `current` matches any tiling slot on `screen` within [`TOLERANCE`].
//
// Used by the restore buffer: only un-snapped ("floating") window bounds are recorded
// as a new restore point, preventing subsequent tile actions from overwriting the original size.
// shortcut: misses windows whose min-size constraint exceeds a 1/3 slot, upgrade if tracking state by HWND.
#[must_use]
pub fn is_snapped(screen: Rect, current: Rect, gap: i32) -> bool {
    let g = gap.max(0);
    [
        h_frac(screen, g, 1, 2, true),
        h_frac(screen, g, 2, 3, true),
        h_frac(screen, g, 1, 3, true),
        h_frac(screen, g, 1, 2, false),
        h_frac(screen, g, 2, 3, false),
        h_frac(screen, g, 1, 3, false),
        v_split(screen, g, true),
        v_split(screen, g, false),
        quarter(screen, g, true, true),
        quarter(screen, g, false, true),
        quarter(screen, g, true, false),
        quarter(screen, g, false, false),
        inset(screen, g),
        almost(screen, g),
    ]
    .into_iter()
    .any(|preset| current.approx_eq(preset))
}

// Shrinks `screen` by `gap` on all four sides, clamping dimensions to at least `1px`.
fn inset(screen: Rect, gap: i32) -> Rect {
    Rect {
        x: screen.x + gap,
        y: screen.y + gap,
        width: (screen.width - gap * 2).max(1),
        height: (screen.height - gap * 2).max(1),
    }
}

// Stateless cycle detector for left/right docked windows: `1/2 -> 2/3 -> 1/3 -> 1/2`.
fn cycle_h(screen: Rect, current: Rect, gap: i32, is_left: bool) -> Rect {
    let half = h_frac(screen, gap, 1, 2, is_left);
    let two_thirds = h_frac(screen, gap, 2, 3, is_left);
    let one_third = h_frac(screen, gap, 1, 3, is_left);

    if current.approx_eq(half) {
        two_thirds
    } else if current.approx_eq(two_thirds) {
        one_third
    } else {
        half
    }
}

// Computes a horizontal fraction (`num / den`) of the usable inner area.
//
// The right tile is derived from the opposing left tile (`opp_left`) so that
// complementary splits (e.g., left `2/3` + right `1/3`) absorb integer division
// remainders and always meet with an exact `gap` seam.
fn h_frac(screen: Rect, gap: i32, num: i32, den: i32, is_left: bool) -> Rect {
    let area = inset(screen, gap);
    let avail = (area.width - gap).max(den);

    if is_left {
        Rect { width: (avail * num) / den, ..area }
    } else {
        let opp_left = (avail * (den - num)) / den;
        Rect { x: area.x + opp_left + gap, width: avail - opp_left, ..area }
    }
}

// Splits the inner work area vertically into top and bottom halves separated by `gap`.
fn v_split(screen: Rect, gap: i32, is_top: bool) -> Rect {
    let area = inset(screen, gap);
    let avail = (area.height - gap).max(2);
    let top_h = avail / 2;

    if is_top {
        Rect { height: top_h, ..area }
    } else {
        Rect { y: area.y + top_h + gap, height: avail - top_h, ..area }
    }
}

// Combines horizontal `1/2` and vertical `1/2` splits into a corner quadrant.
fn quarter(screen: Rect, gap: i32, is_left: bool, is_top: bool) -> Rect {
    let h = h_frac(screen, gap, 1, 2, is_left);
    let v = v_split(screen, gap, is_top);
    Rect { x: h.x, y: v.y, width: h.width, height: v.height }
}

// Computes a centered `85% x 85%` rectangle inside the inset work area.
fn almost(screen: Rect, gap: i32) -> Rect {
    let area = inset(screen, gap);
    center(
        screen,
        Rect { width: (area.width * 85) / 100, height: (area.height * 85) / 100, ..area },
    )
}

// Centers `current` on `screen` without changing its size, clamping oversized
// dimensions to the screen so the title bar stays reachable.
fn center(screen: Rect, current: Rect) -> Rect {
    let width = current.width.min(screen.width);
    let height = current.height.min(screen.height);
    Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { x: 0, y: 0, width: 1920, height: 1080 };
    const DUMMY: Rect = Rect { x: 100, y: 100, width: 400, height: 300 };

    #[test]
    fn left_and_right_cycles_1_2_to_2_3_to_1_3() {
        let l1 = apply(Action::Left, SCREEN, DUMMY, 8);
        let l2 = apply(Action::Left, SCREEN, l1, 8);
        let l3 = apply(Action::Left, SCREEN, l2, 8);
        let l4 = apply(Action::Left, SCREEN, l3, 8);

        assert!(l2.width > l1.width);
        assert!(l3.width < l1.width);
        assert_eq!(l1, l4);

        let r1 = apply(Action::Right, SCREEN, DUMMY, 8);
        let r2 = apply(Action::Right, SCREEN, r1, 8);
        let r3 = apply(Action::Right, SCREEN, r2, 8);
        assert!(r2.width > r1.width);
        assert!(r3.width < r1.width);
    }

    #[test]
    fn odd_resolution_and_gaps_align_to_the_pixel() {
        let odd_screen = Rect { x: 10, y: 20, width: 1365, height: 767 };
        let gap = 9;

        let left = apply(Action::Left, odd_screen, DUMMY, gap);
        let right = apply(Action::Right, odd_screen, DUMMY, gap);

        assert_eq!(left.x - odd_screen.x, gap);
        assert_eq!(right.x - (left.x + left.width), gap);
        assert_eq!((odd_screen.x + odd_screen.width) - (right.x + right.width), gap);

        let left_2_3 = apply(Action::Left, odd_screen, left, gap);
        let right_half = apply(Action::Right, odd_screen, DUMMY, gap);
        let right_2_3 = apply(Action::Right, odd_screen, right_half, gap);
        let right_1_3 = apply(Action::Right, odd_screen, right_2_3, gap);
        assert_eq!(right_1_3.x - (left_2_3.x + left_2_3.width), gap);
    }

    #[test]
    fn center_preserves_window_size() {
        let c = apply(Action::Center, SCREEN, DUMMY, 8);
        assert_eq!(c.width, DUMMY.width);
        assert_eq!(c.height, DUMMY.height);
        assert_eq!(c.x, (1920 - 400) / 2);
        assert_eq!(c.y, (1080 - 300) / 2);
    }

    #[test]
    fn snapped_detection_works_within_tolerance() {
        let left = apply(Action::Left, SCREEN, DUMMY, 8);
        let slightly_off = Rect { x: left.x + 2, ..left };
        assert!(is_snapped(SCREEN, slightly_off, 8));
        assert!(!is_snapped(SCREEN, DUMMY, 8));
    }
}
