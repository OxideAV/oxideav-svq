# oxideav-svq

[![CI](https://github.com/OxideAV/oxideav-svq/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-svq/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-svq.svg)](https://crates.io/crates/oxideav-svq) [![docs.rs](https://docs.rs/oxideav-svq/badge.svg)](https://docs.rs/oxideav-svq) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.
Implemented from the clean-room specifications staged under
[`docs/video/svq1/`](../../docs/video/svq1/) and
[`docs/video/svq3/`](../../docs/video/svq3/).

## Status

**SVQ1: decoder COMPLETE for the I/P forward path — byte-exact
against TWO independent oracle families (the black-box reference
binary and the docs #197 `inter-4mv` fixture's independent decode
oracle, whose 348-MB INTER_4MV wire census our decode reproduces) —
AND a full I/P/B encoder (adaptive λ-tree, MV search, INTER_4MV,
droppable frames).**
`receive_frame` returns real frames; `make_encoder` produces streams
the reference decoder reproduces sample-exact. SVQ3 remains parse +
reconstruction-composition infrastructure gated on the CBP `me(v)`
docs trace (see the SVQ3 section below).

### SVQ1

Full frame decode, validated byte-exact (every Y/U/V sample) against
a reference encoder binary's own decode, used strictly as a black box
— across a 176×144 I-frame, a P-frame, a six-frame I+5P chain (each P
predicting from OUR previous reconstruction), and a 160×120 I+2P
chain exercising overhang macroblocks:

* **Frame header** (`parse_frame_header`): frame code, temporal
  reference, picture type, I-frame trailer chain (checksum, embedded
  string, frame-size code / explicit dimensions).
* **Wire VLC layer** (`svq1_vlc`): all sixteen staged tables (T00
  inter mean s9, T01 intra mean u8, T02 MV component, T03 MB mode,
  T04..T15 per-(level, half) stage counts) as verified prefix-code
  decoders — construction proves prefix-freedom; Kraft sums match the
  audit (15 complete, T02 at 8187/8192); stage count `N = position −
  1`.
* **Codebook** (`svq1_codebook`): the canonical 23 040-byte region at
  functional base `0x5d214..0x62c14` (block-shape-LUT dual-use front
  16 bytes + the staged 23 004-byte payload + the 20-byte tail
  `tables/codebook-tail.csv` — byte-identical with the docs
  Extractor backfill, docs `717a248`). Page layout pinned in the
  Validator role — level-major DESCENDING (L=3 → L=0), intra half
  then inter half per level (`half_byte_offset_in_payload`; neither
  §14.8 working hypothesis) — and the L=2 / L=3 vector byte→sample
  order is hierarchical 4×4 tiles (`vector_byte_to_raster`), not
  whole-block raster.
* **Plane decode** (`svq1_plane`): per-plane MB raster scan,
  breadth-first L=5→L=0 block-tree walk (§3.4 halving geometry,
  MB-padded canvases, overhang decode-and-discard), per-leaf
  stage-count / mean / 4-bit-index reads, wide-accumulator stage
  summation with a single final clamp, mean-only leaves at any level
  (incl. L=5/L=4 — the wiki gate fires on the stage count).
* **Inter path**: T03 MB-mode dispatch (permutation pinned: position
  3 = SKIP on the 1-bit code, 0 = INTER, 1 = INTER_4MV, 2 = INTRA),
  T02 MV components as single signed codewords (spec/06 §6.2.3
  Reading B, `position − 32`), §6.4 median predictor + §6.6 clip +
  §6.8 per-plane MV cache, §6.5 half-pel MC with `(a+b+1)>>1`
  rounding, SKIP copy / INTER / INTER_4MV / INTRA macroblocks, and
  `decode_frame` (I/P/B against an optional reference; B frames
  never become the reference). The §6.7/§6.7.4 (#174) edge question
  is arbitrated for real third-party streams by the
  **reference-window MV clamp** (`clamp_mv_to_reference_window`,
  r391): the MC read clamps each MV component (half-pel domain) so
  the block footprint stays inside the PADDED reference canvas,
  while the §6.8 cache keeps the unclamped vector — pinned uniquely
  by the 25-frame independent-encoder fixture (rival readings — bare
  edge replication, visible-window clamp, clamped cache stores —
  each diverge on it). The chroma planes force the padded window,
  proving §4.7.3 overhang samples are decoded, stored, and read as
  reference data.
* **Mode census** (`decode_frame_with_stats` /
  `decode_inter_plane_with_stats`): exact per-plane
  SKIP/INTER/INTER_4MV/INTRA counts from the wire — the observable
  that requires a full decoder (no per-MB resync exists). Two streams
  are CI-pinned against their independent mode censuses. The
  *retracted* 25-frame #161 stream carries **zero** 4MV MBs, refuting
  its original INTER_4MV-presence claim. The genuine #197 stream
  (`tests/svq1_genuine_4mv_conformance.rs`) carries **348** INTER_4MV
  MBs — every P-frame's luma grid decodes fully INTER_4MV (99/99),
  with the lone non-4MV chroma MB per frame decoding INTRA — in
  macroblock-for-macroblock agreement with that fixture's independent
  wire census. A mode misread cannot hide either way: it
  desynchronises the T03/MV bit stream while the decode stays
  byte-exact.
* **Framework integration** (`registry`): `receive_frame` decodes
  against the held reference and returns a `Yuv420P`
  `oxideav_core::VideoFrame` (native 4:1:0 chroma nearest-neighbour
  bridged; the native planes stay reachable through `svq1_plane`).
* **Robustness**: every-byte truncation, pseudo-random soup, and
  bit-flip sweeps error cleanly.

A **full I/P/B encoder** is implemented and black-box
cross-validated — every stream shape below decodes byte-identical
between our decoder and the reference decoder binary:

* **Leaf search** (`svq1_enc_leaf`): the spec/04 §4.5 stage
  accumulation run as the inverse — rounded residual mean (intra
  `[0,255]` / inter `[-256,+255]`) + greedy ascending-stage descent
  committing each stage's SSE-best vector while it strictly improves
  (up to all six stages), modelling the decoder's wide-accumulation
  arithmetic exactly, with exact wire-bit accounting and the
  inter-only leaf SKIP.
* **Adaptive block tree** (`svq1_enc_tree`): per-macroblock λ-cost
  subdivision over the full L=5..L=0 hierarchy (`SSE + λ·bits`),
  serialised in the decoder's breadth-first per-level queue order.
  `Svq1EncoderMode::Adaptive { lambda }` spans 8466 → 860 bytes on
  the same 176×144 frame (λ 0 → 2048); the bring-up modes
  (`MeanOnlyL5` / `MeanOnlyL3` / `MeanPlusOneStageL3` /
  `MultiStageL3`) remain.
* **P-frames** (`svq1_enc_inter`): per-MB SKIP / INTER / INTER_4MV /
  INTRA λ-cost mode decision; two-phase motion search (full-pel SAD
  around the median predictor + half-pel refine) with differentials
  as signed T02 codewords; INTER_4MV's four serial per-8×8 searches
  against a trial MV cache (`Svq1MvCache::store_subblock`); the
  encoder-side cache mirrors the decoder's §6.8.1 store rules.
  Motion candidates are confined to a **visible-reference window**
  (every visible output reads only visible reference samples) —
  black-box probing showed decoders genuinely diverge on the
  spec/06 §6.7 edge extension and spec/04 §4.7.3 overhang storage,
  both implementation-defined (r391 pinned the second-oracle-family
  law — the padded-window MV clamp above — but the reference binary's
  law is still unpinned, so the encoder keeps the portable window).
  Validated on I+3P chains at 176×144 and the 160×120 overhang
  geometry.
* **INTER_4MV fixture**: the committed quadrant-motion chain
  (byte-identical to the docs #197 `inter-4mv` fixture) is the 4MV
  stream wire-validated in BOTH directions — encoder determinism
  (`tests/svq1_enc_inter_conformance.rs`, ~5× smaller than the
  single-MV encode of the same content) and byte-exact decode against
  an independent black-box oracle whose 348-MB INTER_4MV census our
  decode reproduces (`tests/svq1_genuine_4mv_conformance.rs`). No
  reference encoder binary emits the mode; the earlier retracted #161
  `inter-4mv` fixture carried none (see the census section above), so
  #197 is the first stream to genuinely exercise the decode-side 4MV
  path on real wire data.
* **Droppable (B) frames**: `Svq1InterParams::droppable` emits
  picture type 2; an I+B+P chain whose P predicts from the I
  decodes byte-exact — conforming decoders keep B frames out of the
  reference chain.
* **Registry `Encoder`** (`make_encoder` / `make_encoder_handle` /
  `Svq1EncoderHandle`): `Yuv420P` in, 4:1:0 decimation (exact
  inverse of the decode bridge), keyframe cadence + λ knobs,
  keyframe-flagged packets; registry-level encode→decode round trip
  is CI-pinned.
* **Rate control** (`set_target_frame_bytes`): per-frame byte budget
  met by a deterministic warm-started doubling + bisection over λ
  (smallest λ that fits = highest fidelity in budget; generous
  budgets converge byte-identical to λ = 0; unachievable budgets
  emit best-effort at the λ ceiling) — CI-pinned through the
  registry decoder round trip.
* **Droppable cadence** (`set_droppable_period`): `I B P B P …` GOPs
  from the registry handle; B packets carry picture type 2, predict
  from the last non-droppable frame, and are reference-transparent
  (decoding with the B packets discarded leaves every other frame
  byte-identical — CI-pinned at the registry level).

Remaining SVQ1 tails: the frame-tail checksum polynomial and
embedded-string XOR table (locations still unpinned in the docs
staging); and a native `Yuv410P` pixel format once `oxideav-core`
grows one.

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
* **Picture-plane assembly + intra frame-walk (`svq3_picture`).** The
  full-frame canvas the per-macroblock reconstruction units write into.
  `Svq3Picture` holds three row-major sample planes (luma 16×16/MB,
  chroma 8×8/MB — the wiki §"Macroblock layer" 4:2:0 relationship) sized
  to the macroblock grid. `bind_luma_neighbours` / `bind_chroma_neighbours`
  populate a per-MB carrier's `above` / `leftcol` / `corner` +
  availability from the already-reconstructed canvas pixels at a
  macroblock raster position (raster decode order guarantees the above
  row + left column are reconstructed before the MB is reached);
  `blit_luma` / `blit_chroma` copy a reconstructed carrier's samples back
  into the canvas. `reconstruct_intra_macroblock_into` is the
  picture-aware per-MB step a frame walk emits (bind → the
  spec/01 Gap 2-5 `reconstruct_intra_macroblock` → blit), and
  `reconstruct_intra_frame` is the whole-picture intra frame-walk
  skeleton: it walks every macroblock in raster order (driving one
  `Svq3IntraMacroblockInput` per MB) and assembles the entire intra
  picture with correct cross-macroblock prediction. `to_video_frame`
  bridges the reconstructed canvas to an `oxideav_core::VideoFrame`
  (Yuv420P, Y full-res + Cb/Cr half-res, registry-gated), and
  `luma_reference` / `chroma_reference` expose the canvas as
  `svq3_mc::ReferencePlane` views so a reconstructed frame can serve as
  the reference plane for a subsequent inter-predicted frame. This whole
  layer is wire-format-independent — it threads pixels using only the MB
  raster ordering + the 4:2:0 subsample, both wiki-pinned — and so is
  independent of the CBP / separate-DC docs gaps below (those govern
  *which* residual blocks a macroblock carries, not where a reconstructed
  macroblock lands or how the picture is assembled / output).

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
inline-DC intra-luma residual path, the per-MB grid geometry, the
whole-picture intra frame-walk skeleton (`svq3_picture`), and the
`VideoFrame` output bridge now landed, the *only* thing between here and
a decoded intra frame is the CBP `me(v)` wire decode that resolves which
residual blocks each macroblock carries — everything downstream of that
decode (per-MB reconstruction composition, cross-MB intra prediction,
picture assembly, frame output) is implemented and tested.

## Fuzzing

`fuzz/` is a seven-target libFuzzer harness (nightly + `cargo fuzz`;
CI type-checks it so it cannot rot, runs stay local and bounded):

* **SVQ1** — `svq1_frame_header` (header parse), `svq1_decode_intra`
  (whole-frame decode, structural canvas invariants),
  `svq1_decode_inter` (untrusted P/B bytes against a held reference:
  the committed 176×144 fixture or a synthesised 160×120 overhang
  geometry), and `svq1_enc_roundtrip` — the differential invariant:
  fuzz-derived content/dimensions/mode/knobs are encoded, our decoder
  must accept the stream, and the decoded P-frame must be
  byte-identical to the encoder's own `reconstruction`.
* **SVQ3** — `svq3_extradata` (SEQH walk), `svq3_slice` (envelope
  prefix/size/unpermute + header walk), and `svq3_mb_layer` (MB-type
  walk, intra-4×4 mode VLC, the three Golomb coefficient walkers,
  inter-MB motion header, and bits→reconstruction with
  hostile-magnitude placed coefficients).

Seed the SVQ1 targets from `tests/fixtures/` and run bounded, e.g.:

```sh
cargo fuzz run svq1_enc_roundtrip -- -max_total_time=240 -rss_limit_mb=3000
```

The harness has already paid for itself: it found (and the same
round fixes) a `u32` wrap in the chroma-DC Golomb extension at
near-maximum codes, and `i32` overflow through the entire
dequant/transform pipeline at hostile wire-reachable coefficient
magnitudes — both now widened/saturating with regression tests, with
conforming-domain arithmetic bit-identical.

## Cargo features

Default (`registry`) installs both codecs into the framework registry
and pulls in `oxideav-core`. Disable default features for the
standalone surface — the full SVQ1 frame decoder
(`svq1_plane::decode_frame` returning native YUV 4:1:0 planes,
`parse_frame_header`, the `svq1_*` table/VLC modules) plus the
`svq3*` parse + arithmetic modules — without the framework
dependency.

```toml
[dependencies]
oxideav-svq = "0.1"
# standalone:
# oxideav-svq = { version = "0.1", default-features = false }
```

## License

MIT — see [LICENSE](./LICENSE).
