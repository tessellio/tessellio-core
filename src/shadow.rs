//! The RGBA shadow: a row-granular mirror of the reference surface at
//! capture fidelity.
//!
//! Change detection compares 16-bit cells, but the capture arrives as RGBA —
//! and a row whose RGBA is identical to a row that already fed the reference
//! cannot differ from that reference at cell fidelity (RGBA-equal ⇒
//! RGB565-equal). The shadow holds, per row, RGBA bytes whose conversion is
//! exactly the reference's cells, so detection can settle most rows with one
//! memcmp and pay the conversion only where the capture actually moved.
//!
//! The invariant is **per row and earned**: a row's shadow is valid only after
//! an update the receiver applied wrote that row's cells, and it is withdrawn
//! by anything the encoder cannot mirror (a rebase reconstructs the reference
//! from 16-bit undo values, whose RGBA originals are gone). An invalid row
//! costs the full convert-and-compare path — the same path the encoder ran
//! before the shadow existed — never a wrong answer.

/// A row-granular RGBA mirror of the reference surface.
#[derive(Debug)]
pub struct RgbaShadow {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    /// One validity bit per row (`1` = the row's RGBA mirrors the reference's
    /// cells).
    valid: Vec<u64>,
}

impl RgbaShadow {
    /// A shadow for a `width × height` surface, every row invalid.
    pub fn new(width: u32, height: u32) -> Self {
        let row_bytes = width as usize * 4;
        Self {
            width,
            height,
            rgba: vec![0_u8; row_bytes * height as usize],
            valid: vec![0_u64; (height as usize).div_ceil(64)],
        }
    }

    /// Whether the shadow describes a `width × height` surface.
    pub fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width && self.height == height
    }

    /// Whether row `y`'s RGBA mirrors the reference's cells.
    pub fn row_valid(&self, y: u32) -> bool {
        (y as usize) < self.height as usize
            && self.valid[y as usize / 64] & (1_u64 << (y as usize % 64)) != 0
    }

    /// Row `y`'s RGBA bytes (`width × 4`); meaningless when the row is
    /// invalid.
    pub fn row(&self, y: u32) -> &[u8] {
        let row_bytes = self.width as usize * 4;
        &self.rgba[y as usize * row_bytes..(y as usize + 1) * row_bytes]
    }

    /// Withdraw every row: the shadow no longer mirrors anything.
    pub fn invalidate(&mut self) {
        self.valid.fill(0);
    }

    /// Withdraw one row.
    pub fn invalidate_row(&mut self, y: u32) {
        if (y as usize) < self.height as usize {
            self.valid[y as usize / 64] &= !(1_u64 << (y as usize % 64));
        }
    }

    /// Mark row `y` valid with `rgba` (exactly `width × 4` bytes). The caller
    /// vouches that the row's conversion equals the reference's cells — which
    /// is what a committed write to that row's cells establishes.
    pub fn set_row(&mut self, y: u32, rgba: &[u8]) {
        self.write_row(y, rgba);
        self.valid[y as usize / 64] |= 1_u64 << (y as usize % 64);
    }

    /// Overwrite the columns `x..x + cells` of row `y` with `columns` (the
    /// RGBA bytes of those cells), keeping the row's validity bit.
    ///
    /// A partial write is how a span-shaped commit maintains the mirror: the
    /// untouched columns keep the bytes the invariant already vouches for,
    /// and the written columns carry the bytes the same commit put in the
    /// reference.
    pub fn write_columns(&mut self, y: u32, x: u32, cells: usize, columns: &[u8]) {
        debug_assert_eq!(columns.len(), cells * 4, "column bytes match the cells");
        let start = y as usize * self.width as usize * 4 + x as usize * 4;
        self.rgba[start..start + columns.len()].copy_from_slice(columns);
    }

    fn write_row(&mut self, y: u32, rgba: &[u8]) {
        let row_bytes = self.width as usize * 4;
        debug_assert_eq!(rgba.len(), row_bytes, "a full row's RGBA");
        self.rgba[y as usize * row_bytes..(y as usize + 1) * row_bytes].copy_from_slice(rgba);
    }
}

/// The RGBA rows the outstanding plan retained, for the shadow's commit-time
/// maintenance.
///
/// A plan's writes are 16-bit cells, and the shadow needs the RGBA those
/// cells came from — which only the capture still holds. The encoder retains
/// the RGBA of every row that entered the workspace while planning (the rows
/// writes can name), and the commit replays the writes against the shadow
/// from it. Rows the plan did not retain (a rebase, a discard) simply
/// withdraw their shadow validity.
#[derive(Debug, Default)]
pub struct PendingRgba {
    /// Backing buffer: whole rows, back to back.
    buffer: Vec<u8>,
    /// `(row, byte offset in the buffer)` per retained row; ascending once
    /// [`Self::ensure_sorted`] has run (pushes are ascending in the common
    /// path, but a repair pass can append an earlier row late).
    rows: Vec<(u32, usize)>,
    row_bytes: usize,
    sorted: bool,
}

impl PendingRgba {
    /// Drop every retained row (called when a plan is abandoned or consumed).
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.rows.clear();
        self.row_bytes = 0;
        self.sorted = true;
    }

    /// Retain `rgba` (one full row) for `row`, in capture order. Re-adding a
    /// row keeps the latest copy and drops the earlier one.
    ///
    /// Rows arrive ascending (the capture walks the frame top-down, and the
    /// parallel workers own disjoint ascending bands), so a repeat is the row
    /// just pushed and the general case is a binary search — a frame that
    /// retains every row of a 1080p surface does just over a thousand
    /// lookups, not half a million.
    pub fn push_row(&mut self, row: u32, rgba: &[u8]) {
        if let Some(last) = self.rows.last()
            && last.0 >= row {
                self.sorted = false;
            }
        if let Some(offset) = self.offset_of(row) {
            // A repeat means a repair pass re-converted the row, and the
            // later capture bytes are the ones the plan's writes will name.
            self.buffer[offset..offset + rgba.len()].copy_from_slice(rgba);
            return;
        }
        let offset = self.buffer.len();
        self.buffer.extend_from_slice(rgba);
        self.rows.push((row, offset));
        self.row_bytes = rgba.len();
    }

    /// The retained RGBA of `row`, if the plan kept it. Sorting on first
    /// read is why this takes `&mut self`: the plan is done pushing by the
    /// time anything reads.
    pub fn row(&mut self, row: u32) -> Option<&[u8]> {
        let offset = self.offset_of(row)?;
        Some(&self.buffer[offset..offset + self.row_bytes])
    }

    /// The buffer offset of `row`'s retention, if the plan kept it.
    fn offset_of(&mut self, row: u32) -> Option<usize> {
        // The common repeat is the row just pushed (a repair pass).
        if self.rows.last().is_some_and(|(kept, _)| *kept == row) {
            return self.rows.last().map(|(_, offset)| *offset);
        }
        if !self.sorted {
            self.rows.sort_unstable_by_key(|(kept, _)| *kept);
            self.sorted = true;
        }
        self.rows
            .binary_search_by(|(kept, _)| kept.cmp(&row))
            .ok()
            .map(|index| self.rows[index].1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(y: u32, width: u32) -> Vec<u8> {
        vec![y as u8; width as usize * 4]
    }

    #[test]
    fn rows_start_invalid_and_earn_validity() {
        let mut shadow = RgbaShadow::new(4, 130);
        for y in 0..130 {
            assert!(!shadow.row_valid(y), "row {y} starts invalid");
        }
        shadow.set_row(3, &row(3, 4));
        assert!(shadow.row_valid(3));
        assert!(!shadow.row_valid(4));
        shadow.invalidate_row(3);
        assert!(!shadow.row_valid(3));
        shadow.set_row(70, &row(70, 4));
        shadow.invalidate();
        assert!(!shadow.row_valid(70), "invalidate withdraws every row");
    }

    #[test]
    fn partial_writes_keep_the_row_valid() {
        let mut shadow = RgbaShadow::new(8, 1);
        shadow.set_row(0, &row(0, 8));
        shadow.write_columns(0, 2, 3, &[9_u8; 12]);
        assert!(shadow.row_valid(0));
        let bytes = shadow.row(0);
        assert_eq!(&bytes[0..4], &[0, 0, 0, 0]);
        assert_eq!(&bytes[8..20], &[9; 12]);
        assert_eq!(&bytes[20..24], &[0, 0, 0, 0]);
    }

    #[test]
    fn pending_rows_are_kept_in_order_and_replaced() {
        let mut pending = PendingRgba::default();
        pending.push_row(2, &[2_u8; 8]);
        pending.push_row(5, &[5_u8; 8]);
        assert_eq!(pending.row(2), Some(&[2_u8; 8][..]));
        assert_eq!(pending.row(5), Some(&[5_u8; 8][..]));
        assert_eq!(pending.row(3), None);
        pending.push_row(5, &[7_u8; 8]);
        assert_eq!(pending.row(5), Some(&[7_u8; 8][..]), "the later copy wins");
        pending.clear();
        assert_eq!(pending.row(2), None);
    }
}
