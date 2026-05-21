# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

- **Docs gap (still tracked):** the SVQ1 multi-stage VQ codebooks and
  per-level VLC tables enumerated in the wiki spec's "Appendix A:
  SVQ1 Data Tables" are not yet pinned in `docs/`. Round 3 (the
  encoded plane-data layer) is blocked on that data landing in
  `docs/video/svq1/spec/` or `docs/video/svq1/tables/`. Confirmed
  still open at round-2 dispatch.
- **SVQ3 deferred:** `docs/video/svq3/wiki/Sorenson_Video_3.wiki`
  exists but neither round-1 nor round-2 touched it.

### Provenance

Round 1 was implemented strictly from
`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
Header". Round 2 added line 9 (FourCC list) from the same file and
the `oxideav-core` public API (`Decoder` / `CodecRegistry` /
`CodecParameters` / `Packet` / `ProbeContext` / `register!` macro).
The wiki file is a verbatim local mirror of the multimedia.cx
Sorenson_Video_1 wiki page (fetched 2026-05-06, CC-BY-SA per
multimedia.cx terms). No external library source, no archived `old`
branch of this crate, and no online cross-checks were consulted in
either round.

## [0.0.1] — Round 0 — clean-room rebuild scaffold

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch.
  See `README.md` for the rebuild scope and the strict-isolation
  workspace the Implementer rounds will draw from.
