//! The reference implementation's measured bounds.
//!
//! These are the sizes this codec was measured to ship for the scenes in
//! [`crate::scene`], each one the measured value plus a margin. They are not
//! requirements of the format — the format's own caps are in
//! [`crate::format`] — but they are what a build is held to, so that a change
//! that makes the codec ship more is noticed rather than accepted.
//!
//! The measurements themselves are in the specification's §9.2; the budgets
//! here are those measurements with the margin the codec's suites apply.

/// A 1 px pointer move: 92 bytes across, 73 down.
pub const CURSOR_1PX_MAX_BYTES: usize = 128;
/// A 16 px pointer move with a 14×22 pointer: 115 bytes across, 117 down.
pub const CURSOR_16PX_MAX_BYTES: usize = 160;
/// A 600×40 block retyped: 2 466 bytes.
pub const TYPING_MAX_BYTES: usize = 3_072;
/// A 600×400 window dragged 30 px: 1 633 bytes.
pub const DRAG_MAX_BYTES: usize = 2_048;
/// A 900×620 viewport scrolled 40 px: 4 624 bytes.
pub const SCROLL_MAX_BYTES: usize = 5_120;
/// A 1680×920 page scrolled 40 px: 16 380 bytes, and it stays a chained
/// update.
pub const FULL_WIDTH_SCROLL_MAX_BYTES: usize = 20_480;
/// A 640×360 block of high-entropy content: 259 918 bytes in the update that
/// carries it.
pub const NOISE_FRAME_MAX_BYTES: usize = 266_240;
/// The weighted interactive mix (pointer 50 %, typing 20 %, scroll 15 %, drag
/// 10 %): 777 bytes per update.
pub const MIX_MAX_BYTES: usize = 1_024;
/// A 1080p office self-contained update: 82 799 bytes.
pub const RESET_FRAME_MAX_BYTES: usize = 98_304;

/// The cost of a chained update's encode, over the capture interface's own
/// conversion of the same frame.
///
/// The ratio is what makes the budget machine-independent: the conversion is
/// the same work on every machine and in every build profile, so a slower
/// machine does not fail the gate and a slower codec does. The budget is the
/// measured ratio with a margin.
pub const CHAINED_ENCODE_RATIO: f64 = 3.0;

/// The cost of a 1080p self-contained update's encode, over the capture
/// interface's own conversion of the same frame.
///
/// A self-contained update compresses every region of the surface, so it
/// costs more than a chained one; the budget is the measured ratio with a
/// margin.
pub const RESET_ENCODE_RATIO: f64 = 4.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_budget_is_above_the_measurement_it_covers() {
        const {
            assert!(CURSOR_1PX_MAX_BYTES > 92);
            assert!(CURSOR_16PX_MAX_BYTES > 117);
            assert!(TYPING_MAX_BYTES > 2_466);
            assert!(DRAG_MAX_BYTES > 1_633);
            assert!(SCROLL_MAX_BYTES > 4_624);
            assert!(FULL_WIDTH_SCROLL_MAX_BYTES > 16_380);
            assert!(NOISE_FRAME_MAX_BYTES > 259_918);
            assert!(MIX_MAX_BYTES > 777);
            assert!(RESET_FRAME_MAX_BYTES > 82_799);
        }
    }

    #[test]
    fn the_encode_budgets_are_above_one() {
        const {
            assert!(CHAINED_ENCODE_RATIO > 1.0);
            assert!(RESET_ENCODE_RATIO > CHAINED_ENCODE_RATIO);
        }
    }
}
