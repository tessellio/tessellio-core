//! The reference buffer and its undo journal.
//!
//! The sender's reference advances only over the cells a frame actually sent
//! (masked update), so the reference and the receiver's canvas stay bit-identical
//! by construction. The journal stores, per delta frame, the cells that frame
//! overwrote — reversing those writes in reverse frame order reconstructs the
//! state a receiver reports holding, which is what makes a rebase possible
//! without keeping whole frames around.

use std::collections::VecDeque;

use crate::buffer::Surface;
use crate::error::CodecError;
use crate::format::{JOURNAL_MAX_BYTES, JOURNAL_MAX_UPDATES};

/// One contiguous run of cells written (or restored) by a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteSpan {
    /// Flat cell offset.
    pub offset: u32,
    /// Run length in cells.
    pub len: u32,
}

impl WriteSpan {
    /// Build a span of `len` cells at `offset`.
    pub fn new(offset: u32, len: u32) -> Self {
        Self { offset, len }
    }

    /// Cell bytes carried by the span's values.
    pub fn bytes(self) -> usize {
        self.len as usize * 2
    }
}

/// The cells one frame overwrote, in write order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoSegment {
    /// Flat cell offset.
    pub offset: u32,
    /// Pre-frame cell values at that offset.
    pub values: Vec<u16>,
}

impl UndoSegment {
    /// Cell bytes carried by this segment.
    pub fn bytes(&self) -> usize {
        self.values.len() * 2
    }
}

/// One delta frame's undo record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Frame that produced this undo.
    pub update_id: u64,
    /// State this undo returns to.
    pub base_update_id: u64,
    /// Segments in write order.
    pub segments: Vec<UndoSegment>,
    /// Sum of segment value bytes.
    pub bytes: usize,
}

impl JournalEntry {
    /// Build an entry from its segments.
    pub fn new(update_id: u64, base_update_id: u64, segments: Vec<UndoSegment>) -> Self {
        let bytes = segments.iter().map(UndoSegment::bytes).sum();
        Self {
            update_id,
            base_update_id,
            segments,
            bytes,
        }
    }
}

/// Bounded undo journal: a suffix chain of delta frames.
#[derive(Debug, Default)]
pub struct ReferenceJournal {
    entries: VecDeque<JournalEntry>,
    bytes: usize,
    floor_update_id: u64,
}

impl ReferenceJournal {
    /// An empty journal with no reachable state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every entry and declare `update_id` the oldest reachable state.
    pub fn clear(&mut self, update_id: u64) {
        self.entries.clear();
        self.bytes = 0;
        self.floor_update_id = update_id;
    }

    /// Append one frame's undo, then evict from the front until both bounds
    /// hold.
    pub fn push(&mut self, entry: JournalEntry) {
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
        self.evict_to_bounds();
    }

    /// Oldest frame id a receiver may report holding.
    pub fn floor_update_id(&self) -> u64 {
        self.floor_update_id
    }

    /// Journal size in undo value bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Frames currently retained.
    pub fn frames(&self) -> usize {
        self.entries.len()
    }

    /// Whether the journal is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `held` can be reconstructed from `reference_update_id`.
    pub fn can_reach(&self, held: u64, reference_update_id: u64) -> bool {
        if held == reference_update_id {
            return true;
        }
        if held < self.floor_update_id {
            return false;
        }
        let mut current = reference_update_id;
        for entry in self.entries.iter().rev() {
            if current == held {
                return true;
            }
            if entry.update_id != current {
                return false;
            }
            current = entry.base_update_id;
        }
        current == held
    }

    /// Reverse every write newer than `held` in `buffer`.
    pub fn apply_undo(&self, held: u64, reference_update_id: u64, buffer: &mut Surface) -> bool {
        if held == reference_update_id {
            return true;
        }
        let mut current = reference_update_id;
        for entry in self.entries.iter().rev() {
            if current == held {
                return true;
            }
            if entry.update_id != current {
                return false;
            }
            for segment in entry.segments.iter().rev() {
                let start = segment.offset as usize;
                let end = start + segment.values.len();
                if end > buffer.cell_count() {
                    return false;
                }
                buffer.as_mut_u16()[start..end].copy_from_slice(&segment.values);
            }
            current = entry.base_update_id;
        }
        current == held
    }

    /// Drop the oldest entries until the byte and frame ceilings hold.
    fn evict_to_bounds(&mut self) {
        while !self.entries.is_empty()
            && (self.bytes > JOURNAL_MAX_BYTES || self.entries.len() > JOURNAL_MAX_UPDATES)
        {
            if let Some(evicted) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(evicted.bytes);
                self.floor_update_id = evicted.update_id;
            }
        }
    }
}

/// Copy the written spans from `source` into `dest` (the masked reference
/// update).
pub fn apply_writes(
    dest: &mut Surface,
    source: &Surface,
    spans: &[WriteSpan],
) -> Result<(), CodecError> {
    if !dest.matches(source.width(), source.height()) {
        return Err(CodecError::InvalidSurface(
            "write source and destination geometries differ".to_string(),
        ));
    }
    let source = source.as_u16();
    let dest = dest.as_mut_u16();
    for span in spans {
        let start = span.offset as usize;
        let end = start + span.len as usize;
        if end > source.len() {
            return Err(CodecError::InvalidSurface(format!(
                "write span [{start}..{end}) exceeds {} cells",
                source.len()
            )));
        }
        dest[start..end].copy_from_slice(&source[start..end]);
    }
    Ok(())
}

/// Record the pre-frame values of every written span.
pub fn undo_from_spans(
    base: &Surface,
    spans: &[WriteSpan],
) -> Result<Vec<UndoSegment>, CodecError> {
    let cells = base.as_u16();
    let mut segments = Vec::with_capacity(spans.len());
    for span in spans {
        let start = span.offset as usize;
        let end = start + span.len as usize;
        if end > cells.len() {
            return Err(CodecError::InvalidSurface(format!(
                "undo span [{start}..{end}) exceeds {} cells",
                cells.len()
            )));
        }
        segments.push(UndoSegment {
            offset: span.offset,
            values: cells[start..end].to_vec(),
        });
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(width: u32, height: u32, value: u16) -> Surface {
        let mut buffer = Surface::new(width, height).unwrap();
        buffer.fill(value);
        buffer
    }

    fn segment(offset: u32, values: &[u16]) -> UndoSegment {
        UndoSegment {
            offset,
            values: values.to_vec(),
        }
    }

    #[test]
    fn undo_reverses_frames_in_reverse_order() {
        let mut reference = buffer(8, 2, 10);
        let mut journal = ReferenceJournal::new();
        journal.clear(1);

        // Frame 2 writes cells 0..2; frame 3 writes cells 2..4.
        journal.push(JournalEntry::new(2, 1, vec![segment(0, &[10, 10])]));
        journal.push(JournalEntry::new(3, 2, vec![segment(2, &[10, 10])]));
        reference.as_mut_u16()[0..4].copy_from_slice(&[20, 21, 22, 23]);

        assert!(journal.can_reach(1, 3));
        assert!(journal.can_reach(2, 3));
        assert!(journal.can_reach(3, 3));
        assert!(!journal.can_reach(0, 3));

        let mut scratch = reference.clone();
        assert!(journal.apply_undo(2, 3, &mut scratch));
        assert_eq!(&scratch.as_u16()[0..4], &[20, 21, 10, 10]);
        assert!(journal.apply_undo(1, 3, &mut scratch));
        assert_eq!(&scratch.as_u16()[0..4], &[10, 10, 10, 10]);
    }

    #[test]
    fn segments_reverse_within_a_frame() {
        let mut journal = ReferenceJournal::new();
        journal.clear(1);
        journal.push(JournalEntry::new(
            2,
            1,
            vec![segment(0, &[1, 2]), segment(0, &[3, 4])],
        ));
        let mut buffer = buffer(4, 1, 9);
        buffer.as_mut_u16().copy_from_slice(&[7, 7, 9, 9]);
        assert!(journal.apply_undo(1, 2, &mut buffer));
        assert_eq!(buffer.as_u16(), &[1, 2, 9, 9]);
    }

    #[test]
    fn eviction_raises_the_floor_and_keeps_the_chain_reachable() {
        let mut journal = ReferenceJournal::new();
        journal.clear(0);
        for update_id in 1..=JOURNAL_MAX_UPDATES as u64 + 10 {
            journal.push(JournalEntry::new(update_id, update_id - 1, Vec::new()));
        }
        assert_eq!(journal.frames(), JOURNAL_MAX_UPDATES);
        let reference_id = JOURNAL_MAX_UPDATES as u64 + 10;
        // Ten frames were evicted: 1..=10, so 10 is the oldest reachable state.
        assert_eq!(journal.floor_update_id(), 10);
        assert!(!journal.can_reach(9, reference_id));
        assert!(journal.can_reach(10, reference_id));
        assert!(journal.can_reach(100, reference_id));
        assert!(journal.can_reach(reference_id, reference_id));
        // An entry that does not chain is refused rather than trusted: the
        // single entry returns to frame 1, so frame 2 is not reachable.
        let mut broken = ReferenceJournal::new();
        broken.clear(0);
        broken.push(JournalEntry::new(5, 1, Vec::new()));
        assert!(broken.can_reach(1, 5));
        assert!(!broken.can_reach(2, 5));
    }

    #[test]
    fn byte_ceiling_evicts_old_entries() {
        let mut journal = ReferenceJournal::new();
        journal.clear(0);
        let big = vec![0_u16; JOURNAL_MAX_BYTES / 4];
        for update_id in 1..=6 {
            journal.push(JournalEntry::new(
                update_id,
                update_id - 1,
                vec![segment(0, &big)],
            ));
        }
        assert!(journal.bytes() <= JOURNAL_MAX_BYTES);
        assert!(journal.floor_update_id() > 0);
    }

    #[test]
    fn masked_writes_copy_only_the_written_cells() {
        let mut dest = buffer(4, 1, 0);
        let source = buffer(4, 1, 7);
        apply_writes(&mut dest, &source, &[WriteSpan::new(1, 2)]).unwrap();
        assert_eq!(dest.as_u16(), &[0, 7, 7, 0]);
        assert!(apply_writes(&mut dest, &source, &[WriteSpan::new(3, 2)]).is_err());
    }

    #[test]
    fn undo_segments_capture_the_pre_write_values() {
        let base = buffer(4, 1, 5);
        let segments =
            undo_from_spans(&base, &[WriteSpan::new(0, 2), WriteSpan::new(2, 2)]).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].offset, 0);
        assert_eq!(segments[0].values, vec![5, 5]);
        assert_eq!(segments[1].offset, 2);
        let entry = JournalEntry::new(2, 1, segments);
        assert_eq!(entry.bytes, 8);
        assert!(undo_from_spans(&base, &[WriteSpan::new(3, 2)]).is_err());
    }
}
