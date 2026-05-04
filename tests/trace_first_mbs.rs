//! Diagnostic: trace the first ~20 MBs of an ffmpeg-encoded testsrc
//! 176×144 SVQ1 packet and print what the walker reads at each leaf
//! / split.
//!
//! Round 3 used this harness to confirm two things:
//!
//! 1. **The bit-reader and walker are bit-position correct.** Every
//!    split-flag, multistage-VLC, mean-VLC and 4-bit codebook index
//!    we read on the testsrc fixture matches the actual bitstream
//!    bytes (verified by hand against §14 of the trace doc).
//!
//! 2. **The FFmpeg encoder routinely emits `count > 0` at L=5** on
//!    real (non-uniform) testsrc content — for example MB(32,0) of
//!    a 176×144 testsrc I-frame is coded as a single L=5 leaf with
//!    `count=2`, `mean=26`, two codebook indices. The trace doc
//!    (§7 lines 416–431) claims this never occurs, but the
//!    bitstream contradicts it. Decoding such leaves correctly
//!    requires the L=4 (16×8) and L=5 (16×16) codebooks, which the
//!    trace doc does NOT transcribe (§14.7 lists only the four
//!    4×2 / 4×4 / 8×4 / 8×8 codebooks). This is the round-3 docs
//!    gap, recorded in the CHANGELOG and the README.
//!
//! Skipped if ffmpeg/ffprobe aren't on PATH; printed via `--nocapture`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use oxideav_core::bits::BitReader;
use oxideav_svq::v1::header::parse_header;
use oxideav_svq::v1::tables::{INTRA_MEAN_VLC, INTRA_MULTISTAGE_VLC};
use oxideav_svq::v1::vlc::Vlc;

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

fn cached_packet_one(mov: &PathBuf) -> Vec<u8> {
    if !mov.exists() {
        let _ = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=176x144:rate=30:duration=0.05",
                "-c:v",
                "svq1",
                "-frames:v",
                "1",
                "-y",
            ])
            .arg(mov)
            .status()
            .expect("ffmpeg encode");
    }
    let raw = fs::read(mov).expect("mov");
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
        .arg(mov)
        .output()
        .expect("ffprobe");
    let csv = std::str::from_utf8(&out.stdout).unwrap();
    let line = csv.lines().next().unwrap();
    let cols: Vec<&str> = line.split(',').collect();
    let size: usize = cols[8].parse().unwrap();
    let pos: usize = cols[9].parse().unwrap();
    raw[pos..pos + size].to_vec()
}

#[test]
fn trace_first_mbs_of_testsrc() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not on PATH — skipping");
        return;
    }
    let dir = std::env::temp_dir().join("oxideav_svq_trace_first_mbs");
    fs::create_dir_all(&dir).unwrap();
    let mov = dir.join("testsrc.mov");
    let pkt = cached_packet_one(&mov);
    eprintln!("packet len = {}", pkt.len());

    let (hdr, mut br) = parse_header(&pkt, None).expect("header");
    eprintln!(
        "header: {:?} {}x{} body bit pos = {}",
        hdr.frame_type,
        hdr.width,
        hdr.height,
        br.bit_position()
    );

    let intra_multi: Vec<Vlc> = INTRA_MULTISTAGE_VLC.iter().map(|t| Vlc::new(t)).collect();
    let intra_mean = Vlc::new(INTRA_MEAN_VLC);

    // Walk first 20 MBs of the luma plane. For each one, recursively
    // print the split-flag/count/mean reads we observe.
    for mb_idx in 0..20 {
        let mb_x = (mb_idx % 11) * 16;
        let mb_y = (mb_idx / 11) * 16;
        let bit0 = br.bit_position();
        eprintln!("=== MB#{mb_idx} at ({mb_x},{mb_y}), start bit {bit0} ===");
        walk(&mut br, &intra_multi, &intra_mean, 5, "  ").unwrap();
        let bit1 = br.bit_position();
        eprintln!("    consumed {} bits", bit1 - bit0);
    }
}

fn walk(
    br: &mut BitReader<'_>,
    intra_multi: &[Vlc],
    intra_mean: &Vlc,
    level: u8,
    indent: &str,
) -> Result<(), oxideav_core::Error> {
    if level > 0 {
        let bp = br.bit_position();
        let split = br.read_u32(1)?;
        eprintln!("{indent}L={level} bit{bp} split={split}");
        if split == 1 {
            walk(
                br,
                intra_multi,
                intra_mean,
                level - 1,
                &format!("{indent}  "),
            )?;
            walk(
                br,
                intra_multi,
                intra_mean,
                level - 1,
                &format!("{indent}  "),
            )?;
            return Ok(());
        }
    }
    let bp = br.bit_position();
    let count = intra_multi[level as usize].decode(br)?;
    let after_count = br.bit_position();
    eprintln!(
        "{indent}L={level} bit{bp} LEAF count={count} (count-VLC consumed {} bits)",
        after_count - bp
    );
    if count == -1 {
        eprintln!("{indent}  ⚠ count=-1 in INTRA path (should never happen!)");
        return Ok(());
    }
    let bp = br.bit_position();
    let mean = intra_mean.decode(br)?;
    let after_mean = br.bit_position();
    eprintln!(
        "{indent}L={level} bit{bp} mean={mean} (mean-VLC consumed {} bits)",
        after_mean - bp
    );
    if count > 0 {
        if level >= 4 {
            eprintln!("{indent}  ⚠ count={count} at L={level} — illegal per trace doc §7");
        }
        for stage in 0..(count as usize) {
            let idx = br.read_u32(4)?;
            eprintln!("{indent}  stage{stage}: idx={idx}");
        }
    }
    Ok(())
}
