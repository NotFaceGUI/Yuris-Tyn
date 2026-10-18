//! `compat::Audio`（与 player-core 旧 `audio::Audio` 同签名）测试。

use yuris_audio::{Audio, SoundInfoQuery, SoundInfoValue};

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn bgm_volume_permille_maps_to_engine_volume() {
    let mut audio = Audio::headless();
    audio.play_bgm("theme", fixture("tone.ogg"), Some(800));
    assert!(audio.bgm_playing());
    // 800‰ → 205 / 256
    assert_eq!(
        audio
            .system()
            .sound_info("BGM", SoundInfoQuery::Volume)
            .unwrap(),
        SoundInfoValue::Int(205)
    );
    // 同名再调音量不重启：TIME 不清零。
    audio.system().render_frames(4_800).unwrap();
    let before = audio
        .system()
        .sound_info("BGM", SoundInfoQuery::Time)
        .unwrap();
    audio.play_bgm("theme", fixture("tone.ogg"), Some(500));
    let after = audio
        .system()
        .sound_info("BGM", SoundInfoQuery::Time)
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        audio
            .system()
            .sound_info("BGM", SoundInfoQuery::Volume)
            .unwrap(),
        SoundInfoValue::Int(128)
    );
}

#[test]
fn empty_data_only_adjusts_volume() {
    let mut audio = Audio::headless();
    audio.play_bgm("theme", fixture("tone.ogg"), Some(1000));
    audio.system().render_frames(800).unwrap();
    let t0 = audio
        .system()
        .sound_info("BGM", SoundInfoQuery::Time)
        .unwrap();
    audio.play_bgm("__cur__", Vec::new(), Some(250));
    let t1 = audio
        .system()
        .sound_info("BGM", SoundInfoQuery::Time)
        .unwrap();
    assert_eq!(t0, t1);
    assert_eq!(
        audio
            .system()
            .sound_info("BGM", SoundInfoQuery::Volume)
            .unwrap(),
        SoundInfoValue::Int(64)
    );
}

#[test]
fn different_bgm_replaces_current() {
    let mut audio = Audio::headless();
    audio.play_bgm("a", fixture("tone.wav"), None);
    audio.system().render_frames(400).unwrap();
    audio.play_bgm("b", fixture("tone.ogg"), None);
    assert_eq!(
        audio
            .system()
            .sound_info("BGM", SoundInfoQuery::File)
            .unwrap(),
        SoundInfoValue::Str("b".into())
    );
    assert_eq!(
        audio
            .system()
            .sound_info("BGM", SoundInfoQuery::Time)
            .unwrap(),
        SoundInfoValue::Int(0)
    );
}

#[test]
fn voice_replaces_previous_voice() {
    let mut audio = Audio::headless();
    audio.play_voice("v1", fixture("tone.wav"));
    audio.system().render_frames(200).unwrap();
    audio.play_voice("v2", fixture("tone.wav"));
    assert_eq!(
        audio
            .system()
            .sound_info("VOICE", SoundInfoQuery::File)
            .unwrap(),
        SoundInfoValue::Str("v2".into())
    );
    assert_eq!(
        audio
            .system()
            .sound_info("VOICE", SoundInfoQuery::Time)
            .unwrap(),
        SoundInfoValue::Int(0)
    );
}

#[test]
fn se_log_records_decisions_even_without_bytes() {
    let mut audio = Audio::headless();
    audio.play_se("sse01", Vec::new());
    audio.play_se("sse02", fixture("tone.wav"));
    assert_eq!(audio.se_log, vec!["sse01", "sse02"]);
    // 未命中（空数据）只留痕、不占播放槽 ⇒ 真正发声的 SE 落在 SE#0。
    assert!(audio.system().is_playing("SE#0").unwrap());
    assert!(!audio.system().is_playing("SE#1").unwrap());
}

#[test]
fn se_log_caps_at_32() {
    let mut audio = Audio::headless();
    for i in 0..40 {
        audio.play_se(&format!("s{i}"), Vec::new());
    }
    assert_eq!(audio.se_log.len(), 32);
    assert_eq!(audio.se_log.first().unwrap(), "s8");
    assert_eq!(audio.se_log.last().unwrap(), "s39");
}

#[test]
fn stop_bgm_clears_current() {
    let mut audio = Audio::headless();
    audio.play_bgm("theme", fixture("tone.ogg"), None);
    audio.stop_bgm();
    assert!(!audio.bgm_playing());
    assert_eq!(
        audio
            .system()
            .sound_info("BGM", SoundInfoQuery::Exist)
            .unwrap(),
        SoundInfoValue::Int(0)
    );
}
