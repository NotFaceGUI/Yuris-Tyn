//! 引擎声音命令的数据模型：`SOUND` / `SOUNDEND` / `SOUNDINFO`。
//!
//! ## 证据
//!
//! - 参数名与顺序：游戏自身 `ysc.ybn`（YSCM）；本作 v494 的 `SOUND` 为
//!   `ID / E / FILE / PLAY / PAUSE / FADE / START / RESTART / END / LOOP /
//!   VOLUME / PAN / PITCH / SPEED`，`SOUNDINFO` 为 24 参（`ID…LET`）。
//! - 语义与取值范围：官方手册 `sound/sound.html`、`sound/soundinfo.html`、
//!   `sound/soundend.html`（SDK 0.495）。
//! - v494 处理器（`<game-b>_v494.exe`，sha256 `1ff3135d…`）：
//!   `SOUND 0x44e07c`、`SOUNDEND 0x44f19c`、`SOUNDINFO 0x44f350`。
//!
//! ## 关键结论（Confirmed）
//!
//! - 声音是**以 ID 字符串为键的“声层”**，不是固定的 BGM/SE/Voice 三条通道；
//!   同一 ID 后一次 `FILE` 会替换前一条流，不同 ID 可同时播放（手册示例 \[8\]）。
//! - `VOLUME` 0..256（默认 256）、`PAN` 0..512（默认 256，线性左右）、
//!   `PITCH` 0..512（0.5 倍～1.5 倍，默认 256）、`FADE` 毫秒、
//!   `START/RESTART/END` 毫秒（负 `START/RESTART` = 延迟播放）。
//! - `LOOP` 0 = 无限循环，N≥1 = 总播放次数；**未给 LOOP 时默认播一次**
//!   （成果 163 勘误：此前默认 0 导致所有未显式循环的语音/SE 无限重播）。
//!   依据：手册 `LOOP=0` 明写「無限ループ」，而示例 \[3\]–\[7\] 不带 LOOP 却
//!   描述一次性播放；两者只在「默认值 = 1」下自洽。
//! - `PLAY=1` 对**正在播放**的声层是幂等的（手册示例 \[12\] 用它配合
//!   `VOLUME`/`FADE` 只做音量淡变，不重播）。
//! - `SPEED`（仅 v555 YSCM 有）在 v494 `SOUND` 处理器中**完全未被读取**；
//!   其语义 Unknown，本库不应用该参数。

use crate::error::AudioError;
use crate::info::AudioInfo;

/// `VOLUME` 的最大值（官方手册：0～256）。
pub const VOLUME_MAX: i64 = 256;
/// `PAN` 的默认值/中心（官方手册：0～512，默认 256）。
pub const PAN_CENTER: i64 = 256;
/// `PITCH` 的默认值（1.0 倍；官方手册：0～512，0.5～1.5 倍）。
pub const PITCH_NORMAL: i64 = 256;

/// 一条 `SOUND` 命令。
///
/// 字段全部为 `Option`：`None` = 本次未给该槽（引擎保持声层原值）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SoundCommand {
    /// 声层 ID（YSCM `ID`）。
    pub id: String,
    /// `E`：声层有效/静音（0/1）。
    pub enabled: Option<i64>,
    /// `FILE`：逻辑文件名；可省略扩展名（搜索顺序见 [`search_candidates`]）。
    pub file: Option<String>,
    /// `PLAY`：1 = 播放，0 = 停止。
    pub play: Option<i64>,
    /// `PAUSE`：1 = 暂停，0 = 恢复。
    pub pause: Option<i64>,
    /// `FADE`：淡入/淡出/音量淡变时长（毫秒）。
    pub fade_ms: Option<i64>,
    /// `START`：首次播放起点（毫秒；负值 = 延迟播放）。
    pub start_ms: Option<i64>,
    /// `RESTART`：每次循环后的起点（毫秒；负值 = 循环前延迟）。
    pub restart_ms: Option<i64>,
    /// `END`：终点/循环点（毫秒；0 = 文件末尾）。
    pub end_ms: Option<i64>,
    /// `LOOP`：0 = 无限循环；N≥1 = 总播放次数。
    ///
    /// `None` = 本次未给（声层保持当前设置；**新建声层的默认值 = 1**）。
    pub loop_count: Option<i64>,
    /// `VOLUME`：0～256。
    pub volume: Option<i64>,
    /// `PAN`：0～512（左～右，默认 256）。
    pub pan: Option<i64>,
    /// `PITCH`：0～512（0.5～1.5 倍，默认 256）。
    pub pitch: Option<i64>,
    /// `SPEED`：**Unknown**。v494 处理器不读取该槽；本库解析但不应用。
    pub speed: Option<i64>,
}

impl SoundCommand {
    /// 新建只带 ID 的命令。
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    /// 是否带 `FILE`（需要装载资源）。
    pub fn has_file(&self) -> bool {
        self.file.is_some()
    }

    /// 校验参数取值范围（官方手册 `sound/sound.html`）。
    ///
    /// 越界返回显式错误而不是钳制——引擎侧会钳制，但本内核的铁律是
    /// 「不确定就不猜」；调用方（脚本层）应修正数据。
    pub fn validate(&self) -> Result<(), AudioError> {
        if self.id.is_empty() {
            return Err(AudioError::InvalidParameter {
                name: "ID",
                value: 0,
                reason: "sound layer ID must not be empty",
            });
        }
        if let Some(v) = self.volume {
            if !(0..=VOLUME_MAX).contains(&v) {
                return Err(AudioError::InvalidParameter {
                    name: "VOLUME",
                    value: v,
                    reason: "expected 0..=256",
                });
            }
        }
        if let Some(v) = self.pan {
            if !(0..=512).contains(&v) {
                return Err(AudioError::InvalidParameter {
                    name: "PAN",
                    value: v,
                    reason: "expected 0..=512",
                });
            }
        }
        if let Some(v) = self.pitch {
            if !(0..=512).contains(&v) {
                return Err(AudioError::InvalidParameter {
                    name: "PITCH",
                    value: v,
                    reason: "expected 0..=512",
                });
            }
        }
        if let Some(v) = self.fade_ms {
            if v < 0 {
                return Err(AudioError::InvalidParameter {
                    name: "FADE",
                    value: v,
                    reason: "expected >= 0 milliseconds",
                });
            }
        }
        if let Some(v) = self.loop_count {
            if v < 0 {
                return Err(AudioError::InvalidParameter {
                    name: "LOOP",
                    value: v,
                    reason: "expected 0 (infinite) or a positive play count",
                });
            }
        }
        if let Some(v) = self.end_ms {
            if v < 0 {
                return Err(AudioError::InvalidParameter {
                    name: "END",
                    value: v,
                    reason: "expected >= 0 milliseconds",
                });
            }
        }
        Ok(())
    }
}

/// `SOUNDINFO` 查询项（与 YSCM 参数名一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundInfoQuery {
    /// `EXIST`：声层是否存在（已成功装载过文件）。
    Exist,
    /// `E`：声层有效位。
    Enabled,
    /// `PLAY`：当前是否在播放（含淡出中；暂停仍算播放）。
    Play,
    /// `PAUSE`：是否暂停。
    Pause,
    /// `VOLUME`：当前音量值（淡变中返回当前插值，0～256）。
    Volume,
    /// `PAN`：设置值（0～512）。
    Pan,
    /// `PITCH`：设置值（0～512）。
    Pitch,
    /// `FILE`：装载的原始文件名（字符串）。
    File,
    /// `FILETYPE`：v494 实测 OGG=1 / WAVE=2。
    FileType,
    /// `START`：毫秒。
    Start,
    /// `RESTART`：毫秒。
    Restart,
    /// `END`：毫秒。
    End,
    /// `LOOP`：设置的播放次数（0 = 无限）。
    Loop,
    /// `LOOPCOUNT`：已完成的循环次数。
    LoopCount,
    /// `TIME`：当前播放位置（毫秒）。
    Time,
    /// `TOTALTIME`：总时长（毫秒）。
    TotalTime,
    /// `CHANNEL`：声道数。
    Channel,
    /// `BIT`：位深。
    Bit,
    /// `SAMPLING`：采样率。
    Sampling,
    /// `DATALEFT`：当前左声道样本值（i16 范围）。
    DataLeft,
    /// `DATARIGHT`：当前右声道样本值（i16 范围）。
    DataRight,
    /// `CODEC`：0=WAVE / 1=OGG / 2=ACM WAVE（手册）。
    Codec,
}

impl SoundInfoQuery {
    /// YSCM 参数名。
    pub fn param_name(self) -> &'static str {
        match self {
            Self::Exist => "EXIST",
            Self::Enabled => "E",
            Self::Play => "PLAY",
            Self::Pause => "PAUSE",
            Self::Volume => "VOLUME",
            Self::Pan => "PAN",
            Self::Pitch => "PITCH",
            Self::File => "FILE",
            Self::FileType => "FILETYPE",
            Self::Start => "START",
            Self::Restart => "RESTART",
            Self::End => "END",
            Self::Loop => "LOOP",
            Self::LoopCount => "LOOPCOUNT",
            Self::Time => "TIME",
            Self::TotalTime => "TOTALTIME",
            Self::Channel => "CHANNEL",
            Self::Bit => "BIT",
            Self::Sampling => "SAMPLING",
            Self::DataLeft => "DATALEFT",
            Self::DataRight => "DATARIGHT",
            Self::Codec => "CODEC",
        }
    }
}

/// `SOUNDINFO` 查询结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundInfoValue {
    /// 整数值（写回 `LET` 的 `@` 变量）。
    Int(i64),
    /// 字符串值（写回 `LET` 的 `$` 变量）。
    Str(String),
    /// 语义未取证/不支持（调用方不应写回，也不应猜 0）。
    Unsupported,
}

impl SoundInfoValue {
    /// 取整数值（非整数返回 `None`）。
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }
}

/// `SOUND` 执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundOutcome {
    /// 成功装载（并应用了同一命令的其余参数）。
    Loaded {
        /// 新声层的解码信息。
        info: AudioInfo,
    },
    /// 未装载文件，只应用了参数。
    Applied,
    /// 声层不存在且本次没有 `FILE` —— 引擎同款空操作（v494 `0x44edc5`）。
    IgnoredNoLayer,
}

/// 按官方手册给出 `SOUND FILE=` 的扩展名搜索顺序。
///
/// 手册原文：省略扩展名时依次找「(拡張子なし)」「.OGG」「.WAV」。
/// 因为封包条目实际多为小写，这里把 `.ogg/.wav` 的大小写两种写法都列上；
/// 资源层本身通常会折叠大小写，重复候选不会造成额外语义。
pub fn search_candidates(file: &str) -> Vec<String> {
    let trimmed = file.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    if last.contains('.') {
        return vec![trimmed.to_string()];
    }
    vec![
        trimmed.to_string(),
        format!("{trimmed}.ogg"),
        format!("{trimmed}.OGG"),
        format!("{trimmed}.wav"),
        format!("{trimmed}.WAV"),
    ]
}
