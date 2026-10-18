//! # yuris-audio
//!
//! L6：音频后端。按引擎 `SOUND` 的**声层（sound layer）**模型实现
//! BGM / SE / Voice / 任意命名音频层，而不是固定的三条通道。
//!
//! ## 分层
//!
//! ```text
//! 脚本/播放器  ── SoundCommand / SoundInfoQuery ──┐
//!                                                  ▼
//!                                          AudioSystem（门面）
//!                                    装载/解码 │ 参数应用 │ 查询
//!                                                  ▼
//!                                            Mixer（纯状态机）
//!                                    声层 / 循环 / 淡变 / 音量 / 声像 / 音高
//!                                                  ▼
//!                                       cpal 输出（可选）/ 无头渲染
//! ```
//!
//! ## 证据等级
//!
//! - **Confirmed**：`SOUND`/`SOUNDEND`/`SOUNDINFO` 的参数名、范围与语义
//!   （官方手册 SDK 0.495 `sound/sound.html`、`sound/soundinfo.html`、
//!   `sound/soundend.html`），以及 v494 处理器
//!   （`<game-b>_v494.exe`，sha256 `1ff3135d…`：
//!   `SOUND 0x44e07c`、`SOUNDEND 0x44f19c`、`SOUNDINFO 0x44f350`）中的
//!   声层字段/事件队列布局。
//! - **Likely**：`FILETYPE` 的 `OGG=1 / WAVE=2`（v494 装载路径写入值 +
//!   `SOUNDINFO` 直接返回该字段）。
//! - **UNVERIFIED**（工程选择，已在代码处标注）：淡变曲线、声像曲线、
//!   线性插值重采样、混音限幅。
//! - **Unknown**：`SPEED`（v494 处理器不读取）、v555 与 ACM 特殊 WAVE 的
//!   播放细节。
//!
//! 所有未确认行为都返回显式错误或 `Unsupported`，**不猜**。
//!
//! ## 快速开始
//!
//! ```no_run
//! use yuris_audio::{AudioSystem, SoundCommand, SoundInfoQuery, SoundInfoValue};
//!
//! // 资源加载器：把逻辑名映射到 OGG/WAV 字节（封包/VFS 由调用方负责）。
//! let loader = Box::new(|name: &str| std::fs::read(name).ok());
//! let mut audio = AudioSystem::with_device(loader);
//!
//! let mut cmd = SoundCommand::new("BGM");
//! cmd.file = Some("bgm/theme".into()); // 自动尝试 .ogg / .wav
//! cmd.loop_count = Some(0);            // 无限循环
//! cmd.volume = Some(200);              // 0..256
//! cmd.play = Some(1);
//! audio.sound(&cmd).unwrap();
//!
//! let playing = audio.sound_info("BGM", SoundInfoQuery::Play).unwrap();
//! assert_eq!(playing, SoundInfoValue::Int(1));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod compat;
pub mod decode;
pub mod engine;
pub mod error;
pub mod info;
pub mod mixer;
#[cfg(feature = "device")]
pub mod output;
pub mod sound;

pub use backend::RuntimeAudio;
pub use compat::Audio;
pub use decode::{decode, probe, DecodedAudio};
pub use engine::{AudioLoader, AudioSystem, StateHandle};
pub use error::{AudioError, Result};
pub use info::{AudioCodec, AudioInfo, CODEC_ACM_WAVE, CODEC_OGG, CODEC_WAVE};
pub use mixer::{LayerSnapshot, Mixer, PlayState};
pub use sound::{
    search_candidates, SoundCommand, SoundInfoQuery, SoundInfoValue, SoundOutcome, PAN_CENTER,
    PITCH_NORMAL, VOLUME_MAX,
};

/// crate 版本（与 workspace 同步）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
