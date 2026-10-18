//! 只读探针：解码任意 OGG/WAV 并打印元信息与关键样本（实测封包条目用）。
//!
//! 用法：cargo run -p yuris-audio --example decode_probe -- <audio.ogg|wav>

use yuris_audio::{decode, SoundInfoQuery, SoundCommand, AudioSystem};

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_probe <audio>");
    let bytes = std::fs::read(&path).expect("read audio");
    let t0 = std::time::Instant::now();
    let decoded = decode(&bytes).expect("decode");
    let elapsed = t0.elapsed();
    println!(
        "codec={:?} ch={} rate={} bits={} frames={} duration_ms={} decode_ms={:.1}",
        decoded.info.codec,
        decoded.info.channels,
        decoded.info.sample_rate,
        decoded.info.bits_per_sample,
        decoded.frames(),
        decoded.info.duration_ms(),
        elapsed.as_secs_f64() * 1000.0
    );
    let peak = decoded.samples.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    println!("peak={peak:.4} samples={}", decoded.samples.len());

    // 声层查询走一遍（无头，8k 便于快速渲染）。
    let mut audio = AudioSystem::headless_at(Box::new(|_| None), 8_000);
    audio.load_bytes("BGM", &path, &bytes).expect("load");
    let mut cmd = SoundCommand::new("BGM");
    cmd.play = Some(1);
    cmd.loop_count = Some(0);
    audio.sound(&cmd).expect("sound");
    audio.render_frames(8_000).expect("render 1s");
    println!(
        "play={:?} time_ms={:?} total_ms={:?} codec={:?}",
        audio.sound_info("BGM", SoundInfoQuery::Play).unwrap(),
        audio.sound_info("BGM", SoundInfoQuery::Time).unwrap(),
        audio.sound_info("BGM", SoundInfoQuery::TotalTime).unwrap(),
        audio.sound_info("BGM", SoundInfoQuery::Codec).unwrap()
    );
}
