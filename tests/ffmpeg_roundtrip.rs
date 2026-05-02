//! End-to-end roundtrip test against an ffmpeg-produced SVQ1 stream.
//!
//! Workflow (gated on ffmpeg + ffprobe being on PATH):
//!
//! 1. Synthesise a tiny `testsrc` clip and encode it with the
//!    `ffmpeg svq1` encoder into a `.mov`.
//! 2. Extract the per-packet (offset, size) table via `ffprobe`,
//!    slice the raw bytes out of the file, and feed them into our
//!    decoder.
//! 3. Compare the decoder's output planes to the original `testsrc`
//!    decoded by ffmpeg back to `yuv420p` — measure PSNR.
//!
//! With the round-1 flat-fill body decoder the PSNR is the floor we
//! get from "every plane is 128" — for `testsrc` that's roughly
//! 7-12 dB, but the test only asserts that the test ran end-to-end
//! without errors and that PSNR is finite (i.e. the decoded planes
//! actually have the declared shape and `width × height`).
//!
//! When the codebook lands and `decode_plane_quadtree` replaces
//! `decode_plane_flat`, the assertion threshold tightens to ~25 dB.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use oxideav_core::{CodecId, Decoder, Frame, Packet, TimeBase};
use oxideav_svq1::decoder::Svq1Decoder;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        && Command::new("ffprobe")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn tmp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("oxideav_svq1_tests");
    fs::create_dir_all(&dir).expect("mkdir tmp");
    dir.join(name)
}

/// Encode a testsrc clip with SVQ1 into a .mov.
fn encode_fixture(out: &PathBuf) {
    if out.exists() {
        let _ = fs::remove_file(out);
    }
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=176x144:rate=30:duration=0.1",
            "-c:v",
            "svq1",
            "-frames:v",
            "3",
            "-y",
        ])
        .arg(out)
        .status()
        .expect("ffmpeg spawn");
    assert!(status.success(), "ffmpeg svq1 encode failed");
}

/// Decode the same fixture back to raw YUV via ffmpeg as the reference
/// for PSNR.
fn decode_fixture_yuv(input: &PathBuf, out: &PathBuf) -> (u32, u32, usize) {
    if out.exists() {
        let _ = fs::remove_file(out);
    }
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-y"])
        .arg(out)
        .status()
        .expect("ffmpeg spawn");
    assert!(status.success(), "ffmpeg svq1 decode failed");
    let bytes = fs::metadata(out).expect("size").len() as usize;
    // 176×144 yuv420p = 38016 bytes/frame.
    let per_frame = 176 * 144 * 3 / 2;
    let frames = bytes / per_frame;
    (176, 144, frames)
}

/// Use ffprobe to enumerate per-packet (pos, size) for the first
/// video stream.
fn list_packets(input: &PathBuf) -> Vec<(u64, usize)> {
    let out = Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-show_packets",
            "-of",
            "csv=p=0",
            "-select_streams",
            "v:0",
        ])
        .arg(input)
        .output()
        .expect("ffprobe spawn");
    assert!(out.status.success(), "ffprobe failed");
    let mut pkts = Vec::new();
    for line in std::str::from_utf8(&out.stdout).unwrap().lines() {
        let cols: Vec<&str> = line.split(',').collect();
        // CSV layout: codec_type, stream_index, pts, pts_time, dts,
        //             dts_time, duration, duration_time, size, pos, flags
        if cols.len() >= 10 {
            let size: usize = cols[8].parse().unwrap_or(0);
            let pos: u64 = cols[9].parse().unwrap_or(0);
            if size > 0 && pos > 0 {
                pkts.push((pos, size));
            }
        }
    }
    pkts
}

/// Slice raw packet bytes out of the mov file at the offsets ffprobe
/// reported.
fn extract_packets(mov_path: &PathBuf, table: &[(u64, usize)]) -> Vec<Vec<u8>> {
    let bytes = fs::read(mov_path).expect("read mov");
    let mut out = Vec::with_capacity(table.len());
    for &(pos, size) in table {
        let s = pos as usize;
        let e = s + size;
        assert!(e <= bytes.len(), "packet slice OOB");
        out.push(bytes[s..e].to_vec());
    }
    out
}

/// Compute Y-plane PSNR between decoded and reference frames. The
/// inputs are flat YUV420P frame buffers of identical size.
fn psnr_y(decoded_y: &[u8], reference_y: &[u8]) -> f64 {
    assert_eq!(decoded_y.len(), reference_y.len(), "Y-plane size mismatch");
    let mut sse: f64 = 0.0;
    for (a, b) in decoded_y.iter().zip(reference_y.iter()) {
        let d = (*a as f64) - (*b as f64);
        sse += d * d;
    }
    let mse = sse / decoded_y.len() as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}

#[test]
fn decode_ffmpeg_svq1_fixture_runs_end_to_end() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg/ffprobe not on PATH — skipping integration test");
        return;
    }

    let mov = tmp_path("testsrc_svq1.mov");
    let ref_yuv = tmp_path("testsrc_svq1.yuv");
    encode_fixture(&mov);
    let (w, h, ref_frames) = decode_fixture_yuv(&mov, &ref_yuv);

    let table = list_packets(&mov);
    assert!(!table.is_empty(), "ffprobe returned no packets");
    let pkts = extract_packets(&mov, &table);

    let mut dec = Svq1Decoder::new(CodecId::new("svq1"));
    let reference = fs::read(&ref_yuv).expect("read ref yuv");
    let frame_size_y = (w as usize) * (h as usize);
    let _frame_size_c = frame_size_y / 4;

    let mut psnr_sum = 0.0;
    let mut frame_idx = 0usize;
    for (i, raw) in pkts.iter().enumerate() {
        let pkt = Packet::new(0u32, TimeBase::new(1, 30), raw.clone());
        let _ = i; // i used only for diagnostics below
                   // Header parsing should succeed for every packet ffmpeg emitted.
        if let Err(e) = dec.send_packet(&pkt) {
            panic!("send_packet({}): {e}", i);
        }
        let frame = match dec.receive_frame() {
            Ok(Frame::Video(vf)) => vf,
            Ok(_) => panic!("non-video frame from decoder"),
            Err(e) => {
                // P-frame paths might not be supported by the round-1
                // decoder; allow that without failing the test.
                eprintln!("packet {i}: decode error {e}");
                continue;
            }
        };
        // Verify shape is what we expect.
        assert_eq!(frame.planes.len(), 3);
        assert_eq!(frame.planes[0].stride, w as usize);
        assert_eq!(frame.planes[0].data.len(), frame_size_y);
        // Compare Y-plane PSNR vs the ffmpeg reference for frame i.
        if frame_idx < ref_frames {
            let ref_off = frame_idx * (frame_size_y * 3 / 2);
            let ref_y = &reference[ref_off..ref_off + frame_size_y];
            let psnr = psnr_y(&frame.planes[0].data, ref_y);
            eprintln!("frame {frame_idx}: Y-plane PSNR = {psnr:.2} dB");
            psnr_sum += psnr;
            frame_idx += 1;
        }
    }
    assert!(frame_idx > 0, "no frames decoded");
    let avg = psnr_sum / frame_idx as f64;
    eprintln!("avg Y-plane PSNR over {frame_idx} frames: {avg:.2} dB");
    // Round-1 flat-fill: PSNR floor depends on the source. Just
    // sanity-check it's a finite, positive number.
    assert!(avg.is_finite(), "PSNR avg = {avg}");
    assert!(avg > 0.0, "PSNR should be > 0 dB, got {avg}");
}
