# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Round 339 — SVQ3 16×16 plane (transposed) + 8×8 chroma DC
  predictors (spec/01 Gap 4).** Lands the two H.264-back-referenced
  predictors whose decode-side sample equations
  `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 4 now
  pins. `predict_plane_16x16` implements the standard H.264 16×16
  plane fit (`H`/`V`/`a`/`b`/`c` from `top[]`/`left[]`, constants
  `5`/`32`/`>>6`/`16`/`>>5`) with SVQ3's documented **transpose**
  (`b` applied along `y`, `c` along `x`) and the 8-bit `Clip1`
  writeback. `predict_chroma_dc_8x8` implements Gap 4's chroma "DC
  mode only" rule with the per-4×4-quadrant availability-driven
  averaging (`(Σtop+Σleft+4)>>3` / `(Σ+2)>>2` / `128`). The companion
  `predict_dc_16x16` 16×16 DC predictor covers the macroblock-edge
  fallback where the plane predictor's neighbours are not both
  available. `PRED_16X16_DIM`/`_SAMPLES` and
  `PRED_CHROMA_DIM`/`_SAMPLES` surface the block geometries. 10 new
  unit tests cover uniform-neighbour reproduction, the transpose
  axis-assignment, the DC availability cases, and per-quadrant chroma
  DC independence.

- **Round 339 — SVQ3 4×4 intra-mode binding + the standard-H.264
  predictors.** Lands the four 4×4 intra predictors `docs/video/svq3/
  spec/01-reconstruction-composition.md` Gap 3 names as "standard
  H.264 … unmodified" — `predict_vertical_4x4` (mode 0),
  `predict_horizontal_4x4` (mode 1), `predict_dc_4x4` (mode 2, with
  the availability-driven `(Σtop+Σleft+4)>>3` / `(Σ+2)>>2` / `128`
  averaging set), and `predict_diagonal_down_right_4x4` (mode 4, the
  3-tap `(a+2b+c+2)>>2` main-diagonal filter reading the above-left
  `corner`). The mode-to-predictor binding the README named as a
  lacks-tail item lands as the `Svq3IntraMode` enum (Gap 3's
  `0=Vertical / 1=Horizontal / 2=DC / 3=DiagonalDownLeft /
  4=DiagonalDownRight`, with `Svq3IntraMode::DEFAULT = Dc` per Gap 3's
  "default/fallback predictor is value 2") plus the
  `predict_intra_4x4` dispatcher, which routes a resolved mode to its
  predictor over an `Intra4x4Neighbours` carrier (top row + left
  column + corner + availability flags) and applies the standard
  H.264 DC fallback when a directional predictor's neighbour is
  unavailable. Mode 3 keeps the existing SVQ3 diagonal-down quirk
  (`predict_diagonal_down_4x4`). All predictors are `const fn`. 19 new
  unit tests cover the value↔mode round-trip, each predictor's
  closed form, the DC availability cases, and the dispatcher's routing
  + DC fallback.

- **Round 331 — SVQ3 4×4 scan-order arrays + selection rule.** Lands
  the two 16-entry 4×4 coefficient scan tables that round 233 had
  deferred for lack of a pinned source: `NORMAL_ZIGZAG_4X4_SCAN`
  (`0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15`) and
  `ALT_SCAN_4X4_SCAN`
  (`0, 1, 2, 6, 10, 3, 7, 11, 4, 8, 5, 9, 12, 13, 14, 15`),
  transcribed bit-exact from
  `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 1 (the
  Sorenson Video QuickTime Component `.data` tables at file offsets
  `0x7e5a8` / `0x7e5b8`). Adds the quantiser-driven selection helper
  `select_4x4_scan` (alt-scan only for a luma 4×4-intra block at
  slice quantiser `< 24`, threshold `ALT_SCAN_QUANTISER_THRESHOLD`),
  the `ALT_SCAN_4X4_HALF_LEN = 8` two-half marker (wiki §"Macroblock
  layer" cross-check), and the `place_4x4_normal_zigzag` /
  `place_4x4_alt_scan` / `place_4x4` convenience wrappers over the
  existing generic `place_coefficients_in_scan_order`. 11 new unit
  tests assert the bit-exact byte values, full-permutation /
  DC-first invariants, the two-half split, the selection boundary at
  `Q == 24`, and the placement routing.

- **Round 326 — SVQ1 motion-vector cache + neighbour-selection
  geometry.** New `svq1_mv_cache` module landing the per-plane MV cache
  of `docs/video/svq1/spec/06-motion-vectors.md` §6.8 and the
  neighbour-selection geometry of §6.4.3 (INTER, single MV) / §6.4.4
  (INTER_4MV, per-sub-block), the caller-side counterpart of the §6.4
  median predictor (which takes the three candidate neighbours but
  leaves their selection + storage to the caller). `Svq1MvCache` is a
  `(mb_cols × 2) × (mb_rows × 2)` grid of 8×8-block MVs (the §6.1.1
  granularity invariant), initialised to `(0, 0)` (§6.8.1) with a
  single-frame lifetime (§6.8.2). `inter_neighbours` selects
  `{pl=(r,c−1), pt=(r−2,c), ptr=(r−2,c+2)}` reproducing the wiki grid
  `[1 2 / 3 4]` → `{N, C, E}`; `inter_4mv_neighbours` selects the four
  per-sub-block triples of the §6.4.4 table (including the §6.4.4 note
  that sub-blocks 3/4 use within-MB top-left vectors as `ptr`).
  `decode_inter` / `decode_inter_4mv` compose the predictor + §6.6 clip
  + §6.8.1 cache update; the INTER_4MV path is strictly serial (§6.4.5,
  sub-block N reads sub-blocks `< N`). `store_inter` broadcasts to all
  four slots, `store_skip_intra` applies the §6.9 `(0,0)` reset. Out-of-
  bounds `get` is an **absent** neighbour (§6.4.2), distinct from an
  in-bounds `(0,0)` slot. Pure indexing + storage — no `BitReader` use;
  the differential `(dx, dy)` (the §6.2.3 Reading A/B-ambiguous T02 wire
  decode) is caller-supplied. Verified against the wiki grid positions,
  the §6.4.1 worked example through the cache path, and serial INTER_4MV
  propagation (14 unit tests).

- **Round 320 — SVQ1 motion-vector median-of-three predictor.** New
  `svq1_motion_predictor` module landing the per-block predictor of
  `docs/video/svq1/spec/06-motion-vectors.md` §6.4: the
  component-independent `MEDIAN(pl, pt, ptr)` (`median3`) over the
  previous-left / previous-top / previous-top-right neighbour MVs,
  the §6.4.2 absent-neighbour fallback (`(0,0)`-substituted median for
  two present neighbours, single-vector verbatim short-circuit for one
  present, `(0,0)` for none), and the §6.6 final-vector component clip
  to `[-32, +31]` (`clip_component` / `final_motion_vector`). Pure
  arithmetic — no `BitReader` use. The per-component differential VLC
  (T02) decode is intentionally deferred: spec/06 §6.2.3 / §6.10
  open-item 1 flags a bit-stream-affecting Reading A/B disambiguation
  pending a Validator round, and the predictor + clip arithmetic wired
  here is shared by both readings. New types `Svq1Mv` (signed half-pel
  `(x, y)` pair, `ZERO` constant) and `Neighbours` (three optional
  candidate vectors). Verified bit-exact against the §6.4.1 worked
  example (`pl=(0,0), pt=(5,17), ptr=(-9,12) → predictor=(0,12)`) plus
  tie / saturation / one-vs-two-present coverage (11 unit tests).

- **Round 315 — SVQ1 leaf stage-accumulation reconstruction.** Composes
  the already-staged mean step (`svq1_mean`), within-half codebook lookup
  (`svq1_codebook::codebook_vector_in_half`), and per-step `[0, 255]`
  clamp (`svq1_mean::saturate_u8`) into the fixed-order leaf
  reconstruction the spec pins in
  `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.5 —
  `predictor → mean → stage-1 → … → stage-N`, saturating AFTER EACH add
  (§4.5.1), written in output-raster order (§4.7.1). The composition
  order is the spec's mandate over already-pinned passes; no new
  arithmetic, constant, or codebook addressing is introduced.
  - New `svq1_reconstruct` module:
    - `reconstruct_leaf(level, half, predictor, mean, stages) ->
      Result<Vec<u8>, ReconstructError>` — the per-position §4.5 loop.
      Empty `stages` is the §4.5.4 mean-only collapse (`N = 0`); SKIP
      (`N = -1`, §4.5.5) is the caller's short-circuit, not a value of
      this function.
    - `LeafStage { stage, vec_idx }` — one decoded stage (ascending
      order per §4.2); `MAX_STAGES = 6` (§4.1).
    - `ReconstructError` — absent level (§4.1.2), mis-sized predictor
      (§4.3), stage overflow (§4.1.1), codebook-lookup failure (§4.3).
  - 11 new lib tests including the §4.8 worked example bit-exact
    (`predictor=0, mean=61, stage-1 idx=4, stage-2 idx=14` → raster
    `[55 39 50 77 / 93 81 49 46]`), the §4.8.3 intermediate rows, the
    §4.8.5 per-step-vs-deferred divergence (per-step → 230, not 255),
    inter mean-only saturation, an independent recompute sweep, and the
    four error paths; plus 1 doc test. Total: 449 lib + 7 integration
    + 14 doc = 470 (up from 438 + 7 + 13 = 458).

- **Round 310 — SVQ3 chroma DC full dequantization pipeline.** Composes
  the three already-pinned chroma DC stages into the ordered pipeline the
  wiki spec mandates in `docs/video/svq3/wiki/Sorenson_Video_3.wiki`
  §"Macroblock transform and dequantization", which gives the chroma DC
  dequant expression `dc = (svq3_dequant_coeff[Q] * (block[0] >> 3)) >> 1`
  and notes "chroma DCs need to be **transformed first** using the
  [`8 8 / 8 -8`] matrix" — pinning the order transform → dequant →
  finalise.
  - New `svq3_dequant` helper:
    - `dequantize_chroma_dc_block(q: u32, block: [i32; 4]) -> [i32; 4]`
      `const fn` — chains `apply_chroma_dc_2x2_2d` (two-sided
      `M · X · M^T` transform) → `dequantize_chroma_dc` (per transformed
      sample) → `finalise_dc`, preserving the row-major 2×2 layout. No
      new matrix, constant, or arithmetic is introduced; the composition
      order is the spec's "first" mandate over already-pinned passes.
  - 7 new lib tests (independent brute-force re-derivation, explicit
    staged composition, pure-DC four-equal-outputs, transform-first
    ordering divergence, zero-input, `const`-context evaluability,
    out-of-range-quantiser panic) and 1 doc test. Total: 438 lib + 7
    integration + 13 doc = 458 (up from 431 + 7 + 12 = 450).
    `Svq3DecoderHandle::receive_frame` remains `Error::Unsupported`.

- **Round 302 — SVQ1 within-half codebook-vector accessor.** Surfaces
  the canonical *within-half* vector addressing pinned by
  `docs/video/svq1/spec/14-codebook-architecture.md` §14.5 (the
  `(level, half, stage, vec_idx, byte_idx)` convention) and §14.8 (the
  `half_payload[stage_idx * 16 * V_L + vec_idx * V_L + byte_idx]`
  arithmetic, stated as holding "regardless of hypothesis"). The
  cross-half / cross-level concatenation order remains the still-open
  §14.8 item, so these helpers operate only WITHIN a half the caller
  has isolated.
  - New `svq1_codebook` helpers:
    - `vector_byte_offset_in_half(level, stage, vec_idx) -> Option<usize>`
      `const fn` — the byte offset of stage `stage` (1-based, `1..=6`)
      entry `vec_idx` (`0..=15`) within one level half; `None` for the
      absent L=4 / L=5 levels and for out-of-range `stage` / `vec_idx`.
    - `codebook_vector_in_half(half, level, stage, vec_idx) -> Option<&[i8]>`
      — borrows the `V_L`-byte vector from a caller-supplied half slice,
      returning `None` if the offset is invalid or the half is too short.
  - 11 new lib tests (offset arithmetic vs an independent recompute over
    the full stage×vec grid, unique-and-tiling coverage, half-boundary
    alignment, short-half / absent-level / out-of-range rejection,
    `const`-context evaluability) and 2 doc tests. Total: 431 lib + 7
    integration + 12 doc = 450 (up from 420 + 7 + 10 = 437).
    `Svq1`/`Svq3` decode entry points remain unchanged.

- **Round 295 — SVQ3 two-sided transform `M · X · M^T` composition.**
  Composes the two single-sided passes (`M · X` column pass from rounds
  262/272 and `X · M^T` row pass from round 290) into the full two-sided
  transform implied by the wiki-pinned matrix
  (`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform
  and dequantization"). Pure functional composition of already-pinned
  helpers — no new matrix or constant — and `Svq3DecoderHandle::receive_frame`
  continues to return `oxideav_core::Error::Unsupported`.
  - New `svq3_dequant` helpers:
    - `apply_luma_transform_2d(block: [i32; 16]) -> [i32; 16]` `const fn`
      — `apply_luma_transform_columns(apply_luma_transform_rows(block))`,
      realising `M · (X · M^T) = M · X · M^T`.
    - `apply_chroma_dc_2x2_2d(block: [i32; 4]) -> [i32; 4]` `const fn`
      — the 2×2 chroma DC analogue.
  - Consistent with both single-sided passes, no inter-pass shift, bias,
    or quantiser scaling is applied (the wiki does not enumerate any
    normalisation between passes). The composition order is matrix
    associativity, not an additional spec fact; tests corroborate against
    a brute-force `M · X · M^T` triple-loop reference and confirm the
    column-then-row ordering agrees.

- **Round 290 — SVQ3 transform row-multiply (`X · M^T`) passes.** The
  right-side mirror of the existing single-sided column passes
  (`M · X`), applying the wiki-pinned transform matrix
  (`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform
  and dequantization") against a block's rows instead of its columns.
  Pure arithmetic over the verbatim-pinned matrix `M`; no new bitstream
  reads, and `Svq3DecoderHandle::receive_frame` continues to return
  `oxideav_core::Error::Unsupported`.
  - New `svq3_dequant` helpers:
    - `apply_luma_transform_rows(block: [i32; 16]) -> [i32; 16]`
      `const fn` — `out[r*4+c] = X[r,:] · M[c,:]` (= `(X · M^T)[r,c]`),
      reusing `apply_luma_transform_row` with
      `matrix_row = LUMA_TRANSFORM_MATRIX[c]`.
    - `apply_chroma_dc_2x2_rows(block: [i32; 4]) -> [i32; 4]` `const fn`
      — the 2×2 chroma DC analogue against `[[8, 8], [8, -8]]`.
  - The full two-sided `M · X · M^T` composition the wiki does NOT
    enumerate stays deferred until `docs/video/svq3/` pins the operand
    order and any intermediate rounding.

- **Round 282 — SVQ3 4×4 diagonal-down intra predictor.** The one
  intra predictor the wiki spec pins completely in
  `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra prediction" —
  the 4×4 diagonal-down quirk, whose fill picture
  (`a b c c / b c c c / c c c c / c c c c`) and three closed-form
  samples (`a = (left[1] + top[1]) / 2`, `b = (left[2] + top[2]) /
  2`, `c = (left[3] + top[3]) / 2`) are spelled out verbatim in the
  local mirror — lands as a new `svq3_pred` module. Round 282 is
  pixel arithmetic only — no new bitstream reads — and
  `Svq3DecoderHandle::receive_frame` continues to return
  `oxideav_core::Error::Unsupported`.
  - New `svq3_pred` module surface:
    - `diagonal_down_sample(left_k: u8, top_k: u8) -> u8` `const fn`
      — one predicted sample, the spec's `(left[k] + top[k]) / 2`
      closed form. The spec writes a plain integer `/ 2` with no
      rounding bias; both operands are non-negative samples so the
      division is an exact floor, and the result always fits in `u8`
      (`(255 + 255) / 2 = 255`).
    - `predict_diagonal_down_4x4(left: [u8; 4], top: [u8; 4]) ->
      [u8; 16]` `const fn` — the full 4×4 predictor: derives `a` /
      `b` / `c` from neighbour indices `1` / `2` / `3` and expands
      them through the fill picture. Output is row-major
      (`out[row * 4 + col]`), matching the `svq3_scan` /
      `svq3_dequant` block layout so the eventual predicted+residual
      writeback can combine the two element-wise.
    - `DIAGONAL_DOWN_PATTERN: [u8; 16]` — the spec's fill picture
      flattened row-major; each entry selects one of the derived
      samples (`0` ⇒ `a`, `1` ⇒ `b`, `2` ⇒ `c`).
    - `DIAGONAL_DOWN_NEIGHBOUR_INDICES: [usize; 3] = [1, 2, 3]` —
      the neighbour-array indices the three closed forms consume.
      Element `0` of either neighbour array is never referenced by
      this predictor.
    - `PRED_4X4_DIM = 4` / `PRED_4X4_SAMPLES = 16` block-geometry
      constants.
  - 15 new lib tests cover: the fill-picture transcription and its
    `a×1 / b×2 / c×13` population; the neighbour-index constants;
    the geometry constants; per-sample zero / max / saturation-free
    bounds; floor division on odd sums; left/top symmetry of the
    per-sample average (swept) and of the full block predictor;
    uniform-neighbour identity swept over six values; agreement of
    the block output with the three closed forms position-by-
    position; the explicit picture-row layout against worked `a` /
    `b` / `c` values; element-0 insensitivity under noise injection;
    pattern-indexing consistency; a fully worked numeric example;
    and `const`-site usability of both helpers.
  - 2 doctest examples (one per helper) on the public surface.
  - Total tests: 402 lib + 7 integration + 6 doc = 415 (up from 387
    + 7 + 4 = 398).
  - Round 282 targeted the full intra per-block reconstruction
    composition (coefficients → dequant → inverse transform →
    predicted+residual writeback), but every remaining stage of that
    chain is doc-gapped: the 4×4 scan-order arrays (round 233's
    ambiguity note), the two-sided `M · X · M^T` transform (rounds
    262/272 deferrals), the numeric mode-to-predictor binding (the
    wiki names "diagonal down" without binding it to a `0..=4` mode
    value), the remaining H.264-back-referenced predictors (16×16
    plane "transposed", chroma "always DC"), and the writeback clamp
    all await pinning in `docs/video/svq3/`. The diagonal-down
    predictor is the one intra-path stage the docs fully pin that
    was still missing; it supplies the "predicted" operand of the
    eventual writeback.

- **Round 272 — SVQ3 4×4 luma transform application helpers.** The 4×4
  luma transform matrix the wiki spec
  (`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform
  and dequantization") pins under "Transform coefficients" as
  `[[13,17,1,7],[13,7,-1,-17],[13,-7,-1,17],[13,-17,1,-7]]` (already
  exposed verbatim as [`svq3_dequant::LUMA_TRANSFORM_MATRIX`]) now has a
  per-block application surface mirroring round 262's chroma DC helpers.
  Round 272 is structural arithmetic only — no new bitstream reads — and
  `Svq3DecoderHandle::receive_frame` continues to return
  `oxideav_core::Error::Unsupported`.
  - New `svq3_dequant` module surface:
    - `apply_luma_transform_row(matrix_row: [i32; 4], a: i32, b: i32, c:
      i32, d: i32) -> i32` `const fn` — one matrix-row dot product
      against a 4-point column `[a, b, c, d]`:
      `matrix_row[0]*a + matrix_row[1]*b + matrix_row[2]*c +
      matrix_row[3]*d`. Because every row shares the column-0 value
      [`svq3_dequant::LUMA_TRANSFORM_DC_COLUMN`] = `13`, a pure-DC column
      `[a, 0, 0, 0]` yields `13 * a` for every row.
    - `apply_luma_transform_columns(block: [i32; 16]) -> [i32; 16]`
      `const fn` — applies `LUMA_TRANSFORM_MATRIX` against the columns of
      a row-major 4×4 input block (the `M · X` single-sided pass).
      Input/output are row-major (`block[r*4 + c]` = sample at `(r, c)`);
      `out[r*4 + c] = M[r, :] · X[:, c]`. The unrounded i32 outputs feed
      directly into [`svq3_dequant::dequantize_coefficient`].
  - 9 new lib tests cover: all-ones-column weight summing; pure-DC
    column reducing to `13 * a` swept over six `a` values across all
    four rows; explicit per-row worked dot products against `[1,2,3,4]`
    (`78`, `-44`, `64`, `-46`); row linearity under doubling/negation;
    the all-zero block identity; pure-DC column repeating `13*a` down
    every output row; a single-active-column block matching the per-row
    dot; full-block linearity under doubling and negation; the
    decomposition agreeing with the row helper position-by-position over
    an arbitrary block; and the `const fn` annotation usable in a `const`
    site.
  - 2 doctest examples (one per helper) on the public surface.
  - Total tests: 387 lib + 7 integration + 4 doc = 398 (up from 377 + 7
    + 2 = 386).
  - The full two-sided `M · X · M^T` luma transform — which the wiki
    spec does NOT spell out explicitly — is deliberately NOT folded in
    here; that derivation belongs in a future round once docs pin it.

- **Round 262 — SVQ3 2×2 chroma DC transform application helper.** The
  per-block application of the 2×2 chroma DC transform matrix the wiki
  spec pins in `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
  transform and dequantization" as `[[8, 8], [8, -8]]` (also exposed
  verbatim as [`svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX`]) lands as two
  new `const fn` helpers in the existing `svq3_dequant` module. The wiki
  spec states "chroma DCs need to be transformed first using the
  following matrix" before the [`svq3_dequant::dequantize_chroma_dc`]
  expression is applied; round 262 implements the matrix application
  step as the single-sided `M · X` pass (the unambiguous form `M` alone
  produces against a column vector). Round 262 is structural arithmetic
  only — no new bitstream reads — and `Svq3DecoderHandle::receive_frame`
  continues to return `oxideav_core::Error::Unsupported`.
  - New `svq3_dequant` module surface:
    - `apply_chroma_dc_transform_row(matrix_row: [i32; 2], a: i32, b:
      i32) -> i32` `const fn` — one matrix-row dot product against a
      2-point column. For the first matrix row `[8, 8]` the result is
      `8 * (a + b)`; for the second matrix row `[8, -8]` the result is
      `8 * (a - b)`. Caller passes `CHROMA_DC_TRANSFORM_MATRIX[0]` /
      `CHROMA_DC_TRANSFORM_MATRIX[1]` for the spec's two rows.
    - `apply_chroma_dc_2x2_columns(block: [i32; 4]) -> [i32; 4]`
      `const fn` — applies `CHROMA_DC_TRANSFORM_MATRIX` against the
      columns of a row-major 2×2 input block (the `M · X` single-sided
      pass). The input layout matches
      [`svq3_scan::place_chroma_dc_2x2`]'s row-major output (`block[0]`
      = `(0,0)`, `block[1]` = `(0,1)`, `block[2]` = `(1,0)`, `block[3]`
      = `(1,1)`); the return value is laid out the same way. The
      per-position output is:
      `out[0,0] = 8 * (block[0,0] + block[1,0])`,
      `out[0,1] = 8 * (block[0,1] + block[1,1])`,
      `out[1,0] = 8 * (block[0,0] - block[1,0])`,
      `out[1,1] = 8 * (block[0,1] - block[1,1])`. Suitable for direct
      consumption by the per-sample dequant step that follows.
  - 18 new lib tests cover: row-0 sum-of-pair semantics swept across
    six representative `(a, b)` pairs; row-1 difference-of-pair
    semantics over the same sweep; explicit worked `row · (3, 1)`
    examples (`32` for row 0; `16` for row 1); the all-zero-block
    identity; four single-position-active-bit inputs (top-row,
    bottom-row, diagonal, anti-diagonal); the all-ones block's
    sum-cancelling-difference behaviour; linearity under doubling and
    under negation; the top-row-only / bottom-row-only branch
    asymmetries; per-row column-wise sum / difference identities; the
    `const fn` annotation usable at compile time (a `const OUT: [i32;
    4]` site); and a cross-module sanity case feeding a
    [`svq3_scan::place_chroma_dc_2x2`] output through
    `apply_chroma_dc_2x2_columns`.
  - 2 doctest examples (one per helper) on the public surface.
  - Total tests: 377 lib + 7 integration + 2 doc = 386 (up from 359 + 7
    = 366).
  - The full two-sided `M · X · M^T` transform — which the wiki spec
    does NOT spell out explicitly — is deliberately NOT folded in here.
    That derivation belongs in a future round once docs pin it.

- **Round 245 — SVQ3 alt-scan two-half block walker.** The typed
  two-half walker for SVQ3's alternative-scan coefficient block lands as
  a new `svq3_coeff::AltScanBlock` carrier plus a
  `svq3_coeff::read_alt_scan_block` entry point. The wiki spec
  (`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient decoding")
  pins the alt-scan path as "coded in two parts of up two eight
  coefficients corresponding to each half-scan": each half is an
  independent run of up to `COEFFS_PER_ALT_SCAN_HALF` = 8 coefficients,
  terminated by either an explicit end-of-block sentinel (`code == 0`)
  or by reaching the half's capacity, and the two halves keep their
  run-accumulator cursors strictly independent.
  - New surface in `svq3_coeff`:
    - `AltScanBlock { first_half: Vec<Coefficient>, second_half:
      Vec<Coefficient> }` — typed two-half carrier with
      `len()` / `is_empty()` / `iter_with_half()` (yielding
      `(half_index: u8, Coefficient)` pairs in stream order) /
      `flatten()` (returning a single `Vec<Coefficient>` for callers
      that have already absorbed the half boundary).
    - `read_alt_scan_block(br) -> Result<AltScanBlock>` — sequences
      two `read_alt_scan_half(br)` calls back-to-back, preserving the
      half walker's per-half termination + per-half capacity
      guarantees verbatim.
  - 12 new lib tests pin the contract:
    - both halves empty (two consecutive end-of-block sentinels),
    - first-half-only / second-half-only populated patterns,
    - both halves capped at 8 coefficients each → `len() == 16`,
    - per-half cursor independence (a `(run=0, value=5)` coefficient
      in half 1 does not bleed into half 2's cursor),
    - first-half capacity-cap exit does NOT consume the second
      half's first bit,
    - `BadBitWidth` overflow propagation from either half,
    - `Truncated` propagation from either half,
    - `flatten()` preserves stream order,
    - `iter_with_half()` yields the correct `(half_index,
      Coefficient)` pairs,
    - structural invariant `2 * COEFFS_PER_ALT_SCAN_HALF ==
      COEFFS_PER_4X4_BLOCK`.
  - The per-half 8-position scan-order array the wiki spec depicts in
    §"Macroblock layer" remains deferred per round 233's ambiguity
    note; the two-half block walker landed here exposes the
    coefficient stream in the order it was emitted, leaving the
    scan-position-to-flat-index reshape to the round that pins the
    canonical interpretation in `docs/video/svq3/`.

- **Round 242 — SVQ1 per-stage codebook-index field reader (§4.2).**
  The `4 × N`-bit per-leaf codebook-index run described in
  `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.2 lands as a
  new `svq1_stage_indices` module. The reader consumes `N`
  consecutive 4-bit fields (stage-1 first, stage-`N` last) from the
  existing MSB-first `BitReader`, returns the values as an
  allocation-free `IndexBuffer` of up to six unsigned `u8`s in
  `0..=15`, and surfaces the spec's three invariants — bit-tight (no
  inter-stage padding, §4.2.1), stage-ordered (no permutation field,
  §4.2.1), raw (not VLC, §4.2.1) — through both the function
  signature and a dedicated test set.
  - New `svq1_stage_indices` module surface:
    - `BITS_PER_INDEX: u32 = 4` — per-stage field width per
      spec/04 §4.2 (table row "Width : 4 bits").
    - `MAX_STAGES_PER_LEAF: usize = 6` — upper bound on `N` per
      spec/04 §4.1 stage-count VLC alphabet `{-1, 0..=6}`. Mirrors
      `svq1_codebook::SVQ1_STAGES_PER_LEVEL`.
    - `MAX_VEC_IDX: u8 = 15` — upper bound on each `vec_idx` per
      spec/14 §14.4 "16 vectors per stage" (entries `0..=15`).
    - `bits_for_n_stages(n: usize) -> Option<usize>` `const fn` —
      closed-form `4 × N` for `N ∈ 0..=6`, `None` otherwise.
    - `IndexBuffer` struct with `EMPTY` constant, `len()` /
      `is_empty()` / `indices()` / `get(stage_one_based)`
      accessors. Storage is a `[u8; 6]` plus a `len: u8`; values
      past `len` are not exposed to callers.
    - `read_stage_indices(reader, n_stages) -> Result<IndexBuffer>`
      — reads exactly `4 × n_stages` bits MSB-first; rejects
      `n_stages > MAX_STAGES_PER_LEAF` with `Error::BadBitWidth`;
      returns `IndexBuffer::EMPTY` for `n_stages == 0` without
      consuming any bits (mean-only leaf path per spec/04 §4.5.4);
      propagates `Error::Truncated` if the underlying byte slice
      ends mid-run.
  - Twenty-seven new lib tests cover the closed-form arithmetic
    (`bits_for_n_stages` over the full `0..=7` range), the empty
    buffer's accessor invariants, the degenerate cases (zero stages
    consumes no bits; seven or more stages rejected with
    `BadBitWidth(n)`), each edge of the single-stage value range
    (`0`, `5`, `15`), the multi-stage byte-boundary-crossing cases
    (two-stage byte read, three-stage 12-bit read straddling byte
    0/1, six-stage 24-bit read consuming the spec's "3 bytes
    worth"), the per-stage all-zeros / all-fifteens edges, the
    no-inter-stage-padding invariant via `bits_consumed`
    accounting, the continuation read (post-reader cursor lands at
    the exact bit immediately after the last stage's last bit), the
    three truncation paths (mid-stream, first-stage, mid-third-
    stage), the `IndexBuffer::get` one-based indexing convention
    (`get(0)` returns `None`), and the cross-module
    `MAX_STAGES_PER_LEAF == SVQ1_STAGES_PER_LEVEL` consistency
    invariant.
  - Round 242 is the bitstream-side index reader alone. It does
    NOT perform the §4.3 codebook lookup
    (`(level, half, stage, vec_idx) → V_L signed bytes`) — that
    step still depends on the L=0..L=3 payload's intra-vs-inter /
    stage-vs-level interleave being pinned in `docs/video/svq1/`
    (`crates/oxideav-svq/src/svq1_codebook.rs` "Open work" note).
    Once that interleave lands, the per-leaf decoder will call
    `svq1_stage_indices::read_stage_indices` and feed the returned
    `IndexBuffer` into the codebook-offset arithmetic.

- **Round 239 — SVQ1 mean-step saturating arithmetic.** The per-sample
  mean-step apply arithmetic documented in
  `docs/video/svq1/spec/05-mean-removal.md` §5.4 lands as a new
  `svq1_mean` module. The two halves of the SVQ1 mean family — intra
  (`u8 ∈ [0, 255]` per §5.1.1) and inter (`s9 ∈ [-256, +255]` per
  §5.1.2 / §5.1.3) — are exposed as `const fn` helpers, with the
  underlying clamp matching the wiki spec's "saturate to an unsigned
  byte range, 0..255" mandate (§5.4.3). Round 239 is pure arithmetic —
  no bitstream reads — the future mean-VLC wire-up round will read the
  intra (alphabet 256) and inter (alphabet 512, `min_value = -256`)
  mean-VLC tables per spec/05 §5.7 and feed the decoded mean into
  these helpers.
  - New `svq1_mean` module surface:
    - `INTRA_MEAN_MIN: u8 = 0` and `INTRA_MEAN_MAX: u8 = 255` — the
      intra mean range per spec/05 §5.1.1.
    - `INTER_MEAN_MIN: i16 = -256` and `INTER_MEAN_MAX: i16 = 255` —
      the inter mean range per spec/05 §5.1.2 / §5.1.3.
    - `saturate_u8(value: i16) -> u8` `const fn` — the per-stage
      clamp to the unsigned byte range `[0, 255]` per spec/05 §5.4.3.
    - `apply_intra_mean_step(predictor: u8, mean: u8) -> u8` `const
      fn` — the intra path's per-sample mean step. The canonical case
      has `predictor = 0` per spec/05 §5.4.1.
    - `apply_inter_mean_step(predictor: u8, mean: i16) -> Result<u8,
      MeanError>` `const fn` — the inter path's per-sample mean step.
      The predictor is the motion-compensated reference sample; the
      `saturate_u8` clamp is load-bearing on this path per the
      spec/05 §5.1.2 worked examples (`predictor = 0`, `mean = -256`
      → 0; `predictor = 255`, `mean = +255` → 255).
    - `samples_per_leaf(level: Svq1Level) -> Option<usize>` `const
      fn` — the `V_L` replication count per spec/03 §3.3 (`8 / 16 /
      32 / 64` for L=0..L=3; `None` for L=4 / L=5 since those levels
      do not host a mean-removed VQ leaf per spec/14.10 / §14.11).
    - `MeanError::OutOfRange(i16)` — typed error for inter mean
      values outside the closed `[-256, +255]` domain.
  - Twelve new unit tests in `svq1_mean::tests` cover the boundary
    behaviour: `saturate_u8` below-zero / above-255 / pass-through
    sweep across `[0, 255]`; intra mean-only with `predictor = 0`
    using the spec/05 §5.9 worked-example value `mean = 61` plus the
    range boundaries; intra mean with a non-zero predictor exercising
    the clamp; inter mean negative-residue / positive-residue
    saturation per spec/05 §5.1.2's two worked examples; inter
    `mean = 0` returns the predictor unchanged for every predictor
    in `[0, 255]`; inter out-of-range rejection at `-257` / `+256`;
    `samples_per_leaf` matches spec/03 §3.3's `V_L` numbers for
    L=0..L=3 and returns `None` for L=4 / L=5; intra and inter
    range-constant values match spec/05 §5.1.
  - Total tests: 319 lib + 7 integration = 326 (up from 307 + 7 =
    314).

- **Round 233 — SVQ3 per-block coefficient placement (scan-order
  infrastructure).** The per-block placement step that connects the
  Golomb-decoded `(run, value)` coefficient stream from `svq3_coeff`
  to the 2D block matrix consumed by the dequantization arithmetic in
  `svq3_dequant` lands as a new `svq3_scan` module. Round 233 is
  structural arithmetic only —
  `Svq3DecoderHandle::receive_frame` continues to return
  `oxideav_core::Error::Unsupported` until the 4×4 dezigzag + IDCT
  stages land.
  - New `svq3_scan` module surface:
    - `CHROMA_DC_2X2_SCAN: [usize; 4] = [0, 1, 2, 3]` — the
      unambiguous row-major 2×2 chroma DC scan order. The wiki spec's
      §"Coefficient decoding" notes that "chroma DCs are stored in
      2×2 blocks" without drawing a dezigzag picture for the 2×2
      case; for a 2×2 block stored row-major the row-major /
      column-major / diagonal scan orders all collapse to the same
      four-position list.
    - `CHROMA_DC_2X2_LEN = 4` and `FULL_4X4_LEN = 16` — placement-side
      block-capacity constants mirroring the
      `crate::svq3_coeff::COEFFS_PER_*` lengths.
    - `chroma_dc_2x2_flat_index(row, col) -> Option<usize>` `const fn`
      and its inverse `chroma_dc_2x2_matrix_position(flat) ->
      Option<(usize, usize)>` `const fn` — round-trip helpers
      converting between 2×2 matrix `(row, col)` positions and the
      4-entry row-major flat-index store.
    - `ScanError` enum (`OutOfRange` / `InvalidScanOrderEntry` /
      `ScanOrderLengthMismatch`) — typed errors the placement helpers
      raise when the input coefficient stream's cursor would overrun
      the destination, when the scan-order table contains an
      out-of-range entry, or when the scan-order table's length does
      not match the destination's capacity. Implements `Display` +
      `std::error::Error`.
    - `place_coefficients_in_scan_order::<DEST_LEN>(coeffs,
      scan_order) -> Result<[i32; DEST_LEN], ScanError>` — generic
      `(run, value)` stream → fixed-size flat-array placement helper.
      Initialises the destination to all zeros, advances a placement
      cursor by `coeff.run + 1` per non-zero coefficient per the wiki
      spec's §"Coefficient decoding" semantics, and writes the
      coefficient's signed `value` at the scan-order position
      `cursor + run` mapped through the supplied scan-order table.
    - `place_chroma_dc_2x2(coeffs) -> Result<[i32; 4], ScanError>` —
      convenience wrapper pinned to `CHROMA_DC_2X2_SCAN` /
      `CHROMA_DC_2X2_LEN`. Returns the 4-entry block in row-major
      order so its output feeds directly into the
      `svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX = [[8, 8], [8, -8]]`
      application.
  - +24 lib tests covering: the 2×2 row-major scan-order identity-mapping
    invariant; the 2×2 scan length-vs-capacity agreement; the
    scan-position covers-every-position-exactly-once invariant; the
    scan-position → matrix `(row, col)` decoding for all four entries;
    the `chroma_dc_2x2_flat_index` round-trip + out-of-range-row +
    out-of-range-col rejection guards; the
    `chroma_dc_2x2_matrix_position` out-of-range-flat-index rejection;
    placement of an empty stream → all-zero block; single-coefficient
    placement at each `run` value `(0, 1, 3)`; multi-coefficient
    placement with zero-run + non-zero-run sequences; full-block fill
    via four consecutive run=0 coefficients; negative-value sign
    preservation through placement; cursor-overrun rejection at
    in-stream `run`, cumulative-run, and saturated-`u32::MAX` boundaries;
    scan-order length-mismatch and out-of-range scan-entry rejection
    paths; generic-scan-order placement against a permuted 4-entry table;
    generic-scan-order empty-stream identity for any scan order; and
    `ScanError` `Display` impl sanity (`oxideav-svq:` module prefix
    and offending-value mention). Lib-test total: 283 → 307 (290 →
    314 with the 7 standalone-mode integration test).
  - The wiki's §"Macroblock layer" "Dezigzag pattern (from H.264)"
    ASCII art for the 4×4 scan order is NOT transcribed in round 233:
    the picture has two unresolved characteristics — (a) row-0
    horizontal arrows connect `(0,0)→(0,1)→(0,2)`, matching neither
    the H.264 frame-zigzag opening triple `(0,0)→(0,1)→(1,0)` nor the
    H.264 alt-scan opening triple `(0,0)→(1,0)→(0,1)`; (b) the wiki
    text uses "normal zigzag" as the not-this-picture case without
    depicting it. Both 4×4 scan-order arrays are deferred to a future
    docs round that pins their canonical interpretation.

- **Round 230 — SVQ3 macroblock transform + dequantization arithmetic.**
  The per-coefficient dequantization arithmetic the wiki spec defines
  for SVQ3 in `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
  transform and dequantization" lands as a new `svq3_dequant` module.
  Round 230 is structural arithmetic only —
  `Svq3DecoderHandle::receive_frame` continues to return
  `oxideav_core::Error::Unsupported` until the dezigzag + IDCT stages
  land.
  - New `svq3_dequant` module surface:
    - `LUMA_TRANSFORM_MATRIX: [[i32; 4]; 4]` — verbatim from the wiki
      spec's four-row block
      `[[13, 17, 1, 7], [13, 7, -1, -17], [13, -7, -1, 17], [13, -17,
      1, -7]]`.
    - `LUMA_TRANSFORM_DC_COLUMN = 13` — corroborates the all-rows-share-
      `13`-in-column-0 invariant.
    - `CHROMA_DC_TRANSFORM_MATRIX: [[i32; 2]; 2]` — verbatim from the
      wiki spec's `[[8, 8], [8, -8]]` block.
    - `DEQUANT_COEFF_TABLE: [u32; 32]` — the 32-entry per-quantiser
      scale table, monotonically increasing from `3881` at `Q=0` to
      `141_533` at `Q=31`.
    - `DEQUANT_COEFF_TABLE_LEN = 32` /
      `DEQUANT_QUANTISER_RANGE = 0..32` — length and quantiser-range
      constants matching the slice header's 5-bit quantiser field.
    - `INTRA_LUMA_DC_SCALE = 259_922` (= `13 * 13 * 1538`) /
      `INTRA_LUMA_DC_SCALE_TAIL = 1538` — the spec's intra-luma-DC
      scale and its standalone tail factor.
    - `DEQUANT_SHIFT = 20` / `DEQUANT_ROUND = 0x80000 = 1 << 19` —
      the general dequant's right-shift and round-half-up bias.
    - `CHROMA_DC_PRE_SHIFT = 3` / `CHROMA_DC_POST_SHIFT = 1` — the
      two chroma-DC shifts that total `>> 4` (matching the `8`-scaled
      chroma transform matrix).
    - `dequantize_intra_luma_dc(block_zero) -> i32` `const fn` —
      applies `INTRA_LUMA_DC_SCALE * block_zero`, the spec's
      `13 * 13 * 1538 * block[0]` expression for intra luma blocks
      without separate DC coefficient blocks.
    - `dequantize_chroma_dc(q, block_zero) -> i32` `const fn` —
      applies `(DEQUANT_COEFF_TABLE[q] * (block_zero >> 3)) >> 1`,
      the spec's chroma-DC expression.
    - `dequantize_coefficient(q, coeff, dc) -> i32` `const fn` —
      applies `(coeff * DEQUANT_COEFF_TABLE[q] + dc + 0x80000) >> 20`,
      the spec's general per-coefficient dequant expression.
    - `finalise_dc(dc) -> i32` `const fn` — applies
      `(dc + DEQUANT_ROUND) >> DEQUANT_SHIFT`, the no-AC-contribution
      shortcut equivalent to `dequantize_coefficient(_, 0, dc)`.
  - +40 lib tests covering the luma transform matrix shape +
    row-by-row verbatim content + first-column invariant + row-sum
    invariant, the chroma DC transform matrix verbatim content and
    its per-row sums (16 / 0), the 32-entry dequant table's
    length-vs-quantiser-range agreement + first/last entry +
    every-row verbatim content + strict-monotonicity sweep, the
    `DEQUANT_SHIFT = 20` and `DEQUANT_ROUND = 0x80000` provenance
    identities, the `INTRA_LUMA_DC_SCALE = 13 * 13 * 1538 = 259_922`
    decomposition, the chroma-shift total `>> 4` identity, every
    closed-form helper's zero / one / negative / boundary
    arithmetic checks, the `finalise_dc` round-half-up boundary, a
    monotonic-output-in-quantiser sweep for `dequantize_coefficient`,
    and linearity-in-coeff / additivity-in-dc identities (243 → 283
    lib-test total; 250 → 290 lib + integration).

- **Round 224 — SVQ3 sub-pixel thirdpel interpolation arithmetic.** The
  per-sample interpolation arithmetic the wiki spec defines for SVQ3
  motion compensation in
  `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation"
  lands as a new `svq3_mc` module. Round 224 is structural arithmetic
  only — `Svq3DecoderHandle::receive_frame` continues to return
  `oxideav_core::Error::Unsupported`.
  - New `svq3_mc` module surface:
    - `thirdpel_interpolate_1d(a, b) -> i32` `const fn` —
      `((2 * A + B + 1) * 0x2AB) >> 11`.
    - `thirdpel_interpolate_2d(a, b, c, d) -> i32` `const fn` —
      `((4 * A + 3 * B + 3 * C + 2 * D + 6) * 0xAAB) >> 15`.
    - `THIRDPEL_1D_MULTIPLIER = 0x2AB` (683) / `THIRDPEL_1D_SHIFT = 11`
      / `THIRDPEL_1D_BIAS = 1` / `THIRDPEL_1D_WEIGHT_SUM = 3`
      provenance constants.
    - `THIRDPEL_2D_MULTIPLIER = 0xAAB` (2731) / `THIRDPEL_2D_SHIFT = 15`
      / `THIRDPEL_2D_BIAS = 6` / `THIRDPEL_2D_WEIGHT_SUM = 12`
      provenance constants.
    - `THIRDPEL_2D_WEIGHTS: [[u8; 2]; 2] = [[4, 3], [3, 2]]` — the
      verbatim weight matrix the wiki spec quotes.
    - `stored_sixths_base(precision) -> u32` — returns 6 / 3 / 2 for
      Fullpel / Halfpel / Thirdpel, per the wiki spec's "stored and
      predicted as fraction of six and then rounded to the desired
      base" remark.
    - `is_aligned_to_precision_base(stored_sixths, precision) -> bool`
      — checks whether an already-rounded sixths-grid value is on the
      precision's base. The actual rounding step is NOT implemented
      because the wiki spec text leaves the rounding direction
      unspecified.
  - +31 tests covering the multiplier / shift / bias constants, the
    weight matrix and its sum, the fixed-point reciprocal ratios
    (`683 * 3 = 2049 > 2048`, `2731 * 12 = 32772 > 32768`), formula
    expansion matches for zero / equal / asymmetric inputs, exhaustive
    `0..=255 × 0..=255` byte-range coverage for the 1D form,
    representative-spread byte-range coverage for the 2D form, the
    per-precision storage base lookup, and the alignment predicate
    against negative and positive sixths-grid values (212 → 243
    lib-test total; 219 → 250 lib + integration).

- **Round 217 — SVQ1 u16-LE parameter table (512 records) mirrored.**
  The 1024-byte `u16` parameter table identified by the docs
  collaborator's Extractor 02 pass at file offset `0x59d00..0x5a100`
  (VMA `0x67dc9d00..0x67dca100`, section `.rdata`) is now mirrored
  bit-exact under `crates/oxideav-svq/tables/u16_param_table.{csv,
  meta}` and exposed via the existing `svq1_helper_luts` module:
  - `build.rs` gains a `parse_u16_csv` helper (sister of the
    existing `parse_unsigned_csv`) that asserts the `word_index`
    column is gapless `0..512`, parses the `value_u16` column, and
    emits a `SVQ1_U16_PARAM_TABLE: [u16; 512]` static.
  - New `svq1_helper_luts` surface:
    - `SVQ1_U16_PARAM_TABLE: [u16; 512]` static (1024 bytes total).
    - `u16_param_table() -> &'static [u16]` accessor.
    - `SVQ1_U16_PARAM_TABLE_WORDS = 512` /
      `SVQ1_U16_PARAM_TABLE_BYTES = 1024` length constants.
    - `SVQ1_U16_PARAM_TABLE_FILE_OFFSET = 0x0005_9d00` /
      `SVQ1_U16_PARAM_TABLE_VMA = 0x67dc_9d00` provenance constants.
    - `SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES: [u16; 16]` — the closed
      ascending allowed-value set the meta enumerates
      (`{0x0000, 0x0001, 0x0002, 0x0010, 0x0014, 0x0020, 0x0028,
      0x0048, 0x0068, 0x0081, 0x0082, 0x0084, 0x0101, 0x0102,
      0x0181, 0x0182}`).
  - Seven new lib tests cover length-vs-meta, the source-region
    offsets / image-base derivation, the flush-against-clip-LUT
    geometry (`u16_end_vma == SVQ1_CLIP_LUT_VMA`), the
    every-value-is-in-allowed-set closed-set invariant, the
    four-word zero prelude at `word_index 0..4`, the first
    non-zero `0x0020 × 9` group head at `word_index 4..13`, and a
    strictly-ascending-uniqueness guard over
    `SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES`.
  - `tables/MANIFEST-02.sha256` extended with the two new file
    SHA-256s; both match `docs/video/svq1/tables/MANIFEST-02.sha256`
    bit-for-bit.
  - The table is NOT yet wired into a decode path — exposed for
    the future pixel-reconstruction stage to lift in unchanged.
  - Total tests: 212 lib + 7 integration = 219 (up from 212).
- **Round 203 — SVQ1 saturating-clip + bit-mask helper LUTs.**
  Two small helper LUTs identified by the docs collaborator's
  Extractor 02 pass in the SVQ1 `.rdata` region are now mirrored
  bit-exact under `crates/oxideav-svq/tables/` and exposed as
  compile-time constants via a new `svq1_helper_luts` module:
  - **Saturating-clip LUT** — 768 bytes at reference-binary file
    offset `0x5a100..0x5a400` (VMA `0x67dca100..0x67dca400`,
    section `.rdata`). New `tables/clip_lut.{csv,meta}`. Per the
    meta this is the codec's interpolation / overflow-saturation
    helper — explicitly NOT a VQ codebook.
  - **Bit-position / bit-mask LUT** — 16 bytes at file offset
    `0x5c1c4..0x5c1d4` (VMA `0x67dcc1c4..0x67dcc1d4`). New
    `tables/svc_bitmask_lut.{csv,meta}`. First 8 entries are the
    descending single-bit masks
    `0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01`; last 8 are
    their one's complements
    `0x7f, 0xbf, 0xdf, 0xef, 0xf7, 0xfb, 0xfd, 0xfe`.
  - `build.rs` gains a `parse_unsigned_csv` helper (sister of the
    existing signed-byte CSV parser) that asserts `byte_index` is
    gapless `0..N`, parses the `value_unsigned` column as `u8`,
    and emits `SVQ1_CLIP_LUT: [u8; 768]` + `SVQ1_BITMASK_LUT:
    [u8; 16]` alongside the existing `SVQ1_CODEBOOK_L0L3_BYTES`
    / `SVQ1_CODEBOOK_DESCRIPTOR` / `SVQ1_BLOCK_SHAPE_LUT` /
    `SVQ1_L{4,5}_ABSENCE` constants.
  - New `svq1_helper_luts` module exposes:
    - `SVQ1_CLIP_LUT: [u8; 768]` static
    - `SVQ1_BITMASK_LUT: [u8; 16]` static
    - `clip_lut() -> &'static [u8]` accessor
    - `bitmask_lut() -> &'static [u8]` accessor
    - `SVQ1_CLIP_LUT_BYTES = 768`, `SVQ1_BITMASK_LUT_BYTES = 16`
    - `SVQ1_CLIP_LUT_FILE_OFFSET = 0x0005_a100`,
      `SVQ1_CLIP_LUT_VMA = 0x67dc_a100`,
      `SVQ1_BITMASK_LUT_FILE_OFFSET = 0x0005_c1c4`,
      `SVQ1_BITMASK_LUT_VMA = 0x67dc_c1c4`
  - Eight new lib tests cover length-vs-meta, the descending-mask
    half, the one's-complement half, the documented 16-byte
    bitmask string, the clip-LUT identity-ramp head at offset
    `0x10..0x20` (`0x80..0x8f`), and the derivable image base
    `0x67d70000` (VMA − file-offset) for both LUT regions.
  - `tables/MANIFEST-02.sha256` extended with the four new file
    SHA-256s, matching `docs/video/svq1/tables/MANIFEST-02.sha256`.
  - Total lib tests rise to 205 (was 197); integration tests
    unchanged at 7.

- **Round 197 — SVQ1 L=4 / L=5 codebook ABSENCE wired end-to-end.**
  The docs collaborator's Extractor 02 pass
  (`docs/video/svq1/spec/14.10-codebook-L4.md`,
  `spec/14.11-codebook-L5.md`,
  `tables/codebook-l{4,5}.meta`,
  `provenance/02-codebook-extraction.md`) RESOLVED the L=4 / L=5
  codebook gap as **architecturally absent**: no codebook exists at
  16×8 or 16×16 in the reference build; both block sizes are always
  subdivided to ≤8×8 before quantisation.
  - New crate-local `tables/codebook-l4.meta` + `codebook-l5.meta`
    bit-exact mirrors of `docs/video/svq1/tables/`.
  - `build.rs` now parses the two new meta files at build time;
    a new `parse_absent_meta` helper asserts `status: ABSENT` +
    `level` / `block_size` / `canonical_vector_len_bytes` /
    `canonical_6stage_intra_or_inter_bytes` against the per-level
    invariants (L=4 → `4 / 16x8 / 128 / 12288`; L=5 →
    `5 / 16x16 / 256 / 24576`). A future docs revision that flips
    `status` to "present" or quietly changes the canonical sizes
    fails the build before any consumer relies on the `None`
    branch.
  - New `Svq1AbsentLevelRecord` typed struct
    (`{ level, block_size, canonical_vector_len_bytes,
       canonical_6stage_intra_or_inter_bytes }`).
  - New `pub const SVQ1_L4_ABSENCE: Svq1AbsentLevelRecord` and
    `pub const SVQ1_L5_ABSENCE: Svq1AbsentLevelRecord` populated by
    `build.rs` from the meta files.
  - New `Svq1Level::absence_record() -> Option<Svq1AbsentLevelRecord>`
    `const fn`. Returns `Some(SVQ1_L{4,5}_ABSENCE)` for the
    always-subdivided levels and `None` for L=0..L=3. Documented
    invariant: `absence_record().is_some() ==
    codebook_bytes_per_half().is_none()` for every level.
  - 5 new unit tests in `svq1_codebook::tests` covering the
    `SVQ1_L4_ABSENCE` / `SVQ1_L5_ABSENCE` constants
    (level / block_size / vec_len / per-half), the per-half byte
    count vs vector_length identity, the `absence_record` accessor
    (L=4 / L=5 → Some, L=0..L=3 → None), and the
    `absence_record.is_some() ⇔ codebook_bytes_per_half.is_none()`
    invariant.
  - 7 new integration tests in
    `tests/svq1_codebook_l4_l5_absence.rs` exercising the full
    end-to-end absence contract — `Svq1Level::L4` /
    `Svq1Level::L5` reporting absent through every public surface,
    `Svq1Level::L0..L3` reporting present, the
    `read_block_decision` walker rejecting in-place quantisation
    at L=4 / L=5 with the typed
    `Error::InvalidLevelQuantise(level)` variant (with the level
    field roundtripping through `absence_record()`), the
    block-shape LUT capping at 4 corroborating the absence, and
    the four absence-surface predicates agreeing on every level.
  - Total tests: 197 lib + 7 integration = 204 (was 192 → +12).
  - **Deferred** — the internal intra-vs-inter ordering and
    stage-vs-level interleave WITHIN the 23004-byte L=0..L=3
    payload remains a sibling docs spec task per
    `docs/video/svq1/CODEBOOK_GAP.md` "remaining open work" note.
    Full pixel reconstruction still waits on that layout doc.

- **Round 9 — SVQ1 L=0..L=3 codebook payload landed.** The 23004-byte
  mean-removed multistage VQ payload + 36-byte descriptor/block-shape
  prefix from `docs/video/svq1/tables/` (Extractor 02, file offset
  `0x5d200..0x62c00` of the reference binary `quicktimethirdparty.qtx`
  SHA-256 `ac3509bf22aa1458dfc6e1af980956c0153b4c287af452ae5b9cac6f923be169`)
  are now compile-time constants in a new `svq1_codebook` module.
  - New crate-local `tables/` directory mirrors
    `docs/video/svq1/tables/{codebook-l0l3,codebook-descriptor}.{csv,hex,meta}`
    + `MANIFEST-02.sha256` bit-exact so the in-repo CI checkout can
    build without reaching out to the docs submodule.
  - New `build.rs` parses the two CSVs (`value_signed` column) at
    build time and emits `SVQ1_CODEBOOK_L0L3_BYTES: [i8; 23004]`,
    `SVQ1_CODEBOOK_DESCRIPTOR: [u8; 36]`,
    `SVQ1_BLOCK_SHAPE_LUT: [u8; 16]` under `$OUT_DIR/`.
  - `svq1_codebook::Svq1Level::codebook_bytes_per_half()` const
    method returns `Some(768)` / `Some(1536)` / `Some(3072)` /
    `Some(6144)` for L=0..L=3 (one half — intra OR inter) and `None`
    for L=4 / L=5. Per-half × 2 summed across L=0..L=3 is
    `2 × (768 + 1536 + 3072 + 6144) = 23040 B`, matching the
    `36 B descriptor + 23004 B payload` region total.
  - Public accessors: `codebook_l0l3_payload() -> &'static [i8]`,
    `codebook_descriptor() -> &'static [u8]`, `block_shape_lut() ->
    &'static [u8]`.
  - Public size constants: `SVQ1_CODEBOOK_PAYLOAD_BYTES = 23004`,
    `SVQ1_CODEBOOK_DESCRIPTOR_BYTES = 36`,
    `SVQ1_BLOCK_SHAPE_LUT_LEN = 16`, `SVQ1_STAGES_PER_LEVEL = 6`,
    `SVQ1_ENTRIES_PER_STAGE = 16`.
  - 11 unit tests cover payload + descriptor lengths, the
    full-region size arithmetic, per-level byte counts, L=4 / L=5
    `None` rejection, the 16-entry block-shape LUT against the exact
    byte string `04 04 03 02 04 03 03 02 03 03 02 02 03 02 02 01`
    recorded in `codebook-descriptor.meta` line 22, the LUT cap at
    `1..=4` (corroborating the §14.10 / §14.11 ABSENT findings), the
    first descriptor record's `(b0=0x03, b3=0x18, b4=0x02)` byte
    pattern, the first 16 i8 entries against `codebook-l0l3.hex` row
    1, and accessor aliasing. Total tests now 192 (was 181).
  - **Deferred** — Round 9 deliberately does NOT yet expose a
    `(level, stage, intra_or_inter, vector_idx) → &[i8]` lookup.
    The precise intra-vs-inter ordering and stage-vs-level interleave
    WITHIN the 23004-byte payload is a sibling docs spec task per
    `docs/video/svq1/tables/codebook-l0l3.meta` lines 30-32 ("the
    L0..L3 spec's concern"). Full pixel reconstruction unblocks when
    the internal-layout spec lands. The L=4 / L=5 codebook bytes
    themselves are confirmed ABSENT in this build per
    `docs/video/svq1/spec/14.10-codebook-L4.md` and
    `docs/video/svq1/spec/14.11-codebook-L5.md` — there is nothing
    to add for those levels.

- **Round 8 — SVQ1 block-tree subdivision walker (structural).** The
  recursive subdivide-vs-quantise decision tree described in
  `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
  Plane Data" lands as a new `svq1_blocktree` module. The walker
  reads ONE bit per level at L=1..=L=5 (1 ⇒ subdivide, 0 ⇒ quantise),
  short-circuits the L=0 base case to "quantise" with no bit
  consumed, and rejects the 0-bit (in-place quantise) branch at L=4
  / L=5 with a new dedicated error variant. The L=4 / L=5 rejection
  is corroborated by the newly-staged clean-room codebook-extraction
  docs `docs/video/svq1/spec/14.10-codebook-L4.md` and
  `docs/video/svq1/spec/14.11-codebook-L5.md`, both of which
  resolve to "no codebook stored at this level in the Sorenson Video
  TM for QT R2.0 build — the level is always subdivided".
  - `svq1_blocktree::Svq1Level { L0, L1, L2, L3, L4, L5 }` —
    typed level enum with `block_dims()` / `vector_length()` /
    `rejects_in_place_quantise()` const methods matching the wiki
    spec's level table (4×2 / 4×4 / 8×4 / 8×8 / 16×8 / 16×16; 8 /
    16 / 32 / 64 / 128 / 256 samples; `true` only at L=4 / L=5).
  - `svq1_blocktree::Svq1BlockDecision { Subdivide, Quantise }` —
    typed result of one block-tree node.
  - `svq1_blocktree::read_block_decision(level, &mut BitReader) ->
    Result<Svq1BlockDecision>` — one-bit walker; L=0 short-circuits
    to `Quantise` without reading a bit; L=4 / L=5 0-bit returns
    `Error::InvalidLevelQuantise(level)`.
  - `svq1_blocktree::subdivide(Svq1Level) -> Option<(Svq1Level,
    Svq1Level)>` — `const fn` returning the two child levels for
    any non-leaf level, `None` at L=0.
  - `Error::InvalidLevelQuantise(Svq1Level)` — new error variant
    surfacing the wiki spec's "invalid vector, error out of decode
    since levels 4 and 5 blocks do not use multistage VQ"
    condition. Mapped to `oxideav_core::Error::InvalidData` via the
    existing `From<crate::Error> for oxideav_core::Error` impl.
  - 13 unit tests covering the level table, vector-length table,
    every per-level subdivide/quantise/truncation path, the
    L=4 / L=5 rejection, and a worked 7-bit breadth-first walk.
  - Round 8 stays structural — the per-leaf "stages count VLC +
    mean VLC + (stages × 4)-bit codebook selector" payload
    remains gated on the L=0..L=3 codebook layout / VLC table
    work tracked in `docs/video/svq1/CODEBOOK_GAP.md`.

- **Round 7 — SVQ3 intra-4×4 predictor-from-neighbour resolution
  helper.** The per-sub-block predictor lookup described in
  `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
  information decoding" — `pred_table[top + 1][left + 1][idx]` plus
  the two substitution rules ("when predictors lie outside of slice,
  -1 is used instead", "for 16x16 intra and any inter blocks value of
  2 is used as the predictor") — is now a typed helper in the
  `svq3_mb` module.
  - `svq3_mb::IntraNeighbour { Outside, Intra16x16OrInter, Mode4x4(u8) }`
    — typed neighbour classification carrying the substitution rule
    information.
  - `svq3_mb::IntraNeighbour::lookup_index() -> Result<u8>` — returns
    the `0..=5` index along the table's first / second axis after the
    spec's `+ 1` adjustment, honouring both substitution rules. Errors
    with `Error::BadBitWidth` when `Mode4x4(mode > 4)` is passed.
  - `svq3_mb::resolve_intra_4x4_predictor(top, left, idx) ->
    Result<u8>` — performs the table lookup. Returns the resolved
    intra-prediction mode `0..=4` on success; returns the new
    `Error::InvalidIntraPrediction(top_idx, left_idx, idx)` when the
    looked-up entry is the `-1` sentinel (spec: "input data was
    incorrect or intra modes were predicted incorrectly"); returns
    `Error::BadBitWidth` when `idx > 4`.
  - `svq3_mb::resolve_intra_4x4_pair(top, left, (a, b)) ->
    Result<(u8, u8)>` — walks both elements of an `INTRA_PRED_PAIRS`
    entry against the same neighbour context.
  - `Error::InvalidIntraPrediction(u8, u8, u8)` — new error variant
    surfacing the spec's "table value is -1" malformed-bitstream
    condition. Mapped to `oxideav_core::Error::InvalidData` via the
    existing `From<crate::Error> for oxideav_core::Error` impl.

- **Round 6 — SVQ3 inter-MB motion-vector precision selector.** The
  three-branch decision described in
  `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Inter macroblock
  information decoding" is now a typed reader in the `svq3_mb` module.
  The selector consumes 0, 1, or 2 bits depending on the sequence
  header's `has_thirdpel` / `has_halfpel` flags and returns one of
  three precisions. B-frame inter macroblocks short-circuit to halfpel
  per the spec's §"Macroblock transform and dequantization" remark
  ("it is always halfpel precision in B-frames") and consume no bit.
  - `svq3_mb::Svq3MvPrecision { Fullpel, Halfpel, Thirdpel }` — typed
    sample-grid precision result.
  - `svq3_mb::read_inter_mv_precision_p_frame(br, has_thirdpel,
    has_halfpel) -> Result<Svq3MvPrecision>` — implements the
    three-branch selector verbatim from the wiki spec for P-frame inter
    macroblocks. Bit-consumption pattern: 0 bits when both flags off,
    1 bit when exactly one flag is set, 1-2 bits when both flags are
    set.
  - `svq3_mb::read_inter_mv_precision(br, frame_type, has_thirdpel,
    has_halfpel) -> Result<Svq3MvPrecision>` — frame-type dispatch that
    short-circuits B-frames to halfpel without reading any bit and
    defers P-frame inter to the standalone reader.

- **Round 5 — SVQ3 residual coefficient walker.** The per-block
  Golomb-coded `(run, value)` residual coefficient stream described in
  `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient decoding"
  lands in the new `svq3_coeff` module. Three coefficient-table
  variants (2×2 chroma DC, alternative-scan 4×4 luma-intra-with-low-
  quantiser, normal-zigzag everything else) plus the two run-correction
  arrays (`intra_run`, `inter_run`) are implemented verbatim from the
  wiki spec. The block-level walkers loop until either the end-of-block
  sentinel (`code = 0`) is seen or the block's coefficient capacity is
  reached; structural overflow returns `Error::BadBitWidth`. Round 5
  remains structural — de-zigzag, dequantisation, and IDCT remain out
  of scope.
  - `svq3_coeff::read_chroma_dc_coefficient` /
    `read_alt_scan_coefficient` / `read_normal_scan_coefficient` —
    decode one coefficient (Golomb code + sign bit) per call,
    returning `Ok(None)` on end-of-block / `Ok(Some(Coefficient))`
    otherwise / `Err(Error::Truncated)` on short input.
  - `svq3_coeff::read_chroma_dc_block` / `read_alt_scan_half` /
    `read_normal_scan_block` — block-level walkers that gather the
    `Coefficient` triples up to the per-block coefficient cap (4 /
    8 / 16) and surface structural overflow as
    `Error::BadBitWidth(scan_position)`.
  - `Coefficient { run: u32, value: i32 }` typed result struct.
  - `INTRA_RUN_CORRECTION: [i32; 8]` and
    `INTER_RUN_CORRECTION: [i32; 17]` — the two run-correction arrays
    landed verbatim, with the `[minus ones]` / `[zeroes]` shorthand
    tails handled by the per-table extension formulas.
  - `ALT_SCAN_TABLE_0_15: [(u32, i32); 16]` and
    `NORMAL_SCAN_TABLE_0_15: [(u32, i32); 16]` — verbatim
    transcriptions of the wiki spec's first 16 rows.
  - `COEFFS_PER_4X4_BLOCK = 16`, `COEFFS_PER_CHROMA_DC_BLOCK = 4`,
    `COEFFS_PER_ALT_SCAN_HALF = 8` — block-capacity constants.
  - +36 tests covering single-coefficient table lookups (explicit
    codes + closed-form extensions for both alt-scan and normal-scan),
    sign-bit application, end-of-block sentinel detection, block-
    walker capacity caps, run-overflow rejection, and truncation
    propagation (105 → 141 total).

- **Round 4 — SVQ3 macroblock-type tree walk (structural).** The
  per-macroblock type-Golomb decode + classification from
  `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock layer"
  lands in the new `svq3_mb` module, alongside the intra-prediction
  pair / context-lookup tables and the 4×4 intra scan order from
  §"Intra macroblock information decoding". Round 4 remains
  structural — the macroblock body past the type code (CBP / intra
  mode pair / motion vectors / residual coefficients) is **not**
  decoded; the decoder handle still returns
  `oxideav_core::Error::Unsupported` from `receive_frame`.
  - `svq3_mb::read_mb_type(&mut BitReader, Svq3FrameType) ->
    Result<Svq3MbType>` — reads a single `ue(v)` exp-Golomb code at
    the bit-reader's current position and classifies it against the
    enclosing slice's frame type per the wiki spec table.
  - `svq3_mb::classify_mb_type(Svq3FrameType, u32) ->
    Result<Svq3MbType>` — pre-decoded-code variant for callers that
    need to expose the raw value for CBP / mode-pair follow-up.
  - `Svq3MbType` enum with five variants: `IIntra(IFrameMbType)`,
    `PInter(PFrameInterMode)`, `PIntra(IFrameMbType)`,
    `BInter(BFrameInterMode)`, `BIntra(IFrameMbType)`. Carries the
    underlying I-frame intra MB type for P / B intra MBs after
    peeling the per-slice intra offset (`P_FRAME_INTRA_OFFSET = 8`,
    `B_FRAME_INTRA_OFFSET = 4`).
  - `IFrameMbType` enum: `LumaDcSeparate` (code 0),
    `PredefinedCbpMode(u32)` (codes 1..=24, raw value preserved),
    `LumaDcSeparateNoOthers` (code 25).
  - `PFrameInterMode` enum: `Skip`, `Inter16x16`, `Inter8x16`,
    `Inter16x8`, `Inter8x8`, `Inter4x8`, `Inter8x4`, `Inter4x4`
    (codes 0..=7).
  - `BFrameInterMode` enum: `Direct`, `Forward`, `Backward`,
    `Bidirectional` (codes 0..=3).
  - Predicate helpers: `Svq3MbType::is_intra()` / `is_inter()` /
    `is_skip()` / `num_motion_vectors()` / `intra()` — let the
    downstream residual / MC stage classify without rebuilding the
    enum match.
  - `INTRA_PRED_PAIRS: [(u8, u8); 25]` — the wiki spec's 4×4
    intra-mode pair table in the documented triangular listing
    order. Round 4 lands the table verbatim for a future intra-mode
    Golomb-walk stage.
  - `INTRA_PRED_TABLE: [[[i8; 5]; 6]; 6]` — the wiki spec's
    `pred_table[top + 1][left + 1][idx]` context-lookup table.
    Stored as `i8` so the `-1` sentinel ("out-of-slice predictor"
    per the wiki spec) is representable.
  - `INTRA_4X4_SCAN_ORDER: [u8; 16]` — the wiki spec's 4×4 intra
    sub-block scan order (`(0, 1)(4, 5) … (10, 11)(14, 15)`)
    flattened row-major.
  - Per-slice constants: `I_FRAME_MB_TYPE_MAX = 25`,
    `P_FRAME_MB_TYPE_MAX = 33`, `B_FRAME_MB_TYPE_MAX = 29`,
    `P_FRAME_INTRA_OFFSET = 8`, `B_FRAME_INTRA_OFFSET = 4`.
  - +24 tests covering: I/P/B-frame code-table classification
    (exhaustive), out-of-range rejection, Golomb-decode round trips
    for representative codes in each frame type, MV-count predicate
    correctness, intra-table shape + sentinel + invariants, scan
    order permutation check. Total crate test count
    81 → 105 (+24).

- **Round 3 — SVQ3 SEQH + slice-header parser (structural).** The
  SVQ3 sequence-header (`SEQH` extradata) and per-slice header
  parsers from
  `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Sequence Header" /
  §"Slice Header" land in the new `svq3` module, and the SVQ3 `SVQ3`
  FourCC is registered alongside SVQ1 in
  `oxideav_core::CodecRegistry`. Round 3 is structural-only —
  `Svq3DecoderHandle::receive_frame` returns
  `oxideav_core::Error::Unsupported`; the macroblock layer (motion
  compensation, Golomb-coded residual) is out of round-3 scope.
  - `svq3::SVQ3_SEQH_MAGIC` (`"SEQH"`) + `svq3::SVQ3_FRAME_END`
    (`0xFF`) constants.
  - `svq3::strip_seqh_prefix(&[u8])` — validates the 4-byte SEQH
    marker + 4-byte big-endian length and returns the payload slice.
  - `svq3::parse_sequence_header(&[u8]) -> Result<Svq3SequenceHeader>`
    — walks the bit-packed `3 + (24)? + 1 + 1 + 4 + 1 + (1+8)* + 1`
    layout and returns the typed sequence header. The 7-entry
    `FRAME_SIZE_TABLE` lookup is shared with the SVQ1 parser; the
    code-7 escape reads 12+12-bit explicit dimensions.
  - `svq3::parse_extradata(&[u8])` — convenience wrapper combining
    the two steps above.
  - `svq3::num_macroblocks(&Svq3SequenceHeader)` — derives the
    macroblock count from the parsed sequence header (used by the
    v2 slice-header to compute the macroblock-offset field width).
  - `svq3::unpermute_slice_payload(body, slice_size_size)` — reverses
    the wiki spec's `"first 0-2 bytes ... stored at the very end of
    the slice"` permutation. The further "may be further scrambled"
    descramble step is deferred until the algorithm is documented.
  - `svq3::parse_slice_header(unpermuted_body, version,
    slice_size_size, slice_size, num_mbs, protected)` — parses the
    Golomb-coded frame-code (`0=P`, `1=B`, `2=I`), the
    version-dependent `has_more_slices_v1` / `mb_offset_v2` field,
    the 8-bit frame number, 5-bit slice quantiser, delta-qp /
    unknown / protected-unknown flags, and the optional-byte
    trailer loop. Returns a typed `Svq3SliceHeader`.
  - `svq3::parse_wire_slice(wire_slice, num_mbs, protected)` —
    end-to-end helper: reads the 1-byte version/size-size prefix +
    1-3 byte slice-size field, unpermutes the body, parses the
    slice header, and surfaces the macroblock-layer remainder for
    future use.
  - `svq3::read_ue_golomb(&mut BitReader)` — unsigned exp-Golomb
    decoder (`ue(v)`) per the wiki spec's "extensively uses Golomb
    coding" note + the "based on an early H.264 draft" provenance.
  - `Svq3FrameType` / `SliceVersion` enums + `Svq3SequenceHeader`
    / `Svq3SliceHeader` structs typing every wiki-spec'd field.
  - Registry: `SVQ3_CODEC_ID_STR = "svq3"`, `SVQ3_FOURCC_CODES =
    [b"SVQ3"]`, `make_svq3_decoder` factory, `probe_svq3` probe
    accepting any first-byte whose high 3 bits land in 1..=3 and
    whose low 5 bits decode to version 1 or 2.
  - `Svq3DecoderHandle` implementing `oxideav_core::Decoder`:
    `send_packet` parses the slice header eagerly (or accepts the
    `0xFF` frame-end sentinel without parsing); `receive_frame`
    returns `Error::Unsupported`; `sequence_header()` /
    `last_slice_header()` accessors expose parsed state for
    integrators. The extradata is parsed eagerly at construction.
  - +29 tests covering the new SEQH + slice-header + permutation +
    Golomb paths, the SVQ3 registry registration, and the
    decoder-handle state machine. Total crate test count
    52 → 81 (+29).

- **Round 2 — `oxideav-core` framework integration.** The structural
  SVQ1 frame-header parser is now wired into the framework registry.
  - Default-on `registry` cargo feature gating the `oxideav-core`
    dependency. With `default-features = false` the crate exposes only
    the standalone `parse_frame_header` / `Svq1FrameHeader` / `BitReader`
    / `Error` surface.
  - `register(&mut RuntimeContext)` / `register_codecs(&mut
    CodecRegistry)` entry points plus the `__oxideav_entry` symbol the
    `oxideav_core::register!` macro expands to.
  - `Svq1DecoderHandle` implementing `oxideav_core::Decoder`:
    `send_packet` parses the frame header eagerly (structural failures
    surface at `send_packet` rather than later); the parsed
    `Svq1FrameHeader` is exposed via `Svq1DecoderHandle::last_header()`.
    `receive_frame` returns `Error::Unsupported` because the codebook
    docs-gap is still open.
  - `probe_svq1(&ProbeContext)` registered alongside the FourCC tags:
    `1.0` on a structurally valid header, `0.5` on a truncated header
    or on the no-packet case (FourCC alone is highly disambiguating),
    `0.0` on a structurally invalid header.
  - `From<crate::Error> for oxideav_core::Error` conversion mapping
    every structural failure to `InvalidData(msg)` with a descriptive
    string; `NotImplemented` maps to `Unsupported`.
  - FourCC declarations: `SVQ1` (canonical, also covers `svq1` since
    `CodecTag::fourcc` upper-cases) and `svqi`, sourced from line 9 of
    `docs/video/svq1/wiki/Sorenson_Video_1.wiki`.
  - Inline `ci-standalone` GitHub Actions job exercising the
    `--no-default-features` build + test path so regressions where a
    standalone module accidentally re-imports `oxideav-core` are caught
    at PR time.

- **Round 1 — SVQ1 frame-header parser.** `parse_frame_header` walks
  the bit-packed SVQ1 chunk header documented in
  `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
  Header" and returns a typed `Svq1FrameHeader`. Coverage:
  - 22-bit `frame_code` + the `(frame_code & 0x60) == 0`
    invalid-stream check.
  - 8-bit `temporal_reference`.
  - 2-bit `picture_type` (decoded to `Svq1PictureType::Intra` /
    `Predicted` / `Droppable`); value `3` rejected per spec.
  - I-frame trailer chain: optional 16-bit checksum on frame codes
    `0x50` / `0x60`; optional XOR-obfuscated embedded ASCII string
    when `frame_code ^ 0x10 > 0x50` (captured raw, length-prefixed);
    the 2 + 2 + 1 "unknown" sub-fields; 3-bit `frame_size_code`;
    explicit 12 + 12 width/height on code `7`.
  - 1-bit `checksum_present` flag and its `use_packet:1 +
    component:1 + reserved2:2` (reserved2 must be 0) trailer.
  - 1-bit `unknown_flag_1` flag, its 1 + 4 + 1 + 2 fixed-width
    sub-fields, and the `while next-bit-is-1 { read 8 bits }`
    variable-length tail.
  - `FRAME_SIZE_TABLE` constant for the seven standard dimensions.
  - `SVQ1_FOURCC_CODES` constant listing the three FourCC codes the
    spec attaches to SVQ1.
  - MSB-first `BitReader` sized for the header fields.
- Structural error variants: `InvalidFrameCode`,
  `InvalidPictureType`, `InvalidChecksumTrailer`, `Truncated`,
  `BadBitWidth`; the round-0 `NotImplemented` sentinel remains
  available for unwired API surfaces.

### Notes

- **Docs gap (SVQ1 codebooks, still tracked):** the SVQ1 multi-stage
  VQ codebooks and per-level VLC tables enumerated in the wiki
  spec's "Appendix A: SVQ1 Data Tables" are not yet pinned in
  `docs/`. The encoded plane-data layer is blocked on that data
  landing in `docs/video/svq1/spec/` or `docs/video/svq1/tables/`.
- **Docs gap (SVQ3 macroblock layer):** the wiki spec's
  §"Macroblock layer" + §"Intra macroblock information decoding"
  type-tree + intra-mode pair table + context-lookup table are now
  covered by round 4. Still open downstream of those: the wiki
  spec's §"Coefficient decoding" tables 1/2/3 (handed-off-but-not-
  yet-implemented data tables for the three Golomb-codeword
  branches), §"Inter macroblock information decoding" MV component
  VLC bit-listing (the wiki spec describes the precision-selector
  but not the VLC table itself), §"Macroblock transform and
  dequantization" quantizer table (present in the wiki as
  `svq3_dequant_coeff`; can land alongside coefficient decoding),
  and §"Intra prediction" (mostly a back-reference to H.264 plus
  the three named SVQ3-specific quirks).
- **SVQ3 protected-stream watermark sub-record:** the wiki spec
  describes the byte layout but the variable-length-code encoding
  of the watermark width / height / unknown fields is not pinned,
  and the deflated watermark image's role as decryption input is
  out of round-3 scope. Captured at a structural level via
  `Svq3SequenceHeader::protected`.
- **SVQ3 slice descramble:** the wiki spec's note "Additionally
  slice data may be further scrambled probably in order to prevent
  unauthorised playback" is not yet pinned in `docs/`; round 3
  applies the documented byte-permutation step (first 0-2 bytes
  moved to slice trailer) but not the additional descramble.

### Provenance

Round 1 was implemented strictly from
`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
Header". Round 2 added line 9 (FourCC list) from the same file and
the `oxideav-core` public API (`Decoder` / `CodecRegistry` /
`CodecParameters` / `Packet` / `ProbeContext` / `register!` macro).
Round 3 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Sequence Header" /
§"Slice Header" / §"Packetization" (a verbatim local mirror of the
multimedia.cx Sorenson_Video_3 wiki page, fetched 2026-05-06,
CC-BY-SA per multimedia.cx terms). The SVQ1 wiki file's
`FRAME_SIZE_TABLE` is re-used by the SVQ3 parser (the two formats
share the seven standard dimension pairs).

Round 4 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock layer" /
§"Intra macroblock information decoding" of the same local mirror.
No additional spec documents were opened during round 4.

No external library source,
no archived `old` branch of this crate, and no online cross-checks
were consulted in any of the four rounds.

## [0.0.1] — Round 0 — clean-room rebuild scaffold

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch.
  See `README.md` for the rebuild scope and the strict-isolation
  workspace the Implementer rounds will draw from.
