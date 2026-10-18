//! 只读探针：打开默认声卡，播放一段 OGG/WAV，并报告设备状态。
//!
//! 用法：cargo run -p yuris-audio --example device_probe -- <audio.ogg|wav>

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: device_probe <audio>");
    let bytes = std::fs::read(&path).expect("read audio");
    let mut audio = yuris_audio::AudioSystem::with_device(Box::new(|_| None));
    println!(
        "has_device={} rate={} err={:?}",
        audio.has_device(),
        audio.output_rate(),
        audio.device_error()
    );
    audio.play_bgm(&path, &bytes).expect("play");
    std::thread::sleep(std::time::Duration::from_millis(500));
    println!("playing={:?}", audio.is_playing("BGM"));
    let time = audio
        .sound_info("BGM", yuris_audio::SoundInfoQuery::Time)
        .unwrap();
    println!("time={time:?}");
}
