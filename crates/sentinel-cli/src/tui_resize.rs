//! Shared math for the resizable-panel feature of both TUIs (`inspect`
//! and `query monitor`). Pure functions over percentages and cell
//! coordinates, no ratatui or terminal state, so they unit-test without
//! a backend. The glue (hit-testing against stored areas, mouse capture
//! toggle) lives in `tui.rs` / `monitor.rs`.

/// Smallest share any panel keeps when a boundary is dragged, so a panel
/// can never collapse to nothing.
pub const MIN_PCT: u16 = 10;

/// Which split boundary a drag is currently moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// A boundary identified by its axis and its index among that axis's
/// internal cuts (boundary `b` sits between segment `b` and `b + 1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DragTarget {
    pub axis: Axis,
    pub boundary: usize,
}

/// Move `boundary` so its cumulative cut position becomes `cut_pct`,
/// taking from / giving to only the two adjacent segments. The total
/// stays at 100 and both touched segments stay >= `min_pct`. No-op when
/// the pair has no room to keep both above the minimum.
pub fn set_cut(segments: &mut [u16], boundary: usize, cut_pct: u16, min_pct: u16) {
    if boundary + 1 >= segments.len() {
        return;
    }
    let before: u16 = segments[..boundary].iter().sum();
    let pair_sum = segments[boundary] + segments[boundary + 1];
    if pair_sum < 2 * min_pct {
        return;
    }
    // Clamp in cumulative-position space, then split the pair around it.
    let cut = cut_pct.clamp(before + min_pct, before + pair_sum - min_pct);
    segments[boundary] = cut - before;
    segments[boundary + 1] = pair_sum - segments[boundary];
}

/// Cell position of `boundary`'s line within a container that starts at
/// `start` and spans `len` cells. Used to hit-test a mouse against the
/// border (within a 1-cell tolerance).
#[must_use]
pub fn boundary_cell(segments: &[u16], boundary: usize, start: u16, len: u16) -> u16 {
    // No-op on an out-of-range boundary, matching `set_cut` (a panic on a
    // shared primitive is worse than returning the container origin).
    if boundary >= segments.len() {
        return start;
    }
    let cum: u32 = segments[..=boundary].iter().map(|&s| u32::from(s)).sum();
    start + u16::try_from(u32::from(len) * cum / 100).unwrap_or(len)
}

/// Inverse of `boundary_cell`: the percentage a cursor at cell `pos`
/// represents within `[start, start + len)`. Clamped to `0..=100`.
#[must_use]
pub fn pos_to_pct(pos: u16, start: u16, len: u16) -> u16 {
    if len == 0 {
        return 0;
    }
    let rel = u32::from(pos.saturating_sub(start));
    u16::try_from((rel * 100 / u32::from(len)).min(100)).unwrap_or(100)
}

/// True when `a` is within one cell of `b` (border hit tolerance).
#[must_use]
pub fn near(a: u16, b: u16) -> bool {
    a.abs_diff(b) <= 1
}

/// True when cell `v` falls inside `[start, start + len)`.
#[must_use]
pub fn in_range(v: u16, start: u16, len: u16) -> bool {
    v >= start && v < start.saturating_add(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_cut_two_segments_keeps_sum() {
        let mut seg = [50, 50];
        set_cut(&mut seg, 0, 30, MIN_PCT);
        assert_eq!(seg, [30, 70]);
        assert_eq!(seg[0] + seg[1], 100);
    }

    #[test]
    fn set_cut_three_segments_only_moves_the_pair() {
        // Default inspect columns; drag the Findings|Correlations boundary
        // (boundary 1) right. Traces (segment 0) must not move.
        let mut seg = [20, 30, 50];
        set_cut(&mut seg, 1, 70, MIN_PCT);
        assert_eq!(seg[0], 20, "non-adjacent segment unchanged");
        assert_eq!(seg.iter().sum::<u16>(), 100);
        assert_eq!(seg, [20, 50, 30]);
    }

    #[test]
    fn set_cut_clamps_to_min_on_both_sides() {
        let mut seg = [20, 30, 50];
        // Drag boundary 0 hard left, past the floor.
        set_cut(&mut seg, 0, 0, MIN_PCT);
        assert_eq!(seg[0], MIN_PCT);
        assert_eq!(seg[0] + seg[1], 50);
        // And hard right, past the neighbour's floor.
        set_cut(&mut seg, 0, 100, MIN_PCT);
        assert_eq!(seg[1], MIN_PCT);
        assert_eq!(seg[0] + seg[1], 50);
    }

    #[test]
    fn set_cut_noop_on_last_or_out_of_range_boundary() {
        let mut seg = [50, 50];
        set_cut(&mut seg, 1, 30, MIN_PCT);
        assert_eq!(seg, [50, 50]);
        set_cut(&mut seg, 9, 30, MIN_PCT);
        assert_eq!(seg, [50, 50]);
    }

    #[test]
    fn boundary_cell_out_of_range_returns_start() {
        // Defensive: a bad boundary must not panic, it returns the origin.
        assert_eq!(boundary_cell(&[50, 50], 5, 10, 100), 10);
    }

    #[test]
    fn boundary_cell_and_pos_to_pct_round_trip() {
        // 100-cell container at origin: boundary 0 of [40,60] sits at 40.
        assert_eq!(boundary_cell(&[40, 60], 0, 0, 100), 40);
        assert_eq!(pos_to_pct(40, 0, 100), 40);
        // Offset container: boundary cell shifts by start.
        assert_eq!(boundary_cell(&[50, 50], 0, 10, 100), 60);
        assert_eq!(pos_to_pct(60, 10, 100), 50);
    }

    #[test]
    fn pos_to_pct_handles_zero_len_and_clamps() {
        assert_eq!(pos_to_pct(5, 0, 0), 0);
        assert_eq!(pos_to_pct(0, 10, 100), 0); // below start
        assert_eq!(pos_to_pct(500, 0, 100), 100); // above end
    }

    #[test]
    fn near_and_in_range() {
        assert!(near(10, 11));
        assert!(near(10, 9));
        assert!(!near(10, 12));
        assert!(in_range(10, 10, 5));
        assert!(in_range(14, 10, 5));
        assert!(!in_range(15, 10, 5));
        assert!(!in_range(9, 10, 5));
    }
}
