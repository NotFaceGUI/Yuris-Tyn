//! 音频系统门面：资源装载 → 解码 → 声层命令 → 声卡/无头输出。
//!
//! 与引擎的分工一致：脚本层只表达 `SOUND`/`SOUNDEND`/`SOUNDINFO`；本模块
//! 负责把逻辑文件名解析成字节（调用方注入资源加载器），解码后交给
//! [`Mixer`]；声卡输出只是 [`Mixer::render`] 的一个消费者。
//!
//! 无设备时自动降级为“无头”模式：命令、状态查询与时间推进照常工作，
//! 只是不写声卡（`has_device() == false`，`device_error()` 记录原因）。

use std::sync::{Arc, Mutex};

use crate::decode::decode;
use crate::error::{AudioError, Result};
use crate::info::AudioInfo;
use crate::mixer::{LayerSnapshot, Mixer, PlayState};
use crate::sound::{search_candidates, SoundCommand, SoundInfoQuery, SoundInfoValue, SoundOutcome};

#[cfg(feature = "device")]
use crate::output::OutputDevice;

/// 资源加载器：逻辑名（含候选扩展名）→ 条目字节。
pub type AudioLoader = Box<dyn FnMut(&str) -> Option<Vec<u8>> + Send>;

/// 音频系统。
pub struct AudioSystem {
    mixer: Arc<Mutex<Mixer>>,
    loader: AudioLoader,
    output_rate: u32,
    #[cfg(feature = "device")]
    output: Option<OutputDevice>,
    device_error: Option<String>,
    se_slot: usize,
}

impl AudioSystem {
    /// 无头模式（确定性渲染；不打开声卡）。
    pub fn headless(loader: AudioLoader) -> Self {
        Self::headless_at(loader, 48_000)
    }

    /// 指定输出采样率的无头模式（测试/离线渲染）。
    pub fn headless_at(loader: AudioLoader, output_rate: u32) -> Self {
        let mixer = Arc::new(Mutex::new(Mixer::new(output_rate)));
        let output_rate = mixer.lock().map(|m| m.output_rate()).unwrap_or(output_rate);
        Self {
            mixer,
            loader,
            output_rate,
            #[cfg(feature = "device")]
            output: None,
            device_error: None,
            se_slot: 0,
        }
    }

    /// 空加载器（所有 `FILE=` 都会 `NotFound`；配合 [`AudioSystem::load_bytes`] 使用）。
    pub fn new() -> Self {
        Self::headless(Box::new(|_| None))
    }

    /// 打开默认声卡；失败时降级为无头模式并记录原因。
    pub fn with_device(loader: AudioLoader) -> Self {
        #[cfg(feature = "device")]
        {
            Self::open_device(loader)
        }
        #[cfg(not(feature = "device"))]
        {
            let mut system = Self::headless(loader);
            system.device_error = Some("built without the `device` feature".into());
            system
        }
    }

    #[cfg(feature = "device")]
    fn open_device(loader: AudioLoader) -> Self {
        let mixer = Arc::new(Mutex::new(Mixer::new(48_000)));
        match crate::output::open(mixer.clone()) {
            Ok(output) => {
                let rate = mixer.lock().map(|m| m.output_rate()).unwrap_or(48_000);
                Self {
                    mixer,
                    loader,
                    output_rate: rate,
                    output: Some(output),
                    device_error: None,
                    se_slot: 0,
                }
            }
            Err(e) => {
                yuris_log::warn!("音频设备不可用,降级无头播放: {e}");
                let rate = mixer.lock().map(|m| m.output_rate()).unwrap_or(48_000);
                Self {
                    mixer,
                    loader,
                    output_rate: rate,
                    output: None,
                    device_error: Some(e.to_string()),
                    se_slot: 0,
                }
            }
        }
    }

    /// 当前输出采样率。
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// 是否真正接上了声卡。
    pub fn has_device(&self) -> bool {
        #[cfg(feature = "device")]
        {
            self.output.is_some()
        }
        #[cfg(not(feature = "device"))]
        {
            false
        }
    }

    /// 最近一次设备打开失败原因。
    pub fn device_error(&self) -> Option<&str> {
        self.device_error.as_deref()
    }

    /// 执行一条 `SOUND` 命令。
    ///
    /// `FILE` 的处理顺序与引擎一致：先解析/解码（失败则不动现有声层），
    /// 再替换源，最后按 `E → START → RESTART → END → LOOP → VOLUME →
    /// PLAY → PAUSE` 应用参数。
    pub fn sound(&mut self, cmd: &SoundCommand) -> Result<SoundOutcome> {
        // 先校验全部参数：越界的命令不得替换现有音源。
        cmd.validate()?;
        let mut loaded: Option<AudioInfo> = None;
        if let Some(file) = cmd.file.clone() {
            let (data, _resolved) = self.load(&file)?;
            let decoded = decode(&data)?;
            loaded = Some(decoded.info);
            let mut mixer = self.lock()?;
            // `SOUNDINFO FILE` 返回脚本里写的原始名字（引擎装载路径同样
            // 保存事件字符串，扩展名解析只发生在打开文件时）。
            mixer.set_source(&cmd.id, &file, decoded)?;
        }
        let mut mixer = self.lock()?;
        let outcome = mixer.apply(cmd)?;
        Ok(match loaded {
            Some(info) => SoundOutcome::Loaded { info },
            None => outcome,
        })
    }

    /// 直接装载一段已解码字节（资源层已经拿到数据时）。
    pub fn load_bytes(
        &mut self,
        layer_id: &str,
        file_name: &str,
        bytes: &[u8],
    ) -> Result<AudioInfo> {
        let decoded = decode(bytes)?;
        let mut mixer = self.lock()?;
        mixer.set_source(layer_id, file_name, decoded)
    }

    /// 装载并立即播放（`loop_count`: 0 = 无限，N = 播放 N 次）。
    pub fn play_bytes(
        &mut self,
        layer_id: &str,
        file_name: &str,
        bytes: &[u8],
        loop_count: i64,
    ) -> Result<AudioInfo> {
        let info = self.load_bytes(layer_id, file_name, bytes)?;
        let mut cmd = SoundCommand::new(layer_id);
        cmd.loop_count = Some(loop_count);
        cmd.play = Some(1);
        self.sound(&cmd)?;
        Ok(info)
    }

    /// 播放 BGM（声层 `BGM`，无限循环）。
    pub fn play_bgm(&mut self, file_name: &str, bytes: &[u8]) -> Result<AudioInfo> {
        self.play_bytes("BGM", file_name, bytes, 0)
    }

    /// 播放语音（声层 `VOICE`，新语音替换旧语音；只播一次）。
    ///
    /// 这是给双脚本播放器的桥接约定；YSTB 侧的真实语音层 ID 由脚本决定。
    pub fn play_voice(&mut self, file_name: &str, bytes: &[u8]) -> Result<AudioInfo> {
        self.play_bytes("VOICE", file_name, bytes, 1)
    }

    /// 播放 SE（轮转 8 个声层，允许并发短音效；各只播一次）。
    ///
    /// 这是给双脚本播放器的桥接约定；引擎脚本可自行用任意 ID 建立声层。
    pub fn play_se(&mut self, file_name: &str, bytes: &[u8]) -> Result<AudioInfo> {
        let id = format!("SE#{}", self.se_slot % 8);
        self.se_slot = self.se_slot.wrapping_add(1);
        self.play_bytes(&id, file_name, bytes, 1)
    }

    /// 删除声层（`SOUNDEND`）。返回是否删除了已有声层。
    pub fn sound_end(&self, layer_id: &str) -> Result<bool> {
        Ok(self.lock()?.remove(layer_id))
    }

    /// `SOUNDINFO` 查询。
    pub fn sound_info(&self, layer_id: &str, query: SoundInfoQuery) -> Result<SoundInfoValue> {
        Ok(self.lock()?.query(layer_id, query))
    }

    /// 声层快照。
    pub fn layer(&self, layer_id: &str) -> Result<Option<LayerSnapshot>> {
        Ok(self.lock()?.snapshot(layer_id))
    }

    /// 所有声层 ID。
    pub fn layer_ids(&self) -> Result<Vec<String>> {
        Ok(self.lock()?.layer_ids())
    }

    /// 是否有声层在播放。
    pub fn is_playing(&self, layer_id: &str) -> Result<bool> {
        Ok(self
            .lock()?
            .snapshot(layer_id)
            .map(|s| s.state == PlayState::Playing)
            .unwrap_or(false))
    }

    /// 渲染交错立体声（无头测试/离线导出）。
    pub fn render(&self, out: &mut [f32]) -> Result<()> {
        self.lock()?.render(out);
        Ok(())
    }

    /// 渲染指定帧数。
    pub fn render_frames(&self, frames: usize) -> Result<Vec<f32>> {
        Ok(self.lock()?.render_frames(frames))
    }

    /// 修改输出采样率。仅对无头模式有意义；有声卡时设备速率固定，
    /// 变更会导致时间基准不一致，故忽略并返回 `false`。
    pub fn set_output_rate(&mut self, rate: u32) -> bool {
        if self.has_device() {
            return false;
        }
        if let Ok(mut m) = self.mixer.lock() {
            m.set_output_rate(rate);
            self.output_rate = m.output_rate();
            true
        } else {
            false
        }
    }

    fn load(&mut self, file: &str) -> Result<(Vec<u8>, String)> {
        let candidates = search_candidates(file);
        if candidates.is_empty() {
            return Err(AudioError::NotFound(file.to_string()));
        }
        for candidate in &candidates {
            if let Some(bytes) = (self.loader)(candidate) {
                return Ok((bytes, candidate.clone()));
            }
        }
        Err(AudioError::NotFound(file.to_string()))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Mixer>> {
        self.mixer
            .lock()
            .map_err(|_| AudioError::Internal("mixer mutex poisoned".into()))
    }
}

impl Default for AudioSystem {
    fn default() -> Self {
        Self::new()
    }
}

/// **状态的共享只读句柄**（`Arc<Mutex<Mixer>>` 的克隆）。
///
/// 用途:把「音频状态查询」交给别的层(如 `yuris-vm` 的 `SOUNDINFO`)而不
/// 让它依赖本 crate —— 句柄是纯数据面,不含资源加载器/声卡。
///
/// 与 [`AudioSystem`] 共享同一个 `Mixer`:句柄看到的就是当前播放状态,
/// 不是快照。
#[derive(Clone)]
pub struct StateHandle {
    mixer: Arc<Mutex<Mixer>>,
}

impl StateHandle {
    /// `SOUNDINFO` 查询（与 [`AudioSystem::sound_info`] 同一实现）。
    pub fn query(&self, layer_id: &str, query: SoundInfoQuery) -> SoundInfoValue {
        match self.mixer.lock() {
            Ok(m) => m.query(layer_id, query),
            // 互斥量中毒 = 音频线程已崩;按"无状态可答"返回,不冒充 0。
            Err(_) => SoundInfoValue::Unsupported,
        }
    }

    /// 声层快照。
    pub fn layer(&self, layer_id: &str) -> Option<LayerSnapshot> {
        self.mixer.lock().ok().and_then(|m| m.snapshot(layer_id))
    }
}

impl AudioSystem {
    /// 取状态共享句柄（见 [`StateHandle`]）。
    pub fn state_handle(&self) -> StateHandle {
        StateHandle {
            mixer: self.mixer.clone(),
        }
    }
}
