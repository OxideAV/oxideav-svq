//! Standalone test: decode a flat-gray fixture and compare PSNR.
//!
//! A solid-gray testsrc input has a tiny encoded form (every MB is a
//! single mean-only fill at L=5). This isolates the multistage VLC +
//! mean VLC decoding from the codebook lookups, since count is always
//! 0 and no codebook contributions are made.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use oxideav_core::{CodecId, Decoder, Frame, Packet, TimeBase};
use oxideav_svq::v1::decoder::Svq1Decoder;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn decode_solid_gray_matches_reference() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not on PATH — skipping");
        return;
    }
    let dir = std::env::temp_dir().join("oxideav_svq_gray");
    fs::create_dir_all(&dir).expect("mkdir");
    let mov: PathBuf = dir.join("gray.mov");
    let ref_yuv: PathBuf = dir.join("gray_ref.yuv");
    let _ = fs::remove_file(&mov);
    let _ = fs::remove_file(&ref_yuv);

    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:size=160x120:rate=30:duration=0.05",
            "-c:v",
            "svq1",
            "-frames:v",
            "1",
            "-y",
        ])
        .arg(&mov)
        .status()
        .expect("ffmpeg encode");
    assert!(st.success(), "ffmpeg svq1 encode failed");

    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&mov)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-y"])
        .arg(&ref_yuv)
        .status()
        .expect("ffmpeg decode");
    assert!(st.success(), "ffmpeg svq1 decode failed");

    // The encoded packet sits at offset 36 in the QuickTime file, length 112.
    let bytes = fs::read(&mov).expect("read mov");
    // Use ffprobe to find packet
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
        .arg(&mov)
        .output()
        .expect("ffprobe");
    let csv = std::str::from_utf8(&out.stdout).unwrap();
    let line = csv.lines().next().unwrap();
    let cols: Vec<&str> = line.split(',').collect();
    let pkt_size: usize = cols[8].parse().unwrap();
    let pkt_pos: usize = cols[9].parse().unwrap();
    let pkt_data = &bytes[pkt_pos..pkt_pos + pkt_size];

    let mut dec = Svq1Decoder::new(CodecId::new("svq1"));
    let pkt = Packet::new(0, TimeBase::new(1, 30), pkt_data.to_vec());
    dec.send_packet(&pkt).expect("send");
    let frame = match dec.receive_frame().expect("recv") {
        Frame::Video(vf) => vf,
        _ => panic!("expected video"),
    };
    let y = &frame.planes[0].data;
    eprintln!(
        "gray Y plane min/max/mean: {}/{}/{}",
        y.iter().min().unwrap(),
        y.iter().max().unwrap(),
        y.iter().map(|&b| b as u32).sum::<u32>() / y.len() as u32
    );

    // Reference: ffmpeg's gray fixture
    let ref_data = fs::read(&ref_yuv).expect("read ref");
    let ref_y = &ref_data[..160 * 120];

    // Compute PSNR
    let mut sse: f64 = 0.0;
    for (a, b) in y.iter().zip(ref_y.iter()) {
        let d = (*a as f64) - (*b as f64);
        sse += d * d;
    }
    let mse = sse / y.len() as f64;
    let psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255f64 * 255.0 / mse).log10()
    };
    eprintln!("gray Y PSNR: {psnr} dB");
    assert!(
        psnr > 50.0,
        "expected gray to decode bit-exactly, got PSNR={psnr}"
    );
}
