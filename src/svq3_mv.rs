//! SVQ3 signed-Golomb entropy layer: the signed variable-length codes
//! used for motion-vector differences and the per-macroblock quantiser
//! delta.
//!
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` opens its decode
//! description with *"This codec extensively uses Golomb coding"* and
//! then, in §"Inter macroblock information decoding", states two
//! signed-coded fields:
//!
//! * *"Motion vector differences are coded as signed variable-length
//!   codes Y component first."*
//! * *"And finally before actual coefficient data you may have a
//!   quantiser delta coded as signed variable-length code."*
//!
//! The unsigned reader [`crate::svq3::read_ue_golomb`] already
//! implements the Exp-Golomb `ue(v)` mapping the wiki's coefficient and
//! macroblock-type fields use. A "signed variable-length code" in an
//! Exp-Golomb codec is the canonical *signed* pairing of that same
//! unsigned code: the unsigned `code_num k` is folded onto the signed
//! number line so that `0 → 0`, `1 → +1`, `2 → −1`, `3 → +2`,
//! `4 → −2`, … — i.e. `se(k) = (−1)^(k+1) · ⌈k / 2⌉`. This is a pure,
//! deterministic transform of the already-pinned unsigned reader (no
//! external lookup table); it is the only signed pairing consistent
//! with the wiki's "Golomb coding" framing.
//!
//! ## What this module wires
//!
//! * [`read_se_golomb`] — the signed Exp-Golomb codeword reader,
//!   layered on [`crate::svq3::read_ue_golomb`].
//! * [`MotionVectorDifference`] + [`read_mv_difference`] — one
//!   per-partition motion-vector difference, read **Y component first**
//!   exactly as the wiki dictates, then the X component.
//! * [`read_mb_mv_differences`] — pulls the right *number* of
//!   per-partition MV differences for a given inter-macroblock mode
//!   (driven by [`crate::svq3_mb::Svq3MbType::num_motion_vectors`]),
//!   in raster partition order.
//! * [`read_quantiser_delta`] — the optional signed quantiser delta
//!   that precedes the coefficient data.
//!
//! ## Scope boundary (what this module does NOT do)
//!
//! This is the *wire decode* of the signed fields only. The
//! reconstruction of an absolute motion vector from a difference
//! requires SVQ3's motion-vector predictor (median-style neighbour
//! combination + the fraction-of-six precision rounding described in
//! the wiki §"Motion Compensation"), which is a separate stage. The
//! coded-block-pattern (CBP) field that the wiki flags as *"coded the
//! same way as in H.264"* is **not** wired here: its codeword↔value
//! mapping is the H.264 `me(v)` mapped-Exp-Golomb table, which is not
//! reproduced under `docs/video/svq3/` (see the crate README "lacks"
//! tail and the round report DOCS-GAP note). Reading the CBP field
//! without that table staged would require sourcing it externally,
//! which the clean-room wall forbids.

use crate::bitreader::BitReader;
use crate::error::Result;
use crate::svq3::{read_ue_golomb, Svq3FrameType};
use crate::svq3_mb::{read_inter_mv_precision, Svq3MbType, Svq3MvPrecision};

/// Read one signed Exp-Golomb codeword (`se(v)`).
///
/// The unsigned code number `k` produced by
/// [`crate::svq3::read_ue_golomb`] is folded onto the signed number
/// line:
///
/// | `k` | value |
/// | --- | ----- |
/// | 0   | `0`   |
/// | 1   | `+1`  |
/// | 2   | `−1`  |
/// | 3   | `+2`  |
/// | 4   | `−2`  |
/// | 5   | `+3`  |
/// | …   | …     |
///
/// i.e. `value = (−1)^(k+1) · ⌈k / 2⌉`. Odd `k` map to positive
/// magnitudes, even non-zero `k` to negative magnitudes; magnitude is
/// `(k + 1) / 2`.
///
/// Returns [`crate::error::Error::Truncated`] if the bit-reader runs
/// out of input mid-codeword (propagated from the unsigned reader).
pub fn read_se_golomb(br: &mut BitReader<'_>) -> Result<i32> {
    let k = read_ue_golomb(br)?;
    // magnitude = ceil(k / 2) = (k + 1) / 2 in integer arithmetic.
    let magnitude = ((k + 1) >> 1) as i32;
    // Odd k → positive, even non-zero k → negative, k == 0 → 0.
    if k & 1 == 1 {
        Ok(magnitude)
    } else {
        Ok(-magnitude)
    }
}

/// A single per-partition motion-vector difference, in the codec's
/// native motion-vector units (the wiki's "fraction of six" grid
/// before precision rounding).
///
/// The wiki §"Inter macroblock information decoding" specifies the
/// **Y component is coded first**, then the X component. Both are
/// signed Exp-Golomb codewords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MotionVectorDifference {
    /// Horizontal component difference (`dx`). Coded **second**.
    pub dx: i32,
    /// Vertical component difference (`dy`). Coded **first**.
    pub dy: i32,
}

/// Read one motion-vector difference, Y component first.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Inter macroblock
/// information decoding": *"Motion vector differences are coded as
/// signed variable-length codes Y component first."* So the bit-reader
/// is advanced over the `dy` codeword first, then the `dx` codeword.
///
/// Returns [`crate::error::Error::Truncated`] if either codeword runs
/// past the end of the bit-reader.
pub fn read_mv_difference(br: &mut BitReader<'_>) -> Result<MotionVectorDifference> {
    let dy = read_se_golomb(br)?;
    let dx = read_se_golomb(br)?;
    Ok(MotionVectorDifference { dx, dy })
}

/// Read all per-partition motion-vector differences for an
/// inter-coded macroblock, in raster partition order.
///
/// The count is governed by the macroblock type's
/// [`Svq3MbType::num_motion_vectors`]: SKIP / intra / B-direct carry
/// zero on-wire differences (an empty vector is returned); the inter
/// partition modes carry 1 / 2 / 4 / 8 / 16 differences. Each is read
/// Y-first via [`read_mv_difference`].
///
/// The partition layout that maps each returned index back to a pixel
/// region (8×16 / 16×8 / 8×8 / 4×8 / 8×4 / 4×4) is the inter-mode's
/// own geometry; this reader is responsible only for pulling the right
/// *number* of differences off the wire in order. The absolute motion
/// vector is later formed by the SVQ3 predictor stage (out of scope
/// here — see the module-level docs).
///
/// Returns [`crate::error::Error::Truncated`] if the bit-reader is
/// exhausted before all differences are read.
pub fn read_mb_mv_differences(
    br: &mut BitReader<'_>,
    mb_type: Svq3MbType,
) -> Result<Vec<MotionVectorDifference>> {
    let count = mb_type.num_motion_vectors() as usize;
    let mut diffs = Vec::with_capacity(count);
    for _ in 0..count {
        diffs.push(read_mv_difference(br)?);
    }
    Ok(diffs)
}

/// Read the optional signed quantiser delta that may precede the
/// coefficient data of a macroblock.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Inter macroblock
/// information decoding": *"And finally before actual coefficient data
/// you may have a quantiser delta coded as signed variable-length
/// code."* The presence of the field is signalled by the slice
/// header's "delta quantiser may be present" flag (captured by the
/// slice-header parser); when present the delta is a single signed
/// Exp-Golomb codeword and is added to the running slice quantiser.
///
/// This reader assumes the caller has already determined the field is
/// present; it reads exactly one [`read_se_golomb`] codeword.
///
/// Returns [`crate::error::Error::Truncated`] on a short read.
pub fn read_quantiser_delta(br: &mut BitReader<'_>) -> Result<i32> {
    read_se_golomb(br)
}

/// The decoded inter-macroblock motion header: the chosen sub-pixel
/// precision plus every per-partition motion-vector difference, in
/// raster partition order.
///
/// This is the composition of the two staged inter-MB wire fields the
/// wiki §"Inter macroblock information decoding" describes in order:
///
/// 1. the precision selector (`read_inter_mv_precision`, frame-type
///    aware: B-frames are always halfpel and consume no bit), then
/// 2. the per-partition motion-vector differences, Y component first
///    each ([`read_mb_mv_differences`]).
///
/// The motion-vector *predictor* + absolute-vector reconstruction (the
/// median neighbour combination and the wiki's fraction-of-six
/// precision rounding) is a separate stage and is not part of this
/// header; this struct carries the raw decoded wire fields only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq3InterMacroblockHeader {
    /// The sub-pixel precision for this macroblock's motion
    /// compensation. Always [`Svq3MvPrecision::Halfpel`] for B-frame
    /// inter macroblocks (consumes no bit); selected per the
    /// three-branch rule for P-frame inter macroblocks.
    pub precision: Svq3MvPrecision,
    /// The per-partition motion-vector differences, in raster
    /// partition order. The length equals
    /// [`Svq3MbType::num_motion_vectors`] for the macroblock type.
    pub mv_differences: Vec<MotionVectorDifference>,
}

/// Read an inter-macroblock motion header from the slice body bit
/// reader.
///
/// Composes the precision selector and the per-partition MV-difference
/// list for an inter-coded macroblock, in the wiki's documented field
/// order. `has_thirdpel` / `has_halfpel` come from the sequence
/// header's precision-capability flags; `frame_type` governs whether
/// the precision selector consumes a bit (P-frame) or is implicit
/// halfpel (B-frame).
///
/// SKIP macroblocks carry no motion header and B-direct macroblocks
/// derive their motion vectors rather than coding them; callers should
/// detect those via [`Svq3MbType::is_skip`] /
/// [`Svq3MbType::num_motion_vectors`] before invoking this. When this
/// function is called on a zero-MV inter type it still reads the
/// precision selector (the P-frame precision bit is part of the inter
/// macroblock envelope) but produces an empty `mv_differences` list.
///
/// Returns [`crate::error::Error::Truncated`] on a short read at any
/// field.
pub fn read_inter_macroblock_header(
    br: &mut BitReader<'_>,
    mb_type: Svq3MbType,
    frame_type: Svq3FrameType,
    has_thirdpel: bool,
    has_halfpel: bool,
) -> Result<Svq3InterMacroblockHeader> {
    let precision = read_inter_mv_precision(br, frame_type, has_thirdpel, has_halfpel)?;
    let mv_differences = read_mb_mv_differences(br, mb_type)?;
    Ok(Svq3InterMacroblockHeader {
        precision,
        mv_differences,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq3_mb::{BFrameInterMode, IFrameMbType, PFrameInterMode};

    /// Pack a list of `(value, nbits)` MSB-first into a byte buffer for
    /// the bit reader.
    fn pack(items: &[(u32, u32)]) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();
        for &(value, nbits) in items {
            for i in (0..nbits).rev() {
                bits.push(((value >> i) & 1) as u8);
            }
        }
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, &bit) in bits.iter().enumerate() {
            if bit == 1 {
                out[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        out
    }

    /// Encode unsigned Exp-Golomb `code_num n` as `(value, nbits)`.
    fn ue(n: u32) -> (u32, u32) {
        let code = n + 1;
        let bits = 32 - code.leading_zeros();
        // Exp-Golomb: (bits-1) leading zeros, then `code` in `bits` bits.
        let total = 2 * bits - 1;
        (code, total)
    }

    /// Encode a signed value as its `se(v)` Exp-Golomb code_num then as
    /// raw `(value, nbits)`.
    fn se(v: i32) -> (u32, u32) {
        let k = if v > 0 {
            (2 * v - 1) as u32
        } else {
            (-2 * v) as u32
        };
        ue(k)
    }

    #[test]
    fn se_golomb_zero() {
        let buf = pack(&[ue(0)]);
        let mut br = BitReader::new(&buf);
        assert_eq!(read_se_golomb(&mut br).unwrap(), 0);
    }

    #[test]
    fn se_golomb_canonical_folding() {
        // k → value: 0→0, 1→+1, 2→−1, 3→+2, 4→−2, 5→+3, 6→−3.
        let expected = [
            (0u32, 0i32),
            (1, 1),
            (2, -1),
            (3, 2),
            (4, -2),
            (5, 3),
            (6, -3),
        ];
        for &(k, want) in &expected {
            let buf = pack(&[ue(k)]);
            let mut br = BitReader::new(&buf);
            assert_eq!(read_se_golomb(&mut br).unwrap(), want, "k={k}");
        }
    }

    #[test]
    fn se_golomb_round_trips_signed_range() {
        for v in -20i32..=20 {
            let buf = pack(&[se(v)]);
            let mut br = BitReader::new(&buf);
            assert_eq!(read_se_golomb(&mut br).unwrap(), v, "v={v}");
        }
    }

    #[test]
    fn se_golomb_odd_positive_even_negative() {
        // Odd code_num → positive magnitude, even non-zero → negative.
        for k in 1u32..=10 {
            let buf = pack(&[ue(k)]);
            let mut br = BitReader::new(&buf);
            let got = read_se_golomb(&mut br).unwrap();
            if k & 1 == 1 {
                assert!(got > 0, "k={k} should be positive");
            } else {
                assert!(got < 0, "k={k} should be negative");
            }
            assert_eq!(got.unsigned_abs(), k.div_ceil(2), "magnitude k={k}");
        }
    }

    #[test]
    fn se_golomb_truncated_propagates() {
        // Empty buffer cannot supply even the first stop bit.
        let mut br = BitReader::new(&[]);
        assert!(matches!(
            read_se_golomb(&mut br),
            Err(crate::error::Error::Truncated)
        ));
    }

    #[test]
    fn mv_difference_reads_y_then_x() {
        // dy = +3 (k=5), dx = −2 (k=4), in that wire order.
        let buf = pack(&[se(3), se(-2)]);
        let mut br = BitReader::new(&buf);
        let mv = read_mv_difference(&mut br).unwrap();
        assert_eq!(mv.dy, 3);
        assert_eq!(mv.dx, -2);
    }

    #[test]
    fn mv_difference_zero_zero() {
        let buf = pack(&[ue(0), ue(0)]);
        let mut br = BitReader::new(&buf);
        let mv = read_mv_difference(&mut br).unwrap();
        assert_eq!(mv, MotionVectorDifference { dx: 0, dy: 0 });
    }

    #[test]
    fn mv_difference_truncated_after_y() {
        // Only a single `0` bit: dy reads code_num 0, dx then truncates.
        let buf = pack(&[ue(0)]);
        let mut br = BitReader::new(&buf);
        assert!(matches!(
            read_mv_difference(&mut br),
            Err(crate::error::Error::Truncated)
        ));
    }

    #[test]
    fn mb_mv_differences_count_matches_inter_mode() {
        // Inter8x8 → 4 MVs; supply 4 (dy, dx) pairs all equal to (0,0).
        let zeros: Vec<(u32, u32)> = (0..8).map(|_| ue(0)).collect();
        let buf = pack(&zeros);
        let mut br = BitReader::new(&buf);
        let diffs =
            read_mb_mv_differences(&mut br, Svq3MbType::PInter(PFrameInterMode::Inter8x8)).unwrap();
        assert_eq!(diffs.len(), 4);
        assert!(diffs
            .iter()
            .all(|d| *d == MotionVectorDifference::default()));
    }

    #[test]
    fn mb_mv_differences_inter4x4_reads_sixteen() {
        let zeros: Vec<(u32, u32)> = (0..32).map(|_| ue(0)).collect();
        let buf = pack(&zeros);
        let mut br = BitReader::new(&buf);
        let diffs =
            read_mb_mv_differences(&mut br, Svq3MbType::PInter(PFrameInterMode::Inter4x4)).unwrap();
        assert_eq!(diffs.len(), 16);
    }

    #[test]
    fn mb_mv_differences_skip_reads_nothing() {
        let mut br = BitReader::new(&[]);
        let diffs =
            read_mb_mv_differences(&mut br, Svq3MbType::PInter(PFrameInterMode::Skip)).unwrap();
        assert!(diffs.is_empty());
        assert_eq!(br.bits_consumed(), 0);
    }

    #[test]
    fn mb_mv_differences_intra_reads_nothing() {
        let mut br = BitReader::new(&[]);
        let diffs =
            read_mb_mv_differences(&mut br, Svq3MbType::PIntra(IFrameMbType::LumaDcSeparate))
                .unwrap();
        assert!(diffs.is_empty());
    }

    #[test]
    fn mb_mv_differences_b_direct_reads_nothing() {
        let mut br = BitReader::new(&[]);
        let diffs =
            read_mb_mv_differences(&mut br, Svq3MbType::BInter(BFrameInterMode::Direct)).unwrap();
        assert!(diffs.is_empty());
    }

    #[test]
    fn mb_mv_differences_b_bidirectional_reads_two() {
        let zeros: Vec<(u32, u32)> = (0..4).map(|_| ue(0)).collect();
        let buf = pack(&zeros);
        let mut br = BitReader::new(&buf);
        let diffs =
            read_mb_mv_differences(&mut br, Svq3MbType::BInter(BFrameInterMode::Bidirectional))
                .unwrap();
        assert_eq!(diffs.len(), 2);
    }

    #[test]
    fn mb_mv_differences_truncated_midway_propagates() {
        // Inter16x16 needs one (dy, dx) pair = two codewords; supply one.
        let buf = pack(&[ue(0)]);
        let mut br = BitReader::new(&buf);
        assert!(matches!(
            read_mb_mv_differences(&mut br, Svq3MbType::PInter(PFrameInterMode::Inter16x16)),
            Err(crate::error::Error::Truncated)
        ));
    }

    #[test]
    fn quantiser_delta_signed() {
        let buf = pack(&[se(-4)]);
        let mut br = BitReader::new(&buf);
        assert_eq!(read_quantiser_delta(&mut br).unwrap(), -4);
        let buf2 = pack(&[se(7)]);
        let mut br2 = BitReader::new(&buf2);
        assert_eq!(read_quantiser_delta(&mut br2).unwrap(), 7);
    }

    #[test]
    fn mv_difference_consumes_only_two_codewords() {
        // Two (dy,dx) codewords followed by a sentinel `1` bit; the
        // reader must stop exactly after the second codeword.
        let mut items = vec![se(1), se(-1)];
        items.push((1, 1)); // trailing marker bit that must remain.
        let buf = pack(&items);
        let mut br = BitReader::new(&buf);
        let before = br.total_bits();
        let mv = read_mv_difference(&mut br).unwrap();
        assert_eq!(mv.dy, 1);
        assert_eq!(mv.dx, -1);
        // One bit must remain unconsumed (the trailing marker).
        assert_eq!(br.bits_remaining(), before - br.bits_consumed());
        assert_eq!(br.read_bit().unwrap(), 1);
    }

    #[test]
    fn inter_mb_header_p_frame_16x16_reads_precision_then_one_mv() {
        // No precision flags set → fullpel, zero precision bits.
        // Then one (dy, dx) pair: dy = +2 (k=3), dx = +1 (k=1).
        let buf = pack(&[se(2), se(1)]);
        let mut br = BitReader::new(&buf);
        let hdr = read_inter_macroblock_header(
            &mut br,
            Svq3MbType::PInter(PFrameInterMode::Inter16x16),
            Svq3FrameType::Predicted,
            false,
            false,
        )
        .unwrap();
        assert_eq!(hdr.precision, Svq3MvPrecision::Fullpel);
        assert_eq!(hdr.mv_differences.len(), 1);
        assert_eq!(hdr.mv_differences[0].dy, 2);
        assert_eq!(hdr.mv_differences[0].dx, 1);
    }

    #[test]
    fn inter_mb_header_p_frame_precision_bit_consumed_before_mvs() {
        // has_halfpel only → one precision bit. Bit `1` (!= has_thirdpel
        // false) selects halfpel, then read one MV pair (0, 0).
        let mut items = vec![(1u32, 1u32)];
        items.push(ue(0));
        items.push(ue(0));
        let buf = pack(&items);
        let mut br = BitReader::new(&buf);
        let hdr = read_inter_macroblock_header(
            &mut br,
            Svq3MbType::PInter(PFrameInterMode::Inter16x16),
            Svq3FrameType::Predicted,
            false,
            true,
        )
        .unwrap();
        assert_eq!(hdr.precision, Svq3MvPrecision::Halfpel);
        assert_eq!(hdr.mv_differences.len(), 1);
        assert_eq!(hdr.mv_differences[0], MotionVectorDifference::default());
    }

    #[test]
    fn inter_mb_header_b_frame_is_implicit_halfpel_no_precision_bit() {
        // B-frame forward block: no precision bit consumed, one MV pair.
        let buf = pack(&[se(-1), se(3)]);
        let mut br = BitReader::new(&buf);
        let hdr = read_inter_macroblock_header(
            &mut br,
            Svq3MbType::BInter(BFrameInterMode::Forward),
            Svq3FrameType::Bidirectional,
            true,
            true,
        )
        .unwrap();
        assert_eq!(hdr.precision, Svq3MvPrecision::Halfpel);
        assert_eq!(hdr.mv_differences.len(), 1);
        assert_eq!(hdr.mv_differences[0].dy, -1);
        assert_eq!(hdr.mv_differences[0].dx, 3);
    }

    #[test]
    fn inter_mb_header_8x8_reads_four_mvs() {
        let zeros: Vec<(u32, u32)> = (0..8).map(|_| ue(0)).collect();
        let buf = pack(&zeros);
        let mut br = BitReader::new(&buf);
        let hdr = read_inter_macroblock_header(
            &mut br,
            Svq3MbType::PInter(PFrameInterMode::Inter8x8),
            Svq3FrameType::Predicted,
            false,
            false,
        )
        .unwrap();
        assert_eq!(hdr.mv_differences.len(), 4);
    }

    #[test]
    fn inter_mb_header_skip_reads_precision_only() {
        // SKIP carries zero MVs; with no precision flags it reads no bit.
        let mut br = BitReader::new(&[]);
        let hdr = read_inter_macroblock_header(
            &mut br,
            Svq3MbType::PInter(PFrameInterMode::Skip),
            Svq3FrameType::Predicted,
            false,
            false,
        )
        .unwrap();
        assert_eq!(hdr.precision, Svq3MvPrecision::Fullpel);
        assert!(hdr.mv_differences.is_empty());
    }

    #[test]
    fn inter_mb_header_truncated_in_mvs_propagates() {
        // Inter16x16 with fullpel (no precision bit) but only one
        // codeword supplied → dx truncates.
        let buf = pack(&[ue(0)]);
        let mut br = BitReader::new(&buf);
        assert!(matches!(
            read_inter_macroblock_header(
                &mut br,
                Svq3MbType::PInter(PFrameInterMode::Inter16x16),
                Svq3FrameType::Predicted,
                false,
                false,
            ),
            Err(crate::error::Error::Truncated)
        ));
    }
}
