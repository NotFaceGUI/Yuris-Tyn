//! 音频层统一错误。

use std::path::PathBuf;

/// 音频层错误。
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// 没有可用的音频输出设备（无头运行；调用方可继续跑游戏逻辑）。
    #[error("audio device unavailable: {0}")]
    Device(String),
    /// 逻辑文件名在资源面不存在（`SOUND FILE=` 的候选扩展名全部未命中）。
    #[error("audio file not found: {0}")]
    NotFound(String),
    /// 容器/编码解码失败。
    #[error("audio decode failed: {0}")]
    Decode(String),
    /// 容器或编码不在引擎支持面内（引擎只接受 PCM WAVE 与 OGG Vorbis）。
    #[error("unsupported audio codec: {0}")]
    UnsupportedCodec(String),
    /// 参数越界（引擎参数范围见官方手册 `sound/sound.html`）。
    #[error("invalid sound parameter {name}={value}: {reason}")]
    InvalidParameter {
        /// 参数名（YSCM 名）。
        name: &'static str,
        /// 实际值。
        value: i64,
        /// 约束说明。
        reason: &'static str,
    },
    /// 内部状态错误（互斥锁中毒等）。
    #[error("audio internal error: {0}")]
    Internal(String),
    /// 读取/探测资源时的 IO 错误。
    #[error("audio io error at {path}: {source}")]
    Io {
        /// 出错路径（资源加载器可提供）。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: std::io::Error,
    },
}

/// 音频层结果。
pub type Result<T> = std::result::Result<T, AudioError>;
