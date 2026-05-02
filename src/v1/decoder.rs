//! Top-level packet → `Frame::Video` decoder for SVQ1.
//!
//! This module wires together [`crate::header`] (frame header parse)
//! and [`crate::vq`] (per-plane body walk + flat-fill fallback) into
//! the [`oxideav_core::Decoder`] trait.
//!
//! ## Output pipeline
//!
//! Per packet, in order:
//!
//! 1. Parse the frame header. If it's a P-frame, inherit dimensions
//!    from the cached reference; otherwise read the explicit/preset
//!    width/height.
//! 2. Build the three `Yuv410p` planes (luma at the declared size,
//!    chroma at W/4 × H/4 with at-least-one-MB padding).
//! 3. **Round-1 flat-fill**: each plane is filled with the per-
//!    component midpoint (`128`). The body bits are not yet
//!    semantically parsed pending the multistage VLC + codebook tables
//!    becoming available — see [`crate::vq`] for the rationale.
//! 4. Crop padded planes back to declared dimensions; upsample chroma
//!    2x in each axis to land on `Yuv420P` (the closest `oxideav_core`
//!    pixel format to native 4:1:0).
//! 5. Emit `Frame::Video(VideoFrame)` carrying three [`VideoPlane`]s.

use oxideav_core::frame::VideoPlane;
use oxideav_core::{
    CodecId, CodecParameters, Decoder, Error, Frame, Packet, PixelFormat, Result, VideoFrame,
};

use super::header::{parse_header, FrameHeader, FrameType};
use super::vq::{crop_plane, decode_plane_flat, upsample_chroma_410_to_420, PlaneDims};

/// Decoder state.
#[derive(Debug)]
pub struct Svq1Decoder {
    codec_id: CodecId,
    /// Pending packet between `send_packet` and `receive_frame`.
    pending: Option<Packet>,
    eof: bool,
    /// Last-decoded reference frame's dimensions; needed for P-frames
    /// (which don't re-emit width/height in the header).
    prev_dims: Option<(u16, u16)>,
}

impl Svq1Decoder {
    pub fn new(codec_id: CodecId) -> Self {
        Self {
            codec_id,
            pending: None,
            eof: false,
            prev_dims: None,
        }
    }
}

impl Decoder for Svq1Decoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "svq1: receive_frame must be called before sending another packet",
            ));
        }
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        let Some(pkt) = self.pending.take() else {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        };
        let (header, _br) = parse_header(&pkt.data, self.prev_dims)?;

        // Update reference dims when this frame is itself a reference,
        // or when we don't yet have any (P-non-ref frame as the first
        // packet — pathological but representable).
        if header.is_reference() || self.prev_dims.is_none() {
            self.prev_dims = Some((header.width, header.height));
        }

        let frame = render_flat_fill(&header, pkt.pts);
        Ok(Frame::Video(frame))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.pending = None;
        self.eof = false;
        self.prev_dims = None;
        Ok(())
    }
}

/// Render the round-1 flat-fill `Yuv420P` frame for a parsed header.
fn render_flat_fill(header: &FrameHeader, pts: Option<i64>) -> VideoFrame {
    let luma_dims = PlaneDims::for_luma(header.width, header.height);
    let chroma_dims = PlaneDims::for_chroma(header.width, header.height);

    // INTRA mid-grey, INTER residual centre — both happen to be 128
    // in unsigned-8-bit luma units. The sentinel is intentionally
    // identical so the output remains a recognisable mid-grey card.
    let luma_value = match header.frame_type {
        FrameType::Intra => 128u8,
        FrameType::Predicted | FrameType::PNonReference => 128u8,
    };
    let chroma_value: u8 = 128;

    let y_padded = decode_plane_flat(&luma_dims, luma_value);
    let u_padded = decode_plane_flat(&chroma_dims, chroma_value);
    let v_padded = decode_plane_flat(&chroma_dims, chroma_value);

    let y = crop_plane(&y_padded, &luma_dims);
    let u_410 = crop_plane(&u_padded, &chroma_dims);
    let v_410 = crop_plane(&v_padded, &chroma_dims);
    let (u_420, c_w, _c_h) =
        upsample_chroma_410_to_420(&u_410, &chroma_dims, header.width, header.height);
    let (v_420, _, _) =
        upsample_chroma_410_to_420(&v_410, &chroma_dims, header.width, header.height);

    VideoFrame {
        pts,
        planes: vec![
            VideoPlane {
                stride: luma_dims.width,
                data: y,
            },
            VideoPlane {
                stride: c_w,
                data: u_420,
            },
            VideoPlane {
                stride: c_w,
                data: v_420,
            },
        ],
    }
}

/// Decoder factory used by the registry. The [`PixelFormat::Yuv420P`]
/// hint is informational; we always emit `Yuv420P` regardless of
/// `params.pixel_format`.
pub fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let _ = PixelFormat::Yuv420P; // anchor the dependency on the symbol
    Ok(Box::new(Svq1Decoder::new(params.codec_id.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxideav_core::TimeBase;

    /// Synthesise a minimal valid I-frame packet (same shape as
    /// `header::tests::build_minimal_iframe`).
    fn minimal_iframe_packet() -> Vec<u8> {
        let chunks: [(u64, u32); 9] = [
            (0x20, 22),
            (0, 8),
            (0, 2),
            (0b10, 2),
            (0, 2),
            (0, 1),
            (0, 3),
            (0, 1),
            (0, 1),
        ];
        let mut acc: u128 = 0;
        let mut bits: u32 = 0;
        for &(v, n) in &chunks {
            acc = (acc << n) | (v as u128);
            bits += n;
        }
        let pad = (8 - bits % 8) % 8;
        acc <<= pad;
        let total_bytes = ((bits + pad) / 8) as usize;
        let mut out = vec![0u8; total_bytes];
        for i in 0..total_bytes {
            let shift = (total_bytes - 1 - i) * 8;
            out[i] = ((acc >> shift) & 0xff) as u8;
        }
        out.extend_from_slice(&[0u8; 4]);
        out
    }

    #[test]
    fn round_trip_minimal_iframe() {
        let pkt_data = minimal_iframe_packet();
        let mut dec = Svq1Decoder::new(CodecId::new("svq1"));
        let pkt = Packet::new(0, TimeBase::new(1, 30), pkt_data);
        dec.send_packet(&pkt).unwrap();
        let frame = dec.receive_frame().unwrap();
        match frame {
            Frame::Video(vf) => {
                assert_eq!(vf.planes.len(), 3);
                assert_eq!(vf.planes[0].stride, 160);
                assert_eq!(vf.planes[0].data.len(), 160 * 120);
                assert!(vf.planes[0].data.iter().all(|&b| b == 128));
                // Chroma upsampled to 80×60 (4:2:0 of 160×120).
                assert_eq!(vf.planes[1].stride, 80);
                assert_eq!(vf.planes[1].data.len(), 80 * 60);
                assert!(vf.planes[1].data.iter().all(|&b| b == 128));
            }
            _ => panic!("expected video frame"),
        }
    }

    #[test]
    fn second_send_before_receive_errors() {
        let pkt_data = minimal_iframe_packet();
        let mut dec = Svq1Decoder::new(CodecId::new("svq1"));
        let pkt = Packet::new(0, TimeBase::new(1, 30), pkt_data.clone());
        dec.send_packet(&pkt).unwrap();
        assert!(dec.send_packet(&pkt).is_err());
    }

    #[test]
    fn need_more_when_idle() {
        let mut dec = Svq1Decoder::new(CodecId::new("svq1"));
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMore)));
    }

    #[test]
    fn flush_then_eof() {
        let mut dec = Svq1Decoder::new(CodecId::new("svq1"));
        dec.flush().unwrap();
        assert!(matches!(dec.receive_frame(), Err(Error::Eof)));
    }
}
