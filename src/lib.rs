//! Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec.
//!
//! **Round 282 — SVQ3 4×4 diagonal-down intra predictor.**
//!
//! Round 282 lands the one intra predictor the wiki spec pins
//! completely in `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra
//! prediction" as a new [`svq3_pred`] module: the 4×4 diagonal-down
//! quirk, whose fill picture (`a b c c / b c c c / c c c c / c c c c`)
//! and three closed-form samples (`a = (left[1] + top[1]) / 2`,
//! `b = (left[2] + top[2]) / 2`, `c = (left[3] + top[3]) / 2`) are
//! spelled out verbatim in the local mirror. The per-pair average
//! lands as [`svq3_pred::diagonal_down_sample`], the fill picture as
//! [`svq3_pred::DIAGONAL_DOWN_PATTERN`], and the combined block
//! predictor as [`svq3_pred::predict_diagonal_down_4x4`] (row-major
//! `[u8; 16]` output matching the [`svq3_scan`] / [`svq3_dequant`]
//! block layout, so the eventual predicted+residual writeback can
//! combine the two element-wise). The numeric prediction-mode binding
//! for this predictor, the remaining H.264-back-referenced predictors,
//! and the writeback clamp are NOT pinned in `docs/video/svq3/` and
//! stay deferred. Total tests: 402 lib + 7 integration + 6 doc = 415
//! (up from 387 + 7 + 4 = 398).
//!
//! ## Earlier rounds (carried forward)
//!
//! **Round 262 — SVQ3 2×2 chroma DC transform application helper.**
//!
//! Round 262 lands the per-block application of the 2×2 chroma DC
//! transform matrix the wiki spec pins in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform
//! and dequantization" as `[[8, 8], [8, -8]]` (also exposed verbatim as
//! [`svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX`]). Two new `const fn`
//! helpers surface the transform's two natural application forms:
//! [`svq3_dequant::apply_chroma_dc_transform_row`] carries out one
//! matrix-row dot product against a 2-point column (row 0 produces
//! `8 * (a + b)`, row 1 produces `8 * (a - b)`), and
//! [`svq3_dequant::apply_chroma_dc_2x2_columns`] applies the full
//! matrix against the columns of a row-major 2×2 input block (the
//! `M · X` single-sided pass). The input layout matches
//! [`svq3_scan::place_chroma_dc_2x2`]'s row-major output so the
//! transformed result feeds directly into the subsequent
//! [`svq3_dequant::dequantize_chroma_dc`] per-sample dequant step. The
//! full two-sided `M · X · M^T` transform that the wiki spec does NOT
//! spell out explicitly is deliberately NOT folded in here — that
//! derivation belongs in a future round once docs pin it. Total tests:
//! 377 lib + 7 integration + 2 doc = 386 (up from 359 + 7 = 366).
//!
//! ## Earlier rounds (carried forward)
//!
//! **Round 245 — SVQ3 alt-scan two-half block walker.**
//!
//! Round 245 lands the typed two-half walker for SVQ3's alternative-scan
//! coefficient block as a new [`svq3_coeff::AltScanBlock`] carrier plus
//! a [`svq3_coeff::read_alt_scan_block`] entry point. The wiki spec
//! (`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient decoding")
//! pins the alt-scan path as "coded in two parts of up two eight
//! coefficients corresponding to each half-scan": each half is an
//! independent run of up to [`svq3_coeff::COEFFS_PER_ALT_SCAN_HALF`] =
//! `8` coefficients, terminated by either an explicit end-of-block
//! sentinel (`code == 0`) or by reaching the half's capacity, and the
//! two halves keep their run-accumulator cursors strictly independent.
//! The carrier exposes the two halves as separate `Vec<Coefficient>`
//! fields ([`svq3_coeff::AltScanBlock::first_half`] /
//! [`svq3_coeff::AltScanBlock::second_half`]) for the future placement
//! step that pins the 4×4 scan-order arrays, plus three convenience
//! accessors: [`svq3_coeff::AltScanBlock::len`] returning the cumulative
//! coefficient count bounded by [`svq3_coeff::COEFFS_PER_4X4_BLOCK`] =
//! `16`, [`svq3_coeff::AltScanBlock::iter_with_half`] yielding
//! `(half_index, Coefficient)` pairs in stream order, and
//! [`svq3_coeff::AltScanBlock::flatten`] producing a single
//! `Vec<Coefficient>` for callers that have already absorbed the
//! half-scan boundary. The walker itself just sequences two
//! [`svq3_coeff::read_alt_scan_half`] calls back-to-back, so the
//! per-half termination + per-half capacity guarantees the half walker
//! already carries are preserved verbatim. Round 245 lands the
//! two-half structure only — the per-half 8-position scan-order array
//! the wiki spec depicts in §"Macroblock layer" remains deferred per
//! round 233's ambiguity note. Total tests: 359 lib + 7 integration =
//! 366 (up from 346 + 7 = 353).
//!
//! ## Earlier rounds (carried forward)
//!
//! **Round 242 — SVQ1 per-stage codebook-index field reader (§4.2).**
//!
//! Round 242 lands the per-stage codebook-index field reader the SVQ1
//! spec defines in `docs/video/svq1/spec/04-multistage-vq-decoder.md`
//! §4.2 as a new [`svq1_stage_indices`] module. The wiki source
//! (`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
//! Plane Data") pins the run as `4 × N` raw bits — `N` consecutive
//! 4-bit fields in stage-ascending order with no inter-stage padding
//! and no permutation. The [`svq1_stage_indices::read_stage_indices`]
//! function reads exactly that run from a [`BitReader`] and returns
//! an allocation-free [`svq1_stage_indices::IndexBuffer`] of up to
//! [`svq1_stage_indices::MAX_STAGES_PER_LEAF`] = `6` entries. The
//! per-stage width [`svq1_stage_indices::BITS_PER_INDEX`] = `4`, the
//! maximum index [`svq1_stage_indices::MAX_VEC_IDX`] = `15`, and the
//! closed-form bit-count helper
//! [`svq1_stage_indices::bits_for_n_stages`] (returning `4 × N` for
//! `N ∈ 0..=6` and `None` otherwise) surface the spec invariants for
//! callers that want to range-check or budget bits up front. The
//! reader honours the `N = 0` mean-only leaf path (spec/04 §4.5.4)
//! by returning [`svq1_stage_indices::IndexBuffer::EMPTY`] without
//! consuming any bits; `N > 6` is rejected with
//! [`Error::BadBitWidth`] (the stage-count VLC alphabet excludes that
//! case per spec/04 §4.1.1); mid-run end-of-stream surfaces as
//! [`Error::Truncated`]. Round 242 is the bitstream-side index reader
//! alone — the §4.3 codebook lookup
//! (`(level, half, stage, vec_idx) → V_L bytes`) still depends on
//! the L=0..L=3 payload's intra-vs-inter / stage-vs-level interleave
//! pinned in `docs/video/svq1/`. Total tests: 346 lib + 7 integration
//! = 353 (up from 326).
//!
//! ## Earlier rounds (carried forward)
//!
//! **Round 239 — SVQ1 mean-step saturating arithmetic.**
//!
//! Round 239 lands the per-sample mean-step apply arithmetic
//! documented in `docs/video/svq1/spec/05-mean-removal.md` §5.4 as a
//! new [`svq1_mean`] module. The two halves of the SVQ1 mean
//! family — intra (`u8 ∈ [0, 255]` per §5.1.1) and inter (`s9 ∈
//! [-256, +255]` per §5.1.2 / §5.1.3) — are exposed as the `const
//! fn` helpers [`svq1_mean::apply_intra_mean_step`] and
//! [`svq1_mean::apply_inter_mean_step`], with the underlying
//! [`svq1_mean::saturate_u8`] clamp matching the wiki spec's
//! "saturate to an unsigned byte range, 0..255" mandate (§5.4.3).
//! The companion [`svq1_mean::samples_per_leaf`] helper returns the
//! `V_L` replication count per spec/03 §3.3 (`8 / 16 / 32 / 64` for
//! L=0..L=3; `None` for L=4 / L=5 since those levels do not host a
//! mean-removed VQ leaf per §14.10 / §14.11). The range constants
//! [`svq1_mean::INTRA_MEAN_MIN`] / [`svq1_mean::INTRA_MEAN_MAX`] /
//! [`svq1_mean::INTER_MEAN_MIN`] / [`svq1_mean::INTER_MEAN_MAX`]
//! pin the two-table value-range distinction at the type level. The
//! [`svq1_mean::MeanError`] enum flags out-of-range inter mean
//! values. Round 239 is pure arithmetic — no bitstream reads — the
//! future mean-VLC wire-up round will read the VLC tables at file
//! offsets `0x5cb0c..0x5cf14` (intra alphabet 256) and
//! `0x5c304..0x5cb0c` (inter alphabet 512, `min_value = -256`) per
//! spec/05 §5.7 and feed the decoded mean into these helpers.
//!
//! ## Earlier rounds (carried forward)
//!
//! **Round 233 — SVQ3 per-block coefficient placement (scan-order
//! infrastructure).**
//!
//! Round 233 lands the per-block placement step that connects the
//! Golomb-decoded `(run, value)` coefficient stream from
//! [`svq3_coeff`] to the 2D block matrix consumed by the
//! dequantization arithmetic in [`svq3_dequant`]. The wiki spec's
//! §"Coefficient decoding" describes coefficients being stored in
//! 4×4 (sub)blocks "except for chroma DCs which are stored in 2×2
//! blocks"; round 233 surfaces the **unambiguous 2×2 chroma DC
//! row-major scan** ([`svq3_scan::CHROMA_DC_2X2_SCAN`]) and a generic
//! [`svq3_scan::place_coefficients_in_scan_order`] helper that walks
//! a `(run, value)` stream into a fixed-size flat array per a
//! scan-order table. The convenience wrapper
//! [`svq3_scan::place_chroma_dc_2x2`] pins the chroma DC 2×2 case for
//! direct consumption by the
//! [`svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX`] application. The 4×4
//! scan-order arrays (alt-scan picture + normal-zigzag) are
//! deliberately NOT transcribed — the wiki's §"Macroblock layer"
//! ASCII art for the dezigzag pattern is ambiguous in two ways (see
//! the module's "Open work" section) so round 233 defers the 4×4
//! scan arrays to a future docs round. Round 233 is structural
//! arithmetic only — `Svq3DecoderHandle::receive_frame` continues to
//! return `oxideav_core::Error::Unsupported` until the 4×4 dezigzag +
//! IDCT stages land.
//!
//! **Round 230 — SVQ3 macroblock transform + dequantization arithmetic.**
//!
//! Round 230 lands the per-coefficient dequantization arithmetic the
//! wiki spec defines for SVQ3 in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform
//! and dequantization" as a new [`svq3_dequant`] module. The 4×4 luma
//! transform coefficient matrix
//! ([`svq3_dequant::LUMA_TRANSFORM_MATRIX`]), the 2×2 chroma DC
//! transform matrix ([`svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX`]),
//! and the 32-entry per-quantiser scale table
//! ([`svq3_dequant::DEQUANT_COEFF_TABLE`]) land verbatim; the three
//! closed-form expressions the spec quotes — intra-luma DC,
//! chroma DC, and the general per-coefficient dequant — land as
//! the [`svq3_dequant::dequantize_intra_luma_dc`],
//! [`svq3_dequant::dequantize_chroma_dc`],
//! [`svq3_dequant::dequantize_coefficient`], and
//! [`svq3_dequant::finalise_dc`] `const fn` helpers. Round 230 is
//! structural arithmetic only — `Svq3DecoderHandle::receive_frame`
//! continues to return `oxideav_core::Error::Unsupported` until the
//! dezigzag + IDCT stages land.
//!
//! **Round 224 — SVQ3 sub-pixel thirdpel interpolation arithmetic.**
//!
//! Round 224 lands the per-sample interpolation arithmetic the wiki
//! spec defines for SVQ3 motion compensation in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation"
//! as a new [`svq3_mc`] module. The
//! [`svq3_mc::thirdpel_interpolate_1d`] helper implements the spec's
//! one-direction formula `((2 * A + B + 1) * 0x2AB) >> 11` (with the
//! `0x2AB` / `>> 11` / `+ 1` constants surfaced as
//! [`svq3_mc::THIRDPEL_1D_MULTIPLIER`] / [`svq3_mc::THIRDPEL_1D_SHIFT`]
//! / [`svq3_mc::THIRDPEL_1D_BIAS`]); the
//! [`svq3_mc::thirdpel_interpolate_2d`] helper implements the spec's
//! two-dimensional formula `((4 * A + 3 * B + 3 * C + 2 * D + 6) *
//! 0xAAB) >> 15` against the spec-quoted weight matrix
//! [`svq3_mc::THIRDPEL_2D_WEIGHTS`] = `[[4, 3], [3, 2]]` (with the
//! `0xAAB` / `>> 15` / `+ 6` constants surfaced as
//! [`svq3_mc::THIRDPEL_2D_MULTIPLIER`] / [`svq3_mc::THIRDPEL_2D_SHIFT`]
//! / [`svq3_mc::THIRDPEL_2D_BIAS`]). Both helpers are `const fn` and
//! consume the spec's exact integer arithmetic — no division and no
//! floating point.
//!
//! Round 224 also surfaces the wiki spec's "stored and predicted as
//! fraction of six and then rounded to the desired base" motion-vector
//! storage remark as a per-precision base lookup
//! ([`svq3_mc::stored_sixths_base`] returning 6 / 3 / 2 for Fullpel /
//! Halfpel / Thirdpel) plus an [`svq3_mc::is_aligned_to_precision_base`]
//! predicate. The actual rounding step is NOT implemented — the wiki
//! spec text leaves the rounding direction unspecified, so round 224
//! exposes only the alignment check, deferring the rounding helper to
//! the round that pins the direction in `docs/`.
//!
//! Round 224 is structural arithmetic only —
//! `Svq3DecoderHandle::receive_frame` continues to return
//! `oxideav_core::Error::Unsupported` until the reference-frame-fetch
//! + filter-application stage lands.
//!
//! **Round 217 — SVQ1 u16-LE parameter table (512 records) mirrored.**
//!
//! Round 217 picks up the third Extractor-02-staged helper region
//! from `docs/video/svq1/tables/`: the 1024-byte `u16` parameter
//! table at file offset `0x59d00..0x5a100` (VMA
//! `0x67dc9d00..0x67dca100`, section `.rdata`). The
//! [`svq1_helper_luts::SVQ1_U16_PARAM_TABLE`] static now holds the
//! bit-exact `u16`-LE bytes; the
//! [`svq1_helper_luts::SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES`]
//! ascending list encodes the closed allowed-value set the meta
//! enumerates; and seven new lib tests cover length-vs-meta, source-
//! region provenance, the flush-against-clip-LUT geometry
//! (`u16_end_vma == SVQ1_CLIP_LUT_VMA`), the every-value-in-allowed-
//! set closed-set invariant, the four-word zero prelude at
//! `word_index 0..4`, the first non-zero `0x0020 × 9` group head at
//! `word_index 4..13`, and a strictly-ascending-uniqueness guard over
//! `SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES` itself. The table is NOT yet
//! wired into a decode path — the future pixel-reconstruction stage
//! will lift it in unchanged.
//!
//! **Round 203 — SVQ1 saturating-clip + bit-mask helper LUTs.**
//!
//! Round 203 lands two small helper LUTs the docs collaborator's
//! Extractor 02 pass identified in the SVQ1 `.rdata` region:
//!
//! * The 768-byte saturating-clip LUT at file offset
//!   `0x5a100..0x5a400` (`docs/video/svq1/tables/clip_lut.{csv,meta}`)
//!   — used by the codec on interpolation / overflow-saturation
//!   paths; the meta is explicit that this is NOT a VQ codebook.
//! * The 16-byte bit-position / bit-mask helper LUT at file offset
//!   `0x5c1c4..0x5c1d4`
//!   (`docs/video/svq1/tables/svc_bitmask_lut.{csv,meta}`) — first 8
//!   entries are the descending single-bit masks `0x80..0x01`; last 8
//!   are their one's complements `0x7f..0xfe`.
//!
//! Both CSVs are mirrored bit-exact under `crates/oxideav-svq/tables/`
//! with SHA-256s matching `docs/video/svq1/tables/MANIFEST-02.sha256`.
//! The new [`svq1_helper_luts`] module exposes the constants
//! ([`svq1_helper_luts::SVQ1_CLIP_LUT`],
//! [`svq1_helper_luts::SVQ1_BITMASK_LUT`]), borrowed accessors
//! ([`svq1_helper_luts::clip_lut`],
//! [`svq1_helper_luts::bitmask_lut`]), and the source-region offsets
//! ([`svq1_helper_luts::SVQ1_CLIP_LUT_FILE_OFFSET`],
//! [`svq1_helper_luts::SVQ1_CLIP_LUT_VMA`],
//! [`svq1_helper_luts::SVQ1_BITMASK_LUT_FILE_OFFSET`],
//! [`svq1_helper_luts::SVQ1_BITMASK_LUT_VMA`]) for provenance.
//! Seven new lib tests cover the length / structural invariants
//! (descending bit-mask half, one's-complement half, exact 16-byte
//! string, clip-LUT ramp head at byte `0x10..0x20`, derivable image
//! base of `0x67d70000` for both regions).
//!
//! **Round 9 — SVQ1 L=0..L=3 codebook payload landed.**
//!
//! Round 9 lands the 23004-byte mean-removed multistage VQ payload
//! and its 36-byte descriptor prefix as compile-time constants
//! ([`svq1_codebook`]), parsed at build time from the bit-exact
//! `tables/codebook-l0l3.csv` + `tables/codebook-descriptor.csv`
//! mirrors of `docs/video/svq1/tables/`. The docs were produced by
//! Extractor 02 (`docs/video/svq1/provenance/02-codebook-extraction.md`)
//! from the reference binary `quicktimethirdparty.qtx`
//! SHA-256 `ac3509bf22aa1458dfc6e1af980956c0153b4c287af452ae5b9cac6f923be169`,
//! file offset `0x5d200..0x62c00`. The new
//! [`Svq1Level::codebook_bytes_per_half`] returns the
//! per-level codebook size for one half (intra OR inter) — 768 / 1536
//! / 3072 / 6144 for L=0..L=3, `None` for L=4 / L=5 — matching the
//! `docs/video/svq1/tables/codebook-l0l3.meta` size arithmetic
//! `2 × (768 + 1536 + 3072 + 6144) = 23040 B = 36 B descriptor +
//! 23004 B payload`. The 16-entry block-shape LUT at descriptor
//! offset `+0x14` is exposed via
//! [`svq1_codebook::block_shape_lut`]; all entries are in `1..=4`,
//! corroborating the §14.10 / §14.11 ABSENT findings for L=4 and L=5
//! codebooks. The internal intra-vs-inter ordering and stage-vs-level
//! interleave within the 23004-byte payload remains a sibling docs
//! spec task per `codebook-l0l3.meta` — full pixel reconstruction
//! waits on that layout doc.
//!
//! ## Earlier rounds (carried forward)
//!
//! **Round 8 — SVQ1 block-tree subdivision walker.**
//!
//! Round 8 landed the recursive subdivide-vs-quantise decision tree
//! the SVQ1 wire format defines in
//! `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
//! Plane Data" as a typed module ([`svq1_blocktree`]). The walker
//! covers the spec's six levels (L=5 16×16 down to L=0 4×2), reads
//! one bit per non-leaf decision (with L=0 short-circuiting to
//! "quantise"), and surfaces the wiki spec's
//! `(stages > 0) && (level >= 4)` error path through a new
//! [`Error::InvalidLevelQuantise`] variant. The L=4 / L=5 rejection
//! is corroborated by the clean-room codebook-extraction docs
//! `docs/video/svq1/spec/14.10-codebook-L4.md` and
//! `docs/video/svq1/spec/14.11-codebook-L5.md` (RESOLVED 2026-05-30
//! — no codebook stored at those levels in the Sorenson Video TM
//! for QT R2.0 build; the codebook region only covers L=0..L=3).
//! The per-leaf payload (stages-count VLC + mean VLC + codebook
//! selector bits) is still gated on the L=0..L=3 codebook layout
//! work tracked in `docs/video/svq1/CODEBOOK_GAP.md`.
//!
//! ## Earlier rounds (carried forward)
//!
//! Round 5 — SVQ3 residual coefficient walker.
//!
//! Round 4 landed the SVQ3 macroblock-type tree walk + the intra-
//! prediction lookup tables. Round 5 adds the per-block residual
//! coefficient walker described in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
//! decoding": the three coefficient-table variants the wiki spec
//! enumerates (2×2 chroma DC, alternative-scan 4×4 luma-intra-with-
//! low-quantiser, normal-zigzag everything else) plus the two
//! run-correction arrays (`intra_run`, `inter_run`). The block-level
//! walkers ([`svq3_coeff::read_chroma_dc_block`],
//! [`svq3_coeff::read_alt_scan_half`],
//! [`svq3_coeff::read_normal_scan_block`]) loop until either the
//! end-of-block sentinel (`code = 0`) is seen or the block's
//! coefficient capacity is reached; structural overflow returns
//! [`Error::BadBitWidth`].
//!
//! ## Earlier rounds (carried forward)
//!
//! Round 4 added the per-macroblock type-Golomb walk + classification
//! per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
//! layer", plus the two fixed tables documented in §"Intra macroblock
//! information decoding" (the 25-entry intra-mode pair table and the
//! 6×6×5 intra-mode context-lookup table) and the 4×4 intra scan
//! order. The macroblock walker [`svq3_mb::read_mb_type`] decodes a
//! single `ue(v)` exp-Golomb code, classifies it against the
//! enclosing slice's frame type, and returns a typed
//! [`svq3_mb::Svq3MbType`]. CBP / motion-vector / intra-mode-pair
//! Golomb / intra-prediction wiring remain out of scope — those
//! sub-streams either depend on the H.264 CBP table (cross-spec
//! back-reference) or on the SVQ3 MV component VLC, neither of which
//! is enumerated bit-for-bit in the local mirror.
//!
//! ## Earlier rounds (carried forward)
//!
//! Round 3 — SVQ3 SEQH + slice-header parser, structural only. Round
//! 2 wired SVQ1 into the framework registry, gated behind the
//! default-on `registry` cargo feature.
//!
//! Round 2 — `oxideav-core` framework integration.
//!
//! Round 1 landed the structural SVQ1 frame-header parser. Round 2
//! wires that parser into the framework registry via the default-on
//! `registry` cargo feature: the crate now installs a SVQ1 codec
//! entry in [`oxideav_core::CodecRegistry`] under the FourCC tags
//! enumerated by `docs/video/svq1/wiki/Sorenson_Video_1.wiki` line 9
//! (`svq1` / `SVQ1` / `svqi`) and exposes a [`oxideav_core::Decoder`]
//! implementation whose `send_packet` parses the frame header and
//! whose `receive_frame` returns
//! [`oxideav_core::Error::Unsupported`] for the actual pixel-data
//! decode — the SVQ1 multi-stage VQ codebooks + per-level VLC tables
//! of the spec's "Appendix A: SVQ1 Data Tables" are still blocked on
//! the docs-collaborator extraction task (confirmed at round-2
//! dispatch).
//!
//! ## Standalone vs registry-integrated
//!
//! The default-on `registry` cargo feature pulls in `oxideav-core` and
//! installs the framework `Decoder` trait implementation plus
//! `register_codecs(reg)` / `register(ctx)` entry points. With the
//! feature off (`default-features = false`), the crate exposes a
//! minimal `oxideav-core`-free API: [`parse_frame_header`], the
//! [`Svq1FrameHeader`] result, the [`BitReader`] primitive, and the
//! [`Error`] enum.
//!
//! ## What's available at each layer
//!
//! * Standalone (`--no-default-features`):
//!     * [`parse_frame_header`] — walks every bit of the SVQ1 chunk
//!       header documented in
//!       `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format
//!       And Header" and returns a typed [`Svq1FrameHeader`].
//!     * [`FRAME_SIZE_TABLE`] — the seven well-known dimensions
//!       (160×120 / 128×96 / 176×144 / 352×288 / 704×576 / 240×180 /
//!       320×240).
//!     * [`SVQ1_FOURCC_CODES`] — `svq1`, `SVQ1`, `svqi` (line 9 of the
//!       wiki spec).
//!     * [`Error`] structural error variants plus the
//!       [`Error::NotImplemented`] sentinel for not-yet-wired API
//!       surfaces.
//! * Registry-integrated (default features):
//!     * Everything above, plus:
//!     * `register(&mut RuntimeContext)` — install the SVQ1 codec into
//!       a runtime.
//!     * `register_codecs(&mut CodecRegistry)` — same thing against a
//!       registry directly.
//!     * `probe_svq1(&ProbeContext)` — disambiguating probe wired into
//!       the registration's tag set.
//!     * `make_decoder` factory + `Svq1DecoderHandle` exposing
//!       [`Svq1DecoderHandle::last_header`] for the parsed header.
//!     * `From<crate::Error> for oxideav_core::Error` conversion.
//!
//! ## What round 2 does **not** deliver
//!
//! * The encoded plane data (`Y`, `U`, `V` planes) is **not** decoded.
//!   `receive_frame` returns `Error::Unsupported` until the codebook
//!   docs-gap closes.
//! * The embedded-string body is captured raw; de-obfuscation is
//!   deferred until the per-stream XOR table is pinned in `docs/`.
//! * The checksum byte is captured but not verified — the wiki spec
//!   itself notes "The specific details of the checksum coding are
//!   not all known".
//! * No SVQ3 work yet. SVQ3 is documented in `docs/video/svq3/wiki/`
//!   but the round-2 prompt scoped to SVQ1.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod bitreader;
mod error;
mod header;
pub mod svq1_blocktree;
pub mod svq1_codebook;
pub mod svq1_helper_luts;
pub mod svq1_mean;
pub mod svq1_stage_indices;
pub mod svq3;
pub mod svq3_coeff;
pub mod svq3_dequant;
pub mod svq3_mb;
pub mod svq3_mc;
pub mod svq3_pred;
pub mod svq3_scan;

#[cfg(feature = "registry")]
mod registry;

pub use crate::bitreader::BitReader;
pub use crate::error::{Error, Result};
pub use crate::header::{
    parse_frame_header, ChecksumTrailer, EmbeddedString, Svq1FrameHeader, Svq1PictureType,
    FRAME_SIZE_TABLE,
};

#[cfg(feature = "registry")]
pub use crate::registry::{
    __oxideav_entry, make_decoder, make_svq3_decoder, probe_svq1, probe_svq3, register,
    register_codecs, Svq1DecoderHandle, Svq3DecoderHandle,
};

/// Stable codec id used in the framework registry for SVQ1.
pub const CODEC_ID_STR: &str = "svq1";

/// Stable codec id used in the framework registry for SVQ3.
pub const SVQ3_CODEC_ID_STR: &str = "svq3";

/// FourCC codes the wiki spec attaches to SVQ1 in QuickTime / AVI
/// containers (`docs/video/svq1/wiki/Sorenson_Video_1.wiki` line 9 —
/// "FOURCCs: svq1, SVQ1, svqi"). Listed in the order the wiki page
/// enumerates them. Both `svq1` and `SVQ1` upper-case to the same
/// `CodecTag::fourcc` value; `svqi` is its own tag.
pub const SVQ1_FOURCC_CODES: &[&[u8; 4]] = &[b"svq1", b"SVQ1", b"svqi"];

/// FourCC codes the wiki spec attaches to SVQ3 (`docs/video/svq3/
/// wiki/Sorenson_Video_3.wiki` line 7 — "FOURCCs: SVQ3"). Listed in
/// the order the wiki page enumerates them.
pub const SVQ3_FOURCC_CODES: &[&[u8; 4]] = &[b"SVQ3"];
