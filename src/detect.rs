//! Change detection: exact at wire fidelity.
//!
//! The comparison is word-wise against the encoder's the 16-bit cell format reference, so a
//! cell that differs at wire precision is always sent and a cell that does not
//! differ is never sent. The pipeline is: 64 px dirty-tile map → greedy disjoint
//! rectangles → exact changed-cell bounding boxes → merge to the region cap →
//! per-row changed spans. Every step is deterministic, and no step clones a
//! frame.

use crate::buffer::Surface;
use crate::format::{MAX_SURFACE_DIMENSION, Rect};
#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Change-detection tile size in cells.
///
/// The tile size only shapes rectangle *growth*; the payload is always exact
/// changed cells, so the tile size can no longer leak into the byte counts.
pub const TILE: u32 = 64;

/// Piece-count ceiling while removing rectangle overlaps.
const DISJOINT_PIECE_LIMIT: usize = 256;

/// One maximal run of changed cells inside a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// First column of the run.
    pub x: u32,
    /// Run length in cells.
    pub len: u32,
}

/// Dirty-tile map for one frame pair.
#[derive(Debug, Clone, Default)]
pub struct ChangeMap {
    tiles_x: u32,
    tiles_y: u32,
    bits: Vec<u64>,
    dirty_tiles: u32,
    upper_bound_cells: u64,
}

impl ChangeMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Compare `current` with `reference` and record the dirty tiles.
    ///
    /// Without a comparably sized reference every tile is dirty: "unknown" is
    /// never treated as "unchanged".
    pub fn detect(&mut self, current: &Surface, reference: Option<&Surface>) {
        self.tiles_x = current.width().div_ceil(TILE);
        self.tiles_y = current.height().div_ceil(TILE);
        let tile_count = (self.tiles_x * self.tiles_y) as usize;
        self.bits.clear();
        self.bits.resize(tile_count.div_ceil(64), 0);
        self.dirty_tiles = 0;
        self.upper_bound_cells = 0;

        let reference =
            reference.filter(|buffer| buffer.matches(current.width(), current.height()));
        let Some(reference) = reference else {
            for index in 0..tile_count {
                self.bits[index / 64] |= 1 << (index % 64);
                self.dirty_tiles += 1;
            }
            self.upper_bound_cells = current.cell_count() as u64;
            return;
        };

        #[cfg(feature = "parallel")]
        {
            let tiles_x = self.tiles_x;
            let tiles_y = self.tiles_y;
            let width = current.width();
            let height = current.height();
            let results: Vec<(u32, u32, bool, u64)> = (0..tiles_y)
                .into_par_iter()
                .flat_map(|ty| {
                    let y0 = ty * TILE;
                    let y1 = (y0 + TILE).min(height);
                    (0..tiles_x).map(move |tx| {
                        let x0 = tx * TILE;
                        let x1 = (x0 + TILE).min(width);
                        let mut changed = false;
                        for y in y0..y1 {
                            let left = &current.row(y)[x0 as usize..x1 as usize];
                            let right = &reference.row(y)[x0 as usize..x1 as usize];
                            if left != right {
                                changed = true;
                                break;
                            }
                        }
                        let cells = if changed { u64::from(x1 - x0) * u64::from(y1 - y0) } else { 0 };
                        (tx, ty, changed, cells)
                    }).collect::<Vec<_>>()
                })
                .collect();
            for (tx, ty, changed, cells) in results {
                if changed {
                    let index = (ty * tiles_x + tx) as usize;
                    self.bits[index / 64] |= 1 << (index % 64);
                    self.dirty_tiles += 1;
                    self.upper_bound_cells += cells;
                }
            }
        }
        #[cfg(not(feature = "parallel"))]
        {
        for ty in 0..self.tiles_y {
            let y0 = ty * TILE;
            let y1 = (y0 + TILE).min(current.height());
            for tx in 0..self.tiles_x {
                let x0 = tx * TILE;
                let x1 = (x0 + TILE).min(current.width());
                let mut changed = false;
                for y in y0..y1 {
                    let left = &current.row(y)[x0 as usize..x1 as usize];
                    let right = &reference.row(y)[x0 as usize..x1 as usize];
                    if left != right {
                        changed = true;
                        break;
                    }
                }
                if changed {
                    let index = (ty * self.tiles_x + tx) as usize;
                    self.bits[index / 64] |= 1 << (index % 64);
                    self.dirty_tiles += 1;
                    self.upper_bound_cells += u64::from(x1 - x0) * u64::from(y1 - y0);
                }
            }
        }
        }
    }

    /// Prepare the map for a frame whose conversion happens row by row.
    ///
    /// Every tile is marked dirty up front when no same-geometry reference
    /// exists — "unknown" is never treated as "unchanged" — and the returned
    /// reference is `None` then, because no comparison is needed at all.
    /// Otherwise the caller converts each row and hands it to
    /// [`Self::compare_row`], which performs exactly the comparison
    /// [`Self::detect`] performs, only interleaved with the conversion so the
    /// freshly converted cells are still in cache when they are compared.
    pub fn prepare<'a>(
        &mut self,
        current: &Surface,
        reference: Option<&'a Surface>,
    ) -> Option<&'a Surface> {
        self.prepare_grid(current.width(), current.height());
        let reference =
            reference.filter(|buffer| buffer.matches(current.width(), current.height()));
        let Some(reference) = reference else {
            for index in 0..(self.tiles_x * self.tiles_y) as usize {
                self.bits[index / 64] |= 1 << (index % 64);
                self.dirty_tiles += 1;
            }
            self.upper_bound_cells = current.cell_count() as u64;
            return None;
        };
        Some(reference)
    }

    /// Prepare the map for a grid of `width`×`height` cells.
    ///
    /// Every tile starts clean and the counters start at zero: a map that has
    /// not compared anything yet holds no change. The caller compares the rows
    /// itself, which is what lets several workers compare disjoint bands of
    /// one frame and merge their maps afterwards.
    pub(crate) fn prepare_grid(&mut self, width: u32, height: u32) {
        self.tiles_x = width.div_ceil(TILE);
        self.tiles_y = height.div_ceil(TILE);
        let tile_count = (self.tiles_x * self.tiles_y) as usize;
        self.bits.clear();
        self.bits.resize(tile_count.div_ceil(64), 0);
        self.dirty_tiles = 0;
        self.upper_bound_cells = 0;
    }

    /// Merge another map's dirty tiles into this one.
    ///
    /// The two maps must describe the same tile grid and cover disjoint tiles:
    /// the counters add and the tile bits combine. Two maps that cover the
    /// same tile would count it twice, which is why the merge is the
    /// counterpart of comparing disjoint bands of rows.
    pub(crate) fn merge_from(&mut self, other: &ChangeMap) {
        debug_assert_eq!(self.tiles_x, other.tiles_x);
        debug_assert_eq!(self.tiles_y, other.tiles_y);
        for (index, word) in other.bits.iter().enumerate() {
            self.bits[index] |= *word;
        }
        self.dirty_tiles += other.dirty_tiles;
        self.upper_bound_cells += other.upper_bound_cells;
    }

    /// Compare one freshly converted row against `reference` and mark the tiles
    /// of its band that hold at least one differing cell.
    ///
    /// `current_row` is the row the caller just converted, held in a row-sized
    /// scratch buffer rather than in the workspace. Identical in result to the
    /// matching rows of [`Self::detect`].
    ///
    /// Returns whether the row must reach the workspace: `true` when any tile
    /// covering the row is dirty after this comparison — the row itself holds a
    /// differing cell, or an earlier row of the same tile band did — and
    /// `false` when the row is entirely clean and no tile of its band is dirty,
    /// in which case no rectangle can cover it and it can stay in the scratch.
    pub fn compare_row(&mut self, current_row: &[u16], reference: &Surface, y: u32) -> bool {
        let ty = y / TILE;
        if ty >= self.tiles_y || y >= reference.height() {
            return false;
        }
        let width = current_row.len().min(reference.width() as usize);
        let current = &current_row[..width];
        let reference_row = &reference.row(y)[..width];
        // The cheap path for the common clean row: one wide comparison; only a
        // row that differs needs the per-tile scan.
        if current == reference_row {
            return self.band_has_dirty(ty);
        }
        let band_rows = TILE.min(reference.height() - ty * TILE);
        for tx in 0..self.tiles_x {
            let index = (ty * self.tiles_x + tx) as usize;
            if self.bits[index / 64] & (1 << (index % 64)) != 0 {
                continue;
            }
            let x0 = tx * TILE;
            let x1 = (x0 + TILE).min(width as u32);
            if current_row[x0 as usize..x1 as usize] != reference_row[x0 as usize..x1 as usize] {
                self.bits[index / 64] |= 1 << (index % 64);
                self.dirty_tiles += 1;
                self.upper_bound_cells += u64::from(x1 - x0) * u64::from(band_rows);
            }
        }
        true
    }

    /// Whether any tile of band `ty` is dirty.
    fn band_has_dirty(&self, ty: u32) -> bool {
        if ty >= self.tiles_y || self.dirty_tiles == 0 {
            return false;
        }
        let first = (ty * self.tiles_x) as usize;
        let last = first + self.tiles_x as usize;
        for index in first..last {
            if self.bits[index / 64] & (1 << (index % 64)) != 0 {
                return true;
            }
        }
        false
    }

    /// Whether the tile at `(tx, ty)` is dirty.
    pub fn is_dirty(&self, tx: u32, ty: u32) -> bool {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return false;
        }
        let index = (ty * self.tiles_x + tx) as usize;
        self.bits[index / 64] & (1 << (index % 64)) != 0
    }

    /// Number of dirty tiles.
    pub fn dirty_tiles(&self) -> u32 {
        self.dirty_tiles
    }

    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.dirty_tiles == 0
    }

    /// Upper bound on the changed cell count (the dirty tile area).
    pub fn upper_bound_cells(&self) -> u64 {
        self.upper_bound_cells
    }

    /// Tile grid width.
    pub fn tiles_x(&self) -> u32 {
        self.tiles_x
    }

    /// Tile grid height.
    pub fn tiles_y(&self) -> u32 {
        self.tiles_y
    }
}

/// Greedy rectangle growth over the dirty-tile map.
///
/// Rectangles are disjoint and cover every dirty tile: the top-left scan order
/// extends right along a run of dirty tiles and then down while the whole
/// segment stays dirty.
pub fn rects_from_map(
    map: &ChangeMap,
    width: u32,
    height: u32,
    visited: &mut Vec<bool>,
    out: &mut Vec<Rect>,
) {
    let tiles_x = map.tiles_x();
    let tiles_y = map.tiles_y();
    visited.clear();
    visited.resize((tiles_x * tiles_y) as usize, false);
    out.clear();
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let index = (ty * tiles_x + tx) as usize;
            if !map.is_dirty(tx, ty) || visited[index] {
                continue;
            }
            let mut span = 1_u32;
            while tx + span < tiles_x {
                let candidate = (ty * tiles_x + tx + span) as usize;
                if !map.is_dirty(tx + span, ty) || visited[candidate] {
                    break;
                }
                span += 1;
            }
            let mut rows = 1_u32;
            'grow: while ty + rows < tiles_y {
                for offset in 0..span {
                    let candidate = ((ty + rows) * tiles_x + tx + offset) as usize;
                    if !map.is_dirty(tx + offset, ty + rows) || visited[candidate] {
                        break 'grow;
                    }
                }
                rows += 1;
            }
            for row in 0..rows {
                for column in 0..span {
                    visited[(((ty + row) * tiles_x) + tx + column) as usize] = true;
                }
            }
            let x = tx * TILE;
            let y = ty * TILE;
            out.push(Rect::new(
                x,
                y,
                (span * TILE).min(width - x),
                (rows * TILE).min(height - y),
            ));
        }
    }
}

/// Shrink one tile-aligned rectangle to the exact bounding box of the cells
/// that differ between `current` and `reference`; `None` when none differ.
///
/// `stale` marks the rows the workspace still holds stale — the rows it did
/// not convert because no tile of their band was dirty. A stale row holds no
/// changed cell (it was compared against the reference and found equal), but
/// the workspace's copy of it is not the frame's, so the comparison skips it:
/// comparing it would enlarge the box instead of shrinking it.
pub fn shrink_rect(
    current: &Surface,
    reference: &Surface,
    rect: Rect,
    stale: &[u64],
) -> Option<Rect> {
    if rect.is_empty() || !rect.is_inside(current.width(), current.height()) {
        return None;
    }
    let x0 = rect.x as usize;
    let x1 = (rect.x + rect.width) as usize;
    let mut min_x = u32::MAX;
    let mut max_x = 0_u32;
    let mut min_y = u32::MAX;
    let mut max_y = 0_u32;
    for y in rect.y..rect.y + rect.height {
        if is_stale(stale, y) {
            continue;
        }
        let a = &current.row(y)[x0..x1];
        let b = &reference.row(y)[x0..x1];
        let first = a.iter().zip(b).position(|(left, right)| left != right);
        let Some(first) = first else { continue };
        let last = a
            .iter()
            .zip(b)
            .rposition(|(left, right)| left != right)
            .unwrap_or(first);
        min_x = min_x.min(rect.x + first as u32);
        max_x = max_x.max(rect.x + last as u32);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    if min_x == u32::MAX {
        return None;
    }
    Some(Rect::new(
        min_x,
        min_y,
        max_x - min_x + 1,
        max_y - min_y + 1,
    ))
}

/// Whether the workspace still holds a stale copy of row `y`.
///
/// `stale` is a bitmask with one bit per row, 64 rows per word.
pub fn is_stale(stale: &[u64], y: u32) -> bool {
    let word = (y / 64) as usize;
    match stale.get(word) {
        Some(bits) => bits & (1_u64 << (y % 64)) != 0,
        None => false,
    }
}

/// Shrink every rectangle to its exact bounding box and drop empty ones.
///
/// The rows the workspace still holds stale are skipped, so a rectangle is
/// the box of the cells that changed and not of the cells the workspace
/// happens to hold.
pub fn shrink_all(current: &Surface, reference: &Surface, rects: &mut Vec<Rect>, stale: &[u64]) {
    rects.retain_mut(|rect| match shrink_rect(current, reference, *rect, stale) {
        Some(shrunk) => {
            *rect = shrunk;
            true
        }
        None => false,
    });
}

/// Merge rectangles until at most `limit` remain.
///
/// The rectangles are *grouped* by a uniform grid rather than merged pairwise:
/// every rectangle belongs to the grid cell its centre lies in, and the
/// rectangles of one cell become the smallest rectangle covering them. The
/// grid starts at the change detector's tile size and doubles until its
/// occupied cells fit the limit, so the merged set covers exactly the cells
/// the input covered — the merge adds area, never cells.
///
/// The search is bounded where the pairwise search was not. The grid is
/// coarsened at most `log2(MAX_SURFACE_DIMENSION / TILE)` times, and every
/// coarsening is a sort over the rectangles, so merging `n` rectangles costs
/// `O(n log n)` whatever `n` is: a frame with ten rectangles and a frame with
/// ten thousand cost the same order, and neither can hold the encoder up.
pub fn merge_to_limit(rects: &mut Vec<Rect>, limit: usize) {
    let limit = limit.max(1);
    if rects.len() <= limit {
        return;
    }
    let mut cell = TILE;
    let groups = loop {
        let groups = grid_groups(rects, cell);
        if groups.len() <= limit || cell >= MAX_SURFACE_DIMENSION {
            break groups;
        }
        cell = cell.saturating_mul(2);
    };
    *rects = groups;
    rects.sort_by_key(|rect| (rect.y, rect.x, rect.width, rect.height));
}

/// The smallest rectangle covering the rectangles of every occupied grid cell.
///
/// The cells are visited in `(y, x)` order, so the result is the same for the
/// same rectangles whatever order they arrive in.
fn grid_groups(rects: &[Rect], cell: u32) -> Vec<Rect> {
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_unstable_by_key(|index| cell_of(rects[*index], cell));
    let mut groups: Vec<Rect> = Vec::with_capacity(rects.len());
    let mut occupied: Option<(u32, u32)> = None;
    for index in order {
        let rect = rects[index];
        let key = cell_of(rect, cell);
        match groups.last_mut() {
            Some(last) if occupied == Some(key) => *last = last.union(rect),
            _ => {
                groups.push(rect);
                occupied = Some(key);
            }
        }
    }
    groups
}

/// The grid cell a rectangle belongs to: the cell its centre lies in.
fn cell_of(rect: Rect, cell: u32) -> (u32, u32) {
    let centre_x = rect.x + rect.width / 2;
    let centre_y = rect.y + rect.height / 2;
    (centre_y / cell, centre_x / cell)
}

/// Remove overlaps introduced by merging.
///
/// Later rectangles are clipped against earlier ones (in `(y, x)` order); the
/// pieces of one rectangle stay disjoint by construction. Returns the rectangle
/// set and whether the clipping exceeded its bound, in which case the caller
/// must fall back to a single bounding box.
pub fn make_disjoint(rects: &mut Vec<Rect>) -> bool {
    rects.sort_by_key(|rect| (rect.y, rect.x, rect.width, rect.height));
    let mut accepted: Vec<Rect> = Vec::with_capacity(rects.len());
    let mut current: Vec<Rect> = Vec::new();
    let mut next: Vec<Rect> = Vec::new();
    for rect in rects.iter().copied() {
        current.clear();
        current.push(rect);
        for cut in &accepted {
            next.clear();
            for piece in &current {
                subtract_rect(*piece, *cut, &mut next);
            }
            std::mem::swap(&mut current, &mut next);
            if current.is_empty() {
                break;
            }
            if current.len() > DISJOINT_PIECE_LIMIT {
                return false;
            }
        }
        accepted.extend_from_slice(&current);
    }
    *rects = accepted;
    true
}

/// Subtract `cut` from `rect`, appending the at most four remaining pieces.
fn subtract_rect(rect: Rect, cut: Rect, out: &mut Vec<Rect>) {
    if !rect.intersects(cut) {
        out.push(rect);
        return;
    }
    let cut_right = cut.x + cut.width;
    let cut_bottom = cut.y + cut.height;
    let rect_right = rect.x + rect.width;
    let rect_bottom = rect.y + rect.height;
    if cut.y > rect.y {
        out.push(Rect::new(rect.x, rect.y, rect.width, cut.y - rect.y));
    }
    if cut_bottom < rect_bottom {
        out.push(Rect::new(
            rect.x,
            cut_bottom,
            rect.width,
            rect_bottom - cut_bottom,
        ));
    }
    let band_y = rect.y.max(cut.y);
    let band_bottom = rect_bottom.min(cut_bottom);
    if band_y < band_bottom {
        if cut.x > rect.x {
            out.push(Rect::new(
                rect.x,
                band_y,
                cut.x - rect.x,
                band_bottom - band_y,
            ));
        }
        if cut_right < rect_right {
            out.push(Rect::new(
                cut_right,
                band_y,
                rect_right - cut_right,
                band_bottom - band_y,
            ));
        }
    }
}

/// Smallest rectangle covering every input, or `None` for an empty slice.
pub fn bounding_box(rects: &[Rect]) -> Option<Rect> {
    let mut iter = rects.iter().copied();
    let first = iter.next()?;
    Some(iter.fold(first, |acc, rect| acc.union(rect)))
}

/// Maximal runs of cells differing from `reference` inside one row segment.
///
/// Span offsets are relative to `x0` (the region's own coordinate space), and
/// the return value is the number of differing cells in the segment.
pub fn row_spans(
    current: &Surface,
    reference: &Surface,
    y: u32,
    x0: u32,
    width: u32,
    out: &mut Vec<Span>,
) -> u64 {
    out.clear();
    if width == 0 || y >= current.height() {
        return 0;
    }
    let a = &current.row(y)[x0 as usize..(x0 + width) as usize];
    let b = &reference.row(y)[x0 as usize..(x0 + width) as usize];
    let mut changed = 0_u64;
    let mut index = 0_usize;
    while index < a.len() {
        if a[index] == b[index] {
            index += 1;
            continue;
        }
        let start = index;
        while index < a.len() && a[index] != b[index] {
            index += 1;
        }
        out.push(Span {
            x: start as u32,
            len: (index - start) as u32,
        });
        changed += (index - start) as u64;
    }
    changed
}

/// Default tiling used by the change detector for a frame width/height.
pub fn tile_grid(width: u32, height: u32) -> (u32, u32) {
    (
        width.min(MAX_SURFACE_DIMENSION).div_ceil(TILE),
        height.min(MAX_SURFACE_DIMENSION).div_ceil(TILE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(width: u32, height: u32, value: u16) -> Surface {
        let mut buffer = Surface::new(width, height).unwrap();
        buffer.fill(value);
        buffer
    }

    #[test]
    fn identical_buffers_have_no_dirty_tiles() {
        let current = buffer(200, 130, 0x1234);
        let reference = buffer(200, 130, 0x1234);
        let mut map = ChangeMap::new();
        map.detect(&current, Some(&reference));
        assert!(map.is_empty());
        assert_eq!(map.upper_bound_cells(), 0);
        assert_eq!(map.tiles_x(), 4);
        assert_eq!(map.tiles_y(), 3);
    }

    #[test]
    fn one_changed_cell_dirties_exactly_one_tile() {
        let current = buffer(200, 130, 0);
        let mut reference = buffer(200, 130, 0);
        reference.as_mut_u16()[0] = 1;
        let mut map = ChangeMap::new();
        map.detect(&current, Some(&reference));
        assert_eq!(map.dirty_tiles(), 1);
        assert!(map.is_dirty(0, 0));
        assert!(!map.is_dirty(1, 0));
        assert_eq!(map.upper_bound_cells(), 64 * 64);
    }

    #[test]
    fn a_missing_or_mismatched_reference_dirties_everything() {
        let current = buffer(100, 50, 0);
        let mut map = ChangeMap::new();
        map.detect(&current, None);
        assert_eq!(map.dirty_tiles(), 2);
        assert_eq!(map.upper_bound_cells(), 100 * 50);
        let mismatched = buffer(10, 10, 0);
        map.detect(&current, Some(&mismatched));
        assert_eq!(map.dirty_tiles(), 2);
    }

    #[test]
    fn the_block_wise_path_builds_the_same_map_as_detect() {
        let mut current = buffer(200, 130, 7);
        let mut reference = buffer(200, 130, 7);
        // Dirty tile rows 0 and 2, tiles 1 and 3 of row 1, plus a single cell
        // in the last (partial) tile row.
        reference.as_mut_u16()[5] = 1;
        reference.as_mut_u16()[64 * 5 + 200] = 1;
        reference.as_mut_u16()[129 * 5 + 70] = 1;
        reference.as_mut_u16()[1] = 1;
        current.as_mut_u16()[2] = 1;

        let mut whole = ChangeMap::new();
        whole.detect(&current, Some(&reference));

        let mut block = ChangeMap::new();
        let usable = block.prepare(&current, Some(&reference)).is_some();
        assert!(usable);
        // The block path sees one converted row at a time; the row scratch is
        // exactly what the encoder passes.
        let mut row = vec![0_u16; current.width() as usize];
        let mut kept = 0_u32;
        for y in 0..current.height() {
            row.copy_from_slice(current.row(y));
            if !block.compare_row(&row, &reference, y) {
                kept += 1;
            }
        }
        // Only rows with no dirty tile can be kept out of the workspace; the
        // strictest check is that the maps agree.
        assert!(kept > 0);
        assert_eq!(block.dirty_tiles(), whole.dirty_tiles());
        assert_eq!(block.upper_bound_cells(), whole.upper_bound_cells());
        for ty in 0..whole.tiles_y() {
            for tx in 0..whole.tiles_x() {
                assert_eq!(
                    block.is_dirty(tx, ty),
                    whole.is_dirty(tx, ty),
                    "tile ({tx}, {ty})"
                );
            }
        }

        // Without a usable reference both paths mark everything dirty.
        let mut block = ChangeMap::new();
        assert!(block.prepare(&current, None).is_none());
        assert_eq!(block.dirty_tiles(), whole.tiles_x() * whole.tiles_y());
        assert_eq!(block.upper_bound_cells(), u64::from(200_u32 * 130));
    }
    #[test]
    fn greedy_rectangles_cover_dirty_tiles_disjointly() {
        let current = buffer(192, 128, 0);
        let mut reference = buffer(192, 128, 0);
        // Dirty tiles: (0,0), (1,0) adjacent, and (2,1) alone.
        reference.as_mut_u16()[0] = 1;
        reference.as_mut_u16()[64] = 1;
        let offset = 64 * 192 + 128;
        reference.as_mut_u16()[offset] = 1;
        let mut map = ChangeMap::new();
        map.detect(&current, Some(&reference));
        let mut visited = Vec::new();
        let mut rects = Vec::new();
        rects_from_map(&map, 192, 128, &mut visited, &mut rects);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], Rect::new(0, 0, 128, 64));
        assert_eq!(rects[1], Rect::new(128, 64, 64, 64));
        assert!(!rects[0].intersects(rects[1]));
    }

    #[test]
    fn shrinking_finds_the_exact_changed_box() {
        let current = buffer(128, 128, 0);
        let mut reference = buffer(128, 128, 0);
        for y in 5..9 {
            for x in 7..11 {
                reference.as_mut_u16()[y * 128 + x] = 1;
            }
        }
        let shrunk = shrink_rect(&current, &reference, Rect::new(0, 0, 64, 64), &[]).unwrap();
        assert_eq!(shrunk, Rect::new(7, 5, 4, 4));
        let mut rects = vec![Rect::new(0, 0, 64, 64), Rect::new(64, 64, 64, 64)];
        shrink_all(&current, &reference, &mut rects, &[]);
        assert_eq!(rects, vec![Rect::new(7, 5, 4, 4)]);
    }

    #[test]
    fn shrinking_skips_the_rows_the_workspace_holds_stale() {
        let mut reference = buffer(128, 128, 0);
        reference.as_mut_u16()[5 * 128 + 7] = 1;
        // The workspace still holds a stale copy of row 3: it differs from
        // the reference, but it holds no changed cell.
        let mut workspace = buffer(128, 128, 0);
        workspace.as_mut_u16()[3 * 128] = 9;
        let stale = vec![1_u64 << 3];
        let rect = Rect::new(0, 0, 64, 64);
        assert_eq!(
            shrink_rect(&workspace, &reference, rect, &stale),
            Some(Rect::new(7, 5, 1, 1))
        );
        // Without the stale row the same comparison sees the stale cell.
        assert_eq!(
            shrink_rect(&workspace, &reference, rect, &[]),
            Some(Rect::new(0, 3, 8, 3))
        );
    }

    #[test]
    fn merging_groups_the_rectangles_into_at_most_the_limit() {
        let mut rects = vec![
            Rect::new(0, 0, 10, 10),
            Rect::new(20, 0, 10, 10),
            Rect::new(0, 40, 10, 10),
        ];
        merge_to_limit(&mut rects, 2);
        // All three rectangles lie in one tile, so one group holds them.
        assert_eq!(rects, vec![Rect::new(0, 0, 30, 50)]);
    }

    #[test]
    fn merging_divides_the_rectangles_that_do_not_fit() {
        // One rectangle per tile of a 16×16 tile grid: the grid is coarsened
        // until its occupied cells fit the limit.
        let source: Vec<Rect> = (0..256_u32)
            .map(|index| {
                let x = (index % 16) * 64;
                let y = (index / 16) * 64;
                Rect::new(x, y, 64, 64)
            })
            .collect();
        let mut rects = source.clone();
        merge_to_limit(&mut rects, 4);
        assert!(rects.len() <= 4, "{} rectangles", rects.len());
        assert!(!rects.is_empty());
    }

    #[test]
    fn merging_keeps_every_covered_cell() {
        let source: Vec<Rect> = (0..40_u32)
            .map(|index| {
                let x = (index * 137) % 900;
                let y = (index * 271) % 700;
                Rect::new(x, y, 1 + index % 7, 1 + index % 5)
            })
            .collect();
        let mut rects = source.clone();
        merge_to_limit(&mut rects, 8);
        assert!(rects.len() <= 8);
        for rect in &source {
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    let covered = rects.iter().any(|merged| {
                        x >= merged.x
                            && x < merged.x + merged.width
                            && y >= merged.y
                            && y < merged.y + merged.height
                    });
                    assert!(covered, "cell {x},{y} of {rect:?} is not covered");
                }
            }
        }
    }

    #[test]
    fn merging_leaves_a_small_set_alone() {
        let mut rects = vec![Rect::new(0, 0, 4, 4), Rect::new(8, 8, 4, 4)];
        merge_to_limit(&mut rects, 4);
        assert_eq!(rects.len(), 2);
        assert!(rects.contains(&Rect::new(0, 0, 4, 4)));
        assert!(rects.contains(&Rect::new(8, 8, 4, 4)));
    }

    #[test]
    fn merging_is_deterministic() {
        let source = vec![
            Rect::new(0, 0, 4, 4),
            Rect::new(8, 0, 4, 4),
            Rect::new(0, 8, 4, 4),
            Rect::new(8, 8, 4, 4),
        ];
        let mut first = source.clone();
        let mut second = source;
        merge_to_limit(&mut first, 3);
        merge_to_limit(&mut second, 3);
        assert_eq!(first, second);
    }

    #[test]
    fn overlap_removal_keeps_cells_and_disjointness() {
        let mut rects = vec![Rect::new(0, 0, 10, 10), Rect::new(5, 5, 10, 10)];
        assert!(make_disjoint(&mut rects));
        for (index, rect) in rects.iter().enumerate() {
            for other in rects.iter().skip(index + 1) {
                assert!(!rect.intersects(*other), "{rect:?} overlaps {other:?}");
            }
        }
        // Every cell of both inputs is still covered.
        for y in 0..15 {
            for x in 0..15 {
                let covered = rects
                    .iter()
                    .any(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
                let expected = (x < 10 && y < 10) || (x >= 5 && y >= 5);
                assert_eq!(covered, expected, "cell {x},{y}");
            }
        }
    }

    #[test]
    fn spans_are_maximal_runs_of_changed_cells() {
        let mut current = buffer(16, 1, 0);
        let reference = buffer(16, 1, 0);
        current.as_mut_u16()[2] = 1;
        current.as_mut_u16()[3] = 1;
        current.as_mut_u16()[7] = 1;
        current.as_mut_u16()[9] = 1;
        current.as_mut_u16()[10] = 1;
        let mut spans = Vec::new();
        let changed = row_spans(&current, &reference, 0, 0, 16, &mut spans);
        assert_eq!(changed, 5);
        assert_eq!(
            spans,
            vec![
                Span { x: 2, len: 2 },
                Span { x: 7, len: 1 },
                Span { x: 9, len: 2 },
            ]
        );
    }

    #[test]
    fn bounding_box_covers_every_input() {
        assert_eq!(bounding_box(&[]), None);
        let rects = vec![Rect::new(0, 0, 2, 2), Rect::new(8, 4, 2, 2)];
        assert_eq!(bounding_box(&rects), Some(Rect::new(0, 0, 10, 6)));
    }

    #[test]
    fn tile_grid_matches_the_dimensions() {
        assert_eq!(tile_grid(1920, 1080), (30, 17));
        assert_eq!(tile_grid(64, 64), (1, 1));
        assert_eq!(tile_grid(65, 1), (2, 1));
    }
}
