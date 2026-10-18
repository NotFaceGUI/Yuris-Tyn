//! 音频系统端到端测试：真实 WAV / OGG 字节 → 解码 → 声层命令 → 查询 → 渲染。
//!
//! 夹具由 ffmpeg 生成（0.25 s，440 Hz）：
//! - `fixtures/tone.wav`：8000 Hz / 单声道 / PCM S16LE；
//! - `fixtures/tone.ogg`：8000 Hz / 立体声 / Vorbis。

use std::sync::{Arc, Mutex};

use yuris_audio::{
    decode, search_candidates, AudioCodec, AudioSystem, SoundCommand, SoundInfoQuery,
    SoundInfoValue, SoundOutcome,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn loader_with(files: Vec<(&'static str, Vec<u8>)>) -> (AudioSystem, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls2 = calls.clone();
    let files: Vec<(String, Vec<u8>)> =
        files.into_iter().map(|(n, d)| (n.to_string(), d)).collect();
    let system = AudioSystem::headless_at(
        Box::new(move |name: &str| {
            calls2.lock().unwrap().push(name.to_string());
            files
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, d)| d.clone())
        }),
        8_000,
    );
    (system, calls)
}

#[test]
fn decodes_pcm_wav() {
    let decoded = decode(&fixture("tone.wav")).expect("decode wav");
    assert_eq!(decoded.info.codec, AudioCodec::PcmWav);
    assert_eq!(decoded.info.channels, 1);
    assert_eq!(decoded.info.sample_rate, 8_000);
    assert_eq!(decoded.info.bits_per_sample, 16);
    // 0.25 s ≈ 2000 帧（ffmpeg 可能多/少 1 帧）。
    assert!(
        (decoded.frames() as i64 - 2000).abs() <= 1,
        "frames={}",
        decoded.frames()
    );
    assert_eq!(decoded.info.duration_ms(), 250);
    assert!(decoded.samples.iter().any(|s| s.abs() > 0.1));
}

#[test]
fn decodes_ogg_vorbis() {
    let decoded = decode(&fixture("tone.ogg")).expect("decode ogg");
    assert_eq!(decoded.info.codec, AudioCodec::OggVorbis);
    assert_eq!(decoded.info.channels, 2);
    assert_eq!(decoded.info.sample_rate, 8_000);
    // Vorbis 没有固定位深：引擎按 16 位 PCM 处理（DATALEFT/RIGHT 路径）。
    assert_eq!(decoded.info.bits_per_sample, 16);
    assert!(
        (decoded.frames() as i64 - 2000).abs() <= 64,
        "frames={}",
        decoded.frames()
    );
    // 正弦幅度经 Vorbis 有损压缩后会有衰减；只验证确有能量。
    assert!(
        decoded.samples.iter().any(|s| s.abs() > 0.01),
        "peak={}",
        decoded.samples.iter().fold(0.0f32, |a, s| a.max(s.abs()))
    );
}

#[test]
fn rejects_garbage_and_empty_payloads() {
    assert!(decode(&[]).is_err());
    assert!(decode(b"not an audio file at all").is_err());
}

#[test]
fn extension_search_follows_manual_order() {
    assert_eq!(
        search_candidates("bgm\\theme"),
        vec![
            "bgm\\theme",
            "bgm\\theme.ogg",
            "bgm\\theme.OGG",
            "bgm\\theme.wav",
            "bgm\\theme.WAV"
        ]
    );
    assert_eq!(search_candidates("bgm/theme.ogg"), vec!["bgm/theme.ogg"]);
    assert!(search_candidates("   ").is_empty());
}

#[test]
fn sound_load_play_and_query_across_channels() {
    let (mut audio, calls) = loader_with(vec![("bgm/theme.ogg", fixture("tone.ogg"))]);
    let mut cmd = SoundCommand::new("BGM");
    cmd.file = Some("bgm/theme".into());
    cmd.pan = Some(100);
    cmd.pitch = Some(300);
    cmd.start_ms = Some(50);
    cmd.end_ms = Some(200);
    cmd.loop_count = Some(3);
    cmd.volume = Some(200);
    cmd.play = Some(1);

    let outcome = audio.sound(&cmd).expect("sound");
    match outcome {
        SoundOutcome::Loaded { info } => {
            assert_eq!(info.codec, AudioCodec::OggVorbis);
            assert_eq!(info.channels, 2);
        }
        other => panic!("expected Loaded, got {other:?}"),
    }
    // 手册搜索顺序：先无扩展名，再 .ogg。loader 只命中后者。
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["bgm/theme", "bgm/theme.ogg"]
    );

    // FILE 查询返回脚本原始名（未展开扩展名）。
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::File).unwrap(),
        SoundInfoValue::Str("bgm/theme".into())
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Exist).unwrap(),
        SoundInfoValue::Int(1)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Play).unwrap(),
        SoundInfoValue::Int(1)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Pan).unwrap(),
        SoundInfoValue::Int(100)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Pitch).unwrap(),
        SoundInfoValue::Int(300)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Start).unwrap(),
        SoundInfoValue::Int(50)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::End).unwrap(),
        SoundInfoValue::Int(200)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Loop).unwrap(),
        SoundInfoValue::Int(3)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Codec).unwrap(),
        SoundInfoValue::Int(1) // v494 二进制：OGG = 1
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::FileType).unwrap(),
        SoundInfoValue::Int(1)
    );
    // OGG 有编码器尾部填充 ⇒ 时长略大于 250 ms。
    let total = audio
        .sound_info("BGM", SoundInfoQuery::TotalTime)
        .unwrap()
        .as_int()
        .unwrap();
    assert!((250..=280).contains(&total), "total={total}");
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Channel).unwrap(),
        SoundInfoValue::Int(2)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Sampling).unwrap(),
        SoundInfoValue::Int(8_000)
    );

    // 播完 3 次（每次 END=200ms 截断）后停止。
    audio.render_frames(8_000).unwrap(); // 1 s，足够 3 × 200 ms
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Play).unwrap(),
        SoundInfoValue::Int(0)
    );
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::LoopCount).unwrap(),
        SoundInfoValue::Int(2)
    );
}

#[test]
fn sound_missing_file_does_not_create_layer() {
    let (mut audio, _) = loader_with(Vec::new());
    let mut cmd = SoundCommand::new("BGM");
    cmd.file = Some("nope".into());
    cmd.play = Some(1);
    assert!(audio.sound(&cmd).is_err());
    assert_eq!(
        audio.sound_info("BGM", SoundInfoQuery::Exist).unwrap(),
        SoundInfoValue::Int(0)
    );
}

#[test]
fn sound_on_missing_layer_without_file_is_ignored() {
    let (mut audio, _) = loader_with(Vec::new());
    let mut cmd = SoundCommand::new("BGM");
    cmd.volume = Some(128);
    assert_eq!(audio.sound(&cmd).unwrap(), SoundOutcome::IgnoredNoLayer);
    assert!(audio.layer_ids().unwrap().is_empty());
}

#[test]
fn sound_end_removes_layer() {
    let (mut audio, _) = loader_with(vec![("se/click.wav", fixture("tone.wav"))]);
    let mut cmd = SoundCommand::new("SE");
    cmd.file = Some("se/click.wav".into());
    cmd.play = Some(1);
    audio.sound(&cmd).unwrap();
    assert_eq!(
        audio.sound_info("SE", SoundInfoQuery::Exist).unwrap(),
        SoundInfoValue::Int(1)
    );
    assert!(audio.sound_end("SE").unwrap());
    assert_eq!(
        audio.sound_info("SE", SoundInfoQuery::Exist).unwrap(),
        SoundInfoValue::Int(0)
    );
    assert!(!audio.sound_end("SE").unwrap());
}

#[test]
fn invalid_parameters_are_rejected_before_state_change() {
    let (mut audio, _) = loader_with(vec![("se/click.wav", fixture("tone.wav"))]);
    let mut load = SoundCommand::new("SE");
    load.file = Some("se/click.wav".into());
    audio.sound(&load).unwrap();

    let mut bad = SoundCommand::new("SE");
    bad.volume = Some(300);
    assert!(audio.sound(&bad).is_err());
    let mut bad = SoundCommand::new("SE");
    bad.pan = Some(-1);
    assert!(audio.sound(&bad).is_err());
    let mut bad = SoundCommand::new("SE");
    bad.pitch = Some(513);
    assert!(audio.sound(&bad).is_err());
    let mut bad = SoundCommand::new("SE");
    bad.loop_count = Some(-1);
    assert!(audio.sound(&bad).is_err());
    let mut bad = SoundCommand::new("SE");
    bad.end_ms = Some(-1);
    assert!(audio.sound(&bad).is_err());

    // 校验失败不得半途改状态。
    let snap = audio.layer("SE").unwrap().unwrap();
    assert_eq!(snap.volume, 256);
    assert_eq!(snap.pan, 256);
    assert_eq!(snap.pitch, 256);
}

#[test]
fn convenience_helpers_play_bgm_voice_and_se() {
    let (mut audio, _) = loader_with(Vec::new());
    audio.play_bgm("theme", &fixture("tone.ogg")).unwrap();
    audio.play_voice("v001", &fixture("tone.wav")).unwrap();
    audio.play_se("sse01", &fixture("tone.wav")).unwrap();
    audio.play_se("sse02", &fixture("tone.wav")).unwrap();
    let ids = audio.layer_ids().unwrap();
    assert_eq!(ids, vec!["BGM", "VOICE", "SE#0", "SE#1"]);
    assert!(audio.is_playing("BGM").unwrap());
    // BGM 无限循环 → 渲染 10 秒后仍在播放。
    audio.render_frames(80_000).unwrap();
    assert!(audio.is_playing("BGM").unwrap());
    // Voice / SE 只播一次 → 已结束。
    assert!(!audio.is_playing("VOICE").unwrap());
    assert!(!audio.is_playing("SE#0").unwrap());
}

#[test]
fn headless_mode_reports_no_device() {
    let (audio, _) = loader_with(Vec::new());
    assert!(!audio.has_device());
    assert_eq!(audio.output_rate(), 8_000);
}
