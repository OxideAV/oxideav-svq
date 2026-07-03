//! SVQ1 encoder leaf search — the inverse of the spec/04 §4.5 stage
//! accumulation.
//!
//! The decoder reconstructs a leaf as the fixed-order sum
//! `predictor → mean → stage-1 → … → stage-N` with a single final
//! `[0, 255]` clamp (the wide-accumulation form pinned by the
//! black-box conformance fixtures — see
//! [`crate::svq1_reconstruct::reconstruct_leaf`]). The encoder-side
//! inverse implemented here picks, for one leaf's target samples and
//! per-sample predictor:
//!
//! * the transmitted **mean** — the rounded average of the
//!   `target − predictor` residual, clamped to the half's mean
//!   domain (intra `[0, 255]`, inter `[-256, +255]` per spec/05
//!   §5.1);
//! * up to `max_stages` **stage vector indices**, chosen greedily in
//!   ascending stage order per spec/04 §4.2: at each stage the
//!   SSE-minimising vector of the sixteen in that `(level, half,
//!   stage)` codebook page is committed IF it strictly reduces the
//!   leaf SSE, else the search stops (a shorter stage list is also
//!   cheaper on the wire — the stage-count VLC of spec/03 §3.6 plus
//!   `4N` index bits per spec/04 §4.2).
//!
//! The greedy stage-descent mirrors the multistage-VQ structure
//! itself: each stage's codebook is a residual refinement of the
//! stages before it (spec/14 §14.4), so committing the per-stage SSE
//! minimiser is the natural encoding direction. The searcher models
//! the decoder's arithmetic EXACTLY — wide `i32` accumulation, one
//! final saturation — so `recon` in the returned [`LeafChoice`] is
//! byte-identical to what [`crate::svq1_reconstruct::reconstruct_leaf`]
//! produces for the same wire symbols (asserted in tests).
//!
//! ## Wire cost accounting
//!
//! Every choice carries its exact bit cost so the block-tree searcher
//! (`crate::svq1_enc`) can weigh subdivision against in-place
//! quantisation:
//!
//! * SKIP (`N = −1`, inter only): the stage-count codeword at
//!   alphabet position 0 (spec/04 §4.1's `N = position − 1` mapping).
//! * Mean-only (`N = 0`): stage-count position 1 + the mean codeword.
//! * Mean + stages: stage-count position `N + 1` + mean codeword +
//!   `4N` raw index bits (spec/04 §4.2.1 — raw binary, not VLC).

use crate::svq1_blocktree::Svq1Level;
use crate::svq1_codebook::{
    codebook_half, vector_byte_to_raster, SVQ1_ENTRIES_PER_STAGE, SVQ1_VLC_INTER_MEAN,
    SVQ1_VLC_INTRA_MEAN,
};
use crate::svq1_mean::{saturate_u8, INTER_MEAN_MAX, INTER_MEAN_MIN};
use crate::svq1_vlc::{inter_stage_count_table, intra_stage_count_table, Svq1Half};

/// The coded content of one leaf, as chosen by [`search_leaf`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafCode {
    /// Inter-only `N = −1`: the leaf keeps its motion-compensated
    /// predictor content untouched (spec/04 §4.5.5).
    Skip,
    /// `N = 0..6`: transmitted mean plus `stages.len()` stage vector
    /// indices in ascending stage order (spec/04 §4.5).
    Coded {
        /// The transmitted mean (intra `[0, 255]`, inter
        /// `[-256, +255]`).
        mean: i16,
        /// The 4-bit vector index per committed stage, ascending
        /// stage order.
        stages: Vec<u8>,
    },
}

/// One leaf-search outcome: the wire symbols, their exact bit cost,
/// the resulting SSE against the target, and the decoder-identical
/// reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafChoice {
    /// What goes on the wire.
    pub code: LeafCode,
    /// Exact bit cost of the leaf payload (stage-count VLC + mean VLC
    /// + `4N` index bits; excludes the block-tree decision bit).
    pub bits: u32,
    /// Sum of squared errors of `recon` against the search target.
    pub sse: u64,
    /// The decoder's reconstruction of this leaf, raster order.
    pub recon: Vec<u8>,
}

/// Bit length of the stage-count codeword for `n` stages (`n = -1`
/// is SKIP) at `(level, half)` — alphabet position `n + 1` per the
/// audit-corrected spec/04 §4.1 mapping.
fn stage_count_bits(level: Svq1Level, half: Svq1Half, n: i8) -> u32 {
    let table = match half {
        Svq1Half::Intra => intra_stage_count_table(level),
        Svq1Half::Inter => inter_stage_count_table(level),
    };
    u32::from(table.0[(n + 1) as usize].1)
}

/// Bit length of the mean codeword for `mean` in `half`'s mean table
/// (intra alphabet position = mean; inter position = mean + 256 per
/// spec/05 §5.1.2's `[-256, +255]` domain).
fn mean_bits(half: Svq1Half, mean: i16) -> u32 {
    match half {
        Svq1Half::Intra => u32::from(SVQ1_VLC_INTRA_MEAN.0[mean as usize].1),
        Svq1Half::Inter => u32::from(SVQ1_VLC_INTER_MEAN.0[(mean + 256) as usize].1),
    }
}

/// SSE between the target and the saturated wide accumulator.
fn acc_sse(target: &[u8], acc: &[i32]) -> u64 {
    target
        .iter()
        .zip(acc.iter())
        .map(|(&t, &a)| {
            let d = i64::from(t) - i64::from(saturate_u8((a).clamp(-1024, 1279) as i16));
            (d * d) as u64
        })
        .sum()
}

/// The codebook page for `(level, half)` with every vector re-ordered
/// to output-raster sample order, one `Vec<i16>` per `(stage, vec)`.
///
/// Index arithmetic matches spec/14 §14.5: `stage_idx * 16 + vec_idx`.
fn raster_page(level: Svq1Level, half: Svq1Half) -> Option<Vec<Vec<i16>>> {
    let page = codebook_half(level, half)?;
    let v_l = usize::from(level.vector_length());
    let stages = page.len() / (SVQ1_ENTRIES_PER_STAGE * v_l);
    let mut out = Vec::with_capacity(stages * SVQ1_ENTRIES_PER_STAGE);
    for entry in 0..stages * SVQ1_ENTRIES_PER_STAGE {
        let mut raster = vec![0i16; v_l];
        for byte_idx in 0..v_l {
            raster[vector_byte_to_raster(level, byte_idx)] =
                i16::from(page[entry * v_l + byte_idx]);
        }
        out.push(raster);
    }
    Some(out)
}

/// Search one leaf: pick the mean and up to `max_stages` greedy
/// stage vectors minimising SSE against `target`, given the
/// decoder-visible `predictor` baseline (all-zero for intra leaves
/// per spec/04 §4.6.1; the motion-compensated reference for inter
/// leaves per §4.6.2 / §4.6.3).
///
/// `allow_skip` additionally evaluates the inter-only `N = −1` SKIP
/// leaf (predictor kept verbatim) and returns it when it is at least
/// as good in SSE as the coded alternative — SKIP is never longer on
/// the wire. Callers pass `false` on the intra path (spec/04 §4.9.1:
/// intra SKIP is a stream-format violation).
///
/// At L=4 / L=5 no codebook exists (spec/04 §4.1.2), so the search
/// degenerates to mean-only regardless of `max_stages`.
///
/// # Panics
///
/// Panics if `target` / `predictor` lengths differ from the level's
/// `V_L`, or if `max_stages > 6`.
pub fn search_leaf(
    level: Svq1Level,
    half: Svq1Half,
    target: &[u8],
    predictor: &[u8],
    max_stages: usize,
    allow_skip: bool,
) -> LeafChoice {
    let v_l = usize::from(level.vector_length());
    assert_eq!(target.len(), v_l, "target length != V_L");
    assert_eq!(predictor.len(), v_l, "predictor length != V_L");
    assert!(max_stages <= 6, "max_stages > 6");

    // Rounded residual mean, clamped to the half's domain (spec/05
    // §5.1). Round half away from zero so positive and negative
    // residuals are treated symmetrically.
    let residual_sum: i64 = target
        .iter()
        .zip(predictor.iter())
        .map(|(&t, &p)| i64::from(t) - i64::from(p))
        .sum();
    let len = v_l as i64;
    let mean_rounded = if residual_sum >= 0 {
        (residual_sum + len / 2) / len
    } else {
        (residual_sum - len / 2) / len
    };
    let mean = match half {
        Svq1Half::Intra => mean_rounded.clamp(0, 255) as i16,
        Svq1Half::Inter => {
            mean_rounded.clamp(i64::from(INTER_MEAN_MIN), i64::from(INTER_MEAN_MAX)) as i16
        }
    };

    // Wide accumulator seeded with predictor + mean — the decoder's
    // §4.5 arithmetic with the single final clamp.
    let mut acc: Vec<i32> = predictor
        .iter()
        .map(|&p| i32::from(p) + i32::from(mean))
        .collect();
    let mut sse = acc_sse(target, &acc);
    let mut stages: Vec<u8> = Vec::new();

    // Greedy ascending-stage descent (spec/04 §4.2 ordering).
    if max_stages > 0 {
        if let Some(page) = raster_page(level, half) {
            let mut scratch = vec![0i32; v_l];
            for stage_idx in 0..max_stages {
                let mut best: Option<(u8, u64)> = None;
                for vec_idx in 0..SVQ1_ENTRIES_PER_STAGE {
                    let vector = &page[stage_idx * SVQ1_ENTRIES_PER_STAGE + vec_idx];
                    for (pos, s) in scratch.iter_mut().enumerate() {
                        *s = acc[pos] + i32::from(vector[pos]);
                    }
                    let candidate = acc_sse(target, &scratch);
                    if best.map_or(true, |(_, b)| candidate < b) {
                        best = Some((vec_idx as u8, candidate));
                    }
                }
                let (vec_idx, best_sse) = best.expect("sixteen vectors per stage");
                if best_sse >= sse {
                    break; // no strict improvement — stop the descent
                }
                let vector = &page[stage_idx * SVQ1_ENTRIES_PER_STAGE + usize::from(vec_idx)];
                for (pos, a) in acc.iter_mut().enumerate() {
                    *a += i32::from(vector[pos]);
                }
                sse = best_sse;
                stages.push(vec_idx);
            }
        }
    }

    let n = stages.len();
    let bits = stage_count_bits(level, half, n as i8) + mean_bits(half, mean) + 4 * n as u32;
    let recon: Vec<u8> = acc
        .iter()
        .map(|&a| saturate_u8(a.clamp(-1024, 1279) as i16))
        .collect();
    let coded = LeafChoice {
        code: LeafCode::Coded { mean, stages },
        bits,
        sse,
        recon,
    };

    if allow_skip {
        let skip_sse = target
            .iter()
            .zip(predictor.iter())
            .map(|(&t, &p)| {
                let d = i64::from(t) - i64::from(p);
                (d * d) as u64
            })
            .sum::<u64>();
        if skip_sse <= coded.sse {
            return LeafChoice {
                code: LeafCode::Skip,
                bits: stage_count_bits(level, half, -1),
                sse: skip_sse,
                recon: predictor.to_vec(),
            };
        }
    }
    coded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq1_reconstruct::{reconstruct_leaf, LeafStage};

    fn decode_choice(
        level: Svq1Level,
        half: Svq1Half,
        predictor: &[u8],
        choice: &LeafChoice,
    ) -> Vec<u8> {
        match &choice.code {
            LeafCode::Skip => predictor.to_vec(),
            LeafCode::Coded { mean, stages } => {
                if stages.is_empty() {
                    // Mean-only: what the plane decoder does inline.
                    return predictor
                        .iter()
                        .map(|&p| (i16::from(p) + mean).clamp(0, 255) as u8)
                        .collect();
                }
                let page = codebook_half(level, half).expect("codebook page");
                let leaf_stages: Vec<LeafStage> = stages
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| LeafStage {
                        stage: i + 1,
                        vec_idx: usize::from(v),
                    })
                    .collect();
                reconstruct_leaf(level, page, predictor, *mean, &leaf_stages).expect("reconstructs")
            }
        }
    }

    fn pseudo_block(len: usize, seed: u32) -> Vec<u8> {
        (0..len)
            .map(|i| {
                let x = (i as u32).wrapping_mul(2654435761).wrapping_add(seed);
                ((x >> 16) & 0xff) as u8
            })
            .collect()
    }

    /// The searcher's `recon` must equal the decoder's reconstruction
    /// of the same wire symbols for every level, half, and stage
    /// budget — the round-trip invariant that makes the encoder
    /// bit-consistent by construction.
    #[test]
    fn recon_matches_decoder_reconstruction() {
        for level in [Svq1Level::L0, Svq1Level::L1, Svq1Level::L2, Svq1Level::L3] {
            let v_l = usize::from(level.vector_length());
            for (seed, half) in [(1u32, Svq1Half::Intra), (2, Svq1Half::Inter)] {
                let target = pseudo_block(v_l, seed);
                let predictor = match half {
                    Svq1Half::Intra => vec![0u8; v_l],
                    Svq1Half::Inter => pseudo_block(v_l, seed + 99),
                };
                for max_stages in [0usize, 1, 3, 6] {
                    let choice = search_leaf(level, half, &target, &predictor, max_stages, false);
                    let decoded = decode_choice(level, half, &predictor, &choice);
                    assert_eq!(
                        choice.recon, decoded,
                        "{level:?} {half:?} max_stages={max_stages}"
                    );
                    let sse: u64 = target
                        .iter()
                        .zip(decoded.iter())
                        .map(|(&t, &d)| {
                            let e = i64::from(t) - i64::from(d);
                            (e * e) as u64
                        })
                        .sum();
                    assert_eq!(choice.sse, sse, "reported SSE matches recon");
                }
            }
        }
    }

    /// More stage budget never increases SSE (the greedy descent only
    /// commits strict improvements).
    #[test]
    fn sse_is_monotone_in_stage_budget() {
        let level = Svq1Level::L3;
        let v_l = usize::from(level.vector_length());
        for seed in 0..8u32 {
            let target = pseudo_block(v_l, seed);
            let predictor = vec![0u8; v_l];
            let mut prev = u64::MAX;
            for max_stages in 0..=6usize {
                let choice = search_leaf(
                    level,
                    Svq1Half::Intra,
                    &target,
                    &predictor,
                    max_stages,
                    false,
                );
                assert!(
                    choice.sse <= prev,
                    "seed {seed}: SSE rose from {prev} to {} at budget {max_stages}",
                    choice.sse
                );
                prev = choice.sse;
            }
        }
    }

    /// A target that IS `predictor + mean + stage-1 vector` exactly is
    /// found exactly (zero SSE, one stage).
    #[test]
    fn exact_one_stage_target_is_recovered() {
        let level = Svq1Level::L2;
        let v_l = usize::from(level.vector_length());
        let page = raster_page(level, Svq1Half::Intra).unwrap();
        let vector = &page[7]; // stage 1, vec 7
        let mean = 90i16;
        let target: Vec<u8> = vector.iter().map(|&v| saturate_u8(mean + v)).collect();
        let choice = search_leaf(level, Svq1Half::Intra, &target, &vec![0u8; v_l], 6, false);
        assert_eq!(choice.sse, 0, "exact target must reach zero SSE");
        match &choice.code {
            LeafCode::Coded { stages, .. } => {
                assert_eq!(stages.as_slice(), &[7u8], "stage-1 vector 7 recovered")
            }
            other => panic!("expected coded leaf, got {other:?}"),
        }
    }

    /// A perfect predictor makes SKIP win when allowed: zero residual
    /// means the coded alternative cannot beat SKIP's SSE and SKIP is
    /// cheapest on the wire.
    #[test]
    fn perfect_predictor_prefers_skip() {
        let level = Svq1Level::L3;
        let v_l = usize::from(level.vector_length());
        let predictor = pseudo_block(v_l, 5);
        let choice = search_leaf(level, Svq1Half::Inter, &predictor, &predictor, 6, true);
        assert_eq!(choice.code, LeafCode::Skip);
        assert_eq!(choice.sse, 0);
        assert_eq!(choice.recon, predictor);
    }

    /// L=4 / L=5 degenerate to mean-only regardless of budget
    /// (spec/04 §4.1.2 — no codebook at those levels).
    #[test]
    fn l4_l5_are_mean_only() {
        for level in [Svq1Level::L4, Svq1Level::L5] {
            let v_l = usize::from(level.vector_length());
            let target = pseudo_block(v_l, 3);
            let choice = search_leaf(level, Svq1Half::Intra, &target, &vec![0u8; v_l], 6, false);
            match &choice.code {
                LeafCode::Coded { stages, .. } => {
                    assert!(stages.is_empty(), "{level:?} must not carry stages")
                }
                other => panic!("expected coded leaf, got {other:?}"),
            }
        }
    }

    /// Inter mean is clamped to `[-256, +255]` and negative residuals
    /// produce negative means (spec/05 §5.1.2).
    #[test]
    fn inter_mean_covers_negative_residual() {
        let level = Svq1Level::L1;
        let v_l = usize::from(level.vector_length());
        let predictor = vec![200u8; v_l];
        let target = vec![40u8; v_l];
        let choice = search_leaf(level, Svq1Half::Inter, &target, &predictor, 0, false);
        match choice.code {
            LeafCode::Coded { mean, .. } => assert_eq!(mean, -160),
            other => panic!("expected coded leaf, got {other:?}"),
        }
        assert_eq!(choice.sse, 0);
    }

    /// Bit accounting matches the table lengths exactly.
    #[test]
    fn bit_cost_is_exact() {
        let level = Svq1Level::L3;
        let v_l = usize::from(level.vector_length());
        let target = pseudo_block(v_l, 11);
        let choice = search_leaf(level, Svq1Half::Intra, &target, &vec![0u8; v_l], 6, false);
        let LeafCode::Coded { mean, ref stages } = choice.code else {
            panic!("expected coded leaf");
        };
        let n = stages.len();
        let want = u32::from(intra_stage_count_table(level).0[n + 1].1)
            + u32::from(SVQ1_VLC_INTRA_MEAN.0[mean as usize].1)
            + 4 * n as u32;
        assert_eq!(choice.bits, want);
    }
}
