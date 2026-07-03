//! SVQ1 leaf-block stage-accumulation reconstruction.
//!
//! ## Scope
//!
//! Round 315 lands the **stage-accumulation composition** documented
//! in `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.5 — the
//! fixed-order summation `predictor → mean → stage-1 → … → stage-N`
//! with a `[0, 255]` saturation clamp applied AT EACH STEP (§4.5.1).
//! This is the operational pass that turns the already-staged pieces
//! into a fully reconstructed leaf block:
//!
//! * the per-sample mean step ([`crate::svq1_mean`], spec/05 §5.4 /
//!   spec/04 §4.5.3) — already landed;
//! * the within-half codebook vector lookup
//!   ([`crate::svq1_codebook::codebook_vector_in_half`], spec/14 §14.8)
//!   — already landed;
//! * the per-step `[0, 255]` clamp ([`crate::svq1_mean::saturate_u8`],
//!   spec/04 §4.5.1) — already landed.
//!
//! What this module pins per spec/04 §4.5:
//!
//! * The **addition order** — predictor first, then the mean, then
//!   stage-1, stage-2, … stage-N in ascending stage order (§4.5,
//!   "The order of additions is fixed").
//! * The **per-step saturation** — the accumulator is clamped to
//!   `[0, 255]` after EACH add, including the mean step (§4.5.1,
//!   "The accumulator is clamped to `[0, 255]` after each add. This
//!   includes the mean step (predictor + mean) and every stage
//!   step.").
//! * The **mean-only collapse** (`N = 0`, §4.5.4) and the
//!   **predictor passthrough** when `N = 0` and no mean is added by
//!   the caller — handled by passing an empty stage list.
//! * The **raster write order** (§4.7.1) — the V_L codebook bytes are
//!   summed position-by-position in output-raster order, so the
//!   returned buffer is already in raster order.
//!
//! What this module does **not** cover:
//!
//! * Reading the stage-count VLC (`N`), the mean VLC, or the 4-bit
//!   stage indices from the bitstream — those are the roles of
//!   [`crate::svq1_stage_indices`] (the indices, already landed) and
//!   a future Extractor + Specifier round for the stage-count / mean
//!   VLC tables (spec/04 §4.1, spec/05 §5.7 — bit-patterns not yet
//!   wired). The caller supplies the already-decoded `N`, mean, and
//!   per-stage `(stage, vec_idx)` pairs.
//! * Choosing the intra vs inter codebook half / mean table — that is
//!   the §4.4 half-selection decision the caller makes; this module
//!   accepts whichever half the caller hands it (per the §14.8
//!   still-open cross-half ordering note in
//!   [`crate::svq1_codebook`]).
//! * The motion-compensated predictor itself (§4.6.2 / §4.6.3) — the
//!   caller supplies the per-sample predictor array (all-zero for
//!   intra per §4.6.1, motion-compensated reference samples for
//!   inter).
//!
//! ## Per-step vs deferred saturation
//!
//! Per §4.5.2 a decoder MAY accumulate in a wider register and
//! saturate only on the final write IFF no intermediate step would
//! over- or under-flow `[0, 255]`. This is NOT bit-exact when an
//! intermediate step exceeds the range and a later step's opposite
//! contribution would bring it back (the §4.8.5 pathological example:
//! per-step `sat(sat(sat(0+250)+30)-25) = 230`; deferred
//! `250+30-25 = 255`). This module implements the NORMATIVE per-step
//! form of §4.5.1 — every add is clamped immediately.

use crate::svq1_blocktree::Svq1Level;
use crate::svq1_codebook::codebook_vector_in_half;
use crate::svq1_mean::{samples_per_leaf, saturate_u8};

/// Errors raised by [`reconstruct_leaf`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructError {
    /// The level is L=4 or L=5, which host no mean-removed VQ leaf
    /// per `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.1.2
    /// (the `(stages > 0) && (level >= 4)` validity gate) and
    /// `docs/video/svq1/spec/14.10-codebook-L4.md` / §14.11.
    AbsentLevel(Svq1Level),
    /// The supplied predictor slice does not have exactly `V_L`
    /// samples for the level (per spec/04 §4.3 / §4.7.1 — one
    /// predictor per leaf sample position).
    PredictorLength {
        /// The level whose `V_L` was expected.
        level: Svq1Level,
        /// Number of predictor samples expected (`V_L`).
        expected: usize,
        /// Number of predictor samples supplied.
        got: usize,
    },
    /// More than [`MAX_STAGES`] stages were supplied. A well-formed
    /// leaf carries `N ∈ 1..=6` stages (`N = 0` is mean-only, the
    /// empty stage list) per spec/04 §4.1 — `N > 6` cannot be
    /// produced by a conforming stage-count VLC (§4.1.1).
    TooManyStages {
        /// Maximum permitted (`6`).
        max: usize,
        /// Number supplied.
        got: usize,
    },
    /// A `(stage, vec_idx)` pair did not resolve to a codebook vector
    /// in the supplied half — out-of-range `stage` (not in `1..=6`),
    /// out-of-range `vec_idx` (not in `0..=15`), or a `half` slice
    /// too short to contain the addressed vector. See
    /// [`codebook_vector_in_half`].
    CodebookLookup {
        /// 1-based stage that failed to resolve.
        stage: usize,
        /// Vector index that failed to resolve.
        vec_idx: usize,
    },
}

impl core::fmt::Display for ReconstructError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReconstructError::AbsentLevel(level) => write!(
                f,
                "oxideav-svq: level {level:?} hosts no VQ leaf (spec/04 §4.1.2)"
            ),
            ReconstructError::PredictorLength {
                level,
                expected,
                got,
            } => write!(
                f,
                "oxideav-svq: predictor length {got} != V_L {expected} for {level:?} \
                 (spec/04 §4.3)"
            ),
            ReconstructError::TooManyStages { max, got } => write!(
                f,
                "oxideav-svq: {got} stages exceeds max {max} (spec/04 §4.1.1)"
            ),
            ReconstructError::CodebookLookup { stage, vec_idx } => write!(
                f,
                "oxideav-svq: codebook lookup failed for stage {stage} vec_idx {vec_idx} \
                 (spec/04 §4.3)"
            ),
        }
    }
}

impl std::error::Error for ReconstructError {}

/// Maximum number of codebook stages a single SVQ1 leaf can carry,
/// `6` per `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.1
/// (the stage-count VLC range `N ∈ {-1, 0..6}`). `N = -1` is SKIP
/// (no reconstruction, handled by the caller, see §4.5.5) and `N = 0`
/// is the mean-only collapse (the empty stage list, §4.5.4).
pub const MAX_STAGES: usize = 6;

/// One decoded codebook stage of a leaf: the 1-based `stage` number
/// (`1..=6`) and the 4-bit `vec_idx` (`0..=15`) that select the
/// mean-removed vector for that stage.
///
/// Per spec/04 §4.2 the stages are supplied to [`reconstruct_leaf`]
/// in ascending stage order; the `stage` field is carried explicitly
/// so the codebook addressing of §14.8 (which keys on the stage
/// number) is unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafStage {
    /// 1-based stage number (`1..=6`).
    pub stage: usize,
    /// 4-bit codebook vector index (`0..=15`).
    pub vec_idx: usize,
}

/// Reconstruct one SVQ1 leaf block per
/// `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.5.
///
/// Composes the fixed-order, per-step-saturating accumulation:
///
/// ```text
///   acc = predictor(pos)                                # §4.6
///   acc = saturate_u8(acc + mean)                        # §4.5 mean step
///   for stage in stages (ascending):                     # §4.2 order
///       acc = saturate_u8(acc + codebook[stage][vec][pos])
///   sample(pos) = acc                                    # §4.7 raster store
/// ```
///
/// for every one of the level's `V_L` sample positions, in
/// output-raster order (§4.7.1).
///
/// # Arguments
///
/// * `level` — the leaf level (`L=0..L=3`); `L=4` / `L=5` are
///   rejected with [`ReconstructError::AbsentLevel`] per §4.1.2.
/// * `half` — the codebook half (intra OR inter) for `level`, as a
///   `&[i8]` the caller has isolated from the contiguous payload (the
///   §14.8 cross-half ordering is still open — see
///   [`crate::svq1_codebook`]). A correctly-sized half is
///   [`Svq1Level::codebook_bytes_per_half`] bytes.
/// * `predictor` — the per-sample baseline (§4.6): `&[0; V_L]` for
///   intra (§4.6.1), or the motion-compensated reference samples for
///   inter (§4.6.2 / §4.6.3). MUST be exactly `V_L` long.
/// * `mean` — the already-decoded per-leaf mean, as an `i16` so it
///   covers both the intra `[0, 255]` and inter `[-256, +255]`
///   domains (spec/05 §5.1). For the mean-only case (`N = 0`) pass an
///   empty `stages` list; the mean step still runs (§4.5.4).
/// * `stages` — the decoded stages in ascending order, up to
///   [`MAX_STAGES`]. An empty slice is the mean-only collapse
///   (§4.5.4). More than `MAX_STAGES` is rejected with
///   [`ReconstructError::TooManyStages`].
///
/// # Returns
///
/// A `Vec<u8>` of `V_L` reconstructed samples in raster order, or a
/// [`ReconstructError`] for an absent level, a mis-sized predictor, a
/// stage overflow, or a failed codebook lookup. The SKIP case
/// (`N = -1`, §4.5.5) is NOT a value of this function — the caller
/// short-circuits it before calling (the block stays unchanged).
///
/// ```
/// use oxideav_svq::svq1_blocktree::Svq1Level;
/// use oxideav_svq::svq1_reconstruct::{reconstruct_leaf, LeafStage};
///
/// // Mean-only intra L=0 leaf (N = 0): every sample == mean.
/// // Predictor is zero (intra); an empty half slice is fine because
/// // no codebook lookup happens for N = 0.
/// let out = reconstruct_leaf(Svq1Level::L0, &[], &[0; 8], 61, &[]).unwrap();
/// assert_eq!(out, vec![61u8; 8]);
/// ```
pub fn reconstruct_leaf(
    level: Svq1Level,
    half: &[i8],
    predictor: &[u8],
    mean: i16,
    stages: &[LeafStage],
) -> Result<Vec<u8>, ReconstructError> {
    let v_l = samples_per_leaf(level).ok_or(ReconstructError::AbsentLevel(level))?;

    if predictor.len() != v_l {
        return Err(ReconstructError::PredictorLength {
            level,
            expected: v_l,
            got: predictor.len(),
        });
    }
    if stages.len() > MAX_STAGES {
        return Err(ReconstructError::TooManyStages {
            max: MAX_STAGES,
            got: stages.len(),
        });
    }

    // Resolve every stage's V_L codebook vector up front so a lookup
    // failure is reported before any per-sample work begins. Each
    // vector's bytes are re-ordered from their stored hierarchical
    // 4×4-tile order into output-raster order via
    // `vector_byte_to_raster` (a no-op for L=0 / L=1; the
    // empirically-pinned tile permutation for L=2 / L=3 — see
    // `crate::svq1_codebook::vector_byte_to_raster`).
    let mut stage_vectors: Vec<Vec<i8>> = Vec::with_capacity(stages.len());
    for s in stages {
        let vector = codebook_vector_in_half(half, level, s.stage, s.vec_idx).ok_or(
            ReconstructError::CodebookLookup {
                stage: s.stage,
                vec_idx: s.vec_idx,
            },
        )?;
        let mut raster = vec![0i8; v_l];
        for (byte_idx, &value) in vector.iter().enumerate() {
            raster[crate::svq1_codebook::vector_byte_to_raster(level, byte_idx)] = value;
        }
        stage_vectors.push(raster);
    }

    // §4.5: per-position fixed-order accumulation in output-raster
    // order — WIDE accumulator, saturated once at the final store.
    //
    // spec/04 §4.5.1 reads the wiki's saturation note as a per-stage
    // clamp, with §4.5.2 flagging the wider-accumulator variant as
    // an open Validator question (the two differ exactly when an
    // intermediate stage overshoots `[0, 255]` and a later stage
    // pulls the sum back in range). The black-box conformance
    // fixture pins the WIDE variant: per-stage clamping loses the
    // overshoot on eight chroma samples of the intra fixture
    // (`got < reference` by the clipped amount), while the wide
    // accumulate + final clamp reconstructs every plane byte-exact.
    // This resolves spec/04 §4.11 item 2 (and spec/14 §14.3's
    // matching caveat) in the Validator role — an erratum for the
    // per-stage reading, since the sum of a leaf's stages CAN
    // transiently leave `[0, 255]`.
    let mut out = Vec::with_capacity(v_l);
    for (pos, &pred) in predictor.iter().enumerate() {
        let mut acc = pred as i16 + mean;
        // §4.2 ascending-stage order.
        for vector in &stage_vectors {
            acc += vector[pos] as i16;
        }
        out.push(saturate_u8(acc));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an L=0 intra codebook half (768 bytes) that places the
    /// spec/04 §4.8.2 worked-example vectors at their addressed slots:
    ///
    /// * stage 1, vec_idx 4  = `7, -16, -10, 20, 7, -17, -10, 20`
    /// * stage 2, vec_idx 14 = `-13, -6, -1, -4, 25, 37, -2, -35`
    ///
    /// All other slots are zero. The slot offsets follow the §14.8
    /// within-half arithmetic (`stage_idx * 16 * V_L + vec_idx * V_L`):
    /// stage 1 vec 4 → `0*128 + 4*8 = 32`; stage 2 vec 14 →
    /// `1*128 + 14*8 = 240`.
    fn worked_example_half() -> Vec<i8> {
        let mut half = vec![0i8; Svq1Level::L0.codebook_bytes_per_half().unwrap()];
        let s1 = [7i8, -16, -10, 20, 7, -17, -10, 20];
        let s2 = [-13i8, -6, -1, -4, 25, 37, -2, -35];
        half[32..40].copy_from_slice(&s1);
        half[240..248].copy_from_slice(&s2);
        half
    }

    #[test]
    fn worked_example_section_4_8_is_bit_exact() {
        // docs/video/svq1/spec/04-multistage-vq-decoder.md §4.8:
        // intra L=0 leaf, N = 2, mean = 61, stage-1 idx = 4,
        // stage-2 idx = 14. Final block (raster):
        //   row 0:  55  39  50  77
        //   row 1:  93  81  49  46
        let half = worked_example_half();
        let stages = [
            LeafStage {
                stage: 1,
                vec_idx: 4,
            },
            LeafStage {
                stage: 2,
                vec_idx: 14,
            },
        ];
        let out = reconstruct_leaf(Svq1Level::L0, &half, &[0u8; 8], 61, &stages).unwrap();
        assert_eq!(out, vec![55, 39, 50, 77, 93, 81, 49, 46]);
    }

    #[test]
    fn worked_example_intermediate_after_mean_and_stage1() {
        // §4.8.3 table rows: after the mean step every sample is 61;
        // after stage 1 the block is [68 45 51 81 / 68 44 51 81].
        // Reproduce by truncating the stage list.
        let half = worked_example_half();
        // After the mean step alone (N = 0).
        let after_mean = reconstruct_leaf(Svq1Level::L0, &half, &[0u8; 8], 61, &[]).unwrap();
        assert_eq!(after_mean, vec![61u8; 8]);
        // After mean + stage 1 only.
        let after_s1 = reconstruct_leaf(
            Svq1Level::L0,
            &half,
            &[0u8; 8],
            61,
            &[LeafStage {
                stage: 1,
                vec_idx: 4,
            }],
        )
        .unwrap();
        assert_eq!(after_s1, vec![68, 45, 51, 81, 68, 44, 51, 81]);
    }

    #[test]
    fn mean_only_collapse_fills_with_mean() {
        // §4.5.4: for N = 0 intra (predictor = 0) every sample is the
        // mean exactly (the clamp is a no-op). Across all four levels.
        for level in [Svq1Level::L0, Svq1Level::L1, Svq1Level::L2, Svq1Level::L3] {
            let v_l = samples_per_leaf(level).unwrap();
            let out = reconstruct_leaf(level, &[], &vec![0u8; v_l], 200, &[]).unwrap();
            assert_eq!(out, vec![200u8; v_l]);
        }
    }

    #[test]
    fn mean_only_inter_predictor_saturation_is_load_bearing() {
        // §4.5.4: for inter (per-sample predictor varies, mean can be
        // negative) the mean-only clamp is load-bearing. predictor =
        // [0, 50, 255, 128], mean = -100 → [0, 0, 155, 28].
        let pred = [0u8, 50, 255, 128, 10, 200, 100, 1];
        let out = reconstruct_leaf(Svq1Level::L0, &[], &pred, -100, &[]).unwrap();
        assert_eq!(out, vec![0, 0, 155, 28, 0, 100, 0, 0]);
    }

    #[test]
    fn wide_accumulation_preserves_transient_overshoot() {
        // §4.8.5's pathological case: mean = 250, stage-1 = +30 at a
        // position, stage-2 = -25 at the same position. A per-step
        // clamp would lose the overshoot:
        //   sat(0+250)=250, sat(250+30)=255, sat(255-25)=230.
        // The wide accumulator gives 250+30-25 = 255 (clamped once
        // at the store). The black-box conformance fixture pins the
        // WIDE form (spec/04 §4.11 item 2 resolved): eight chroma
        // samples of the intra fixture reconstruct byte-exact ONLY
        // under wide accumulation.
        let mut half = vec![0i8; Svq1Level::L0.codebook_bytes_per_half().unwrap()];
        // stage 1 vec 0 at offset 0; put +30 at position 0.
        half[0] = 30;
        // stage 2 vec 0 at offset 128; put -25 at position 0.
        half[128] = -25;
        let stages = [
            LeafStage {
                stage: 1,
                vec_idx: 0,
            },
            LeafStage {
                stage: 2,
                vec_idx: 0,
            },
        ];
        let out = reconstruct_leaf(Svq1Level::L0, &half, &[0u8; 8], 250, &stages).unwrap();
        assert_eq!(
            out[0], 255,
            "wide accumulation preserves the transient overshoot"
        );
    }

    #[test]
    fn independent_recompute_matches_for_random_like_inputs() {
        // Brute-force re-derivation of the §4.5 loop in a separate
        // closure, exercised across a deterministic pseudo-random
        // sweep of predictors / mean / stage vectors at L=1 (V_L=16).
        let level = Svq1Level::L1;
        let v_l = samples_per_leaf(level).unwrap();
        let half_len = level.codebook_bytes_per_half().unwrap();
        // Fill the half with a deterministic ramp so each (stage,vec)
        // vector has distinct, signed content.
        let half: Vec<i8> = (0..half_len)
            .map(|j| ((j as i32 * 37 + 11) % 200 - 100) as i8)
            .collect();
        let stages = [
            LeafStage {
                stage: 1,
                vec_idx: 3,
            },
            LeafStage {
                stage: 3,
                vec_idx: 9,
            },
            LeafStage {
                stage: 6,
                vec_idx: 15,
            },
        ];
        for seed in 0u32..32 {
            let predictor: Vec<u8> = (0..v_l)
                .map(|p| ((seed.wrapping_mul(31) + p as u32 * 7) % 256) as u8)
                .collect();
            let mean = (seed as i16 * 17) - 256;
            let got = reconstruct_leaf(level, &half, &predictor, mean, &stages).unwrap();

            // Independent recompute (wide accumulation, single final
            // clamp; L=1 vector bytes are already raster-ordered so
            // no byte→sample permutation applies).
            let mut want = Vec::with_capacity(v_l);
            for (pos, &pred) in predictor.iter().enumerate() {
                let mut acc = pred as i16 + mean;
                for s in &stages {
                    let v = codebook_vector_in_half(&half, level, s.stage, s.vec_idx).unwrap();
                    acc += v[pos] as i16;
                }
                want.push(saturate_u8(acc));
            }
            assert_eq!(got, want, "mismatch at seed {seed}");
        }
    }

    #[test]
    fn rejects_absent_levels() {
        assert_eq!(
            reconstruct_leaf(Svq1Level::L4, &[], &[0u8; 128], 0, &[]),
            Err(ReconstructError::AbsentLevel(Svq1Level::L4))
        );
        assert_eq!(
            reconstruct_leaf(Svq1Level::L5, &[], &[0u8; 256], 0, &[]),
            Err(ReconstructError::AbsentLevel(Svq1Level::L5))
        );
    }

    #[test]
    fn rejects_mis_sized_predictor() {
        // L=0 wants V_L = 8; supply 7.
        let err = reconstruct_leaf(Svq1Level::L0, &[], &[0u8; 7], 0, &[]).unwrap_err();
        assert_eq!(
            err,
            ReconstructError::PredictorLength {
                level: Svq1Level::L0,
                expected: 8,
                got: 7,
            }
        );
    }

    #[test]
    fn rejects_too_many_stages() {
        let stages: Vec<LeafStage> = (0..MAX_STAGES + 1)
            .map(|_| LeafStage {
                stage: 1,
                vec_idx: 0,
            })
            .collect();
        let err = reconstruct_leaf(Svq1Level::L0, &[0i8; 768], &[0u8; 8], 0, &stages).unwrap_err();
        assert_eq!(
            err,
            ReconstructError::TooManyStages {
                max: MAX_STAGES,
                got: MAX_STAGES + 1,
            }
        );
    }

    #[test]
    fn rejects_codebook_lookup_failure() {
        // Out-of-range stage (0) — §14.8 stage is 1-based.
        let err = reconstruct_leaf(
            Svq1Level::L0,
            &[0i8; 768],
            &[0u8; 8],
            0,
            &[LeafStage {
                stage: 0,
                vec_idx: 0,
            }],
        )
        .unwrap_err();
        assert_eq!(
            err,
            ReconstructError::CodebookLookup {
                stage: 0,
                vec_idx: 0,
            }
        );
        // A half too short to hold an addressed vector also fails.
        let err2 = reconstruct_leaf(
            Svq1Level::L0,
            &[0i8; 4], // far too short for stage 1 vec 0 (needs 8 bytes)
            &[0u8; 8],
            0,
            &[LeafStage {
                stage: 1,
                vec_idx: 0,
            }],
        )
        .unwrap_err();
        assert_eq!(
            err2,
            ReconstructError::CodebookLookup {
                stage: 1,
                vec_idx: 0,
            }
        );
    }

    #[test]
    fn full_six_stage_leaf_stays_in_range() {
        // §4.10.1 accumulator range: with all six stages present the
        // per-step clamp keeps every output in [0, 255]. Use an L=3
        // (V_L=64) half of all +127 and predictor 0, mean 0: stage 1
        // saturates to 127, then 255 by stage 2, and stays 255.
        let level = Svq1Level::L3;
        let half = vec![127i8; level.codebook_bytes_per_half().unwrap()];
        let stages: Vec<LeafStage> = (1..=MAX_STAGES)
            .map(|stage| LeafStage { stage, vec_idx: 0 })
            .collect();
        let out = reconstruct_leaf(level, &half, &[0u8; 64], 0, &stages).unwrap();
        assert_eq!(out, vec![255u8; 64]);
    }
}
