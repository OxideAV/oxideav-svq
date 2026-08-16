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
the reference decoder reproduces sample-exact. SVQ3 now has the full
binary-anchored entropy + transform layers AND a slice-level intra
frame walk wired through the registry decoder: the I-frame
macroblock-type wire mapping is fixture-pinned, and **299 of the 300
macroblocks of a real Sorenson-encoded I-frame decode pixel-exactly**
against the staged black-box reference decode — the single remaining
blocker to unassisted end-to-end I-frame pixels is the first
macroblock's element sequence (see the SVQ3 section below).

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

The SVQ3 layers are built clean-room from the staged
`docs/video/svq3/` chapters (spec/01–06, tables/01–06, the wiki
snapshot). Every wire-format element and arithmetic stage below is
individually spec-anchored and unit-tested.

* **Container + slice framing** (`svq3`): `SEQH` extradata (spec/02),
  the permuted slice envelope + per-slice header (frame code, version,
  quantiser, delta flag), and the macroblock-grid geometry.
* **The universal variable-length code** (`svq3::read_universal_code`,
  spec/06 §1): SVQ3's single `2n+1`-bit code with terminator bits
  interleaved among the data bits, carrying every macroblock-layer
  element; the signed fold (`svq3_mv::read_signed_code`, §1.1) for MV
  differences + the quantiser delta. (Codes 0…2 coincide with
  exp-Golomb, so the slice frame-code alphabet is unchanged; codes ≥ 3
  differ, which the earlier reader got wrong.)
* **Macroblock types** (`svq3_mb`, spec/04 §4.5 + tables/03): the
  dispatch numbering — intra 4×4 (`Intra4x4`), intra 16×16
  (`Intra16x16(Intra16x16Params)`, factored
  `9 + pred_mode + 4·cbp_chroma + 12·luma_ac`), the separate-DC /
  no-other-blocks type (`SeparateDcOnly`), and the inter modes — with
  the **I-frame wire mapping fixture-pinned** (r446): the staged
  320×240 fixture's uniform-black 300-macroblock sync frame tiles as
  299 identical 14-bit units `[type 0][eight pair codes][CBP code 3 →
  pattern 0]` ending bit-exactly at the slice boundary, so I wire 0 =
  intra 4×4 (dispatch 33), 1…24 = the 24 intra-16×16 records
  (dispatch +8), 25 = the dispatch-8 separate-DC type by elimination.
  Plus the intra-4×4 prediction-mode pair VLC + `INTRA_PRED_TABLE`
  context resolution (now with cross-macroblock mode threading,
  `decode_intra_4x4_modes_with_context`) and the MV-precision
  selector (spec/05 §2).
* **Coded-block-pattern** (`svq3_cbp`, spec/03 + tables/01):
  `cbp_luma` (one bit per 8×8 quadrant, raster order) + the shared
  3-valued `cbp_chroma` class, decoded from one universal code number
  through the 48-entry **intra** or **inter** mapping table (decode
  direction, cross-checked against both binary components in
  tables/01's `.meta`). The intra 16×16 types carry no CBP element —
  their pattern is implied by the type.
* **Residual entropy** (`svq3_coeff`, spec/06 §2/§5 + tables/05–06):
  the three `(level, run)` code books (`normal_scan` / `alternate_scan`
  / `chroma_dc`) with their arithmetic escape constructions (run masks
  15/7/3, shifts 5/4/3, per-run bases), and the per-block decoders
  (`decode_residual_4x4_normal` with the spec/04 §4.3 scan-start
  parameter, `decode_residual_4x4_alt`'s two independent half-scans,
  `decode_chroma_dc_2x2`). Every escape (book, run) class is
  test-pinned to continue its tabulated magnitude ladder by one step;
  an over-long run is a bitstream error (§5), never a wrap.
* **The core 4×4 inverse transform** (`svq3_dequant`, spec/04 §1 +
  spec/01 Gap 2): the measured basis with the corrected third column
  `13, −13, −13, 13` (the wiki's `1, −1, −1, 1` gave the wrong norm;
  the spec's single-coefficient `2²⁰` responses are pinned as tests),
  the fused two-sided `M·X·Mᵀ` + `+dc +0x80000 >>20` store (the only
  post-transform shift), and the 32-entry dequant ladder.
* **Both secondary transforms** (`svq3_dequant`, spec/04 §2/§4): the
  **chroma DC** 2×2 Hadamard halved with truncation toward zero
  (`chroma_dc_secondary_transform` / `dequantize_chroma_dc_levels`,
  measured `(−3,0,0,0) → four −1s`) scattered as the additive
  `169·B_k`, and the **luma DC** transform (`luma_dc_secondary_transform`)
  — the core transform scaled by the verbatim `1538` — for the intra
  16×16 separate-DC block. Chroma blocks index the ladder through the
  chroma quantiser remap (§3, tables/02).
* **Intra predictors** (`svq3_pred`, spec/01 Gap 3/4): the five 4×4
  modes (incl. the SVQ3 diagonal-down quirk), the 16×16
  transposed-plane, DC, and standard vertical/horizontal predictors,
  and the chroma DC-only predictor; the `Clip1(pred + residual)`
  writeback (Gap 5).
* **Reconstruction composition** (`svq3_recon`, `svq3_picture`):
  per-macroblock 4×4-intra / 16×16-intra luma + chroma-plane
  reconstruction from placed coefficient grids, cross-macroblock
  neighbour binding, the intra frame-walk skeleton, and the
  `oxideav_core::VideoFrame` (Yuv420P) output bridge.
* **Motion compensation** (`svq3_mc`, spec/05): reference-plane views
  with edge-replication clamping, the third-pel / half-pel / full-pel
  interpolation kernels, and the sixths-grid MV split.
* **The intra frame walk** (`svq3_frame`, r446): the slice-level
  access-unit decoder — slice envelope walk (v1 multi-slice
  continuation, v2 macroblock offsets), per-macroblock type dispatch,
  the full intra-4×4 grammar (pair codes with cross-macroblock
  `pred_table` contexts, explicit intra CBP, optional quantiser
  delta, quadrant-ordered alt/normal residual blocks, the fixed
  intra-luma DC scale), the intra-16×16 grammar (separate luma DC
  block → secondary transform → `169·v_k` scatter, scan-start-1 AC,
  implied chroma), and the per-plane chroma section — wired into the
  registry decoder: `make_svq3_decoder` accepts the QuickTime
  `SMI `-wrapped extradata and `receive_frame` returns cropped
  `Yuv420P` frames for intra access units.

**Validated on real wire data (locally, against the staged
fixtures):** skipping the first macroblock's 48 bits of the 320×240
fixture's sync frame (its span is pinned exactly by the 299-unit tail
tiling), the walk decodes the remaining **299 macroblocks pixel-exact
on all three planes** (76 800 luma + 38 400 chroma samples, zero
mismatches) and consumes the slice to within its 2 padding bits — and
the skip length 24 codes is the *unique* value in 1…119 that reaches
the last macroblock at all. The staged chroma arithmetic is also
pinned end-to-end by the 240×128 fixture: its first Cb DC code number
1464 → level −185 through the staged chroma book → −58 through the
remap/Hadamard-halve/`169·B` pipeline → exactly the expected uniform
Cb 70 against the 128 prediction.

**Remaining blocker — the leading-macroblock element sequence.**
Both staged fixtures' I-frames open with a first macroblock whose
element list the staged grammars cannot account for: in the black
320×240 frame it consumes 24 universal codes
(`[4][0×7][1,0,1,1,3,1,15][0×9]`) where a spec'd empty 16×16 consumes
2, every nonzero code is provably non-coefficient (the frame is
uniformly black), and both fixtures share the 12-bit `[4][0×7]`
prefix. Content-bearing macroblocks later in the real streams also
carry elements the staged chapters don't pin (the walk desyncs inside
them under every element-order variant tried). Needed from the docs
side: the decoder's I-frame macroblock-loop element order read from
the staged decompressor — the wire→dispatch type adjustment per slice
type, every element between mb_type and the residual lists for each
intra family (including any leading/first-macroblock-only elements),
the cross-plane chroma stream interleaving, the 16×16
`intra16x16_pred_mode` → predictor binding, and the luma DC
predictor's no-neighbour fallback (the black frame pins it near 0,
not the H.264 128).

## Fuzzing

`fuzz/` is an eight-target libFuzzer harness (nightly + `cargo fuzz`;
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
  prefix/size/unpermute + header walk, both header versions swept
  directly), and `svq3_mb_layer` (MB-type walk, intra-4×4 mode VLC,
  the three residual block decoders, inter-MB motion header, and
  bits→reconstruction with hostile-magnitude placed coefficients).
* **Framework** — `registry_stream`: the `make_decoder` /
  `make_svq3_decoder` handles driven end-to-end over arbitrary
  packetisations (`send_packet` → `receive_frame` drain → `flush`),
  including the SVQ3 extradata-at-construction path.

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
