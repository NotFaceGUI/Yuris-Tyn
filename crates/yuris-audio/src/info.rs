//! 音频元信息与引擎 `SOUNDINFO` 的编码/文件类型映射。
//!
//! 证据等级：
//! - **Confirmed**：`CODEC` 的 `0=WAVE / 1=OGG / 2=ACM WAVE` 来自官方手册
//!   `sound/soundinfo.html`；
//! - **Confirmed**：`FILETYPE`（SOUNDINFO 槽 9）在 v494 处理器里直接返回
//!   声层的 `+0x105`；该字段在 `SOUND` 的装载路径按探测结果写入
//!   `5 → 2`（WAVE）、`6 → 1`（OGG）（`<game-b>_v494.exe` SOUND
//!   `0x44e07f`/`0x44e1ef..0x44e26f`，sha256 `1ff3135d…`）。

use std::fmt;

/// 官方 SDK 0.495 手册 `SOUNDINFO CODEC`：WAVE。
///
/// **注意**：样本 v494 二进制实测 WAVE = 2（见 [`AudioCodec::sound_code`]）。
/// 保留这三个常量是为了让调用方能显式区分两套口径，不是本库的默认回答。
pub const CODEC_WAVE: i64 = 0;
/// 官方 SDK 0.495 手册 `SOUNDINFO CODEC`：OGG（两套口径一致）。
pub const CODEC_OGG: i64 = 1;
/// 官方 SDK 0.495 手册 `SOUNDINFO CODEC`：ACM WAVE。
pub const CODEC_ACM_WAVE: i64 = 2;

/// 音频编码分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// 标准 PCM WAVE（引擎 `SOUNDINFO FILETYPE = 2`）。
    PcmWav,
    /// OGG Vorbis（引擎 `SOUNDINFO FILETYPE = 1`）。
    OggVorbis,
    /// ACM WAVE（特殊编码 WAVE；官方手册明言引擎不支持，本内核显式拒绝）。
    AcmWav,
    /// 其他容器/编码。
    Other,
}

impl AudioCodec {
    /// `SOUNDINFO CODEC` 查询值。
    ///
    /// **版本分叉（实测 + 手册不一致，留痕）**：
    /// - v494 二进制（样本 `<game-b>_v494.exe`）在解码器分派里写
    ///   `+0x107`：OGG = **1**（`0x422ca6`）、WAVE = **2**（`0x4233e4`）、
    ///   其他/ACM = **0**（`0x423552`）；`SOUNDINFO` 槽 22 原样返回该字段
    ///   （`0x44f93c`）。
    /// - 官方 SDK 0.495 手册 `sound/soundinfo.html` 则写
    ///   `0=WAVE / 1=OGG / 2=ACM WAVE`。
    ///
    /// 两者在 OGG 上一致、在 WAVE/ACM 上冲突。本库按**样本二进制实测值**
    /// 回答（`PcmWav=2`、`OggVorbis=1`、`AcmWav=0`）；使用 0.495+ 手册口径的
    /// 调用方需自行换算（或等待 `VersionProfile` 接入）。
    /// `Other` 不在任何取值面内，返回 `-1`，调用方应视为 `Unsupported`。
    pub fn sound_code(self) -> i64 {
        match self {
            Self::PcmWav => 2,
            Self::OggVorbis => 1,
            Self::AcmWav => 0,
            Self::Other => -1,
        }
    }

    /// `SOUNDINFO FILETYPE` 查询值（v494 SOUND 装载路径实测：OGG=1 / WAVE=2）。
    pub fn file_type_code(self) -> i64 {
        match self {
            Self::PcmWav => 2,
            Self::OggVorbis => 1,
            Self::AcmWav => 0,
            Self::Other => 0,
        }
    }

    /// 稳定的显示名（日志/错误用）。
    pub fn name(self) -> &'static str {
        match self {
            Self::PcmWav => "PCM WAVE",
            Self::OggVorbis => "OGG Vorbis",
            Self::AcmWav => "ACM WAVE",
            Self::Other => "unknown",
        }
    }

    /// 是否为引擎声明支持的编码面。
    pub fn is_supported(self) -> bool {
        matches!(self, Self::PcmWav | Self::OggVorbis)
    }
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// 解码后的音频元信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInfo {
    /// 编码分类。
    pub codec: AudioCodec,
    /// 声道数（1 / 2；大于 2 的容器仍记录原值，混音取前两声道）。
    pub channels: u16,
    /// 源采样率（Hz）。
    pub sample_rate: u32,
    /// 位深。
    ///
    /// WAVE 取容器声明值；OGG Vorbis 没有固定位深，取 **16**——v494 的
    /// `DATALEFT/DATARIGHT` 读数路径只接受 8/16 位（`0x44fd05` 的
    /// `cmp edx,8 / cmp edx,0x10`），说明引擎把 Vorbis 解成 16 位 PCM。
    pub bits_per_sample: u16,
    /// 每声道总帧数（解码完成后精确值）。
    pub total_frames: u64,
}

impl AudioInfo {
    /// 总时长（毫秒，向下取整）。
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.total_frames.saturating_mul(1000) / u64::from(self.sample_rate)
    }

    /// 帧数换算成毫秒。
    pub fn frames_to_ms(&self, frames: u64) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        frames.saturating_mul(1000) / u64::from(self.sample_rate)
    }
}
