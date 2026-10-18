//! yuris-debug —— 调试框架(成果 163):FPS 徽章 + VM 运行时序 HUD。
//!
//! ## 形态
//!
//! 独立 winit 调试窗(AlwaysOnTop、不抢焦点、自有 wgpu 设备),**不侵入**
//! 游戏画面渲染路径 —— `yuris-render` 的 present 管线与本框架零耦合。
//!
//! ## 与内核解耦(有意为之)
//!
//! 本 crate **不依赖** yuris-vm / yuris-player-core / yuris-render:
//! 遥测数据由播放器外壳(调用方,如 `yuris-cli`)在每帧推入 [`Telemetry`],
//! VM 状态由调用方映射成本 crate 自有的 [`VmRunState`]。这样内核侧的并行
//! 改动不会波及调试框架,反之亦然;接线点收敛在调用方的组合根里。
//!
//! ## 证据纪律
//!
//! HUD 展示的全部数字都是**墙钟实测**(各帧 tick/present 耗时、帧到达间隔),
//! 不含任何"看起来应该是这样"的引擎语义推断 —— 展示层不引入新的证据等级。

pub mod ease;
pub mod gfx;
pub mod hud;
pub mod telemetry;
pub mod text;
pub mod view;

pub use telemetry::{FrameSample, Telemetry, VmRunState};
pub use view::DebugOverlay;
