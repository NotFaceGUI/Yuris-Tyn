//! # yuris-input
//!
//! L6：输入层。把平台事件翻译成**引擎语义**的输入状态。
//!
//! ## 分层
//!
//! ```text
//! 平台壳(winit / Android / wasm / 测试)
//!      │  PlatformEvent（键、指针、滚轮、触摸、手柄、失焦）
//!      ▼
//! InputState::push_event  ──►  每逻辑帧 tick()  ──►  引擎状态
//!                                                     ├─ 64 位帧计数器（_INP / _KEY_* / _MOUSE_*）
//!                                                     ├─ 按下次数（_KEYNUM_* / _MOUSENUM_*）
//!                                                     ├─ _KEYLAST / _MOUSELAST
//!                                                     └─ 光标逻辑坐标（_MOUSE_X / _MOUSE_Y）
//! ```
//!
//! VM 侧通过 [`InputState::read_sysvar`] / [`InputState::write_sysvar`] /
//! [`InputState::inp`] / [`InputState::write_inp`] 读写，与引擎的
//! “系统变量 + `_INP` 数组”两个面一一对应。
//!
//! ## 证据等级
//!
//! - **Confirmed**：帧计数器四态转移（手册 `basic/input.html` +
//!   `FUN_0041a4b8`/`FUN_00461530`）、脚本写回钳制（`FUN_00447014`）、
//!   `_KEYLAST`/`_MOUSELAST`（`FUN_0045a3b4`）、`_INP` 按 VK 索引
//!   （`FUN_00427350`）、`INPUT[KEY/MOUSE/PAD]` 与 `MOUSE[MODE/TIME/MOVE/
//!   CLICK/WHEEL/CURSOR]` 关键字（官方手册 `system/input.html`、`system/mouse.html`）。
//! - **Likely / UNVERIFIED**：日文键盘符号键的 VK 逐值（见 [`keys`]）、
//!   触摸→鼠标映射（平台约定）。
//! - **Unknown**：`_MOUSE_L1/L2/L3` 等双击分级语义、手柄按钮编号与官方
//!   `_strtocode` 的 384.. 表不一致处 —— 一律按引擎表顺序输出，不做猜测性分级。
//!
//! ## 跨平台
//!
//! 核心模块**零平台依赖**（可在 wasm32 上编译）；`winit` 适配器是可选
//! feature。Android/wasm 壳只需要把各自的事件翻译成 [`PlatformEvent`]，
//! 触摸用 [`TouchAdapter`] 合成鼠标语义。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod coords;
pub mod hit;
pub mod keys;
pub mod mapping;
pub mod state;
pub mod touch;

#[cfg(feature = "winit")]
pub mod winit_adapter;

pub use backend::InputBackendImpl;
pub use coords::{LogicalPoint, Viewport};
pub use hit::{hit_topmost, AlphaMask, HitTarget, Rect};
pub use keys::{ButtonCode, Key};
pub use mapping::{PadAxis, PlatformEvent, PointerButton, TouchPhase};
pub use state::{InputLatches, InputSettings, InputSource, InputState};
pub use state::snapshot::InputSnapshotData;
pub use touch::TouchAdapter;

/// crate 版本（与 workspace 同步）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
