# oxideav-svq

Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec scaffold for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.
Implemented from the clean-room specifications staged under
[`docs/video/svq1/`](../../docs/video/svq1/) and
[`docs/video/svq3/`](../../docs/video/svq3/).

## Status

**Decode pipelines wired up to the per-macroblock unit; the
frame-level entry point is not yet exposed.** Both decoders parse their
bitstreams into typed structures and stage the required tables. For
SVQ3, a single 4×4-intra macroblock now reconstructs **end-to-end from
slice bits to a 16×16 luma plane** (intra-mode VLC → predictor →
residual → writeback). `receive_frame` still returns
`oxideav_core::Error::Unsupported`: the slice-level frame walk that
drives the per-macroblock units across a whole picture is gated on the
one remaining CBP `me(v)` docs trace (see the SVQ3 gap note below).
What is implemented:

### SVQ1

* Full frame-header parse (`parse_frame_header` → `Svq1FrameHeader`):
  frame code, temporal reference, picture type, I-frame trailer chain,
  frame-size code / dimensions.
* The block-tree subdivision walker (all six levels) and per-stage
  codebook-index field reader.
* The L0..L3 multistage-VQ codebook payload, block-shape LUT, and the
  saturating-clip / bit-mask / u16 parameter helper LUTs, all embedded
  as bit-exact compile-time constants. L4 / L5 codebooks are
  architecturally absent (always subdivided) and modelled as such.
* Mean-removal arithmetic (intra `u8` / inter `s9`, saturating).
* The inter motion-vector **median-of-three predictor**
  (`svq1_motion_predictor`): the component-independent
  `MEDIAN(pl, pt, ptr)` baseline with the absent-neighbour fallback
  (one-present short-circuit, `(0,0)`-substituted median otherwise),
  plus the `[-32, +31]` final-vector component clip (spec/06 §6.4 /
  §6.6). Verified bit-exact against the §6.4.1 worked example
  (`pl=(0,0), pt=(5,17), ptr=(-9,12) → predictor=(0,12)`). The
  per-component differential VLC (T02) read is deferred: spec/06
  §6.2.3 flags a bit-stream-affecting Reading A/B ambiguity pending a
  Validator round, so only the unambiguous predictor + clip
  arithmetic (shared by both readings) is wired.
* The per-plane **motion-vector cache + neighbour-selection geometry**
  (`svq1_mv_cache`): the `(mb_cols × 2) × (mb_rows × 2)` grid of 8×8
  block MVs (spec/06 §6.8 / §6.1.1 granularity invariant), the INTER
  single-MV neighbour triple `{pl=(r,c−1), pt=(r−2,c), ptr=(r−2,c+2)}`
  (§6.4.3, reproducing the wiki grid `[1 2 / 3 4] → {N, C, E}`), the
  four per-sub-block INTER_4MV triples of §6.4.4 (with the within-MB
  top-left-as-`ptr` deviation for sub-blocks 3/4), the strictly-serial
  §6.4.5 INTER_4MV decode, and the §6.8.1 / §6.9 cache-update + SKIP /
  INTRA `(0,0)` reset rules. Out-of-bounds lookups are *absent*
  neighbours (§6.4.2), distinct from in-bounds `(0,0)` slots. Pure
  indexing + storage feeding `svq1_motion_predictor`; the differential
  `(dx, dy)` remains caller-supplied (the deferred T02 wire decode).
* The per-leaf stage-accumulation reconstruction (`reconstruct_leaf`):
  the fixed-order `predictor → mean → stage-1 … stage-N` summation with
  the `[0, 255]` clamp applied after every add, in output-raster order
  (spec/04 §4.5 / §4.7.1). Verified bit-exact against the §4.8 worked
  example (`mean=61`, two stages → `[55 39 50 77 / 93 81 49 46]`).

The remaining SVQ1 gap is the intra-vs-inter / stage-vs-level
interleave *within* the codebook payload (which fixes the byte offset
of each `(level, half)` page the reconstructor reads), plus the
stage-count / mean / index VLC wire-up, and the MV-component VLC
(T02) whose Reading A/B disambiguation (spec/06 §6.2.3) awaits a
Validator round. The inter MV path now has its predictor (§6.4),
final-vector clip (§6.6), per-plane cache (§6.8), INTER /
INTER_4MV neighbour-selection geometry (§6.4.3 / §6.4.4), and the
**half-pel reference sampling** (`svq1_mc`, §6.5 / §6.7) wired; only
the T02 differential wire decode remains on the MV path. Until the
deferred field decodes land, full plane reconstruction is blocked on
bitstream-driven field decode.

* The **half-pel motion-compensation reference sampler** (`svq1_mc`):
  `Svq1ReferencePlane` (row-major plane view with §6.7.2 edge-
  replication clamping), `sample_halfpel` (the §6.5.1 parity-driven
  interpolator: integer-pel direct / horizontal two-tap / vertical
  two-tap / bilinear four-tap, each with the round-toward-+∞ bias),
  `motion_compensate_block` (the §6.5.2 8×8 reference patch), and
  `reconstruct_inter_l3_block` (the first end-to-end *reference plane +
  MV → reconstructed inter sub-block*, composing the MC patch as the
  §4.6.2 inter predictor of `reconstruct_leaf`). The §6.5.1 / §6.7.2
  conventions are the spec's documented de-facto baseline (binary
  confirmation deferred to a Validator round).

### SVQ3

* `SEQH` extradata + per-slice header parse (with the byte-permutation
  reversed), macroblock-type tree walk, and the Golomb-coded
  `(run, value)` residual coefficient walkers (chroma-DC, alt-scan,
  normal-scan).
* Per-block coefficient placement: the 2×2 chroma DC scan order plus
  both 4×4 scan-order arrays — the normal zigzag
  (`NORMAL_ZIGZAG_4X4_SCAN`) and the alternate scan
  (`ALT_SCAN_4X4_SCAN`), transcribed bit-exact from the
  `docs/video/svq3/spec/01` Gap 1 binary tables (`.data` offsets
  `0x7e5a8` / `0x7e5b8`), with the quantiser-driven selection rule
  (`select_4x4_scan`: alt-scan only for a luma 4×4-intra block at
  quantiser `< 24`) and the `place_4x4*` wrappers. The alt-scan's
  two-half (8 + 8) structure is asserted against the wiki cross-check.
* Dequantization arithmetic (luma / chroma-DC transform matrices,
  the per-quantiser scale table, the dequant expressions), the
  two-sided `M·X·Mᵀ` transform composition, thirdpel motion-compensation
  interpolation, and the full set of intra predictors.
* **Motion-compensation reference path (`svq3_mc`).** `ReferencePlane`
  (row-major picture-plane view with H.264 edge-replication clamping for
  unrestricted motion vectors) + `fetch_fullpel_block` (clamped
  integer-pel block copy); `split_mv_component` decomposing a stored
  sixths-grid MV component into integer-pel + sub-pel remainder
  (wiki §"Motion Compensation" "fraction of six"); the whole-block
  thirdpel interpolators (`interpolate_block_thirdpel_h` / `_v` / `_2d`
  composing the pinned 1-D / 2-D per-sample formulas across a block with
  `Clip1` saturation); and `predict_inter_block_fullpel`, the first
  end-to-end *MV → predicted block* path (full-pel case; sub-pel-phase
  filter selection is a deferred docs gap).
* **Intra predictors + mode binding (spec/01 Gap 3/4).** The five 4×4
  intra modes are pinned: `Svq3IntraMode` (`0=Vertical / 1=Horizontal
  / 2=DC / 3=DiagonalDownLeft / 4=DiagonalDownRight`, default DC per
  Gap 3) with the `predict_vertical_4x4` / `predict_horizontal_4x4` /
  `predict_dc_4x4` / `predict_diagonal_down_right_4x4` predictors and
  the SVQ3 diagonal-down quirk (`predict_diagonal_down_4x4`), routed by
  the `predict_intra_4x4` dispatcher (standard-H.264 DC fallback at
  edges) over an `Intra4x4Neighbours` carrier. The 16×16 luma plane
  (SVQ3's *transposed* fit, `predict_plane_16x16`), the 16×16 DC
  fallback (`predict_dc_16x16`), and the chroma "DC mode only"
  predictor (`predict_chroma_dc_8x8`, per-quadrant availability
  averaging) land from Gap 4.
* **Residual interleave per-element formula (spec/01 Gap 2).**
  `dequantize_transform_luma_block` (and its `_with_dc` separate-DC
  variant) compose the now-pinned per-element formula
  `out = (coeff·DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20`: a
  per-coefficient dequant-scale, the two-sided `M·X·Mᵀ` transform
  (`apply_luma_transform_2d`), then the fused `+0x80000 >>20` — the
  single post-transform shift (no extra H.264 `>>6`).
* **Macroblock-level reconstruction composition (`svq3_recon`).**
  `reconstruct_intra_luma_macroblock` drives the 16 luma 4×4
  sub-blocks of a `LumaMacroblock` in the wiki's documented processing
  order (`INTRA_4X4_SCAN_ORDER` / `LUMA_BLOCK_GRID_POS`), assembling
  each sub-block's neighbours from the running reconstructed plane
  (including the out-of-MB above/left rows), selecting + applying the
  predictor, and composing the residual via Gap 5's
  `Clip1(pred + residual)` writeback.
  `reconstruct_intra_luma_macroblock_from_coeffs` is the
  residual-owning end-to-end form: it takes the placed coefficient
  grids + slice quantiser and runs the Gap 2 interleave internally.
  `reconstruct_intra_16x16_luma_macroblock_from_coeffs` is the
  16×16-intra counterpart (`Svq3Luma16x16Mode::{Plane, Dc}` — one
  macroblock-wide predictor, residuals added in raster order, no
  per-sub-block mode sequencing). `reconstruct_intra_chroma_plane_from_coeffs`
  reconstructs an 8×8 `ChromaPlane`: DC-only prediction (Gap 4), the
  separate 2×2 chroma-DC block Hadamard + chroma-dequant into
  pre-finalisation per-quadrant DC terms, and the chroma-AC interleave
  with each quadrant's DC folded into the fused `+ dc + 0x80000 >>20`
  store. `reconstruct_intra_macroblock` ties all three planes together:
  an `Svq3IntraMacroblock` (luma + Cb + Cr) reconstructed in one call,
  dispatching the luma regime on `Svq3LumaIntra::{Blocks4x4, Whole16x16}`
  — the per-macroblock assembly unit a frame walk emits.
* The predicted+residual writeback composition (`reconstruct_sample` /
  `reconstruct_4x4`): the 8-bit saturating `Clip1(pred + residual)` sum
  with no extra rounding on the add, pinned by spec/01 Gap 5.
* **Signed-Golomb entropy layer + inter-macroblock motion header
  (`svq3_mv`).** The signed variable-length codes the wiki §"Inter
  macroblock information decoding" names: `read_se_golomb` (signed
  Exp-Golomb `se(v)`, the canonical signed pairing of the existing
  `read_ue_golomb`), `read_mv_difference` (one MV difference, **Y
  component first** then X, into `MotionVectorDifference`),
  `read_mb_mv_differences` (the exact per-partition count from
  `Svq3MbType::num_motion_vectors`), and `read_quantiser_delta`. The
  `read_inter_macroblock_header` composer joins the frame-type aware
  precision selector (`read_inter_mv_precision`) and the MV-difference
  list into an `Svq3InterMacroblockHeader` — the first end-to-end
  parse of the SVQ3 inter-MB header from raw slice bits.

* **Intra-4×4 prediction-mode VLC wire decode + per-MB mode driver
  (`svq3_mb`).** `read_intra_4x4_pred_pair` reads one unsigned
  exp-Golomb `ue(v)` codeword (the wiki §"Intra macroblock information
  decoding" pairs are listed in a contiguous `0..=24` enumeration and
  §"Decoding Process" states the codec "extensively uses Golomb
  coding", so the code-number indexes `INTRA_PRED_PAIRS` directly — the
  same convention `read_mb_type` uses). `INTRA_4X4_PRED_BLOCK_PAIRS`
  groups the 16 sub-blocks into the eight `(first, second)` index pairs
  the wiki picture parenthesises (one codeword per pair).
  `decode_intra_4x4_modes` is the per-macroblock driver: for each pair
  it reads one codeword then resolves both blocks' modes via the
  `INTRA_PRED_TABLE` lookup against each block's own running top/left
  neighbour modes (in-MB 4×4 neighbours as `Mode4x4`, out-of-MB edges
  per the wiki's "-1 when outside slice" / "value 2 for 16×16-intra or
  inter" rules), returning an `Intra4x4ModeGrid`.
* **Bitstream-driven intra-4×4 luma reconstruction (`svq3_recon`).**
  `decode_and_reconstruct_intra_luma_macroblock` composes the mode VLC
  decode with the residual interleave + predictor + writeback loop into
  the first end-to-end *slice bits → reconstructed 16×16 luma plane*
  path for a 4×4-intra macroblock (modes read from the wire, no longer
  caller-supplied). `intra_modes_from_grid` bridges the decoded grid to
  the `Svq3IntraMode` array.
* **Intra-luma DC scale residual path (`svq3_dequant` / `svq3_recon`).**
  `dequantize_transform_intra_luma_block` applies the wiki's intra-luma
  DC handling (`dc = 13·13·1538·block[0]` as the post-transform additive
  override, the inline DC coefficient zeroed out of the AC dequant) and
  `reconstruct_intra_luma_macroblock_from_coeffs_intra_dc` drives it per
  macroblock — the correct DC path for the inline-DC 4×4-intra MB types
  (`1..=24`).
* **Macroblock-grid geometry (`svq3`).** `mb_grid_dims` /
  `Svq3MacroblockPosition` / `macroblock_position` give the raster
  column/row + intra above/left neighbour availability the frame walk
  threads into the per-MB intra decode.

The remaining SVQ3 gap toward a decoded intra frame is now the
**CBP coded-block-pattern read**, which the wiki defers wholesale to
H.264 ("CBP is coded the same way as in H.264"). Its codeword↔value
mapping is the H.264 `me(v)` mapped-Exp-Golomb table (ITU-T Table 9-4,
intra/inter × chroma-format), which is **not reproduced** under
`docs/video/svq3/`, so the CBP wire decode — and therefore which
residual blocks are present, hence how to parse the per-MB coefficient
stream — stays gated on a docs trace. The **separate-DC luma block**
branch (MB types `0` / `25`, "luma DCs coded in a separate 4×4 block")
likewise needs the separate luma-DC block transform + distribution,
also unpinned under `docs/video/svq3/`. With the intra-mode VLC, the
inline-DC intra-luma residual path, and the per-MB grid geometry now
landed, the only thing between here and a decoded intra frame is the
slice-level frame walk — and the one CBP `me(v)` trace that walk needs
to know which residual blocks each macroblock carries.

## Cargo features

Default (`registry`) installs both codecs into the framework registry
and pulls in `oxideav-core`. Disable default features for the
standalone parser surface (`parse_frame_header` / `Svq1FrameHeader` /
`BitReader` plus the `svq3*` parse + arithmetic modules) without the
framework dependency.

```toml
[dependencies]
oxideav-svq = "0.1"
# standalone:
# oxideav-svq = { version = "0.1", default-features = false }
```

## License

MIT — see [LICENSE](./LICENSE).
