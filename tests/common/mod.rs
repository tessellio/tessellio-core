//! Shared scene generators and frame helpers for the codec test suites.
//!
//! Every scene is deterministic, so byte budgets, corpus cases, and property
//! harnesses all run on the same cells on every machine.

#![allow(dead_code)]
#![allow(unreachable_pub, clippy::needless_range_loop)]

use tessellio_core::CapturedFrame;
use tessellio_core::PointerShape;
use tessellio_core::{
    CodecConfig, Coverage, DecodeError, EncodedUpdate, Part, ScreenStreamEncoder, Surface,
    SurfaceCanvas, UpdateSplitter, decode_region_ops, rgba_to_cells,
};

/// Deterministic linear-congruential generator (numerical recipes constants).
#[derive(Debug, Clone)]
pub struct Lcg {
    state: u32,
}

impl Lcg {
    /// Generator with the given seed (0 is replaced by a non-zero state).
    pub fn new(seed: u32) -> Self {
        Self { state: seed | 1 }
    }

    /// Next 32-bit word.
    pub fn next_u32(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        self.state
    }

    /// Next byte.
    pub fn next_u8(&mut self) -> u8 {
        (self.next_u32() >> 24) as u8
    }

    /// Next value in `0..bound`.
    pub fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            0
        } else {
            self.next_u32() % bound
        }
    }
}

/// Opaque solid frame.
pub fn plain(width: u32, height: u32, color: [u8; 4]) -> CapturedFrame {
    let mut rgba = vec![0_u8; (width * height * 4) as usize];
    for cell in rgba.chunks_exact_mut(4) {
        cell.copy_from_slice(&color);
    }
    CapturedFrame::new(width, height, rgba).expect("frame")
}

/// Paint a rectangle.
pub fn fill_rect(frame: &mut CapturedFrame, x: u32, y: u32, w: u32, h: u32, color: [u8; 4]) {
    for row in y..(y + h).min(frame.height) {
        for column in x..(x + w).min(frame.width) {
            let offset = ((row * frame.width + column) * 4) as usize;
            frame.rgba[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

/// A pseudo-font: eight 6×10 glyphs as bit rows, repeated deterministically.
const GLYPHS: [[u16; 10]; 8] = [
    [
        0b011110, 0b100001, 0b100001, 0b100001, 0b111111, 0b100001, 0b100001, 0b100001, 0b100001, 0,
    ],
    [
        0b111110, 0b100001, 0b100001, 0b111110, 0b100001, 0b100001, 0b100001, 0b111110, 0, 0,
    ],
    [
        0b011110, 0b100001, 0b100000, 0b100000, 0b100000, 0b100000, 0b100000, 0b100001, 0b011110, 0,
    ],
    [
        0b111110, 0b100001, 0b100001, 0b100001, 0b100001, 0b100001, 0b100001, 0b111110, 0, 0,
    ],
    [
        0b111111, 0b100000, 0b100000, 0b111110, 0b100000, 0b100000, 0b100000, 0b100000, 0, 0,
    ],
    [
        0b111111, 0b100000, 0b100000, 0b111110, 0b100000, 0b100000, 0b100000, 0b100000, 0, 0,
    ],
    [
        0b011110, 0b100001, 0b100000, 0b101111, 0b100001, 0b100001, 0b100001, 0b011110, 0, 0,
    ],
    [
        0b100001, 0b100001, 0b100001, 0b111111, 0b100001, 0b100001, 0b100001, 0b100001, 0, 0,
    ],
];

/// One line of pseudo-text: glyphs from a repeating sequence, ~15 % ink.
///
/// `variant` selects a different glyph sequence so two lines of the same
/// document can differ the way typed text does.
pub fn text_line(frame: &mut CapturedFrame, x: u32, y: u32, columns: u32, variant: u32) {
    let ink = [20, 20, 24, 255];
    let mut lcg = Lcg::new(variant.wrapping_mul(2_654_435_761) ^ 0x9e37_79b9);
    for column in 0..columns {
        let glyph = GLYPHS[(lcg.below(GLYPHS.len() as u32)) as usize];
        let gx = x + column * 7;
        for (row, bits) in glyph.iter().enumerate() {
            if *bits == 0 {
                continue;
            }
            for bit in 0..6 {
                if bits & (1 << bit) != 0 {
                    fill_rect(frame, gx + bit, y + row as u32, 1, 1, ink);
                }
            }
        }
    }
}

/// An office-like desktop: menu bar, sidebar, document with text lines, taskbar.
///
/// Text lines repeat the same glyph sequences vertically, which is what makes a
/// scroll on this scene a cheap delta (and what a real document looks like).
pub fn office(width: u32, height: u32, seed: u32) -> CapturedFrame {
    let mut frame = plain(width, height, [244, 244, 246, 255]);
    let sidebar = 240.min(width);
    let menu_rows = 28.min(height);
    let taskbar_rows = 36.min(height);
    let body_bottom = height.saturating_sub(taskbar_rows);
    fill_rect(&mut frame, 0, 0, width, menu_rows, [52, 54, 60, 255]);
    if body_bottom > menu_rows {
        fill_rect(
            &mut frame,
            0,
            menu_rows,
            sidebar,
            body_bottom - menu_rows,
            [32, 34, 38, 255],
        );
    }
    if height > taskbar_rows {
        fill_rect(
            &mut frame,
            0,
            height - taskbar_rows,
            width,
            taskbar_rows,
            [24, 26, 32, 255],
        );
    }
    // Sidebar entries.
    if sidebar > 32 {
        for entry in 0..18_u32 {
            let y = menu_rows + 24 + entry * 26;
            if y + 12 < body_bottom {
                text_line(
                    &mut frame,
                    24,
                    y,
                    (sidebar - 32) / 7,
                    seed.wrapping_add(entry),
                );
            }
        }
    }
    // A photo-like window: a smooth gradient with light dithering, the kind of
    // content a real desktop carries and the reason a lossless reset frame is
    // hundreds of kilobytes rather than kilobytes.
    let photo_x = sidebar.saturating_add(32).min(width);
    let photo_y = menu_rows.saturating_add(24);
    let photo_w = 420.min(width.saturating_sub(photo_x));
    let photo_h = 300.min(body_bottom.saturating_sub(photo_y));
    for row in 0..photo_h {
        for column in 0..photo_w {
            let dither = ((column.wrapping_mul(7) + row.wrapping_mul(13)) % 9) as u16;
            let red = (column * 200 / photo_w.max(1)) as u16 + dither;
            let green = (row * 180 / photo_h.max(1)) as u16 + dither;
            let blue = ((column + row) * 160 / (photo_w + photo_h).max(1)) as u16 + 40;
            fill_rect(
                &mut frame,
                photo_x + column,
                photo_y + row,
                1,
                1,
                [red as u8, green as u8, blue as u8, 255],
            );
        }
    }
    // Document text, one distinct variant per line: a real document whose
    // content does not repeat (the scroll scenes use `document`, which does).
    let text_x = sidebar.saturating_add(24);
    let columns = (width.saturating_sub(text_x + 48)) / 7;
    if columns >= 6 {
        let mut y = photo_y + photo_h + 16;
        let mut line = 0_u32;
        while y + 12 < body_bottom {
            text_line(
                &mut frame,
                text_x,
                y,
                columns.min(180),
                seed.wrapping_add(line.wrapping_mul(2_654_435_761)),
            );
            line += 1;
            y += 20;
        }
    }
    frame
}

/// A document of identical text lines spaced 20 rows apart.
///
/// Identical lines are what a table, a log, or a code listing looks like, and
/// they are what makes a full-page scroll a cheap delta; the *typed* and
/// *reset* scenes use [`office`], whose lines all differ.
pub fn document(width: u32, height: u32) -> CapturedFrame {
    let mut frame = plain(width, height, [248, 248, 250, 255]);
    let mut y = 12;
    while y + 12 < height {
        text_line(&mut frame, 24, y, ((width - 48) / 7).min(260), 7);
        y += 20;
    }
    frame
}

/// A flat desktop with a movable window: a border, a title bar, and a body.
pub fn desktop_with_window(width: u32, height: u32, window: (u32, u32, u32, u32)) -> CapturedFrame {
    let mut frame = plain(width, height, [40, 44, 52, 255]);
    fill_rect(&mut frame, 0, 0, width, 28, [30, 32, 38, 255]);
    let (x, y, w, h) = window;
    fill_rect(&mut frame, x, y, w, h, [70, 74, 84, 255]);
    fill_rect(&mut frame, x + 1, y + 1, w - 2, h - 2, [236, 236, 238, 255]);
    fill_rect(&mut frame, x + 1, y + 1, w - 2, 22, [60, 64, 74, 255]);
    frame
}

/// Fill a rectangle with deterministic noise (the incompressible case).
pub fn noise_block(frame: &mut CapturedFrame, x: u32, y: u32, w: u32, h: u32, seed: u32) {
    let mut lcg = Lcg::new(seed);
    for row in y..(y + h).min(frame.height) {
        for column in x..(x + w).min(frame.width) {
            let offset = ((row * frame.width + column) * 4) as usize;
            frame.rgba[offset] = lcg.next_u8();
            frame.rgba[offset + 1] = lcg.next_u8();
            frame.rgba[offset + 2] = lcg.next_u8();
            frame.rgba[offset + 3] = 0xff;
        }
    }
}

/// Move the rows of a region up by `shift` (a page scroll), repainting the
/// freshly exposed strip with the same periodic document content.
pub fn scroll_region(frame: &mut CapturedFrame, x: u32, y: u32, w: u32, h: u32, shift: u32) {
    let source = frame.clone();
    for row in 0..h.saturating_sub(shift) {
        for column in 0..w {
            let from = (((y + row + shift) * source.width) + x + column) as usize * 4;
            let to = (((y + row) * source.width) + x + column) as usize * 4;
            let cell = [
                source.rgba[from],
                source.rgba[from + 1],
                source.rgba[from + 2],
                source.rgba[from + 3],
            ];
            frame.rgba[to..to + 4].copy_from_slice(&cell);
        }
    }
    // The exposed strip repeats the content that scrolled out of view: content
    // periodic in rows makes the whole region identical after the scroll, so
    // only this strip differs.
    for row in h.saturating_sub(shift)..h {
        for column in 0..w {
            let from = (((y + row) * source.width) + x + column) as usize * 4;
            let cell = [
                source.rgba[from],
                source.rgba[from + 1],
                source.rgba[from + 2],
                source.rgba[from + 3],
            ];
            let to = (((y + row) * source.width) + x + column) as usize * 4;
            frame.rgba[to..to + 4].copy_from_slice(&cell);
        }
    }
}

/// A 14×22 arrow pointer snapshot at `(x, y)`: opaque ink, transparent elsewhere.
pub fn cursor_at(x: i32, y: i32) -> PointerShape {
    let width = 14_u32;
    let height = 22_u32;
    let mut rgba = vec![0_u8; (width * height * 4) as usize];
    for row in 0..height {
        for column in 0..width {
            let inside = if row < 14 {
                column < row + 1
            } else {
                row < 20 && column >= 3 && column < 8
            };
            if !inside {
                continue;
            }
            let edge = column == 0
                || (row < 14 && column == row)
                || (row >= 14 && (column == 3 || column == 7))
                || row == height - 1;
            let color = if edge {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            };
            let offset = ((row * width + column) * 4) as usize;
            rgba[offset..offset + 4].copy_from_slice(&color);
        }
    }
    PointerShape {
        x,
        y,
        width,
        height,
        rgba,
    }
}

/// Composite the arrow pointer onto a frame at its hotspot.
pub fn draw_cursor(frame: &mut CapturedFrame, x: i32, y: i32) {
    tessellio_core::composite_pointer_in_place(frame, &cursor_at(x, y));
}

/// the 16-bit cell format conversion of a frame (the wire-fidelity expectation).
pub fn expect565(frame: &CapturedFrame) -> Vec<u16> {
    let mut buffer = Surface::new(frame.width, frame.height).expect("buffer");
    rgba_to_cells(&frame.rgba, frame.width, frame.height, &mut buffer).expect("convert");
    buffer.as_u16().to_vec()
}

/// Codec configuration with the documented defaults.
pub fn codec_config() -> CodecConfig {
    CodecConfig::default()
}

/// Encode one frame and commit it with `update_id`.
pub fn encode_frame(
    encoder: &mut ScreenStreamEncoder,
    frame: &CapturedFrame,
    update_id: u64,
) -> EncodedUpdate {
    let planned = encoder
        .encode(frame)
        .expect("encode")
        .expect("a planned frame");
    encoder.commit(planned, update_id)
}

/// Encode one frame, tolerating the idle path (returns the committed frame).
pub fn encode_frame_opt(
    encoder: &mut ScreenStreamEncoder,
    frame: &CapturedFrame,
    update_id: u64,
) -> Option<EncodedUpdate> {
    encoder
        .encode(frame)
        .expect("encode")
        .map(|planned| encoder.commit(planned, update_id))
}

/// Split an encoded frame into its parts.
pub fn parts_of(frame: &EncodedUpdate) -> Vec<Part> {
    UpdateSplitter::new().part(frame.clone()).expect("parts")
}

/// Decode a frame with the receiver oracle.
pub fn decode_frame(frame: &EncodedUpdate) -> SurfaceCanvas {
    let mut canvas = SurfaceCanvas::new();
    canvas.apply(frame).expect("apply");
    canvas
}

/// Decode a frame onto an existing canvas.
pub fn apply_frame(canvas: &mut SurfaceCanvas, frame: &EncodedUpdate) -> Result<(), DecodeError> {
    canvas.apply(frame)
}

/// Decode one region payload (sparse) into a fresh buffer.
pub fn decode_region(region: &tessellio_core::Region) -> Vec<u16> {
    let mut out = vec![0_u16; (region.width * region.height) as usize];
    let mut scratch = Vec::new();
    decode_region_ops(
        &region.payload,
        region.width,
        region.height,
        Coverage::Sparse,
        &mut out,
        &mut scratch,
    )
    .expect("region decodes");
    out
}

/// Total payload bytes of a frame.
pub fn update_bytes(frame: &EncodedUpdate) -> usize {
    frame.payload_bytes()
}

/// Cell count of all regions of a frame.
pub fn region_cells(frame: &EncodedUpdate) -> u64 {
    frame
        .regions
        .iter()
        .map(|region| u64::from(region.width) * u64::from(region.height))
        .sum()
}

/// Number of CPU cores available to the process.
pub fn cores() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}
