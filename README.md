# oxideav-svq

Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Round 2 — `oxideav-core` framework integration.** The round-1
structural SVQ1 frame-header parser is now wired into the framework
registry via a default-on `registry` cargo feature: the crate
installs a SVQ1 codec entry in
[`oxideav_core::CodecRegistry`](https://docs.rs/oxideav-core) under
the FourCC tags enumerated by
[`docs/video/svq1/wiki/Sorenson_Video_1.wiki`](https://github.com/OxideAV/docs/blob/master/video/svq1/wiki/Sorenson_Video_1.wiki)
line 9 (`svq1` / `SVQ1` / `svqi`) and exposes an
`oxideav_core::Decoder` implementation whose `send_packet` parses the
frame header and whose `receive_frame` returns
`oxideav_core::Error::Unsupported` for the actual pixel-data decode.

What round 2 adds on top of round 1:

* Default-on `registry` cargo feature gating the `oxideav-core`
  dependency. Image / pipeline consumers that only want the frame-
  header parser can depend with `default-features = false` and skip
  the framework dependency tree.
* `oxideav_core::Decoder` implementation (`Svq1DecoderHandle`):
  `send_packet` parses the frame header eagerly (the bitstream's
  structural failure modes surface at `send_packet` rather than later
  at `receive_frame`); the parsed `Svq1FrameHeader` is exposed via
  `Svq1DecoderHandle::last_header()`. `receive_frame` returns
  `Error::Unsupported` until the codebook docs-gap closes.
* Disambiguating `probe_svq1` registered alongside the FourCC tags.
  Returns `1.0` on a structurally valid header, `0.5` on a header
  that's plausibly SVQ1 but truncated, `0.5` when no packet is
  available (FourCC alone is highly disambiguating), `0.0` on a
  structurally invalid header.
* `register(&mut RuntimeContext)` / `register_codecs(&mut
  CodecRegistry)` / `make_decoder(&CodecParameters)` entry points
  plus the `__oxideav_entry` symbol the `oxideav_core::register!`
  macro expands to (so `oxideav-meta`'s build.rs picks the crate up
  automatically).
* `From<crate::Error> for oxideav_core::Error` conversion mapping
  every structural failure to `InvalidData(msg)` with a descriptive
  string; `NotImplemented` maps to `Unsupported`.

What round 2 covers (carried from round 1):

* The 22-bit frame-code field plus the `(frame_code & 0x60) == 0`
  invalid-stream check.
* The 8-bit temporal-reference counter.
* The 2-bit picture-type field, decoded to a `Svq1PictureType` enum
  (`Intra` / `Predicted` / `Droppable`); value `3` is rejected per
  the spec.
* The I-frame trailer chain: optional 16-bit checksum when the frame
  code is `0x50` or `0x60`, optional XOR-obfuscated embedded ASCII
  string when `frame_code ^ 0x10 > 0x50`, the 2 + 2 + 1 "unknown"
  sub-fields, the 3-bit frame-size code, and the explicit 12 + 12
  width/height escape for code `7`.
* The 1-bit checksum-present flag and its `use_packet_checksum:1 +
  component_checksums:1 + reserved2:2` (reserved2 must be 0) trailer.
* The 1-bit `unknown_flag_1` flag, its 1 + 4 + 1 + 2 fixed-width
  sub-fields, and the variable-length `while next-bit-is-1 { read a
  byte }` tail.
* A constant `FRAME_SIZE_TABLE` for the seven well-known dimension
  pairs (160×120 / 128×96 / 176×144 / 352×288 / 704×576 / 240×180 /
  320×240) and the FourCC list (`svq1`, `SVQ1`, `svqi`).
* Four documented structural error conditions plus the truncation
  guard.

What round 2 still does **not** cover:

* The encoded plane data (`Y` / `U` / `V`) that follows the header.
  Decoding those requires the SVQ1 multi-stage VQ codebooks and the
  per-level VLC tables enumerated in the spec's "Appendix A: SVQ1
  Data Tables" — confirmed at round-2 dispatch as still blocked on
  the docs-collaborator task tracking the codebook byte-list
  extraction. `Decoder::receive_frame` returns `Error::Unsupported`
  until that lands.
* Embedded-string de-obfuscation. The parser captures the raw
  obfuscated bytes plus the declared length; de-obfuscation is
  deferred until the `string_xor_table[]` is pinned in `docs/`.
* Checksum verification. The spec itself notes "The specific details
  of the checksum coding are not all known"; the value is captured
  for future verification once the algorithm is documented.
* SVQ3. The `docs/video/svq3/wiki/Sorenson_Video_3.wiki` snapshot
  exists but the round-2 prompt scoped to SVQ1.

## Cargo feature surface

```toml
[dependencies]
# Default: framework-integrated. Pulls in oxideav-core and installs
# the SVQ1 codec into the framework registry.
oxideav-svq = "0.0"

# Standalone: just the SVQ1 frame-header parser, no oxideav-core
# dependency. Suitable for consumers that need to peek at SVQ1
# dimensions / picture-type without bringing in the framework.
oxideav-svq = { version = "0.0", default-features = false }
```

The crate-level standalone API (always available):

* `parse_frame_header(bytes: &[u8]) -> Result<Svq1FrameHeader>`
* `Svq1FrameHeader { frame_code, temporal_reference, picture_type,
  checksum, embedded_string, width, height, frame_size_code,
  checksum_trailer, unknown_flag_1_payload, unknown_flag_1_extras,
  header_end_bit, … }`
* `BitReader::new(&bytes).read_bits(n)` / `read_bit()` / `peek_bit()`
* `FRAME_SIZE_TABLE` + `SVQ1_FOURCC_CODES` + `Svq1PictureType`
* The `Error` enum + `Result<T>` alias.

The framework-integrated surface (when `registry` is on):

* `register(&mut RuntimeContext)`
* `register_codecs(&mut CodecRegistry)`
* `make_decoder(&CodecParameters) -> Result<Box<dyn Decoder>>`
* `probe_svq1(&ProbeContext) -> Confidence`
* `Svq1DecoderHandle::last_header()`
* `From<crate::Error> for oxideav_core::Error`

## Clean-room provenance

Round 2 was implemented strictly from
`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
Header" + line 9 (FourCC list) — a verbatim local mirror of the
multimedia.cx Sorenson_Video_1 wiki page (fetched 2026-05-06, CC-BY-SA
per multimedia.cx terms). No external library source, no archived
`old` branch of this crate, and no online cross-checks were
consulted. Codebook / VLC-table contents were not read or
transcribed; they will land behind the documented docs-gap once that
gap is closed.

The wiki spec's "Appendix A: SVQ1 Data Tables" lists upstream
source-tree pointers for the codebook + VLC byte arrays. Those
pointers are explicitly **not** followed; round 2 implements the
framework-integration layer alone and the docs-gap note above tracks
the upstream blocker for the data-table layer.
