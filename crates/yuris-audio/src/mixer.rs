//! 声层状态机与混音器（纯逻辑，不依赖声卡，可确定性测试）。
//!
//! 模型直接对应引擎的 `SOUND` 声层（v494 反汇编 + 官方手册）：
//! - 声层以 `ID` 字符串为键，一个 ID 一条流；不同 ID 并行混音。
//! - `FILE` 装载替换源并复位播放位置/声像/音高/起止点，保留 `E`、音量、
//!   `LOOP`（`0x4246e0` 的装载路径只重置部分字段）。
//! - 音量是 0..256 的**原始值**；引擎 `FADE` 对原始值做线性斜坡
//!   （`0x424a1c` 的 step = Δvolume/ms），输出幅度 = `(volume/256)²`
//!   （`0x424f18` 换算 `4000·log10(v/256)` 百分之一分贝）。
//! - `PAN` 走 DirectSound 对数声像（`0x425088`）。
//! - 播放/停止/暂停/循环/淡变/音量/声像/音高全部是样本精确状态，不依赖
//!   墙钟；调用 [`Mixer::render`] 推进时间。
//!
//! 未取证的工程选择（`UNVERIFIED` 标注）：
//! - DirectSound 声像值到左右声道的具体曲线；
//! - 采样率转换/变调取线性插值；
//! - 混音溢出做硬限幅。

use std::sync::Arc;

use crate::decode::DecodedAudio;
use crate::error::{AudioError, Result};
use crate::info::AudioInfo;
use crate::sound::{
    SoundCommand, SoundInfoQuery, SoundInfoValue, SoundOutcome, PAN_CENTER, PITCH_NORMAL,
    VOLUME_MAX,
};

/// 声层播放状态。
///
/// `Playing` 覆盖「正常播放 / 暂停 / 淡出中」——与引擎 `SOUNDINFO PLAY`
/// 的判定一致（`+0x108 ∈ [2,4]`，v494 `0x44fa9c`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// 停止。
    Stopped,
    /// 播放中（含暂停、淡出中）。
    Playing,
}

/// 声层只读快照（日志、`SOUNDINFO`、测试用）。
#[derive(Debug, Clone, PartialEq)]
pub struct LayerSnapshot {
    /// 声层 ID。
    pub id: String,
    /// 源文件名（脚本里写的原始名，未展开扩展名）。
    pub file: String,
    /// 解码信息。
    pub info: AudioInfo,
    /// `E`。
    pub enabled: bool,
    /// 当前音量 0～256（淡变中为当前插值）。
    pub volume: i64,
    /// `PAN` 0～512。
    pub pan: i64,
    /// `PITCH` 0～512。
    pub pitch: i64,
    /// `START` 毫秒。
    pub start_ms: i64,
    /// `RESTART` 毫秒。
    pub restart_ms: i64,
    /// `END` 毫秒。
    pub end_ms: i64,
    /// `LOOP` 设置（0 = 无限）。
    pub loop_setting: i64,
    /// 已完成循环次数。
    pub loop_count: u64,
    /// 是否暂停。
    pub paused: bool,
    /// 播放状态。
    pub state: PlayState,
    /// 当前播放位置（毫秒）。
    pub time_ms: u64,
}

/// 0..1 线性包络（单位 = 引擎原始音量，不是幅度）。
#[derive(Debug, Clone, Copy)]
struct Envelope {
    current: f32,
    target: f32,
    remaining: u64,
    step: f32,
}

impl Envelope {
    fn instant(value: f32) -> Self {
        let value = value.clamp(0.0, 1.0);
        Self {
            current: value,
            target: value,
            remaining: 0,
            step: 0.0,
        }
    }

    fn ramp_to(&mut self, target: f32, frames: u64) {
        let target = target.clamp(0.0, 1.0);
        if frames == 0 {
            *self = Self::instant(target);
            return;
        }
        self.target = target;
        self.remaining = frames;
        self.step = (target - self.current) / frames as f32;
    }

    fn tick(&mut self) {
        if self.remaining == 0 {
            return;
        }
        self.remaining -= 1;
        if self.remaining == 0 {
            self.current = self.target;
            self.step = 0.0;
        } else {
            self.current += self.step;
        }
    }

    fn is_ramping(&self) -> bool {
        self.remaining > 0
    }
}

struct SoundLayer {
    id: String,
    file: String,
    info: AudioInfo,
    samples: Arc<[f32]>,
    /// `E` 原始值（引擎 `+0x106`）。
    ///
    /// **Confirmed（v494 `0x424ec4`）**：引擎用 `cmp [..+0x106],1; sete`
    /// —— 只有 `E == 1` 才出声，其余任何值（0/2/…）都按静音处理。
    /// `SOUNDINFO E` 返回该原始值。
    enabled_raw: i64,
    /// 最近一次 `VOLUME` 设置（0..1 原始值，引擎 `+0x150`）。
    volume_setting: f32,
    /// 当前音量（0..1 原始值，引擎 `+0x148`）；淡变时线性插值。
    volume: Envelope,
    /// 淡出中（到达 0 即停止）。
    stopping: bool,
    pan: i64,
    pitch: i64,
    start_ms: i64,
    restart_ms: i64,
    end_ms: i64,
    /// `LOOP` 设置（0 = 无限，N = 总播放次数；**新建声层默认 1**）。
    loop_setting: i64,
    loop_count: u64,
    /// 已开始的播放次数（1 基）。
    plays: u64,
    paused: bool,
    state: PlayState,
    /// 源采样帧位置（可为小数，线性插值）。
    pos: f64,
    /// 负 `START`/`RESTART` 的延迟，单位 = 输出帧。
    delay_frames: u64,
    last_left: i16,
    last_right: i16,
}

impl SoundLayer {
    fn total_frames(&self) -> u64 {
        self.info
            .total_frames
            .min(self.samples.len() as u64 / u64::from(self.info.channels))
    }

    fn total_ms(&self) -> i64 {
        self.info.duration_ms() as i64
    }

    fn ms_to_src_frames(&self, ms: i64) -> f64 {
        if self.info.sample_rate == 0 {
            return 0.0;
        }
        ms.max(0) as f64 * f64::from(self.info.sample_rate) / 1000.0
    }

    fn start_frame(&self) -> f64 {
        self.ms_to_src_frames(self.start_ms)
    }

    fn restart_frame(&self) -> f64 {
        self.ms_to_src_frames(self.restart_ms)
    }

    fn end_frame(&self) -> f64 {
        let total = self.total_frames() as f64;
        if self.end_ms > 0 {
            self.ms_to_src_frames(self.end_ms).min(total)
        } else {
            total
        }
    }

    fn time_ms(&self) -> u64 {
        if self.info.sample_rate == 0 {
            return 0;
        }
        // 引擎 `SOUNDINFO TIME` 读声层当前播放位置（`+0x198/+0x19c`，
        // 0x44f66a）；停止后位置字段保留。
        self.info.frames_to_ms(self.pos.max(0.0) as u64)
    }

    /// 读取源位置处的插值立体声样本（原始幅度，不含音量/声像）。
    fn sample_at(&self, pos: f64) -> (f32, f32) {
        let frames = self.total_frames();
        if frames == 0 {
            return (0.0, 0.0);
        }
        let ch = self.info.channels as usize;
        let i0 = (pos.max(0.0) as u64).min(frames - 1) as usize;
        let i1 = (i0 + 1).min(frames as usize - 1);
        let frac = (pos - i0 as f64).clamp(0.0, 1.0) as f32;
        let (l0, r0) = read_frame(&self.samples, ch, i0);
        let (l1, r1) = read_frame(&self.samples, ch, i1);
        (l0 + (l1 - l0) * frac, r0 + (r1 - r0) * frac)
    }

    fn finish(&mut self) {
        self.state = PlayState::Stopped;
        // 引擎结束路径不清 `+0x109`（见 PAUSE 处理注释），这里同样保留。
        self.stopping = false;
        self.plays = 0;
        self.volume.target = self.volume.current;
        self.volume.remaining = 0;
        self.delay_frames = 0;
        // 位置冻结在停止点（引擎 `SOUNDINFO TIME` 读位置字段）。
    }
}

fn read_frame(samples: &[f32], channels: usize, frame: usize) -> (f32, f32) {
    let base = frame * channels;
    match channels {
        0 => (0.0, 0.0),
        1 => {
            let v = samples.get(base).copied().unwrap_or(0.0);
            (v, v)
        }
        _ => (
            samples.get(base).copied().unwrap_or(0.0),
            samples.get(base + 1).copied().unwrap_or(0.0),
        ),
    }
}

/// 声层混音器。
pub struct Mixer {
    output_rate: u32,
    layers: Vec<SoundLayer>,
}

impl Mixer {
    /// 新建混音器。`output_rate` 为设备采样率（无头测试可自选）。
    pub fn new(output_rate: u32) -> Self {
        Self {
            output_rate: output_rate.clamp(1, 192_000),
            layers: Vec::new(),
        }
    }

    /// 当前输出采样率。
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// 修改输出采样率（切换设备时）。
    pub fn set_output_rate(&mut self, rate: u32) {
        self.output_rate = rate.clamp(1, 192_000);
    }

    /// 声层数量。
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// 是否没有声层。
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// 所有声层 ID（稳定插入序）。
    pub fn layer_ids(&self) -> Vec<String> {
        self.layers.iter().map(|l| l.id.clone()).collect()
    }

    /// 声层快照。
    pub fn snapshot(&self, id: &str) -> Option<LayerSnapshot> {
        self.layer(id).map(|l| LayerSnapshot {
            id: l.id.clone(),
            file: l.file.clone(),
            info: l.info,
            enabled: l.enabled_raw == 1,
            volume: (l.volume.current * VOLUME_MAX as f32).round() as i64,
            pan: l.pan,
            pitch: l.pitch,
            start_ms: l.start_ms,
            restart_ms: l.restart_ms,
            end_ms: l.end_ms,
            loop_setting: l.loop_setting,
            loop_count: l.loop_count,
            paused: l.paused,
            state: l.state,
            time_ms: l.time_ms(),
        })
    }

    /// 装载/替换声层源。
    ///
    /// 与 v494 装载路径（`0x4246e0`）一致：重置 `PAN=256`、`PITCH=256`、
    /// `START/RESTART/END=0`、播放状态与位置；保留 `E`、音量设置与 `LOOP`。
    pub fn set_source(&mut self, id: &str, file: &str, decoded: DecodedAudio) -> Result<AudioInfo> {
        if id.is_empty() {
            return Err(AudioError::InvalidParameter {
                name: "ID",
                value: 0,
                reason: "sound layer ID must not be empty",
            });
        }
        if decoded.info.channels == 0 || decoded.samples.is_empty() {
            return Err(AudioError::Decode("empty decoded audio".into()));
        }
        let samples: Arc<[f32]> = decoded.samples.into();
        if let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) {
            layer.file = file.to_string();
            layer.info = decoded.info;
            layer.samples = samples;
            layer.state = PlayState::Stopped;
            layer.stopping = false;
            layer.plays = 0;
            layer.loop_count = 0;
            layer.pan = PAN_CENTER;
            layer.pitch = PITCH_NORMAL;
            layer.start_ms = 0;
            layer.restart_ms = 0;
            layer.end_ms = 0;
            layer.pos = 0.0;
            layer.delay_frames = 0;
            layer.last_left = 0;
            layer.last_right = 0;
            return Ok(decoded.info);
        }
        let layer = SoundLayer {
            id: id.to_string(),
            file: file.to_string(),
            info: decoded.info,
            samples,
            enabled_raw: 1,
            volume_setting: 1.0,
            volume: Envelope::instant(1.0),
            stopping: false,
            pan: PAN_CENTER,
            pitch: PITCH_NORMAL,
            start_ms: 0,
            restart_ms: 0,
            end_ms: 0,
            // **默认播一次**（成果 163 勘误）。
            //
            // `LOOP=0` 在引擎里是**无限循环**（手册 `sound/sound.html`），
            // 但「未给 LOOP」不能等价于 0：手册示例 [3]–[7] 都不带 LOOP，
            // 描述的都是「从 3.2 秒处播放一次」这类一次性行为。若默认取 0，
            // 所有未显式给 LOOP 的语音/SE 会无限循环（实机已复现）。
            // v494 装载路径（`0x4246e0`）重置 PAN/PITCH/START/RESTART/END、
            // **不碰** LOOP（`+0x10c`）⇒ 换源保留本设置。
            loop_setting: 1,
            loop_count: 0,
            plays: 0,
            paused: false,
            state: PlayState::Stopped,
            pos: 0.0,
            delay_frames: 0,
            last_left: 0,
            last_right: 0,
        };
        self.layers.push(layer);
        Ok(decoded.info)
    }

    /// 删除声层（`SOUNDEND`）。返回是否真的删除了。
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.layers.len();
        self.layers.retain(|l| l.id != id);
        self.layers.len() != before
    }

    /// 应用一条 `SOUND` 命令的**非 FILE 参数**（FILE 由调用方先 `set_source`）。
    ///
    /// 与 v494 入队顺序一致（`0x6821c1..0x6821cc` 的槽读取点）：
    /// `E → START → RESTART → END → LOOP → PAN → PITCH → VOLUME →
    /// PLAY(含 FADE) → PAUSE`。各字段互不依赖，顺序只影响同字段多次赋值。
    pub fn apply(&mut self, cmd: &SoundCommand) -> Result<SoundOutcome> {
        // 参数先校验，避免半途修改声层。
        cmd.validate()?;

        let Some(layer) = self.layers.iter_mut().find(|l| l.id == cmd.id) else {
            // **声层不存在 = 引擎同款空操作**（v494 `0x44edc5`:
            // `test ebp,ebp / jne …` —— 无 FILE 时直接 `xor eax,eax; ret`）。
            //
            // 因此「不给 FILE 的纯参数命令」（`SOUND[ID="BGM/" E=1 VOLUME=256]`
            // 这类启动期初始化）在这里**不是异常**，不能报「从未装载音源」——
            // 实测该形态在本作语料里成批出现（166 次/500 帧），旧告警把
            // 正常用法全部误报成资源缺失。
            //
            // 只有「给了 FILE 却仍找不到声层」才是真异常（资源解析失败在
            // 调用方 `set_source` 已经报警），这里不再重复。
            return Ok(SoundOutcome::IgnoredNoLayer);
        };

        if let Some(v) = cmd.enabled {
            // 引擎只认 `E == 1` 为有效（见字段文档）。
            layer.enabled_raw = v;
        }
        // 先应用 `END`，再按**生效终点**校验 `START`（v494 PLAY 路径
        // `0x424d42`：`START >= END` → 归 0；`END==0` 表示文件末尾）。
        if let Some(v) = cmd.end_ms {
            // 引擎 `0x425364`：END > 总时长 → 总时长。
            let total = layer.total_ms().max(1);
            layer.end_ms = v.min(total);
        }
        let effective_end = if layer.end_ms > 0 {
            layer.end_ms
        } else {
            layer.total_ms()
        };
        if let Some(v) = cmd.start_ms {
            // 引擎 `0x4252a4`：START >= 总时长 → 0；负值原样保留（延迟播放）。
            // 另一处（`0x424d42`）用 END 再夹一次，这里统一按生效终点。
            layer.start_ms = if v >= effective_end && v >= 0 { 0 } else { v };
        }
        if let Some(v) = cmd.restart_ms {
            let total = layer.total_ms();
            layer.restart_ms = if v >= total && v >= 0 { 0 } else { v };
        }
        if let Some(v) = cmd.loop_count {
            layer.loop_setting = v;
        }

        let fade_frames = cmd
            .fade_ms
            .filter(|ms| *ms > 0)
            .map(|ms| ms as u64 * u64::from(self.output_rate) / 1000)
            .unwrap_or(0);

        if let Some(v) = cmd.pan {
            layer.pan = v;
        }
        if let Some(v) = cmd.pitch {
            layer.pitch = v;
        }
        if let Some(v) = cmd.volume {
            layer.volume_setting = v as f32 / VOLUME_MAX as f32;
            if cmd.play.is_some() {
                // 引擎 `0x425230`（事件 6）：只更新目标；真正的斜坡在
                // PLAY 事件（`0x424a1c`）里按 FADE 计算。
            } else {
                // 引擎 `0x4251e8`（事件 5）：立即生效（FADE 被忽略）。
                layer.volume = Envelope::instant(layer.volume_setting);
            }
        }

        if let Some(play) = cmd.play {
            if play != 0 {
                begin_play(layer, self.output_rate, fade_frames);
            } else {
                begin_stop(layer, fade_frames);
            }
        }
        if let Some(pause) = cmd.pause {
            // **Confirmed（v494 `0x42515c`）**：`+0x109` 由 PAUSE 事件无条件写入，
            // 装载/播放/结束路径**都不清除**它（全 .text 仅一处写点）。
            // 因此恢复只能靠显式 `PAUSE=0`（手册示例 \[11\]），`PLAY=1` 不会解除暂停。
            layer.paused = pause != 0;
        }
        // SPEED：v494 处理器不读取该槽；显式忽略（Unknown，不猜语义）。
        Ok(SoundOutcome::Applied)
    }

    /// `SOUNDINFO` 查询。声层不存在时按引擎路径返回 `Int(0)`。
    pub fn query(&self, id: &str, query: SoundInfoQuery) -> SoundInfoValue {
        let Some(layer) = self.layer(id) else {
            return SoundInfoValue::Int(0);
        };
        match query {
            SoundInfoQuery::Exist => SoundInfoValue::Int(1),
            SoundInfoQuery::Enabled => SoundInfoValue::Int(layer.enabled_raw),
            SoundInfoQuery::Play => SoundInfoValue::Int((layer.state == PlayState::Playing) as i64),
            SoundInfoQuery::Pause => SoundInfoValue::Int(layer.paused as i64),
            SoundInfoQuery::Volume => {
                SoundInfoValue::Int((layer.volume.current * VOLUME_MAX as f32).round() as i64)
            }
            SoundInfoQuery::Pan => SoundInfoValue::Int(layer.pan),
            SoundInfoQuery::Pitch => SoundInfoValue::Int(layer.pitch),
            SoundInfoQuery::File => SoundInfoValue::Str(layer.file.clone()),
            SoundInfoQuery::FileType => SoundInfoValue::Int(layer.info.codec.file_type_code()),
            SoundInfoQuery::Start => SoundInfoValue::Int(layer.start_ms),
            SoundInfoQuery::Restart => SoundInfoValue::Int(layer.restart_ms),
            SoundInfoQuery::End => SoundInfoValue::Int(layer.end_ms),
            SoundInfoQuery::Loop => SoundInfoValue::Int(layer.loop_setting),
            SoundInfoQuery::LoopCount => SoundInfoValue::Int(layer.loop_count as i64),
            SoundInfoQuery::Time => SoundInfoValue::Int(layer.time_ms() as i64),
            SoundInfoQuery::TotalTime => SoundInfoValue::Int(layer.info.duration_ms() as i64),
            SoundInfoQuery::Channel => SoundInfoValue::Int(i64::from(layer.info.channels)),
            SoundInfoQuery::Bit => SoundInfoValue::Int(i64::from(layer.info.bits_per_sample)),
            SoundInfoQuery::Sampling => SoundInfoValue::Int(i64::from(layer.info.sample_rate)),
            SoundInfoQuery::DataLeft => SoundInfoValue::Int(i64::from(layer.last_left)),
            SoundInfoQuery::DataRight => SoundInfoValue::Int(i64::from(layer.last_right)),
            SoundInfoQuery::Codec => {
                let code = layer.info.codec.sound_code();
                if code < 0 {
                    SoundInfoValue::Unsupported
                } else {
                    SoundInfoValue::Int(code)
                }
            }
        }
    }

    /// 渲染交错立体声到 `out`（长度必须为偶数；每帧 2 个 `f32`）。
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        let out_rate = self.output_rate;
        for frame in 0..frames {
            let mut left = 0.0f32;
            let mut right = 0.0f32;
            for layer in &mut self.layers {
                if layer.state != PlayState::Playing || layer.paused {
                    continue;
                }
                if layer.delay_frames > 0 {
                    layer.delay_frames -= 1;
                    continue;
                }
                if layer.total_frames() == 0 {
                    layer.finish();
                    continue;
                }
                if layer.pos >= layer.end_frame() && !wrap_or_finish(layer, out_rate) {
                    continue;
                }
                if layer.delay_frames > 0 {
                    layer.delay_frames -= 1;
                    continue;
                }

                let (sl, sr) = layer.sample_at(layer.pos);
                layer.last_left = to_i16(sl);
                layer.last_right = to_i16(sr);
                let step = if out_rate == 0 {
                    1.0
                } else {
                    (f64::from(layer.info.sample_rate) / f64::from(out_rate))
                        * (layer.pitch as f64 / PITCH_NORMAL as f64)
                };
                layer.pos += step.max(0.0);
                if layer.pos >= layer.end_frame() {
                    wrap_or_finish(layer, out_rate);
                }

                // 原始音量 → 幅度：引擎把 VOLUME 换算成
                // `4000·log10(v/256)` 百分之一分贝（v494 `0x424f18`），
                // 即幅度 = (v/256)²。
                let vol = layer.volume.current;
                let amp = vol * vol;
                let enabled = if layer.enabled_raw == 1 { 1.0 } else { 0.0 };
                let (gl, gr) = pan_gains(layer.pan);
                left += sl * amp * gl * enabled;
                right += sr * amp * gr * enabled;

                layer.volume.tick();
                if layer.stopping
                    && layer.volume.target <= f32::EPSILON
                    && layer.volume.current <= f32::EPSILON
                    && !layer.volume.is_ramping()
                {
                    layer.finish();
                }
            }
            out[frame * 2] = left.clamp(-1.0, 1.0);
            out[frame * 2 + 1] = right.clamp(-1.0, 1.0);
        }
    }

    /// 渲染指定帧数并返回交错立体声缓冲（无头/测试用）。
    pub fn render_frames(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        self.render(&mut out);
        out
    }

    fn layer(&self, id: &str) -> Option<&SoundLayer> {
        self.layers.iter().find(|l| l.id == id)
    }
}

fn begin_play(layer: &mut SoundLayer, out_rate: u32, fade_frames: u64) {
    if layer.state == PlayState::Playing {
        // 播放中（含淡出中）再给 PLAY=1：只按 FADE 把音量滑到设定目标，
        // 不重播。淡出中被叫停时引擎会反向斜坡（`0x424acc` 交换
        // 当前/目标音量），等价于从当前衰减值回升。
        // 注意：**不清除 `paused`**（引擎 `+0x109` 只有 PAUSE 事件能改）。
        layer.stopping = false;
        layer.volume.ramp_to(layer.volume_setting, fade_frames);
        return;
    }
    layer.state = PlayState::Playing;
    layer.stopping = false;
    layer.loop_count = 0;
    layer.plays = 1;
    layer.last_left = 0;
    layer.last_right = 0;
    // v494 `0x424c7f`：播放路径把淡变基准置 0 ⇒ 带 FADE 的首次播放
    // 从静音淡入到目标音量。
    if fade_frames > 0 {
        layer.volume = Envelope::instant(0.0);
        layer.volume.ramp_to(layer.volume_setting, fade_frames);
    } else {
        layer.volume = Envelope::instant(layer.volume_setting);
    }
    if layer.start_ms < 0 {
        let delay_ms = layer.start_ms.unsigned_abs();
        layer.delay_frames = delay_ms * u64::from(out_rate) / 1000;
        layer.pos = 0.0;
    } else {
        layer.delay_frames = 0;
        layer.pos = layer.start_frame();
    }
}

fn begin_stop(layer: &mut SoundLayer, fade_frames: u64) {
    if layer.state != PlayState::Playing {
        return;
    }
    // 不触碰 `paused`（引擎 `+0x109` 只由 PAUSE 事件写）。
    if fade_frames > 0 {
        layer.stopping = true;
        layer.volume.ramp_to(0.0, fade_frames);
    } else {
        layer.volume = Envelope::instant(0.0);
        layer.finish();
    }
}

/// 到达终点后的循环/停止处理。返回是否仍在播放。
fn wrap_or_finish(layer: &mut SoundLayer, out_rate: u32) -> bool {
    if layer.loop_setting > 0 && layer.plays >= layer.loop_setting as u64 {
        layer.finish();
        return false;
    }
    layer.loop_count += 1;
    layer.plays += 1;
    if layer.restart_ms < 0 {
        layer.pos = 0.0;
        layer.delay_frames = layer.restart_ms.unsigned_abs() * u64::from(out_rate) / 1000;
    } else {
        layer.pos = layer.restart_frame();
        if layer.pos >= layer.end_frame() {
            layer.pos = 0.0;
        }
    }
    true
}

/// `PAN` 0..512 → 左右增益。
///
/// **Confirmed（v494 `0x425088`）**：引擎先把 PAN 转成 DirectSound 声像值
/// （单位 = **百分之一分贝**）：
/// - `pan == 256` → 0（居中）；
/// - `pan < 256`：`4000·log10(pan/256)`（负值），`pan == 0` → -10000；
/// - `pan > 256`：`-4000·log10((512-pan)/256)`（正值），`pan == 512` → +10000；
/// - 越界 → 按 256 处理。
///
/// DirectSound 的 pan 语义 = **衰减对侧声道**，量纲是百分之一分贝 ⇒
/// 增益 = `10^(ds/2000)`。因此 `PAN=128`（半左）时右声道是 `10^-0.602 ≈ 0.25`
/// （即相当于把 `pan/256` 的幅度再平方），不是线性 0.5。
///
/// **Likely**：近侧声道保持 1.0（DS pan 只衰减对侧）。引擎自身不混音，
/// 全部交给 DS；本库按此模型还原。
fn pan_gains(pan: i64) -> (f32, f32) {
    let pan = pan.clamp(0, 512);
    let ds = if pan == PAN_CENTER {
        0.0
    } else if pan < PAN_CENTER {
        if pan == 0 {
            -10_000.0
        } else {
            let ratio = pan as f64 / PAN_CENTER as f64;
            (4_000.0 * ratio.log10()).max(-10_000.0)
        }
    } else if pan == 512 {
        10_000.0
    } else {
        let ratio = (512 - pan) as f64 / PAN_CENTER as f64;
        (-4_000.0 * ratio.log10()).min(10_000.0)
    };
    // ds 的**绝对值**才是衰减量（dB/100）；近侧保持 1.0，对侧衰减。
    let atten = 10f64.powf(-ds.abs() / 2000.0) as f32;
    if ds <= 0.0 {
        (1.0, atten)
    } else {
        (atten, 1.0)
    }
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::{AudioCodec, AudioInfo};

    fn decoded(rate: u32, frames: usize) -> DecodedAudio {
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = (i as f32 * 0.01).sin();
            samples.push(v);
            samples.push(v);
        }
        DecodedAudio {
            info: AudioInfo {
                codec: AudioCodec::PcmWav,
                channels: 2,
                sample_rate: rate,
                bits_per_sample: 16,
                total_frames: frames as u64,
            },
            samples,
        }
    }

    fn constant(rate: u32, frames: usize, value: f32) -> DecodedAudio {
        DecodedAudio {
            info: AudioInfo {
                codec: AudioCodec::PcmWav,
                channels: 2,
                sample_rate: rate,
                bits_per_sample: 16,
                total_frames: frames as u64,
            },
            samples: vec![value; frames * 2],
        }
    }

    #[test]
    fn pan_curve_matches_directsound() {
        assert_eq!(pan_gains(256), (1.0, 1.0));
        let full_left = pan_gains(0);
        assert_eq!(full_left.0, 1.0);
        assert!(full_left.1 < 1e-4, "right={}", full_left.1);
        let full_right = pan_gains(512);
        assert!(full_right.0 < 1e-4, "left={}", full_right.0);
        assert_eq!(full_right.1, 1.0);

        // PAN=128 → ds = 4000·log10(0.5) ≈ -1204 → 对侧增益 10^(-0.602) ≈ 0.25。
        let half_left = pan_gains(128);
        assert!((half_left.0 - 1.0).abs() < 1e-6);
        assert!((half_left.1 - 0.25).abs() < 1e-3, "right={}", half_left.1);

        // 对称性。
        let half_right = pan_gains(384);
        assert!((half_right.0 - 0.25).abs() < 1e-3, "left={}", half_right.0);
        assert!((half_right.1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn volume_curve_is_squared() {
        let mut mixer = Mixer::new(8_000);
        mixer
            .set_source("BGM", "a.wav", constant(8_000, 8_000, 0.5))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.volume = Some(128); // 幅度 = (128/256)^2 = 0.25 → 0.5*0.25 = 0.125
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        let out = mixer.render_frames(1);
        assert!((out[0] - 0.125).abs() < 1e-6, "left={}", out[0]);
        assert!((out[1] - 0.125).abs() < 1e-6, "right={}", out[1]);
    }

    #[test]
    fn missing_layer_info_zeros() {
        let mixer = Mixer::new(8_000);
        assert_eq!(
            mixer.query("nope", SoundInfoQuery::Exist),
            SoundInfoValue::Int(0)
        );
        assert_eq!(
            mixer.query("nope", SoundInfoQuery::Volume),
            SoundInfoValue::Int(0)
        );
    }

    #[test]
    fn command_without_file_on_missing_layer_is_noop() {
        let mut mixer = Mixer::new(8_000);
        let mut cmd = SoundCommand::new("BGM");
        cmd.play = Some(1);
        assert_eq!(mixer.apply(&cmd).unwrap(), SoundOutcome::IgnoredNoLayer);
        assert!(mixer.is_empty());
    }

    #[test]
    fn load_resets_pan_pitch_times_but_keeps_volume_and_loop() {
        let mut mixer = Mixer::new(8_000);
        mixer
            .set_source("BGM", "a.ogg", decoded(8_000, 8_000))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.volume = Some(128);
        cmd.pan = Some(0);
        cmd.pitch = Some(384);
        cmd.start_ms = Some(100);
        cmd.end_ms = Some(500);
        cmd.loop_count = Some(3);
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        mixer.render_frames(800);
        mixer
            .set_source("BGM", "b.ogg", decoded(8_000, 8_000))
            .unwrap();
        let snap = mixer.snapshot("BGM").unwrap();
        assert_eq!(snap.volume, 128);
        assert_eq!(snap.loop_setting, 3);
        assert_eq!(snap.pan, PAN_CENTER);
        assert_eq!(snap.pitch, PITCH_NORMAL);
        assert_eq!(snap.start_ms, 0);
        assert_eq!(snap.end_ms, 0);
        assert_eq!(snap.state, PlayState::Stopped);
    }

    #[test]
    fn fade_in_ramps_volume_and_reports_current() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 1_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.volume = Some(256);
        cmd.fade_ms = Some(1000);
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Volume),
            SoundInfoValue::Int(0)
        );
        mixer.render_frames(500); // 0.5 s
        let mid = mixer.query("BGM", SoundInfoQuery::Volume).as_int().unwrap();
        assert!((mid - 128).abs() <= 1, "mid={mid}");
        mixer.render_frames(600);
        let end = mixer.query("BGM", SoundInfoQuery::Volume).as_int().unwrap();
        assert_eq!(end, 256);
    }

    #[test]
    fn fade_out_stops_after_duration() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 8_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        mixer.render_frames(100);
        let mut stop = SoundCommand::new("BGM");
        stop.play = Some(0);
        stop.fade_ms = Some(500);
        mixer.apply(&stop).unwrap();
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Play),
            SoundInfoValue::Int(1)
        );
        mixer.render_frames(400);
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Play),
            SoundInfoValue::Int(1)
        );
        mixer.render_frames(200);
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Play),
            SoundInfoValue::Int(0)
        );
    }

    #[test]
    fn finite_loop_stops_after_count() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 100, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.loop_count = Some(2);
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        // 100 帧 = 100ms；两遍 = 200ms。
        mixer.render_frames(150);
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Play),
            SoundInfoValue::Int(1)
        );
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::LoopCount),
            SoundInfoValue::Int(1)
        );
        mixer.render_frames(100);
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Play),
            SoundInfoValue::Int(0)
        );
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::LoopCount),
            SoundInfoValue::Int(1)
        );
    }

    #[test]
    fn default_loop_plays_once() {
        // 成果 163 勘误：未给 LOOP 的声层默认只播一次（不是无限）。
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("VOICE", "a.wav", constant(1_000, 100, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("VOICE");
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        assert_eq!(
            mixer.query("VOICE", SoundInfoQuery::Loop),
            SoundInfoValue::Int(1)
        );
        mixer.render_frames(300);
        assert_eq!(
            mixer.query("VOICE", SoundInfoQuery::Play),
            SoundInfoValue::Int(0)
        );
    }

    #[test]
    fn infinite_loop_keeps_playing() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 100, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.loop_count = Some(0);
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        mixer.render_frames(1_000);
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Play),
            SoundInfoValue::Int(1)
        );
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::LoopCount),
            SoundInfoValue::Int(10)
        );
    }

    #[test]
    fn e_only_one_is_audible() {
        // 引擎 `cmp [..+0x106],1; sete`：E=2 也按静音处理。
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("SE", "a.wav", constant(1_000, 8_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("SE");
        cmd.enabled = Some(2);
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        let out = mixer.render_frames(50);
        assert!(out.iter().all(|s| *s == 0.0), "E=2 必须静音");
        assert_eq!(
            mixer.query("SE", SoundInfoQuery::Enabled),
            SoundInfoValue::Int(2)
        );
    }

    #[test]
    fn play_does_not_clear_pause() {
        // `+0x109` 全 .text 只有 PAUSE 事件一处写点 ⇒ PLAY=1 不解除暂停。
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("VOICE", "a.wav", constant(1_000, 8_000, 1.0))
            .unwrap();
        let mut pause = SoundCommand::new("VOICE");
        pause.pause = Some(1);
        mixer.apply(&pause).unwrap();
        let mut play = SoundCommand::new("VOICE");
        play.play = Some(1);
        mixer.apply(&play).unwrap();
        assert_eq!(
            mixer.query("VOICE", SoundInfoQuery::Pause),
            SoundInfoValue::Int(1)
        );
        let out = mixer.render_frames(100);
        assert!(out.iter().all(|s| *s == 0.0));
        let mut resume = SoundCommand::new("VOICE");
        resume.pause = Some(0);
        mixer.apply(&resume).unwrap();
        let out = mixer.render_frames(100);
        assert!(out.iter().any(|s| s.abs() > 0.0));
    }

    #[test]
    fn start_clamped_against_end() {
        // PLAY 路径 `0x424d42`：START >= END → START 归 0。
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 2_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.end_ms = Some(500);
        cmd.start_ms = Some(800); // >= END(500) ⇒ 归 0
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Start),
            SoundInfoValue::Int(0)
        );
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Time),
            SoundInfoValue::Int(0)
        );
    }

    #[test]
    fn pause_freezes_time_and_playback() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 8_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        mixer.render_frames(100);
        let mut pause = SoundCommand::new("BGM");
        pause.pause = Some(1);
        mixer.apply(&pause).unwrap();
        let t0 = mixer.query("BGM", SoundInfoQuery::Time).as_int().unwrap();
        let out = mixer.render_frames(500);
        assert!(out.iter().all(|s| *s == 0.0));
        let t1 = mixer.query("BGM", SoundInfoQuery::Time).as_int().unwrap();
        assert_eq!(t0, t1);
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Pause),
            SoundInfoValue::Int(1)
        );
        let mut resume = SoundCommand::new("BGM");
        resume.pause = Some(0);
        mixer.apply(&resume).unwrap();
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Pause),
            SoundInfoValue::Int(0)
        );
    }

    #[test]
    fn start_seeks_and_negative_start_delays() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 2_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.start_ms = Some(500);
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Time),
            SoundInfoValue::Int(500)
        );

        let mut delayed = SoundCommand::new("VOICE");
        delayed.start_ms = Some(-250);
        delayed.play = Some(1);
        mixer
            .set_source("VOICE", "b.wav", constant(1_000, 2_000, 1.0))
            .unwrap();
        mixer.remove("BGM");
        mixer.apply(&delayed).unwrap();
        let out = mixer.render_frames(200); // 250ms 延迟内应静音
        assert!(out.iter().all(|s| *s == 0.0));
        let out = mixer.render_frames(100); // 之后开始发声
        assert!(out.iter().any(|s| *s > 0.0));
    }

    #[test]
    fn two_layers_mix_concurrently() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 8_000, 0.25))
            .unwrap();
        mixer
            .set_source("SE", "b.wav", constant(1_000, 8_000, 0.25))
            .unwrap();
        for id in ["BGM", "SE"] {
            let mut cmd = SoundCommand::new(id);
            cmd.play = Some(1);
            mixer.apply(&cmd).unwrap();
        }
        let out = mixer.render_frames(1);
        assert!((out[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn disabled_layer_is_silent_but_advances() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 8_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.enabled = Some(0);
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        let out = mixer.render_frames(100);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Time),
            SoundInfoValue::Int(100)
        );
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Play),
            SoundInfoValue::Int(1)
        );
    }

    #[test]
    fn play_while_playing_does_not_restart() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 8_000, 1.0))
            .unwrap();
        let mut cmd = SoundCommand::new("BGM");
        cmd.play = Some(1);
        mixer.apply(&cmd).unwrap();
        mixer.render_frames(300);
        mixer.apply(&cmd).unwrap();
        assert_eq!(
            mixer.query("BGM", SoundInfoQuery::Time),
            SoundInfoValue::Int(300)
        );
    }

    #[test]
    fn sound_end_removes_layer() {
        let mut mixer = Mixer::new(1_000);
        mixer
            .set_source("BGM", "a.wav", constant(1_000, 8_000, 1.0))
            .unwrap();
        assert!(mixer.remove("BGM"));
        assert!(!mixer.remove("BGM"));
        assert!(mixer.is_empty());
    }
}
