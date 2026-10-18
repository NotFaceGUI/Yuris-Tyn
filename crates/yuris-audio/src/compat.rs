//! 与 `yuris-player-core` 现有 `audio::Audio` **同签名**的兼容层。
//!
//! 现有播放器直接调 `Audio::play_bgm(name, Vec<u8>, permille)` /
//! `play_voice` / `play_se`，且用 `se_log` 做决策断言。迁移到本 crate 时
//! 只需把导入从 `crate::audio::Audio` 换成 `yuris_audio::compat::Audio`，
//! 调用点不用改。
//!
//! 语义映射：
//! - `play_bgm` 的 `volume_permille`（0..1000）→ 引擎 `VOLUME`（0..256）；
//! - 空 `data` = 只调音量（`\BGM(,800)` 空首槽语义）；
//! - 同名 BGM 不重启，只调音量（与原实现一致）；
//! - voice 用声层 `VOICE`（新替旧，播一次）；
//! - SE 轮转 `SE#0..SE#7`（并发）；
//! - `se_log` 保留最近 32 条决策（未命中也会记录，与原实现一致）。
//!
//! 注意：这是**桥接约定**，不是引擎语义。脚本驱动的新代码应直接使用
//! [`AudioSystem`] + [`SoundCommand`]。

use crate::engine::AudioSystem;
use crate::error::Result;
use crate::sound::SoundCommand;

/// 兼容层音频对象。
pub struct Audio {
    system: AudioSystem,
    /// 当前 BGM 名（同名不重启）。
    cur_bgm: Option<String>,
    /// 当前 BGM 音量（0..1 原始值）。
    bgm_volume: f32,
    /// 最近 SE 播放决策留痕（含未命中；上限 32）。
    pub se_log: Vec<String>,
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

impl Audio {
    /// 打开默认声卡（无设备时自动降级无头）。
    pub fn new() -> Self {
        Self {
            system: AudioSystem::with_device(Box::new(|_| None)),
            cur_bgm: None,
            bgm_volume: 1.0,
            se_log: Vec::new(),
        }
    }

    /// 无头构造（测试/CI）。
    pub fn headless() -> Self {
        Self {
            system: AudioSystem::headless_at(Box::new(|_| None), 48_000),
            cur_bgm: None,
            bgm_volume: 1.0,
            se_log: Vec::new(),
        }
    }

    /// 内层系统（需要查询/高级操作时用）。
    pub fn system(&self) -> &AudioSystem {
        &self.system
    }

    /// 内层系统（可变）。
    pub fn system_mut(&mut self) -> &mut AudioSystem {
        &mut self.system
    }

    /// 播 BGM（循环）。`data` 为空 = 仅调音量。
    ///
    /// 与原实现一致：同名曲目重复调用不会重启；不同名先停旧曲。
    pub fn play_bgm(&mut self, name: &str, data: Vec<u8>, volume_permille: Option<i64>) {
        let volume = volume_permille.map(permille_to_volume);
        if data.is_empty() {
            // `\BGM(,800)`：仅调音量。
            if let Some(v) = volume {
                self.bgm_volume = v as f32 / crate::VOLUME_MAX as f32;
                let mut cmd = SoundCommand::new("BGM");
                cmd.volume = Some(v);
                cmd.play = Some(1); // 带 PLAY 时按目标值斜坡；无 FADE 立即生效
                let _ = self.system.sound(&cmd);
            }
            return;
        }
        if self.cur_bgm.as_deref() == Some(name) {
            if let Some(v) = volume {
                self.bgm_volume = v as f32 / crate::VOLUME_MAX as f32;
                let mut cmd = SoundCommand::new("BGM");
                cmd.volume = Some(v);
                cmd.play = Some(1);
                let _ = self.system.sound(&cmd);
            }
            return;
        }
        self.stop_bgm();
        match self.system.play_bgm(name, &data) {
            Ok(info) => {
                let mut cmd = SoundCommand::new("BGM");
                cmd.volume = Some(volume.unwrap_or((self.bgm_volume * 256.0).round() as i64));
                cmd.play = Some(1);
                let _ = self.system.sound(&cmd);
                self.cur_bgm = Some(name.to_string());
                yuris_log::info!("[audio] BGM {name} 循环 {info:?}");
            }
            Err(e) => {
                yuris_log::warn!("[audio] BGM {name} 解码失败: {e}");
            }
        }
    }

    /// 停止 BGM。
    pub fn stop_bgm(&mut self) {
        let _ = self.system.sound_end("BGM");
        self.cur_bgm = None;
    }

    /// 播 voice（声层 `VOICE`，新替旧）。
    pub fn play_voice(&mut self, name: &str, data: Vec<u8>) {
        match self.system.play_voice(name, &data) {
            Ok(_) => yuris_log::debug!("[audio] voice {name}"),
            Err(e) => yuris_log::warn!("[audio] voice 解码失败 {name}: {e}"),
        }
    }

    /// 播 SE（轮转 8 声层；`data` 为空 = 只记录决策不出声，与原实现一致）。
    pub fn play_se(&mut self, name: &str, data: Vec<u8>) {
        if self.se_log.len() >= 32 {
            self.se_log.remove(0);
        }
        self.se_log.push(name.to_string());
        if data.is_empty() {
            return;
        }
        if let Err(e) = self.system.play_se(name, &data) {
            yuris_log::warn!("[audio] SE 解码失败 {name}: {e}");
        }
    }

    /// 是否有 BGM 在播（新增，便于测试）。
    pub fn bgm_playing(&self) -> bool {
        self.system.is_playing("BGM").unwrap_or(false)
    }

    /// 执行任意 `SOUND` 命令（新增入口）。
    pub fn sound(&mut self, cmd: &SoundCommand) -> Result<()> {
        self.system.sound(cmd).map(|_| ())
    }
}

/// 千分比（0..1000）→ 引擎 `VOLUME`（0..256）。
fn permille_to_volume(permille: i64) -> i64 {
    ((permille.clamp(0, 1000) as f32 / 1000.0) * 256.0).round() as i64
}
