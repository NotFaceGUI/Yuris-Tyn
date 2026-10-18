//! 播放器侧音频：转发到 [`yuris_audio::compat`]。
//!
//! **成果 162（2026-10-17）**：原 rodio 直连实现（三通道 + `Sink`）已退役。
//! 现由 `yuris-audio` 的声层模型统一提供：
//! - 同一套状态机同时服务脚本 `SOUND` 命令与播放器快捷调用；
//! - BGM / VOICE / SE 只是声层 ID 的桥接约定（见 [`yuris_audio::compat`] 文档）；
//! - 解码、淡变、循环、音量/声像/音高与 `SOUNDINFO` 查询共用一条路径；
//! - 无输出设备时自动降级无头（不再依赖 rodio 的 `OutputStream`）。
//!
//! 保留 `Audio` 的公开方法与 `se_log` 字段，调用点无需改动。

pub use yuris_audio::compat::Audio;
