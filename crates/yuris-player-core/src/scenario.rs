//! 剧本(Scenario)通道 —— **实现已迁至 `yuris-vm::scenario`**(成果 168)。
//!
//! 本模块只做两件事:
//! 1. 重新导出 VM 侧的运行器与统一文本类型(播放器既有调用点零改动);
//! 2. 保留 `Player: ScenarioHost` 的宿主契约名(`yuris_player_core::ScenarioHost`)。
//!
//! 迁移理由:剧本页与 VM 组级执行同属「脚本解释」层。两种发行形态 ——
//! v555 明文 `.txt` 剧本、v49x 编译内联 `WORD` —— 现在共用同一条
//! `TextChannel`,播放器不再各自维护一份文本路径与编码判定。

pub use yuris_vm::scenario::{
    ScenarioHost, ScenarioPlayer, TextChannel, TextLine, TextSource, TitleMenuAction,
};
