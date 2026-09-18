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

**SVQ3: I and P decode byte-exact on every access unit of the staged
real-stream fixtures (10/10, all three planes), plus the vendor
decoder's intra-picture edge filter** — `Svq3PictureDecoder` /
`make_svq3_decoder` turn a `SEQH` and access units into `Yuv420P`
frames. Not decoded: B slices and the extended macroblock-layer mode
(neither is specified by the staged docs; both are reported as
unsupported).

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

Built clean-room from `docs/video/svq3/` (spec/01–09, tables/00–09,
the fixtures). Every wire element and arithmetic stage is
spec-anchored and unit-tested; the whole is pinned end to end by the
AU-by-AU scorecard of `tests/svq3_fixture_conformance.rs` against the
staged fixtures' black-box reference decodes (`expected.yuv`, the
reconstruction *without* the spec/09 pass — the harness finds the
fixtures through `OXIDEAV_SVQ3_FIXTURES` or the umbrella docs checkout
and skips when neither is present, so it runs in the local gate, not
on CI):

| Fixture | AU | Type | Result (filter off) |
| --- | --- | --- | --- |
| `real-sample-240x128` (quantiser 0–1, 120 MBs) | 0 | I | byte-exact |
| | 1 – 5 | P | byte-exact (full-pel, chroma half-sample motion, intra MBs in P) |
| `real-sample-320x240-short-seqh` (quantiser 13, 300 MBs) | 0 | I | byte-exact |
| | 1 | P | byte-exact |
| | 2 | I | byte-exact |
| | 3 | P | byte-exact (half-pel macroblocks) |

With the edge filter on (the default) the 320×240 stream's AU2 and AU3
reproduce the vendor component's own pictures — SHA-256
`7f07e2acc656…` and `cf9ec710700a…` as recorded in spec/09 §6 — and
the remaining eight access units are unchanged by the pass.

What the decoder implements, by chapter:

* **spec/02 — `SEQH`**: the size code / explicit dimensions and the
  ten-bit flag group (half-/third-pel enables, post-filter hint,
  extended mode, the reserved constants, `no_b_frames`, the bit-9
  escape, `protected`); bare or `SMI `-wrapped (`parse_extradata_flexible`).
* **spec/06 §1 / spec/07 §1 — the universal code**: `0 d₁ 0 d₂ … 0 dₙ 1`
  (a marker before every data bit), code number `2ⁿ − 1 + value`; the
  signed fold for deltas and motion-vector differences.
* **spec/07 §2–§4 — envelope, header, loop**: packet byte
  (`L = (b >> 5) & 3`, `T = b & 0x9f`), the relocated payload bytes,
  the zero packet and the `0xff` end marker; packet-type-1 (`encrypted`)
  and type-2 (`first_mb`, width `max(6, ⌈log₂(mb_count + 1)⌉)`) headers
  with `mode` echoing the `SEQH` flag and the §3.3 extended-mode field
  sequence parsed; the macroblock loop ended by the byte-boundary rule
  or the picture's last macroblock; a per-slice 4×4-block
  availability / context map.
* **spec/07 §5–§10 — intra macroblocks**: per-slice-type type code
  numbers (I: 0 / 1…24; P: 0…7 / 8 / 9…32 / 33 flat-128; B: 0…3 / 4 /
  5…28); intra 4×4 with eight `tables/07` pair codes resolved through
  the `tables/08` neighbour contexts into the decoder's modes (0 DC,
  1 vertical, 2 horizontal, 3 diagonal-down-right, 4 averaged
  diagonal — directional modes without their neighbour are a bitstream
  error), the intra-table pattern, the P/B-only conditional delta,
  two-list alternate-scan blocks below quantiser 24; intra 16×16 with
  its always-present delta, the separate luma DC block through the
  core transform × 1538 (spec/04 §4), scan-start-1 AC and the implied
  chroma class; the flat-128 type; the chroma section as Cb DC, Cr DC,
  then the eight AC blocks; every residual list ended by code number 0
  and bounded per spec/06 §5; coefficients `level × dequant[q]` with
  the chroma remap (spec/06 §4, spec/04 §3).
* **spec/01 / spec/04 — reconstruction**: the measured 13/17/7 core
  transform with the fused `+0x80000 >> 20` store, the 2×2 chroma DC
  Hadamard halved toward zero and its `169·B_k` scatter, the
  `Clip1` writeback; intra 16×16 DC / vertical / horizontal / plane
  and the per-quadrant H.264 chroma DC rule.
* **spec/08 — P-slice inter macroblocks**: the skip type's co-located
  copy; the precision selector; one vertical-first MVD pair per
  partition (16×16, 8×16, 16×8, 8×8, 4×8, 8×4, 4×4) on the median
  predictor with the picture clamp, converted per precision and stored
  in sixths; luma and chroma motion compensation through the spec/05
  §4 kernels (full copy, half-pel bilinear, third-pel one- and
  two-dimensional); the inter-table pattern, conditional delta and
  normal-zigzag residual; uncoded tail macroblocks copied from the
  reference.
* **spec/09 — the intra-picture edge filter**: the `tables/09` limit
  per plane, vertical then horizontal sweeps over every 4-sample edge,
  `delta = trunc((4(q0 − p0) + (p1 − q1)) / 8)` clipped to `±limit`;
  applied after every I picture (`Svq3DecodeOptions::intra_edge_filter`).

**Fixture-pinned readings beyond the chapter text** (each a standing
docs ask, see below): the 16×16 plane predictor's gradients are
H.264's eight taps including the above-left corner, transposed, at
half H.264's scale (`(G + 16) >> 5`; `(5G + 48) >> 7` fits the
corpus equally — they differ only for |G| ≥ ~100); the chroma DC
predictor's off-diagonal quadrants take one neighbour (top-right the
top row, bottom-left the left column); the chroma motion vector is the
luma vector halved with truncation toward zero, half-sample phases
through the bilinear kernel (pinned for full-pel luma vectors only).

**Not decoded**: B slices (spec/07 §5 dispatch only) and the extended
macroblock-layer mode (header fields parsed, per-macroblock `u(3)`
unspecified) — `Error::NotImplemented` / `Unsupported`; the
protected-stream payload; the one-access-unit output delay the vendor
decoder applies to streams with `no_b_frames = 0` (pictures are
returned as decoded, which is the fixtures' display order).

## Fuzzing

`fuzz/` is an eleven-target libFuzzer harness (nightly + `cargo fuzz`;
CI type-checks it so it cannot rot, runs stay local and bounded):

* **SVQ1** — `svq1_frame_header` (header parse), `svq1_decode_intra`
  (whole-frame decode, structural canvas invariants),
  `svq1_decode_inter` (untrusted P/B bytes against a held reference:
  the committed 176×144 fixture or a synthesised 160×120 overhang
  geometry), and `svq1_enc_roundtrip` — the differential invariant:
  fuzz-derived content/dimensions/mode/knobs are encoded, our decoder
  must accept the stream, and the decoded P-frame must be
  byte-identical to the encoder's own `reconstruction`.
* **SVQ3** — `svq3_extradata` (SEQH walk), `svq3_slice` (envelope,
  unpermute, both header types, the extended-mode field sequence),
  `svq3_mb_layer` (type walk, pair codes, residual decoders, inter
  motion header, bits→reconstruction with hostile coefficients),
  `svq3_intra_frame` (intra access units at fuzz-derived geometries),
  `svq3_access_units` (I + P chains through `Svq3PictureDecoder`,
  filter on and off, every partition shape and sub-pel phase), and
  `svq3_filter_mc` (the edge filter and motion compensation over
  arbitrary planes and vectors).
* **Framework** — `registry_stream`: the `make_decoder` /
  `make_svq3_decoder` handles driven end-to-end over arbitrary
  packetisations (`send_packet` → `receive_frame` drain → `flush`),
  including the SVQ3 extradata-at-construction path.

Seed the SVQ1 targets from `tests/fixtures/` and run bounded, e.g.:

```sh
cargo fuzz run svq1_enc_roundtrip -- -max_total_time=240 -rss_limit_mb=3000
```

Findings so far, each fixed in the round that found it with a
regression test: a `u32` wrap in the chroma-DC escape at near-maximum
codes; `i32` overflow through the dequant/transform pipeline at
hostile coefficient magnitudes; motion-vector differences at the i32
limits overflowing the predictor add / stored multiply (now
saturating, with clamp and motion-compensation origins in i64); the
edge filter reading one sample past a plane sized 1 mod 4.

## Cargo features

Default (`registry`) installs both codecs into the framework registry
and pulls in `oxideav-core`. Disable default features for the
standalone surface — the full SVQ1 frame decoder
(`svq1_plane::decode_frame` returning native YUV 4:1:0 planes,
`parse_frame_header`, the `svq1_*` table/VLC modules) plus the
complete SVQ3 decoder (`Svq3PictureDecoder` over `SEQH` + access
units, returning `Svq3Picture` canvases) — without the framework
dependency.

```toml
[dependencies]
oxideav-svq = "0.1"
# standalone:
# oxideav-svq = { version = "0.1", default-features = false }
```

## License

MIT — see [LICENSE](./LICENSE).
