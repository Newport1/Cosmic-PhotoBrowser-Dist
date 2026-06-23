#[allow(dead_code)]
pub fn loupe_step(len: usize, cursor: usize, forward: bool, wrap: bool) -> usize {
    if len <= 1 {
        return 0;
    }

    let cursor = cursor.min(len - 1);
    if forward {
        if cursor + 1 < len {
            cursor + 1
        } else if wrap {
            0
        } else {
            len - 1
        }
    } else if cursor > 0 {
        cursor - 1
    } else if wrap {
        len - 1
    } else {
        0
    }
}

#[allow(dead_code)]
pub fn grid_step(idx: usize, len: usize, cols: usize, dx: i32, dy: i32) -> usize {
    if len == 0 {
        return 0;
    }

    let cols = cols.max(1);
    let idx = idx.min(len - 1);
    let row = idx / cols;
    let col = idx % cols;
    let max_row = (len - 1) / cols;

    if dy < 0 && dy.unsigned_abs() as usize > row {
        return 0;
    }
    if dy > 0 && row.saturating_add(dy as usize) > max_row {
        return len - 1;
    }

    let new_row = row.saturating_add_signed(dy as isize).min(max_row);
    let row_start = new_row * cols;
    let row_end = (row_start + cols - 1).min(len - 1);
    let new_col = col.saturating_add_signed(dx as isize).min(cols - 1);

    (row_start + new_col).clamp(row_start, row_end)
}

#[allow(dead_code)]
pub fn page_step(idx: usize, len: usize, cols: usize, rows: usize, down: bool) -> usize {
    let rows = rows.max(1);
    let dy = if down { rows as i32 } else { -(rows as i32) };
    grid_step(idx, len, cols, 0, dy)
}

#[cfg(test)]
mod tests {
    use super::{grid_step, loupe_step, page_step};

    #[test]
    fn loupe_step_saturates_without_wrap() {
        assert_eq!(loupe_step(5, 0, false, false), 0);
        assert_eq!(loupe_step(5, 4, true, false), 4);
        assert_eq!(loupe_step(5, 2, true, false), 3);
        assert_eq!(loupe_step(5, 2, false, false), 1);
    }

    #[test]
    fn loupe_step_wraps_at_edges() {
        assert_eq!(loupe_step(5, 4, true, true), 0);
        assert_eq!(loupe_step(5, 0, false, true), 4);
    }

    #[test]
    fn loupe_step_len0_or_len1_returns_zero() {
        assert_eq!(loupe_step(0, 99, true, true), 0);
        assert_eq!(loupe_step(1, 99, false, false), 0);
    }

    #[test]
    fn grid_step_clamps_at_left_and_right_edges() {
        assert_eq!(grid_step(0, 6, 3, -1, 0), 0);
        assert_eq!(grid_step(2, 6, 3, 1, 0), 2);
        assert_eq!(grid_step(4, 6, 3, 1, 0), 5);
    }

    #[test]
    fn grid_step_clamps_at_top_and_bottom_edges() {
        assert_eq!(grid_step(1, 6, 3, 0, -1), 0);
        assert_eq!(grid_step(4, 6, 3, 0, 1), 5);
    }

    #[test]
    fn grid_step_cols1_moves_vertically_and_clamps() {
        assert_eq!(grid_step(2, 4, 1, 1, 0), 2);
        assert_eq!(grid_step(2, 4, 1, 0, 1), 3);
        assert_eq!(grid_step(2, 4, 1, 0, -1), 1);
    }

    #[test]
    fn grid_step_len0_returns_zero() {
        assert_eq!(grid_step(0, 0, 3, 1, 1), 0);
    }

    #[test]
    fn page_step_moves_by_full_rows() {
        assert_eq!(page_step(5, 30, 4, 3, true), 17);
        assert_eq!(page_step(17, 30, 4, 3, false), 5);
    }

    #[test]
    fn page_step_clamps_at_top_and_bottom() {
        assert_eq!(page_step(5, 30, 4, 3, false), 0);
        assert_eq!(page_step(21, 30, 4, 3, true), 29);
    }
}
