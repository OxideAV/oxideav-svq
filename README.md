# oxideav-svq

Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec scaffold for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.
Implemented from the clean-room specifications staged under
[`docs/video/svq1/`](../../docs/video/svq1/) and
[`docs/video/svq3/`](../../docs/video/svq3/).

## Status

**Structural scaffold — no pixel output yet.** Both decoders parse
their bitstreams into typed structures and stage the required tables,
but `receive_frame` returns `oxideav_core::Error::Unsupported` until
the full reconstruction pipelines are wired. What is implemented:

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
final-vector clip (§6.6), per-plane cache (§6.8) and INTER /
INTER_4MV neighbour-selection geometry (§6.4.3 / §6.4.4) wired; only
the T02 differential wire decode and the half-pel reference sampling
(§6.5, de-facto only) remain. Until the deferred field decodes land,
full plane reconstruction is blocked on bitstream-driven field decode.

### SVQ3

* `SEQH` extradata + per-slice header parse (with the byte-permutation
  reversed), macroblock-type tree walk, and the Golomb-coded
  `(run, value)` residual coefficient walkers (chroma-DC, alt-scan,
  normal-scan).
* Per-block coefficient placement (2×2 chroma DC scan order),
  dequantization arithmetic (luma / chroma-DC transform matrices,
  the per-quantiser scale table, the dequant expressions), the
  two-sided `M·X·Mᵀ` transform composition, thirdpel motion-compensation
  interpolation, and the 4×4 diagonal-down intra predictor.

The remaining SVQ3 gaps — the 4×4 scan-order arrays, the
mode-to-predictor binding, the other intra predictors, the
motion-vector-component VLC, CBP coding, and the IDCT writeback — await
pinning in `docs/video/svq3/`.

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
