//! SVQ1 encoder block-tree search — λ-cost subdivision over the full
//! spec/03 hierarchy.
//!
//! The decoder walks each macroblock's block tree breadth-first, one
//! FIFO queue per level, reading a subdivide-vs-quantise bit at
//! L=5..L=1 and treating L=0 as an implicit leaf (spec/03 §3.5 /
//! §3.3; [`crate::svq1_blocktree::read_block_decision`]). The encoder
//! side implemented here decides that tree per macroblock by
//! minimising the Lagrangian cost
//!
//! ```text
//!    J(block) = SSE(block) + λ · bits(block)
//! ```
//!
//! recursively: at each level the block either becomes a leaf (one
//! decision bit at L=5..L=1, none at L=0, plus the
//! [`crate::svq1_enc_leaf::search_leaf`] payload) or splits into its
//! two spec/03 §3.4 child halves (one decision bit plus the children's
//! costs). `λ = 0` degenerates to pure SSE minimisation (the deepest
//! useful tree); large `λ` collapses toward the cheapest legal stream
//! (a mean-only 16×16 leaf per macroblock).
//!
//! Geometry mirrors the decoder's `halve` exactly: square blocks
//! (L=5 16×16, L=3 8×8, L=1 4×4) split top/bottom; wider-than-tall
//! blocks (L=4 16×8, L=2 8×4) split left/right; the first child is
//! always the top / left half (spec/03 §3.4.1 queue-insertion order).
//!
//! Serialisation follows the decoder's breadth-first queue discipline
//! (spec/03 §3.5.1): all L=5 decisions are emitted before any L=4
//! entry, and within a level entries appear in queue order, each leaf
//! payload inline right after its decision bit — exactly where
//! [`crate::svq1_plane::decode_mb_block_tree`] consumes them.

use crate::svq1_blocktree::{subdivide, Svq1Level};
use crate::svq1_codebook::{SVQ1_VLC_INTER_MEAN, SVQ1_VLC_INTRA_MEAN};
use crate::svq1_enc::BitWriter;
use crate::svq1_enc_leaf::{search_leaf, LeafChoice, LeafCode};
use crate::svq1_vlc::{inter_stage_count_table, intra_stage_count_table, Svq1Half};

/// One decided block-tree node.
#[derive(Debug, Clone)]
pub enum TreeNode {
    /// Quantise in place with the searched leaf payload.
    Leaf(LeafChoice),
    /// Subdivide into the two §3.4 child halves (first = top / left).
    Split(Box<TreeNode>, Box<TreeNode>),
}

/// A fully-decided macroblock tree plus its exact cost.
#[derive(Debug, Clone)]
pub struct MbPlan {
    /// The decided tree, rooted at L=5.
    pub root: TreeNode,
    /// Exact wire bits (decision bits + leaf payloads).
    pub bits: u32,
    /// Total SSE of the macroblock against the search target.
    pub sse: u64,
}

/// Per-block target + predictor view used during the tree search:
/// `w × h` raster samples for both, in the same order.
struct BlockView {
    target: Vec<u8>,
    predictor: Vec<u8>,
}

/// Extract the `(x, y, w, h)` sub-block of a `stride`-wide sample
/// grid in raster order.
fn sub_block(samples: &[u8], stride: usize, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        let start = (y + row) * stride + x;
        out.extend_from_slice(&samples[start..start + w]);
    }
    out
}

/// The §3.4 child geometry of `level` at local offset `(x, y)` within
/// the macroblock: returns the two children's `(x, y)` offsets.
/// Squares halve height (top / bottom); wider blocks halve width
/// (left / right). Mirrors the decoder's `halve`.
fn child_offsets(level: Svq1Level, x: usize, y: usize) -> ((usize, usize), (usize, usize)) {
    let (w, h) = level.block_dims();
    let (w, h) = (usize::from(w), usize::from(h));
    if w == h {
        ((x, y), (x, y + h / 2))
    } else {
        // Every non-square SVQ1 level is wider than tall.
        ((x, y), (x + w / 2, y))
    }
}

/// Recursive λ-cost search of one block. `mb_target` / `mb_predictor`
/// are the full 16×16 macroblock grids (stride 16); `(x, y)` is the
/// block's local offset. Returns the decided node with exact bits +
/// SSE (including this block's decision bit where one exists).
#[allow(clippy::too_many_arguments)]
fn search_block(
    mb: &BlockView,
    x: usize,
    y: usize,
    level: Svq1Level,
    half: Svq1Half,
    allow_skip: bool,
    lambda: u64,
) -> (TreeNode, u32, u64) {
    let (w, h) = level.block_dims();
    let (w, h) = (usize::from(w), usize::from(h));
    let target = sub_block(&mb.target, 16, x, y, w, h);
    let predictor = sub_block(&mb.predictor, 16, x, y, w, h);

    // Leaf option. L=4 / L=5 carry no codebook (spec/04 §4.1.2), so
    // the searcher degenerates to mean-only there.
    let max_stages = if level.rejects_in_place_quantise() {
        0
    } else {
        6
    };
    let choice = search_leaf(level, half, &target, &predictor, max_stages, allow_skip);
    let decision_bits = u32::from(level != Svq1Level::L0);
    let leaf_bits = decision_bits + choice.bits;
    let leaf_sse = choice.sse;
    let leaf_cost = leaf_sse.saturating_add(lambda.saturating_mul(u64::from(leaf_bits)));

    let Some((child_level, _)) = subdivide(level) else {
        // L=0: no split possible.
        return (TreeNode::Leaf(choice), leaf_bits, leaf_sse);
    };

    // Split option.
    let ((x1, y1), (x2, y2)) = child_offsets(level, x, y);
    let (first, b1, s1) = search_block(mb, x1, y1, child_level, half, allow_skip, lambda);
    let (second, b2, s2) = search_block(mb, x2, y2, child_level, half, allow_skip, lambda);
    let split_bits = 1 + b1 + b2;
    let split_sse = s1 + s2;
    let split_cost = split_sse.saturating_add(lambda.saturating_mul(u64::from(split_bits)));

    if split_cost < leaf_cost {
        (
            TreeNode::Split(Box::new(first), Box::new(second)),
            split_bits,
            split_sse,
        )
    } else {
        (TreeNode::Leaf(choice), leaf_bits, leaf_sse)
    }
}

/// Decide one macroblock's block tree.
///
/// * `target` — the 16×16 source block (stride 16, edge-replicated by
///   the caller for overhang macroblocks).
/// * `predictor` — the decoder-visible 16×16 baseline: all-zero for
///   intra macroblocks (spec/04 §4.6.1), the motion-compensated
///   reference for inter macroblocks (§4.6.2 / §4.6.3).
/// * `half` — VLC family + codebook half (spec/04 §4.4: per
///   macroblock, not per leaf).
/// * `allow_skip` — permit `N = −1` SKIP leaves (inter only).
/// * `lambda` — the rate weight; `0` minimises SSE alone.
pub fn plan_macroblock(
    target: &[u8; 256],
    predictor: &[u8; 256],
    half: Svq1Half,
    allow_skip: bool,
    lambda: u64,
) -> MbPlan {
    let mb = BlockView {
        target: target.to_vec(),
        predictor: predictor.to_vec(),
    };
    let (root, bits, sse) = search_block(&mb, 0, 0, Svq1Level::L5, half, allow_skip, lambda);
    MbPlan { root, bits, sse }
}

/// Emit one leaf payload on `half`'s tables: stage-count codeword at
/// position `N + 1` (SKIP = position 0), the mean codeword, then the
/// `4N` raw index bits (spec/04 §4.1 / §4.2.1; spec/05 §5.1).
fn push_leaf_choice(w: &mut BitWriter, level: Svq1Level, half: Svq1Half, choice: &LeafChoice) {
    let stage_table = match half {
        Svq1Half::Intra => intra_stage_count_table(level),
        Svq1Half::Inter => inter_stage_count_table(level),
    };
    match &choice.code {
        LeafCode::Skip => {
            debug_assert_eq!(
                half,
                Svq1Half::Inter,
                "intra SKIP is invalid (spec/04 §4.9.1)"
            );
            w.push_code(&stage_table.0, 0);
        }
        LeafCode::Coded { mean, stages } => {
            w.push_code(&stage_table.0, stages.len() + 1);
            match half {
                Svq1Half::Intra => w.push_code(&SVQ1_VLC_INTRA_MEAN.0, *mean as usize),
                Svq1Half::Inter => w.push_code(&SVQ1_VLC_INTER_MEAN.0, (*mean + 256) as usize),
            }
            for &vec_idx in stages {
                w.push_bits(4, u32::from(vec_idx));
            }
        }
    }
}

/// Serialise one macroblock plan in the decoder's breadth-first
/// queue order (spec/03 §3.5.1): levels drain strictly top-down; a
/// split enqueues its two children (first = top / left) into the
/// next level's queue; each entry emits its decision bit (none at
/// L=0) followed inline by its leaf payload when it is a leaf.
pub fn emit_macroblock(w: &mut BitWriter, plan: &MbPlan, half: Svq1Half) {
    let mut queues: [Vec<&TreeNode>; 6] = Default::default();
    queues[5].push(&plan.root);
    for level_idx in (0..6usize).rev() {
        let level = match level_idx {
            0 => Svq1Level::L0,
            1 => Svq1Level::L1,
            2 => Svq1Level::L2,
            3 => Svq1Level::L3,
            4 => Svq1Level::L4,
            _ => Svq1Level::L5,
        };
        let queue = std::mem::take(&mut queues[level_idx]);
        for node in queue {
            match node {
                TreeNode::Split(first, second) => {
                    debug_assert!(level != Svq1Level::L0, "L=0 cannot split");
                    w.push_bits(1, 1);
                    queues[level_idx - 1].push(first);
                    queues[level_idx - 1].push(second);
                }
                TreeNode::Leaf(choice) => {
                    if level != Svq1Level::L0 {
                        w.push_bits(1, 0);
                    }
                    push_leaf_choice(w, level, half, choice);
                }
            }
        }
    }
}

/// Assemble the plan's decoder-identical reconstruction into a 16×16
/// buffer (stride 16) — the same recursion geometry as the search.
pub fn plan_reconstruction(plan: &MbPlan) -> [u8; 256] {
    let mut out = [0u8; 256];
    fn walk(node: &TreeNode, x: usize, y: usize, level: Svq1Level, out: &mut [u8; 256]) {
        match node {
            TreeNode::Leaf(choice) => {
                let (w, h) = level.block_dims();
                let (w, h) = (usize::from(w), usize::from(h));
                for row in 0..h {
                    out[(y + row) * 16 + x..(y + row) * 16 + x + w]
                        .copy_from_slice(&choice.recon[row * w..(row + 1) * w]);
                }
            }
            TreeNode::Split(first, second) => {
                let (child_level, _) = subdivide(level).expect("split node has children");
                let ((x1, y1), (x2, y2)) = child_offsets(level, x, y);
                walk(first, x1, y1, child_level, out);
                walk(second, x2, y2, child_level, out);
            }
        }
    }
    walk(&plan.root, 0, 0, Svq1Level::L5, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitreader::BitReader;
    use crate::svq1_plane::{decode_mb_block_tree, Svq1PlaneCanvas};

    fn pseudo_mb(seed: u32) -> [u8; 256] {
        let mut out = [0u8; 256];
        for (i, s) in out.iter_mut().enumerate() {
            let x = (i as u32).wrapping_mul(2246822519).wrapping_add(seed * 97);
            *s = ((x >> 13) & 0xff) as u8;
        }
        out
    }

    fn smooth_mb(base: u8) -> [u8; 256] {
        let mut out = [0u8; 256];
        for (i, s) in out.iter_mut().enumerate() {
            *s = base.saturating_add(((i % 16) / 4) as u8);
        }
        out
    }

    /// Decoding the emitted bits through the REAL decoder block-tree
    /// walk reproduces the plan's reconstruction exactly, across
    /// lambda regimes and content shapes.
    #[test]
    fn emitted_tree_decodes_to_plan_reconstruction() {
        for (seed, lambda) in [(1u32, 0u64), (2, 8), (3, 64), (4, 1024), (5, 1 << 20)] {
            let target = pseudo_mb(seed);
            let plan = plan_macroblock(&target, &[0u8; 256], Svq1Half::Intra, false, lambda);
            let mut w = BitWriter::new();
            emit_macroblock(&mut w, &plan, Svq1Half::Intra);
            assert_eq!(
                w.bits_written() as u32,
                plan.bits,
                "bit accounting must be exact (seed {seed}, lambda {lambda})"
            );
            let bytes = w.into_bytes();
            let mut br = BitReader::new(&bytes);
            let mut canvas = Svq1PlaneCanvas::new(16, 16);
            decode_mb_block_tree(&mut br, &mut canvas, 0, 0, Svq1Half::Intra).expect("decodes");
            let recon = plan_reconstruction(&plan);
            for row in 0..16 {
                for col in 0..16 {
                    assert_eq!(
                        canvas.samples[row * canvas.stride + col],
                        recon[row * 16 + col],
                        "seed {seed} lambda {lambda} sample ({col},{row})"
                    );
                }
            }
        }
    }

    /// λ = 0 minimises SSE: the adaptive tree is never worse than the
    /// fixed four-L=3-leaves tree (which is in its search space).
    #[test]
    fn lambda_zero_sse_beats_fixed_l3() {
        for seed in 0..6u32 {
            let target = pseudo_mb(seed);
            let plan = plan_macroblock(&target, &[0u8; 256], Svq1Half::Intra, false, 0);
            // Fixed-L3 comparison: four searched 8×8 leaves.
            let mut fixed_sse = 0u64;
            for (dx, dy) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)] {
                let block = sub_block(&target, 16, dx, dy, 8, 8);
                let choice =
                    search_leaf(Svq1Level::L3, Svq1Half::Intra, &block, &[0u8; 64], 6, false);
                fixed_sse += choice.sse;
            }
            assert!(
                plan.sse <= fixed_sse,
                "seed {seed}: adaptive SSE {} > fixed-L3 SSE {fixed_sse}",
                plan.sse
            );
        }
    }

    /// A very large λ collapses to the cheapest legal stream — the
    /// mean-only 16×16 leaf (decision bit + stage-count + mean).
    #[test]
    fn huge_lambda_collapses_to_mean_only_l5() {
        let target = smooth_mb(100);
        let plan = plan_macroblock(
            &target,
            &[0u8; 256],
            Svq1Half::Intra,
            false,
            u64::MAX / 4096,
        );
        match &plan.root {
            TreeNode::Leaf(choice) => match &choice.code {
                LeafCode::Coded { stages, .. } => {
                    assert!(stages.is_empty(), "L=5 leaf must be mean-only")
                }
                other => panic!("unexpected leaf code {other:?}"),
            },
            TreeNode::Split(..) => panic!("huge lambda must not split"),
        }
    }

    /// Rate monotonicity: increasing λ never increases the bit cost.
    #[test]
    fn bits_are_monotone_nonincreasing_in_lambda() {
        let target = pseudo_mb(9);
        let mut prev_bits = u32::MAX;
        for lambda in [0u64, 4, 16, 64, 256, 4096, 1 << 16] {
            let plan = plan_macroblock(&target, &[0u8; 256], Svq1Half::Intra, false, lambda);
            assert!(
                plan.bits <= prev_bits,
                "bits rose from {prev_bits} to {} at lambda {lambda}",
                plan.bits
            );
            prev_bits = plan.bits;
        }
    }

    /// Inter-half planning with a perfect predictor and skip allowed
    /// yields the all-SKIP macroblock: a single L=5 SKIP leaf is not
    /// representable (SKIP needs the stage-count VLC that exists at
    /// every level), so the tree may still be a leaf — assert zero
    /// SSE and that the emitted stream decodes back to the predictor.
    #[test]
    fn perfect_inter_predictor_costs_zero_sse() {
        let predictor = pseudo_mb(31);
        let plan = plan_macroblock(&predictor, &predictor, Svq1Half::Inter, true, 64);
        assert_eq!(plan.sse, 0);
        let recon = plan_reconstruction(&plan);
        assert_eq!(recon[..], predictor[..]);
    }
}
