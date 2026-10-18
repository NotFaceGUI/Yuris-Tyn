//! 音频状态查询边界（`SOUNDINFO`）。
//!
//! ## 为什么 VM 里需要这个 trait
//!
//! `SOUNDINFO` 是**查询**命令：脚本用它读声层的 `EXIST` / `PLAY` / `VOLUME` /
//! `TIME` / `TOTALTIME` / `CHANNEL` / `BIT` / `SAMPLING` / `CODEC` 等状态，
//! 结果写进 `LET` 目标。音频层在 `yuris-audio`（L6），而 VM（L3）**不得**
//! 反向依赖平台层 —— 那会破坏 `RuntimeApi` 是唯一边界的分层。
//!
//! 于是用依赖注入：宿主（`yuris-player-core`，它同时持有 VM 与音频系统）把
//! 一个 [`SoundStateProbe`] 实现塞进 VM。VM 只认这个 trait，
//! 与 [`crate::host::PacFileIndex`]（`FILEINFO`/`CG` 的存在性查询）同一模式。
//!
//! ## 未注入时怎么办
//!
//! **不写假 0**（那是编造值）。VM 侧按「无查询通道」显式挂起（strict）并在
//! 横幅里说明；调用方能从 [`SoundInfoValue::Unavailable`] 区分「答 0」与
//! 「答不出来」。

/// `SOUNDINFO` 可查询的字段（与 YSCM 参数名一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundField {
    /// `EXIST`：声层存在（已成功装载过音源）。
    Exist,
    /// `E`：声层有效位。
    Enabled,
    /// `PLAY`：是否在播放。
    Play,
    /// `PAUSE`：是否暂停。
    Pause,
    /// `VOLUME`：音量（0..256）。
    Volume,
    /// `PAN`：声像（0..512）。
    Pan,
    /// `PITCH`：音高（0..512）。
    Pitch,
    /// `FILE`：装载的原始文件名（字符串）。
    File,
    /// `FILETYPE`：引擎文件类型码。
    FileType,
    /// `START`：起点毫秒。
    Start,
    /// `RESTART`：重播点毫秒。
    Restart,
    /// `END`：终点毫秒。
    End,
    /// `LOOP`：循环设置（0 = 无限）。
    Loop,
    /// `LOOPCOUNT`：已完成的循环次数。
    LoopCount,
    /// `TIME`：当前播放位置毫秒。
    Time,
    /// `TOTALTIME`：总时长毫秒。
    TotalTime,
    /// `CHANNEL`：声道数。
    Channel,
    /// `BIT`：位深。
    Bit,
    /// `SAMPLING`：采样率。
    Sampling,
    /// `DATALEFT`：当前左声道样本。
    DataLeft,
    /// `DATARIGHT`：当前右声道样本。
    DataRight,
    /// `CODEC`：编码码（引擎口径）。
    Codec,
}

impl SoundField {
    /// 由 YSCM 参数名解析（大小写不敏感）。
    pub fn from_param_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_uppercase().as_str() {
            "EXIST" => Self::Exist,
            "E" => Self::Enabled,
            "PLAY" => Self::Play,
            "PAUSE" => Self::Pause,
            "VOLUME" => Self::Volume,
            "PAN" => Self::Pan,
            "PITCH" => Self::Pitch,
            "FILE" => Self::File,
            "FILETYPE" => Self::FileType,
            "START" => Self::Start,
            "RESTART" => Self::Restart,
            "END" => Self::End,
            "LOOP" => Self::Loop,
            "LOOPCOUNT" => Self::LoopCount,
            "TIME" => Self::Time,
            "TOTALTIME" => Self::TotalTime,
            "CHANNEL" => Self::Channel,
            "BIT" => Self::Bit,
            "SAMPLING" => Self::Sampling,
            "DATALEFT" => Self::DataLeft,
            "DATARIGHT" => Self::DataRight,
            "CODEC" => Self::Codec,
            _ => return None,
        })
    }

    /// 该字段是否是字符串型（写回 `$` 变量）。
    pub fn is_string(self) -> bool {
        matches!(self, Self::File)
    }
}

/// 一次 `SOUNDINFO` 查询的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundInfoValue {
    /// 整数值。
    Int(i64),
    /// 字符串值（CP932 原始字节）。
    Str(Vec<u8>),
    /// 查询通道存在但该字段无真值（编码未取证等）—— **不得**当成 0。
    Unsupported,
}

/// 音频层状态查询边界。
///
/// 实现方（播放器）负责把 `yuris-audio` 的 `SoundInfoQuery` 映射过来；
/// VM 只按 [`SoundField`] 提问。
pub trait SoundStateProbe {
    /// 查询某个声层 ID 的某个字段。
    ///
    /// 声层不存在时实现方应返回「引擎对不存在声层的应答」（`EXIST`=0、
    /// 其余按引擎口径），**不要**返回 `None` 了事 —— 返回 `None` 表示
    /// 「本实现不知道这个字段」，VM 会按缺失处理。
    fn query(&self, layer_id: &str, field: SoundField) -> Option<SoundInfoValue>;
}
