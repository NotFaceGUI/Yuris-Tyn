//! `yuris_runtime::AudioBackend` 适配器。
//!
//! L4 trait 只有 `play_bgm / play_se / stop_bgm` 三个动作、且 `ResourceId`
//! 不携带文件名，因此这里维护 `ResourceId → 字节` 注册表：驱动层在加载资源
//! 时调用 [`RuntimeAudio::register`]，播放时按注册表取字节交给
//! [`AudioSystem`]。
//!
//! 桥接约定（不是引擎真值）：
//! - BGM → 声层 `BGM`，无限循环；
//! - SE → 轮转声层 `SE#0..SE#7`，各播一次（允许短音效并发）；
//! - 语音请直接用 [`AudioSystem::play_voice`]（固定声层 `VOICE`，新替旧）。

use std::collections::HashMap;
use std::sync::Arc;

use yuris_runtime::{AudioBackend, BackendError};
use yuris_scene::ResourceId;

use crate::engine::AudioSystem;
use crate::error::AudioError;
use crate::mixer::PlayState;

/// 面向 L4 `AudioBackend` 的音频适配器。
pub struct RuntimeAudio {
    system: AudioSystem,
    resources: HashMap<u64, Arc<Vec<u8>>>,
}

impl RuntimeAudio {
    /// 创建无头适配器（测试/服务端）。
    pub fn headless() -> Self {
        Self {
            system: AudioSystem::headless(Box::new(|_| None)),
            resources: HashMap::new(),
        }
    }

    /// 创建带默认声卡的适配器（无设备时自动降级）。
    pub fn new() -> Self {
        Self {
            system: AudioSystem::with_device(Box::new(|_| None)),
            resources: HashMap::new(),
        }
    }

    /// 注册 `ResourceId` 对应的音频字节。
    pub fn register(&mut self, id: ResourceId, bytes: Vec<u8>) {
        self.resources.insert(id.0, Arc::new(bytes));
    }

    /// 释放注册表里的资源。
    pub fn unregister(&mut self, id: ResourceId) -> Option<Arc<Vec<u8>>> {
        self.resources.remove(&id.0)
    }

    /// 内层音频系统。
    pub fn system(&self) -> &AudioSystem {
        &self.system
    }

    /// 内层音频系统（可变）。
    pub fn system_mut(&mut self) -> &mut AudioSystem {
        &mut self.system
    }

    /// 当前 BGM 是否在播放。
    pub fn bgm_playing(&self) -> bool {
        self.system
            .layer("BGM")
            .ok()
            .flatten()
            .map(|s| s.state == PlayState::Playing)
            .unwrap_or(false)
    }

    fn bytes(&self, id: ResourceId) -> Result<Arc<Vec<u8>>, BackendError> {
        self.resources
            .get(&id.0)
            .cloned()
            .ok_or(BackendError::ResourceNotFound(id))
    }
}

impl Default for RuntimeAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBackend for RuntimeAudio {
    fn play_bgm(&mut self, id: ResourceId) -> yuris_runtime::Result<()> {
        let bytes = self.bytes(id)?;
        let name = format!("res:{}", id.0);
        self.system
            .play_bgm(&name, &bytes)
            .map(|_| ())
            .map_err(to_backend)
    }

    fn play_se(&mut self, id: ResourceId) -> yuris_runtime::Result<()> {
        let bytes = self.bytes(id)?;
        let name = format!("res:{}", id.0);
        self.system
            .play_se(&name, &bytes)
            .map(|_| ())
            .map_err(to_backend)
    }

    fn stop_bgm(&mut self) -> yuris_runtime::Result<()> {
        self.system.sound_end("BGM").map(|_| ()).map_err(to_backend)
    }
}

fn to_backend(e: AudioError) -> BackendError {
    BackendError::Other(e.to_string())
}
