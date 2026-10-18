//! `RuntimeAudio`（L4 `AudioBackend` 适配器）测试。

use std::sync::{Arc, Mutex};

use yuris_audio::{AudioSystem, RuntimeAudio, SoundInfoQuery, SoundInfoValue};
use yuris_runtime::{AudioBackend, BackendError};
use yuris_scene::ResourceId;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn play_bgm_registers_and_starts_looping_layer() {
    let mut audio = RuntimeAudio::headless();
    let id = ResourceId(7);
    audio.register(id, fixture("tone.ogg"));
    audio.play_bgm(id).unwrap();
    assert!(audio.bgm_playing());
    let snap = audio.system().layer("BGM").unwrap().unwrap();
    assert_eq!(snap.loop_setting, 0);
    assert_eq!(snap.state, yuris_audio::PlayState::Playing);
}

#[test]
fn unknown_resource_is_reported_not_silently_ignored() {
    let mut audio = RuntimeAudio::headless();
    let id = ResourceId(42);
    match audio.play_bgm(id) {
        Err(BackendError::ResourceNotFound(got)) => assert_eq!(got.0, 42),
        other => panic!("expected ResourceNotFound, got {other:?}"),
    }
    match audio.play_se(id) {
        Err(BackendError::ResourceNotFound(_)) => {}
        other => panic!("expected ResourceNotFound, got {other:?}"),
    }
}

#[test]
fn se_layers_rotate_and_allow_concurrency() {
    let mut audio = RuntimeAudio::headless();
    let bytes = fixture("tone.wav");
    for i in 0..10 {
        let id = ResourceId(i);
        audio.register(id, bytes.clone());
        audio.play_se(id).unwrap();
    }
    let ids = audio.system().layer_ids().unwrap();
    assert_eq!(ids.len(), 8, "SE 轮转 8 个声层: {ids:?}");
    assert!(ids.iter().all(|id| id.starts_with("SE#")));
}

#[test]
fn stop_bgm_removes_layer() {
    let mut audio = RuntimeAudio::headless();
    let id = ResourceId(1);
    audio.register(id, fixture("tone.ogg"));
    audio.play_bgm(id).unwrap();
    assert!(audio.bgm_playing());
    audio.stop_bgm().unwrap();
    assert!(!audio.bgm_playing());
    assert_eq!(
        audio
            .system()
            .sound_info("BGM", SoundInfoQuery::Exist)
            .unwrap(),
        SoundInfoValue::Int(0)
    );
}

#[test]
fn unregister_drops_bytes() {
    let mut audio = RuntimeAudio::headless();
    let id = ResourceId(3);
    audio.register(id, fixture("tone.wav"));
    assert!(audio.unregister(id).is_some());
    match audio.play_se(id) {
        Err(BackendError::ResourceNotFound(_)) => {}
        other => panic!("expected ResourceNotFound, got {other:?}"),
    }
}

#[test]
fn custom_loader_is_used_by_audio_system() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls2 = calls.clone();
    let bytes = fixture("tone.wav");
    let mut system = AudioSystem::headless(Box::new(move |name: &str| {
        calls2.lock().unwrap().push(name.to_string());
        if name == "se/click.wav" {
            Some(bytes.clone())
        } else {
            None
        }
    }));
    let mut cmd = yuris_audio::SoundCommand::new("SE");
    cmd.file = Some("se/click".into());
    cmd.play = Some(1);
    system.sound(&cmd).unwrap();
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["se/click", "se/click.ogg", "se/click.OGG", "se/click.wav"]
    );
    assert!(system.is_playing("SE").unwrap());
}
