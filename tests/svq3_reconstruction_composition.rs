//! SVQ3 per-block reconstruction composition — coefficient-stream to
//! reconstructed-plane integration tests.
//!
//! These tests exercise the **full per-block reconstruction chain** that
//! `docs/video/svq3/spec/01-reconstruction-composition.md` pins, end to
//! end across module boundaries:
//!
//! 1. a `(run, value)` residual coefficient stream
//!    ([`oxideav_svq::svq3_coeff::Coefficient`]) is placed into a 4×4
//!    grid via the quantiser-selected scan order
//!    ([`oxideav_svq::svq3_scan::place_4x4`], Gap 1 selection rule);
//! 2. each placed grid is run through the residual interleave
//!    (dequant·scale → two-sided `M·X·Mᵀ` transform → fused
//!    `+0x80000 >>20`, Gap 2) and reconstructed against the
//!    intra-predicted block (Gap 3/4) with the saturating
//!    `Clip1(pred + residual)` writeback (Gap 5) by
//!    [`oxideav_svq::svq3_recon::reconstruct_intra_luma_macroblock_from_coeffs`].
//!
//! The wire-format decode that *produces* the coefficient stream / mode
//! list / quantiser (the intra-mode VLC, CBP, MV-VLC) is a deferred
//! docs-gap; these tests supply those inputs directly and verify the
//! reconstruction composition that consumes them.

use oxideav_svq::svq3_coeff::Coefficient;
use oxideav_svq::svq3_dequant::dequantize_transform_luma_block;
use oxideav_svq::svq3_pred::Svq3IntraMode;
use oxideav_svq::svq3_recon::{
    reconstruct_intra_luma_macroblock_from_coeffs, LumaMacroblock, MB_LUMA_BLOCKS,
};
use oxideav_svq::svq3_scan::{
    place_4x4, ALT_SCAN_4X4_SCAN, ALT_SCAN_QUANTISER_THRESHOLD, NORMAL_ZIGZAG_4X4_SCAN,
};

/// Build a `[i32; 16]` coefficient grid for every luma sub-block by
/// placing the supplied `(run, value)` stream through the
/// quantiser-selected 4×4 scan. The same stream is placed into all 16
/// sub-blocks (a deterministic, fully-specified fixture).
fn place_all_blocks(
    stream: &[Coefficient],
    is_luma_4x4_intra: bool,
    q: u32,
) -> [[i32; 16]; MB_LUMA_BLOCKS] {
    let placed = place_4x4(stream, is_luma_4x4_intra, q).expect("placement in range");
    [placed; MB_LUMA_BLOCKS]
}

#[test]
fn empty_coefficient_stream_reconstructs_flat_dc_plane() {
    // No coefficients ⇒ zero residual everywhere ⇒ a DC-only intra MB
    // with no neighbours predicts and reconstructs a flat 128 plane.
    let coeffs = place_all_blocks(&[], true, 12);
    let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
    let mut mb = LumaMacroblock::new();
    reconstruct_intra_luma_macroblock_from_coeffs(&mut mb, &modes, &coeffs, 12);
    assert!(
        mb.samples.iter().all(|&s| s == 128),
        "empty stream + DC + no neighbours must be flat 128"
    );
}

#[test]
fn dc_only_coefficient_lifts_the_whole_block_uniformly() {
    // A single (run=0, value=v) coefficient lands at scan position 0,
    // i.e. the DC term. Its residual interleave produces a flat residual
    // (13·13 outer product), so each sub-block's DC-predicted plane is
    // lifted uniformly.
    let q = 14;
    let stream = [Coefficient { run: 0, value: 1 }];
    let coeffs = place_all_blocks(&stream, true, q);
    let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];

    let mut mb = LumaMacroblock::new();
    reconstruct_intra_luma_macroblock_from_coeffs(&mut mb, &modes, &coeffs, q);

    // Block 0 (pixel origin (0,0)) has no neighbours ⇒ DC predicts 128;
    // the flat residual lifts it to clip(128 + residual[0], 0, 255).
    let residual = dequantize_transform_luma_block(q, coeffs[0]);
    let expected = (128 + residual[0]).clamp(0, 255) as u8;
    for y in 0..4 {
        for x in 0..4 {
            assert_eq!(mb.sample(x, y), expected, "block-0 pixel ({x},{y})");
        }
    }
    assert_ne!(expected, 128, "a non-zero DC must move the plane");
}

#[test]
fn quantiser_below_threshold_selects_alternate_scan() {
    // Gap 1: a luma 4×4-intra block at q < 24 uses the alternate scan,
    // so a coefficient placed at a non-DC scan position lands at a
    // *different* raster cell than the normal-zigzag would place it.
    let q_alt = ALT_SCAN_QUANTISER_THRESHOLD - 1; // < 24 ⇒ alt scan
    let q_zig = ALT_SCAN_QUANTISER_THRESHOLD; // >= 24 ⇒ normal zigzag

    // (run=2, value=1): skip scan positions 0 and 1, place at scan
    // position 2 — the first position where the two scan tables diverge
    // (normal zigzag → raster 4, alternate scan → raster 2).
    let stream = [Coefficient { run: 2, value: 1 }];

    let alt = place_4x4(&stream, true, q_alt).expect("alt placement");
    let zig = place_4x4(&stream, true, q_zig).expect("zigzag placement");

    // Scan position 2 maps to a different raster index under the two
    // scan tables, so the placed grids must differ.
    let alt_cell = ALT_SCAN_4X4_SCAN[2];
    let zig_cell = NORMAL_ZIGZAG_4X4_SCAN[2];
    assert_ne!(alt_cell, zig_cell, "scan tables must differ at position 2");
    assert_eq!(alt[alt_cell], 1);
    assert_eq!(zig[zig_cell], 1);
    assert_ne!(alt, zig, "scan selection must change coefficient placement");
}

#[test]
fn full_chain_with_run_value_stream_is_deterministic_and_clamped() {
    // Drive a richer stream (a DC + two AC runs) through the full chain
    // and assert the reconstruction is fully populated and saturating-
    // clamped to the 8-bit range.
    let q = 9;
    let stream = [
        Coefficient { run: 0, value: 5 },  // DC
        Coefficient { run: 2, value: -3 }, // skip 2, AC
        Coefficient { run: 0, value: 2 },  // next AC
    ];
    let coeffs = place_all_blocks(&stream, true, q);
    let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];

    let mut mb = LumaMacroblock::new();
    reconstruct_intra_luma_macroblock_from_coeffs(&mut mb, &modes, &coeffs, q);

    // Every sample is a valid u8 (saturation is implicit in the type) and
    // the plane is not the trivial flat-128 (the stream carries energy).
    assert!(
        mb.samples.iter().any(|&s| s != 128),
        "a non-trivial residual stream must move the plane off flat-128"
    );

    // Determinism: re-running yields byte-identical output.
    let mut mb2 = LumaMacroblock::new();
    reconstruct_intra_luma_macroblock_from_coeffs(&mut mb2, &modes, &coeffs, q);
    assert_eq!(mb.samples, mb2.samples);
}

#[test]
fn vertical_mode_with_neighbour_and_residual_composes_pred_plus_residual() {
    // A vertical-mode MB with an available `above` row: block 0 predicts
    // by copying `above`, then adds its residual. Verify the composition
    // matches predict(above) + residual at the block-0 top-left sample.
    let q = 16;
    let stream = [Coefficient { run: 0, value: 4 }]; // DC residual
    let coeffs = place_all_blocks(&stream, true, q);
    let modes = [Svq3IntraMode::Vertical; MB_LUMA_BLOCKS];

    let mut mb = LumaMacroblock::new();
    let mut above = [0u8; 16];
    for (x, a) in above.iter_mut().enumerate() {
        *a = 60 + (x as u8);
    }
    mb.above = above;
    mb.above_available = true;

    reconstruct_intra_luma_macroblock_from_coeffs(&mut mb, &modes, &coeffs, q);

    // Block 0 top-left: vertical predicts above[0]; flat DC residual adds
    // residual[0]. (residual is flat for a pure-DC coefficient.)
    let residual = dequantize_transform_luma_block(q, coeffs[0]);
    let expected = (i32::from(above[0]) + residual[0]).clamp(0, 255) as u8;
    assert_eq!(mb.sample(0, 0), expected);
}
