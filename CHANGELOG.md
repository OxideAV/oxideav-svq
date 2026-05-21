# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
  §"Macroblock layer" / §"Coefficient decoding" /
  §"Intra macroblock information decoding" /
  §"Inter macroblock information decoding" /
  §"Macroblock transform and dequantization" / §"Intra prediction"
  / §"Motion Compensation" sections are present in the local mirror
  but round 3 deliberately scoped to structural parse only;
  follow-up rounds will exercise them once the slice-payload
  descramble algorithm and the protected-stream watermark sub-record
  encoding are documented.
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

No external library source (FFmpeg / libavcodec / MPlayer / etc.),
no archived `old` branch of this crate, and no online cross-checks
were consulted in any of the three rounds.

## [0.0.1] — Round 0 — clean-room rebuild scaffold

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch.
  See `README.md` for the rebuild scope and the strict-isolation
  workspace the Implementer rounds will draw from.
