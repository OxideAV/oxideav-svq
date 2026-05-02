# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.1] - 2026-05-02

### Added

- Initial scaffold of the pure-Rust Sorenson SVQ1 (FourCC `SVQ1`)
  video decoder. Parses the 22-bit `frame_code` packet prefix, the
  I-/P-frame header (temporal reference, frame type, preset/explicit
  frame size, optional checksum and extra-data blocks), and decodes
  I-frame bodies via hierarchical multistage vector quantisation
  using fixed codebooks. Output is YUV 4:1:0 in an
  `oxideav_core::VideoFrame`. P-frames currently surface as
  `Error::Unsupported` (no motion-compensation pipeline yet); see
  the README "Gaps" section for the full list.
