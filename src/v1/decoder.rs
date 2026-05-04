//! Top-level packet → `Frame::Video` decoder for SVQ1.
//!
//! This module wires together [`crate::v1::header`] (frame header parse)
//! and [`crate::v1::vq`] (per-plane body walk) into the
//! [`oxideav_core::Decoder`] trait.
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
//! 3. **I-frame**: walk every MB through the hierarchical multistage
//!    VQ decoder ([`crate::v1::vq::decode_plane_intra`]).
//!    **P-frame**: round-2 falls back to flat-fill mid-grey at every
//!    MB; motion comp + INTER VQ residual lands in round 3.
//! 4. Crop padded planes back to declared dimensions; upsample chroma
//!    2x in each axis to land on `Yuv420P` (the closest `oxideav_core`
//!    pixel format to native 4:1:0).
//! 5. Emit `Frame::Video(VideoFrame)` carrying three [`VideoPlane`]s.

use oxideav_core::frame::VideoPlane;
use oxideav_core::{
    CodecId, CodecParameters, Decoder, Error, Frame, Packet, PixelFormat, Result, VideoFrame,
};

use super::header::{parse_header, FrameHeader, FrameType};
use super::vq::{
    crop_plane, decode_plane_flat, decode_plane_intra, upsample_chroma_410_to_420, PlaneDims,
    VlcBundle,
};

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
        let (header, mut br) = parse_header(&pkt.data, self.prev_dims)?;

        // Update reference dims when this frame is itself a reference,
        // or when we don't yet have any (P-non-ref frame as the first
        // packet — pathological but representable).
        if header.is_reference() || self.prev_dims.is_none() {
            self.prev_dims = Some((header.width, header.height));
        }

        let frame = match header.frame_type {
            FrameType::Intra => render_intra(&header, pkt.pts, &mut br)?,
            FrameType::Predicted | FrameType::PNonReference => {
                // Round 2 stub: P-frames are decoded as flat-fill
                // mid-grey. Motion compensation + INTER VQ residual
                // arrive in round 3; until then the I-frame pipeline
                // already exercises every VLC, every codebook, and
                // every quad-tree branch.
                render_flat_fill(&header, pkt.pts)
            }
        };
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

/// Render an I-frame by walking the per-plane multistage VQ tree.
fn render_intra(
    header: &FrameHeader,
    pts: Option<i64>,
    br: &mut oxideav_core::bits::BitReader<'_>,
) -> Result<VideoFrame> {
    let luma_dims = PlaneDims::for_luma(header.width, header.height);
    let chroma_dims = PlaneDims::for_chroma(header.width, header.height);

    let vlcs = VlcBundle::build();

    let y_padded = decode_plane_intra(br, &vlcs, &luma_dims)?;
    let u_padded = decode_plane_intra(br, &vlcs, &chroma_dims)?;
    let v_padded = decode_plane_intra(br, &vlcs, &chroma_dims)?;

    let y = crop_plane(&y_padded, &luma_dims);
    let u_410 = crop_plane(&u_padded, &chroma_dims);
    let v_410 = crop_plane(&v_padded, &chroma_dims);
    let (u_420, c_w, _c_h) =
        upsample_chroma_410_to_420(&u_410, &chroma_dims, header.width, header.height);
    let (v_420, _, _) =
        upsample_chroma_410_to_420(&v_410, &chroma_dims, header.width, header.height);

    Ok(VideoFrame {
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
    })
}

/// Render a flat-fill `Yuv420P` frame for a parsed header (P-frame
/// fallback in round 2).
fn render_flat_fill(header: &FrameHeader, pts: Option<i64>) -> VideoFrame {
    let luma_dims = PlaneDims::for_luma(header.width, header.height);
    let chroma_dims = PlaneDims::for_chroma(header.width, header.height);

    let luma_value = 128u8;
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

    /// Synthesise a minimal valid I-frame packet (160×120, all-MB
    /// flat-fill at mean=128).
    ///
    /// Body layout per MB (level 5 leaf): split=0; multistage L5
    /// count=0 → "1" (1 bit); intra_mean(128) → row 128 of §14.4 =
    /// "01001001" (8 bits). Total 10 bits/MB.
    fn minimal_iframe_packet() -> Vec<u8> {
        use oxideav_core::bits::BitWriter;
        let mut bw = BitWriter::new();
        // Header chunks (same as header::tests::build_minimal_iframe):
        bw.write_u32(0x20, 22);
        bw.write_u32(0, 8);
        bw.write_u32(0, 2); // frame_type=Intra
        bw.write_u32(0b10, 2); // reserved
        bw.write_u32(0, 2); // reserved
        bw.write_u32(0, 1); // reserved
        bw.write_u32(0, 3); // size_code=0 → 160x120
        bw.write_u32(0, 1); // checksum_block_flag
        bw.write_u32(0, 1); // extra_data_block_flag
                            // Body: write per-MB pattern for all luma MBs (160x128 padded → 80 MBs)
                            // and all chroma MBs (40x32 padded to 48x32 → 6 MBs each).
                            // 160×120 luma → padded 160×128 → 80 MBs.
        let luma_mbs = (160 / 16) * (128 / 16);
        // 160×120 chroma at 4:1:0 → 40×30 → padded 48×32 → 6 MBs per chroma plane.
        let chroma_padded_w = (40usize + 15) & !15;
        let chroma_padded_h = (30usize + 15) & !15;
        let chroma_mbs = (chroma_padded_w / 16) * (chroma_padded_h / 16);
        for _ in 0..(luma_mbs + 2 * chroma_mbs) {
            bw.write_bit(false); // split=0 at L5
            bw.write_u32(0b1, 1); // multistage L5: count=0
            bw.write_u32(0b0100_1001, 8); // intra_mean(128)
        }
        // Pad some
        bw.write_u32(0, 8);
        bw.into_bytes()
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
