//! SVQ1 half-pel motion-compensation reference sampling.
//!
//! ## Provenance
//!
//! This module implements the half-pel reference interpolator and the
//! edge-of-frame sample addressing of
//! `docs/video/svq1/spec/06-motion-vectors.md` §6.5 (the half-pel
//! sampling grid + the `(a + b + 1) >> 1` interpolator) and §6.7 (the
//! de-facto edge-replication border extension).
//!
//! SVQ1 motion-vector components are SIGNED HALF-PEL units (§6.5): a
//! component value of `1` addresses the half-way point between a sample
//! and its neighbour, not the next integer-pel sample. The §6.5.1
//! interpolator splits each MV component into an integer-pel part and a
//! half-pel parity bit, then selects one of four reconstruction rules:
//!
//! * both even → integer-pel direct copy,
//! * x odd / y even → horizontal two-tap average,
//! * x even / y odd → vertical two-tap average,
//! * both odd → bilinear four-tap average,
//!
//! each with the canonical round-toward-positive-infinity bias (`+1`
//! for the two-tap halves, `+2` for the bilinear case) the wiki source
//! attests as MPEG-2-style. Out-of-frame reference samples are resolved
//! by edge replication (§6.7.2), clamping each integer-pel address to
//! the plane bounding box before the average.
//!
//! ## Wall / scope
//!
//! Both §6.5.1 and §6.7.2 are flagged in the spec as the **de-facto**
//! conventions (wiki-attested for the half-pel grid, MPEG-2-style for
//! the rounding + edge extension); the spec defers exact
//! binary-instruction confirmation to a Validator round. This module
//! implements that documented convention only — no external decoder
//! source is consulted. The per-component differential VLC (T02) decode
//! that produces the MV is a separate, still-deferred stage
//! (`docs/video/svq1/spec/06-motion-vectors.md` §6.2.3 Reading A/B
//! ambiguity); this module takes an already-resolved [`Svq1Mv`].

use crate::svq1_motion_predictor::Svq1Mv;

/// A borrowed read-only view of an SVQ1 reference picture plane (Y, U,
/// or V component) for half-pel motion compensation.
///
/// The plane is row-major, `width × height` integer-pel samples,
/// indexed `samples[y * width + x]`. Out-of-bounds integer-pel
/// addresses are resolved by edge replication per
/// `docs/video/svq1/spec/06-motion-vectors.md` §6.7.2 (each coordinate
/// clamped to `[0, width-1]` / `[0, height-1]`).
#[derive(Debug, Clone, Copy)]
pub struct Svq1ReferencePlane<'a> {
    samples: &'a [u8],
    width: usize,
    height: usize,
}

impl<'a> Svq1ReferencePlane<'a> {
    /// Wrap a row-major sample slice as a `width × height` reference
    /// plane.
    ///
    /// Returns `None` if `samples.len() != width * height` or if either
    /// dimension is zero.
    #[must_use]
    pub fn new(samples: &'a [u8], width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 || samples.len() != width * height {
            return None;
        }
        Some(Self {
            samples,
            width,
            height,
        })
    }

    /// Plane width in integer-pel samples.
    #[inline]
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Plane height in integer-pel samples.
    #[inline]
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Fetch the integer-pel sample at `(x, y)` with edge-replication
    /// clamping (§6.7.2).
    #[inline]
    #[must_use]
    pub fn sample_clamped(&self, x: i32, y: i32) -> u8 {
        let cx = x.clamp(0, self.width as i32 - 1) as usize;
        let cy = y.clamp(0, self.height as i32 - 1) as usize;
        self.samples[cy * self.width + cx]
    }
}

/// Side length of an SVQ1 motion-compensated sub-block in integer-pel
/// samples (the per-8×8-block MV granularity of §6.1.1 / §6.5.2).
pub const MC_BLOCK_DIM: usize = 8;

/// Interpolate one half-pel reference sample at the half-pel address
/// `(half_x, half_y)` from `plane`, per
/// `docs/video/svq1/spec/06-motion-vectors.md` §6.5.1.
///
/// `half_x` / `half_y` are signed half-pel coordinates: the integer-pel
/// part is `half >> 1` (floored toward −∞) and the half-pel phase is the
/// low bit. The four §6.5.1 cases select integer-pel direct, horizontal
/// two-tap, vertical two-tap, or bilinear four-tap averaging, each with
/// the round-toward-positive-infinity bias. Out-of-frame integer-pel
/// addresses edge-replicate via [`Svq1ReferencePlane::sample_clamped`].
#[inline]
#[must_use]
pub fn sample_halfpel(plane: &Svq1ReferencePlane<'_>, half_x: i32, half_y: i32) -> u8 {
    // Integer-pel floor + half-pel parity (rem_euclid keeps the phase
    // non-negative for negative half-pel coordinates).
    let x = half_x.div_euclid(2);
    let y = half_y.div_euclid(2);
    let frac_x = half_x.rem_euclid(2);
    let frac_y = half_y.rem_euclid(2);

    let a = plane.sample_clamped(x, y) as i32;
    match (frac_x, frac_y) {
        // both even → integer-pel direct copy.
        (0, 0) => a as u8,
        // x odd, y even → horizontal two-tap average.
        (1, 0) => {
            let b = plane.sample_clamped(x + 1, y) as i32;
            ((a + b + 1) >> 1) as u8
        }
        // x even, y odd → vertical two-tap average.
        (0, 1) => {
            let b = plane.sample_clamped(x, y + 1) as i32;
            ((a + b + 1) >> 1) as u8
        }
        // both odd → bilinear four-tap average.
        _ => {
            let b = plane.sample_clamped(x + 1, y) as i32;
            let c = plane.sample_clamped(x, y + 1) as i32;
            let d = plane.sample_clamped(x + 1, y + 1) as i32;
            ((a + b + c + d + 2) >> 2) as u8
        }
    }
}

/// Reconstruct the `MC_BLOCK_DIM × MC_BLOCK_DIM` (8×8) reference patch
/// that predicts an SVQ1 sub-block, per
/// `docs/video/svq1/spec/06-motion-vectors.md` §6.5.2.
///
/// `(base_col, base_row)` is the sub-block's top-left position in
/// **integer-pel** units; `mv` is the resolved half-pel motion vector
/// for the block. The top-left half-pel sample address is
/// `(base_col*2 + mv.x, base_row*2 + mv.y)` and the 8×8 output covers
/// integer-pel steps (each output column/row advances the half-pel
/// address by 2). Output is row-major (`out[row*8 + col]`), each sample
/// interpolated via [`sample_halfpel`] with §6.7.2 edge replication.
#[must_use]
pub fn motion_compensate_block(
    plane: &Svq1ReferencePlane<'_>,
    base_col: i32,
    base_row: i32,
    mv: Svq1Mv,
) -> Vec<u8> {
    let origin_half_x = base_col * 2 + mv.x;
    let origin_half_y = base_row * 2 + mv.y;
    let mut out = Vec::with_capacity(MC_BLOCK_DIM * MC_BLOCK_DIM);
    for row in 0..MC_BLOCK_DIM {
        let hy = origin_half_y + (row as i32) * 2;
        for col in 0..MC_BLOCK_DIM {
            let hx = origin_half_x + (col as i32) * 2;
            out.push(sample_halfpel(plane, hx, hy));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp_plane(width: usize, height: usize) -> Vec<u8> {
        (0..width * height).map(|i| (i % 256) as u8).collect()
    }

    #[test]
    fn reference_plane_validates_dimensions() {
        assert!(Svq1ReferencePlane::new(&[0u8; 5], 2, 3).is_none());
        assert!(Svq1ReferencePlane::new(&[], 0, 3).is_none());
        assert!(Svq1ReferencePlane::new(&[0u8; 6], 2, 3).is_some());
    }

    #[test]
    fn reference_plane_reports_dimensions() {
        let buf = ramp_plane(4, 5);
        let plane = Svq1ReferencePlane::new(&buf, 4, 5).unwrap();
        assert_eq!(plane.width(), 4);
        assert_eq!(plane.height(), 5);
    }

    #[test]
    fn sample_clamped_is_row_major_and_clamps() {
        let buf = ramp_plane(4, 3);
        let plane = Svq1ReferencePlane::new(&buf, 4, 3).unwrap();
        assert_eq!(plane.sample_clamped(2, 1), 6);
        assert_eq!(plane.sample_clamped(-3, 0), plane.sample_clamped(0, 0));
        assert_eq!(plane.sample_clamped(99, 99), plane.sample_clamped(3, 2));
    }

    #[test]
    fn halfpel_both_even_is_integer_pel_direct() {
        let buf = ramp_plane(8, 8);
        let plane = Svq1ReferencePlane::new(&buf, 8, 8).unwrap();
        // half (4, 6) → integer (2, 3) → samples[3*8+2] = 26.
        assert_eq!(sample_halfpel(&plane, 4, 6), 26);
    }

    #[test]
    fn halfpel_horizontal_is_two_tap_average() {
        let buf = ramp_plane(8, 8);
        let plane = Svq1ReferencePlane::new(&buf, 8, 8).unwrap();
        // half (5, 6): x odd → average of (2,3) and (3,3).
        let a = plane.sample_clamped(2, 3) as i32;
        let b = plane.sample_clamped(3, 3) as i32;
        assert_eq!(sample_halfpel(&plane, 5, 6) as i32, (a + b + 1) >> 1);
    }

    #[test]
    fn halfpel_vertical_is_two_tap_average() {
        let buf = ramp_plane(8, 8);
        let plane = Svq1ReferencePlane::new(&buf, 8, 8).unwrap();
        // half (4, 7): y odd → average of (2,3) and (2,4).
        let a = plane.sample_clamped(2, 3) as i32;
        let b = plane.sample_clamped(2, 4) as i32;
        assert_eq!(sample_halfpel(&plane, 4, 7) as i32, (a + b + 1) >> 1);
    }

    #[test]
    fn halfpel_bilinear_is_four_tap_average() {
        let buf = ramp_plane(8, 8);
        let plane = Svq1ReferencePlane::new(&buf, 8, 8).unwrap();
        // half (5, 7): both odd → bilinear over the 2×2 at (2,3).
        let a = plane.sample_clamped(2, 3) as i32;
        let b = plane.sample_clamped(3, 3) as i32;
        let c = plane.sample_clamped(2, 4) as i32;
        let d = plane.sample_clamped(3, 4) as i32;
        assert_eq!(
            sample_halfpel(&plane, 5, 7) as i32,
            (a + b + c + d + 2) >> 2
        );
    }

    #[test]
    fn halfpel_negative_coordinates_floor_phase() {
        let buf = ramp_plane(8, 8);
        let plane = Svq1ReferencePlane::new(&buf, 8, 8).unwrap();
        // half (-1, 0): x = floor(-1/2) = -1, frac_x = 1 (odd) → average
        // of clamped (-1,0)→(0,0) and (0,0). Both clamp to column 0.
        let a = plane.sample_clamped(-1, 0) as i32;
        let b = plane.sample_clamped(0, 0) as i32;
        assert_eq!(sample_halfpel(&plane, -1, 0) as i32, (a + b + 1) >> 1);
    }

    #[test]
    fn halfpel_uniform_plane_is_flat() {
        let buf = vec![137u8; 8 * 8];
        let plane = Svq1ReferencePlane::new(&buf, 8, 8).unwrap();
        // Every parity case averages equal samples → the same value.
        for &(hx, hy) in &[(4, 4), (5, 4), (4, 5), (5, 5)] {
            assert_eq!(sample_halfpel(&plane, hx, hy), 137, "({hx},{hy})");
        }
    }

    #[test]
    fn motion_compensate_zero_mv_is_collocated_copy() {
        let buf = ramp_plane(16, 16);
        let plane = Svq1ReferencePlane::new(&buf, 16, 16).unwrap();
        let block = motion_compensate_block(&plane, 1, 2, Svq1Mv::ZERO);
        assert_eq!(block.len(), 64);
        for row in 0..8 {
            for col in 0..8 {
                // Zero MV, even origin → integer-pel direct copy.
                let want = plane.sample_clamped(1 + col as i32, 2 + row as i32);
                assert_eq!(block[row * 8 + col], want, "row={row} col={col}");
            }
        }
    }

    #[test]
    fn motion_compensate_integer_mv_shifts_by_two_halfpels() {
        let buf = ramp_plane(16, 16);
        let plane = Svq1ReferencePlane::new(&buf, 16, 16).unwrap();
        // MV (2, -2) half-pels = (+1, -1) integer pels.
        let block = motion_compensate_block(&plane, 3, 3, Svq1Mv::new(2, -2));
        for row in 0..8 {
            for col in 0..8 {
                let want = plane.sample_clamped(3 + 1 + col as i32, 3 - 1 + row as i32);
                assert_eq!(block[row * 8 + col], want, "row={row} col={col}");
            }
        }
    }

    #[test]
    fn motion_compensate_uniform_plane_is_flat_for_any_mv() {
        let buf = vec![88u8; 16 * 16];
        let plane = Svq1ReferencePlane::new(&buf, 16, 16).unwrap();
        for mv in [Svq1Mv::new(1, 0), Svq1Mv::new(0, 1), Svq1Mv::new(1, 1)] {
            let block = motion_compensate_block(&plane, 2, 2, mv);
            assert!(block.iter().all(|&s| s == 88), "mv={mv:?}");
        }
    }

    #[test]
    fn motion_compensate_out_of_frame_mv_edge_replicates() {
        let buf = ramp_plane(8, 8);
        let plane = Svq1ReferencePlane::new(&buf, 8, 8).unwrap();
        // Large negative MV pushes the patch above-left of the plane.
        let block = motion_compensate_block(&plane, 0, 0, Svq1Mv::new(-32, -32));
        // -32 half-pels = -16 integer pels; the whole 8×8 patch lands at
        // (-16..-9, -16..-9), entirely outside, so it edge-replicates
        // the corner sample.
        assert!(block.iter().all(|&s| s == plane.sample_clamped(0, 0)));
    }

    #[test]
    fn motion_compensate_each_output_step_is_one_integer_pel() {
        // Output columns/rows advance the half-pel address by 2 → one
        // integer pel per output step. Verify against an explicit ramp.
        let buf = ramp_plane(16, 16);
        let plane = Svq1ReferencePlane::new(&buf, 16, 16).unwrap();
        let block = motion_compensate_block(&plane, 0, 0, Svq1Mv::ZERO);
        // out[0] = (0,0), out[1] = (1,0), out[8] = (0,1).
        assert_eq!(block[0], plane.sample_clamped(0, 0));
        assert_eq!(block[1], plane.sample_clamped(1, 0));
        assert_eq!(block[8], plane.sample_clamped(0, 1));
    }
}
