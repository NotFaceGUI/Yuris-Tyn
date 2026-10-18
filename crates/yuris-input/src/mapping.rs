//! 平台无关输入事件。
//!
//! 平台壳（winit / Android / wasm / 测试）只负责把原始事件翻译成
//! [`PlatformEvent`] 并推入 [`InputState`](crate::state::InputState)；
//! 引擎语义全部在状态机里实现，平台层不认识 `$55[1]`、`_INP` 或按钮码。
//!
//! 坐标一律是**窗口客户区物理像素**；由
//! [`Viewport`](crate::coords::Viewport) 反算成逻辑坐标。

/// 指针按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// 左键（引擎 `_MOUSE_L`，VK 1）。
    Left,
    /// 右键（`_MOUSE_R`，VK 2）。
    Right,
    /// 中键（`_MOUSE_M`，VK 4）。
    Middle,
    /// 其他按钮（引擎无变量，忽略状态但保留事件）。
    Other(u16),
}

/// 触摸阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    /// 按下。
    Started,
    /// 移动。
    Moved,
    /// 抬起。
    Ended,
    /// 被系统取消。
    Cancelled,
}

/// 手柄轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PadAxis {
    /// 左摇杆 X。
    LeftX,
    /// 左摇杆 Y。
    LeftY,
    /// 十字键 X（-1 左 / +1 右）。
    DPadX,
    /// 十字键 Y（-1 上 / +1 下）。
    DPadY,
}

/// 平台无关事件。
#[derive(Debug, Clone, PartialEq)]
pub enum PlatformEvent {
    /// 键盘按键状态变化。
    Key {
        /// 逻辑键。
        key: Key,
        /// true = 按下；false = 抬起。
        pressed: bool,
        /// 系统自动重复（不会重置计数器、不计按下次数）。
        repeat: bool,
    },
    /// 指针移动到窗口像素 `(x, y)`。
    PointerMoved {
        /// 窗口像素 X。
        x: f64,
        /// 窗口像素 Y。
        y: f64,
    },
    /// 指针按钮状态变化（坐标取状态机当前光标）。
    PointerButton {
        /// 按钮。
        button: PointerButton,
        /// true = 按下。
        pressed: bool,
    },
    /// 滚轮。
    Wheel {
        /// 有符号增量（Windows `WHEEL_DELTA` 口径：正 = 上/远离用户）。
        delta: i32,
        /// 窗口像素 X。
        x: f64,
        /// 窗口像素 Y。
        y: f64,
    },
    /// 触摸（由 [`TouchAdapter`](crate::touch::TouchAdapter) 转成鼠标语义）。
    Touch {
        /// 触摸点 id。
        id: u64,
        /// 阶段。
        phase: TouchPhase,
        /// 窗口像素 X。
        x: f64,
        /// 窗口像素 Y。
        y: f64,
    },
    /// 窗口失焦：释放全部按键（引擎 `WM_KILLFOCUS` 会清键盘状态）。
    FocusLost,
    /// 手柄按钮（`@_PAD_BUTTON(n, m)`，`m` 0..31）。
    PadButton {
        /// 手柄号（0..15）。
        pad: u8,
        /// 按钮号（0..31）。
        button: u8,
        /// true = 按下。
        pressed: bool,
    },
    /// 手柄轴（带死区；跨过死区时转成方向键按下/抬起）。
    PadAxis {
        /// 手柄号（0..15）。
        pad: u8,
        /// 轴。
        axis: PadAxis,
        /// 轴值（-1.0..1.0）。
        value: f32,
        /// 死区（绝对值小于它视为 0）。
        deadzone: f32,
    },
}

use crate::keys::Key;
