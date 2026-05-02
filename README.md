# oxideav-svq

Pure-Rust **Sorenson Video 1 (SVQ1)** video decoder for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.
SVQ1 (FourCC `SVQ1`) is the late-1990s Apple QuickTime hierarchical-
multistage VQ codec used between roughly 1998 and 2002.  Zero C
dependencies.

## Status

| Feature                                                    | Status   |
|------------------------------------------------------------|----------|
| 22-bit `frame_code` parse + legality test                  | works    |
| `temporal_reference` + `frame_type`                        | works    |
| I-frame `reserved` + `frame_size_code` + preset/explicit   | works    |
| `checksum_block_flag` skip path                            | works    |
| `extra_data_block_flag` + stop-1+u(8) chain skip           | works    |
| YUV 4:1:0 → YUV 4:2:0 chroma upsample                      | works    |
| `Decoder` trait wiring + `Codec` registration              | works    |
| Hierarchical multistage VQ body decode                     | **stub** |
| P-frame motion-compensation                                | gap      |
| Header obfuscation pre-pass for `frame_code != 0x20`       | gap      |
| Optional packet checksum / embedded-string scrambling      | skipped (consumed but not validated/decoded) |

## Round-1 flat-fill body decoder

The SVQ1 body is a hierarchical multistage VQ with **fixed codebooks**:
six levels × six stages × sixteen entries × per-level pixel count of
signed 8-bit perturbations, plus six per-level multistage VLC tables
(intra + inter), an intra mean VLC, an inter mean VLC and an H.263-
style motion-component VLC.

`docs/video/svq1/svq1-trace-reverse-engineering.md` documents the
**bitstream syntax** end-to-end but is explicit that the codebook bytes
and VLC table contents "must be reverse-engineered from a reference
decoder". Workspace policy further bars us from copying any third-
party source verbatim. Until those tables land in
`docs/video/svq1/svq1-tables.md` as a clean-room transcription, the
body decoder operates in **flat-fill fallback mode**: every plane is
filled with the per-component midpoint (`128` luma, `128` chroma after
the unsigned mapping). The header is fully parsed bit-correctly, the
declared frame size is honoured, the chroma planes are upsampled from
native 4:1:0 to the framework's `Yuv420P`, and the resulting
`VideoFrame` is structurally valid for every downstream consumer
(filter chain, sink, GUI player) — the picture content is just
mid-grey for the moment.

## Gaps

- **Multistage VLC + codebook tables.** Round-2 work; tracked under
  workspace MEMORY notes. Once landed, `decode_plane_flat` in
  `src/vq.rs` is replaced by `decode_plane_quadtree` and the round-1
  flat-fill drops out.
- **P-frame motion-compensation.** Block-type VLC, MV component VLC +
  median predictor, half-pel bilinear interpolation, 4V sub-block
  variant — unimplemented. Today the decoder produces mid-grey for P-
  frames too, so the temporal sequence still flows but no motion is
  reconstructed.
- **Header obfuscation pre-pass.** When `frame_code in {0x40, 0x50,
  0x60}` the decoder must un-swap-and-XOR bytes `[4..36)` of the
  packet before parsing the rest of the header. Our trace corpus is
  100 % `frame_code = 0x20` so we surface a clear `unsupported`
  error for the obfuscated path rather than half-implementing it.
- **`frame_size_code = 4`.** Not exercised by any sample in the trace
  corpus; the table entry is left blank and the decoder rejects the
  code as `unsupported` (rather than guessing).

## Installation

```toml
[dependencies]
oxideav-core = "0.1"
oxideav-svq = "0.0"
```

## Quick use

```rust,no_run
use oxideav_core::CodecRegistry;

let mut codecs = CodecRegistry::new();
oxideav_svq::register(&mut codecs);
```

The decoder claims the QuickTime FourCC `SVQ1` (and its lowercase
`svq1` spelling sometimes seen in MOV `stsd` boxes); the container
crates (oxideav-mp4) recognise both.

## References

- `docs/video/svq1/svq1-trace-reverse-engineering.md` — primary
  clean-room behavioural-trace spec.
- MultimediaWiki — *Sorenson Video 1*. <https://wiki.multimedia.cx/index.php/Sorenson_Video_1>
- Mike Melanson — *VQ Case Study: Sorenson Video 1*. <https://multimedia.cx/eggs/vq-case-study-sorenson-video-1/>
- VideoLAN Wiki — *Sorenson Video*. <https://wiki.videolan.org/Sorenson_Video/>

There is no IETF RFC for SVQ1 and no SDP `a=rtpmap` registration; the
codec was distributed inside QuickTime/MOV containers and never
standardised for streaming.

## License

Licensed under the [MIT License](LICENSE).
