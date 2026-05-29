//! Crate-local error type.
//!
//! Round 1 surfaces the structural-parse failure modes the SVQ1
//! frame-header decoder of `docs/video/svq1/wiki/Sorenson_Video_1.wiki`
//! §"Stream Format And Header" can produce. Subsequent rounds will
//! grow new variants as the codebook / motion / IDCT layers land —
//! every variant added here stays additive and the round-0
//! [`Error::NotImplemented`] sentinel remains for surfaces the
//! scaffold has not yet wired up.

/// Errors produced by the SVQ1 frame-header parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The 22-bit `frame code` at the start of the chunk has both bit
    /// 6 and bit 5 clear (i.e. `frame_code & 0x60 == 0`). Per
    /// `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format
    /// And Header" the chunk is malformed in that case and the
    /// decoder must abort before reading any further fields.
    InvalidFrameCode(u32),
    /// The 2-bit `picture type` field decoded to the value `3`. The
    /// wiki spec lists that value as "invalid frame code, do not
    /// proceed with frame decode".
    InvalidPictureType,
    /// The `unknown_field_1` 2-bit field guarded by `checksum
    /// present` decoded to a non-zero value, which the wiki spec
    /// classifies as an "invalid frame header" condition.
    InvalidChecksumTrailer,
    /// The bit stream ran out of bytes before a fixed-width header
    /// field could be read in full.
    Truncated,
    /// The bit-reader was asked for an out-of-range field width (0 or
    /// >32). Indicates a bug in the parser, not a malformed stream.
    BadBitWidth(u32),
    /// The intra-4×4 prediction `(top, left, idx)` lookup landed on an
    /// `INTRA_PRED_TABLE` entry holding the sentinel `-1`. Per
    /// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
    /// information decoding" — "If table value is -1 then input data
    /// was incorrect or intra modes were predicted incorrectly." The
    /// tuple is `(top_index, left_index, idx)` after the spec's
    /// `top + 1` / `left + 1` adjustment.
    InvalidIntraPrediction(u8, u8, u8),
    /// Reserved scaffold variant. Surfaces from API endpoints (codec
    /// registration, frame decode) that the round-1 frame-header
    /// parser has not yet wired up.
    NotImplemented,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::InvalidFrameCode(code) => {
                write!(
                    f,
                    "oxideav-svq: invalid SVQ1 frame code 0x{code:06x} (bits 6 and 5 both clear)"
                )
            }
            Error::InvalidPictureType => {
                f.write_str("oxideav-svq: SVQ1 picture-type field decoded to the reserved value 3")
            }
            Error::InvalidChecksumTrailer => f.write_str(
                "oxideav-svq: SVQ1 checksum-trailer unknown_field_1 must be zero per the spec",
            ),
            Error::Truncated => {
                f.write_str("oxideav-svq: SVQ1 chunk truncated mid frame-header field")
            }
            Error::BadBitWidth(n) => {
                write!(
                    f,
                    "oxideav-svq: bit-reader rejected width {n} (must be 1..=32)"
                )
            }
            Error::InvalidIntraPrediction(top, left, idx) => {
                write!(
                    f,
                    "oxideav-svq: intra-4x4 prediction lookup landed on -1 sentinel \
                     at INTRA_PRED_TABLE[{top}][{left}][{idx}]"
                )
            }
            Error::NotImplemented => f.write_str(
                "oxideav-svq: clean-room rebuild in progress — see crates/oxideav-svq/README.md",
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Crate-local `Result` alias.
pub type Result<T> = core::result::Result<T, Error>;
