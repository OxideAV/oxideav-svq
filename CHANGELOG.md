# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

No external library source (FFmpeg / libavcodec / MPlayer / etc.),
no archived `old` branch of this crate, and no online cross-checks
were consulted in any of the four rounds.

## [0.0.1] — Round 0 — clean-room rebuild scaffold

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch.
  See `README.md` for the rebuild scope and the strict-isolation
  workspace the Implementer rounds will draw from.
