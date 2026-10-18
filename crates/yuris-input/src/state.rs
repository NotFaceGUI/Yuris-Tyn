//! 引擎输入状态机。
//!
//! ## 语义（Confirmed：官方手册 + 0.495 反汇编）
//!
//! 每个按钮保存一个 **64 位有符号帧计数器**（手册 `basic/input.html`）：
//!
//! ```text
//! 按下： 1 → 2 → 3 → 4 → …
//! 抬起： -1 → -2 → -3 → -4 → …
//! ```
//!
//! 手册明确：值是 64 位，按 60fps 计约 49 亿年才会溢出，因此可以放心
//! 逐帧 ±1（实现用 `wrapping_add/sub`）。
//!
//! 引擎每帧对每个键做四态转移（`FUN_0041a4b8` + `FUN_00461530` 的函数表
//! `0x801e84`，索引 = `prev_down + down`）：
//!
//! | prev | down | 动作 | 地址 |
//! |---|---|---|---|
//! | 0 | 0 | value -= 1 | `0x45a118` |
//! | 0 | 1 | value = +1 | `0x45a168` |
//! | 1 | 0 | value = -1 | `0x45a148` |
//! | 1 | 1 | value += 1 | `0x45a130` |
//!
//! ## 脚本写回（Confirmed：`FUN_00447014`）
//!
//! 脚本可以写输入变量（手册 ○ 标记），但引擎会按**当前物理状态**钳制：
//!
//! ```text
//! prev_down == 0: value >= 0 → 写 0；value < 0 → 写 value
//! prev_down != 0: value >  0 → 写 value；否则写 0
//! ```
//!
//! 这正是 ERIS 按钮脚本“点击只让一个按钮响应”的实现方式：
//! 物理按住时写 `@_INP(code)=2` 会保持 2；没按住时写正数会被清 0。
//!
//! ## `_KEYLAST` / `_MOUSELAST`（Confirmed：`FUN_0045a3b4`）
//!
//! - `WM_LBUTTONDOWN` → `DAT_0080190c = 1`；右键 = 2；中键 = 4。
//! - 键盘消息 → `DAT_00801908 = 键码`。
//! 脚本把它们清 0 以取消 skip（ERIS `es_Text` 实证）。

use std::collections::HashMap;

use crate::coords::{LogicalPoint, Viewport};
use crate::keys::{self, ButtonCode};
use crate::mapping::{PadAxis, PlatformEvent, PointerButton};

/// 输入源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    /// 键盘（`INPUT[KEY=…]`）。
    Keyboard,
    /// 鼠标（`INPUT[MOUSE=…]`）。
    Mouse,
    /// 手柄（`INPUT[PAD=…]`）。
    Pad,
}

/// `INPUT` / `MOUSE` 命令设置的输入参数。
#[derive(Debug, Clone, PartialEq)]
pub struct InputSettings {
    /// 键盘输入是否启用（`INPUT[KEY]`，缺省 1）。
    pub keyboard: bool,
    /// 鼠标输入是否启用（`INPUT[MOUSE]`，缺省 1）。
    pub mouse: bool,
    /// 手柄输入是否启用（`INPUT[PAD]`，缺省 1）。
    pub pad: bool,
    /// 光标显示模式（`MOUSE[MODE]`：0 常隐 / 1 自动隐藏 / 2 常显）。
    pub mouse_mode: u8,
    /// 自动隐藏毫秒数（`MOUSE[TIME]`）。
    pub mouse_time_ms: u32,
    /// 鼠标移动是否参与反应（`MOUSE[MOVE]`）。
    pub move_reactive: bool,
    /// 鼠标点击是否参与反应（`MOUSE[CLICK]`）。
    pub click_reactive: bool,
    /// 滚轮是否参与反应（`MOUSE[WHEEL]`）。
    pub wheel_reactive: bool,
    /// 光标形状编号（`MOUSE[CURSOR]`，0..13）。
    pub cursor: u8,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            keyboard: true,
            mouse: true,
            pad: true,
            // 引擎缺省光标可见（MOUSE 命令尚未执行时）。具体 MODE 缺省值
            // 手册未明说；ERIS 启动即 `MOUSE[MODE=1 TIME=2500 ...]`，取 2
            // 作为“可见”的安全缺省（UNVERIFIED）。
            mouse_mode: 2,
            mouse_time_ms: 2500,
            move_reactive: true,
            click_reactive: true,
            wheel_reactive: true,
            cursor: 0,
        }
    }
}

/// 单帧边沿/动作锁存（消费后清空）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputLatches {
    /// 本帧有左键按下（受 `MOUSE[CLICK]` 门控）。
    pub clicked: bool,
    /// 本帧有右键按下。
    pub right_clicked: bool,
    /// 本帧有任意键按下（键盘或鼠标）。
    pub any_pressed: bool,
    /// 本帧有 Esc 按下。
    pub escape: bool,
    /// 本帧有 Enter 按下。
    pub enter: bool,
    /// 本帧有 Space 按下。
    pub space: bool,
    /// Ctrl 当前是否按住。
    pub ctrl_held: bool,
}

impl InputLatches {
    /// 取走左键点击。
    pub fn take_clicked(&mut self) -> bool {
        std::mem::take(&mut self.clicked)
    }
    /// 取走任意按键。
    pub fn take_any_pressed(&mut self) -> bool {
        std::mem::take(&mut self.any_pressed)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ButtonSlot {
    /// 面板可见的 64 位帧计数器。
    value: i64,
    /// 上一帧的物理按下状态（引擎 `_DAT_00685400`）。
    down: bool,
    /// 按下次数（`_KEYNUM_*` / `_MOUSENUM_*`）。
    count: u32,
}

/// 引擎输入状态。
#[derive(Debug, Clone)]
pub struct InputState {
    viewport: Viewport,
    settings: InputSettings,
    buttons: [ButtonSlot; ButtonCode::VK_COUNT],
    /// 手柄方向（每个手柄 4 个：上/下/左/右，引擎 384..387）。
    pad_dirs: [[ButtonSlot; 4]; 16],
    /// 手柄按钮（每个手柄 32 个）。
    pad_buttons: [[ButtonSlot; 32]; 16],
    /// 本帧待提交的键盘/鼠标按下状态。
    pending: [bool; ButtonCode::VK_COUNT],
    /// 一次性脉冲（滚轮/鼠标移动）：本帧按下、下帧自动抬起。
    pulse: [bool; ButtonCode::VK_COUNT],
    pad_dir_pending: [[bool; 4]; 16],
    pad_button_pending: [[bool; 32]; 16],
    cursor_px: (f64, f64),
    cursor: LogicalPoint,
    /// `_CODE_*` 常量被脚本改写后的覆盖表（手册标 ○ = 可赋值；
    /// 键位设置功能靠它）。
    code_overrides: HashMap<u16, u16>,
    last_key: u16,
    last_mouse: u16,
    any: ButtonSlot,
    any_pending: bool,
    connected_pads: u8,
    frame: u64,
    latches: InputLatches,
    warp_request: Option<(i32, i32)>,
    /// 本帧按下边沿的按钮（用于 `$55[1]` 键名广播；取走锁存时清空）。
    press_edge: Option<ButtonCode>,
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

impl InputState {
    /// 新建状态（全部计数器 0；逻辑画布 1920×1080，窗口未初始化）。
    pub fn new() -> Self {
        Self {
            viewport: Viewport::default(),
            settings: InputSettings::default(),
            buttons: [ButtonSlot::default(); ButtonCode::VK_COUNT],
            pad_dirs: [[ButtonSlot::default(); 4]; 16],
            pad_buttons: [[ButtonSlot::default(); 32]; 16],
            pending: [false; ButtonCode::VK_COUNT],
            pulse: [false; ButtonCode::VK_COUNT],
            pad_dir_pending: [[false; 4]; 16],
            pad_button_pending: [[false; 32]; 16],
            cursor_px: (0.0, 0.0),
            cursor: LogicalPoint {
                x: 0,
                y: 0,
                inside: false,
            },
            code_overrides: HashMap::new(),
            last_key: 0,
            last_mouse: 0,
            any: ButtonSlot::default(),
            any_pending: false,
            connected_pads: 0,
            frame: 0,
            latches: InputLatches::default(),
            warp_request: None,
            press_edge: None,
        }
    }

    /// 当前逻辑视口。
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// 设置/更新视口。
    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = viewport;
        self.cursor = self.viewport.to_logical(self.cursor_px.0, self.cursor_px.1);
    }

    /// 窗口尺寸变化。
    pub fn resize(&mut self, w: u32, h: u32) {
        self.viewport.resize(w, h);
        self.cursor = self.viewport.to_logical(self.cursor_px.0, self.cursor_px.1);
    }

    /// 逻辑画布变化。
    pub fn set_logical_size(&mut self, w: f32, h: f32) {
        self.viewport.set_logical(w, h);
        self.cursor = self.viewport.to_logical(self.cursor_px.0, self.cursor_px.1);
    }

    /// 输入设置。
    pub fn settings(&self) -> &InputSettings {
        &self.settings
    }

    /// 可变输入设置（`INPUT`/`MOUSE` 命令用；优先走下面的具名方法）。
    pub fn settings_mut(&mut self) -> &mut InputSettings {
        &mut self.settings
    }

    /// 启用/禁用输入源（`INPUT[KEY/MOUSE/PAD]`）。
    ///
    /// 禁用后该源的事件被忽略，且状态机对该源**冻结**（不发散）。
    /// 重新启用时按当时的物理状态产生边沿 —— 与引擎“整段更新被跳过”
    /// 的形态一致（`FUN_0041a4b8` 的源开关分支）。
    pub fn set_source_enabled(&mut self, source: InputSource, enabled: bool) {
        match source {
            InputSource::Keyboard => self.settings.keyboard = enabled,
            InputSource::Mouse => self.settings.mouse = enabled,
            InputSource::Pad => self.settings.pad = enabled,
        }
    }

    /// 应用 `MOUSE[MODE/TIME/MOVE/CLICK/WHEEL/CURSOR]`（缺省项不变）。
    pub fn apply_mouse_settings(
        &mut self,
        mode: Option<u8>,
        time_ms: Option<u32>,
        move_reactive: Option<bool>,
        click_reactive: Option<bool>,
        wheel_reactive: Option<bool>,
        cursor: Option<u8>,
    ) {
        if let Some(v) = mode {
            self.settings.mouse_mode = v.min(2);
        }
        if let Some(v) = time_ms {
            self.settings.mouse_time_ms = v;
        }
        if let Some(v) = move_reactive {
            self.settings.move_reactive = v;
        }
        if let Some(v) = click_reactive {
            self.settings.click_reactive = v;
        }
        if let Some(v) = wheel_reactive {
            self.settings.wheel_reactive = v;
        }
        if let Some(v) = cursor {
            self.settings.cursor = v;
        }
    }

    /// 光标逻辑坐标（`@_MOUSE_X/@_MOUSE_Y`）。
    pub fn cursor(&self) -> (i64, i64) {
        (self.cursor.x, self.cursor.y)
    }

    /// 光标是否在画布内。
    pub fn cursor_inside(&self) -> bool {
        self.cursor.inside
    }

    /// 最近一次窗口像素坐标。
    pub fn cursor_window_px(&self) -> (f64, f64) {
        self.cursor_px
    }

    /// 直接设置逻辑光标（兼容旧 `set_input_cursor` 路径；反向换算窗口像素）。
    pub fn set_cursor_logical(&mut self, x: i64, y: i64) {
        self.cursor = LogicalPoint {
            x,
            y,
            inside: x >= 0
                && y >= 0
                && (x as f32) < self.viewport.logical_w
                && (y as f32) < self.viewport.logical_h,
        };
        let (wx, wy) = self.viewport.to_window(x as f64, y as f64);
        self.cursor_px = (wx, wy);
    }

    /// 请求把 OS 光标移到窗口像素坐标（`MOUSE[X/Y]`）。
    pub fn request_warp(&mut self, x: i32, y: i32) {
        self.warp_request = Some((x, y));
    }

    /// 取走光标 warp 请求（平台壳消费）。
    pub fn take_warp(&mut self) -> Option<(i32, i32)> {
        self.warp_request.take()
    }

    /// 已处理帧数。
    pub fn frame_index(&self) -> u64 {
        self.frame
    }

    /// 当前锁存。
    pub fn latches(&self) -> InputLatches {
        self.latches
    }

    /// 取走左键点击锁存。
    pub fn take_clicked(&mut self) -> bool {
        self.latches.take_clicked()
    }

    /// 取走“任意键按下”锁存。
    pub fn take_any_pressed(&mut self) -> bool {
        self.latches.take_any_pressed()
    }

    /// 取走全部本帧锁存（播放器在 tick 后调用一次）。
    pub fn take_latches(&mut self) -> InputLatches {
        let latches = std::mem::take(&mut self.latches);
        self.press_edge = None;
        latches
    }

    /// 本帧按下边沿的按钮名（引擎 `$55[1]` 键名广播用；无按下 → None）。
    ///
    /// 名字形如 `MOUSE_L` / `ENTER` / `F5` / `A`（不含 `_KEY_` 前缀）。
    /// 取值在 [`take_latches`](Self::take_latches) 时清空，保证同一帧
    /// 只广播一次。
    pub fn last_press_name(&self) -> Option<&'static str> {
        self.press_edge.and_then(keys::button_name)
    }

    /// 推入平台事件（在两次 [`tick`](Self::tick) 之间可推多条）。
    pub fn push_event(&mut self, ev: PlatformEvent) {
        match ev {
            PlatformEvent::Key {
                key,
                pressed,
                repeat,
            } => {
                if !self.settings.keyboard {
                    return;
                }
                let Some(code) = key.to_button() else {
                    return;
                };
                if !code.is_vk() {
                    return;
                }
                let idx = code.raw() as usize;
                if pressed {
                    if !repeat {
                        self.pending[idx] = true;
                        self.latches.any_pressed = true;
                        // WM_KEYDOWN → DAT_00801908 = 键码（`FUN_0045a3b4`）。
                        self.last_key = code.raw();
                        self.press_edge = Some(code);
                        match code {
                            keys::ESC => self.latches.escape = true,
                            keys::ENTER => self.latches.enter = true,
                            keys::SPACE => self.latches.space = true,
                            _ => {}
                        }
                    }
                } else {
                    self.pending[idx] = false;
                }
                if code == keys::CTRL {
                    self.latches.ctrl_held = pressed;
                }
            }
            PlatformEvent::PointerMoved { x, y } => {
                if !self.settings.mouse {
                    return;
                }
                self.update_cursor(x, y);
                if self.settings.move_reactive {
                    self.pulse[keys::MOUSE_MOVE.raw() as usize] = true;
                    self.pending[keys::MOUSE_MOVE.raw() as usize] = true;
                }
            }
            PlatformEvent::PointerButton { button, pressed } => {
                if !self.settings.mouse {
                    return;
                }
                let code = match button {
                    PointerButton::Left => keys::MOUSE_LEFT,
                    PointerButton::Right => keys::MOUSE_RIGHT,
                    PointerButton::Middle => keys::MOUSE_MIDDLE,
                    PointerButton::Other(_) => return,
                };
                let idx = code.raw() as usize;
                if pressed {
                    self.pending[idx] = true;
                    self.latches.any_pressed = true;
                    if code == keys::MOUSE_LEFT {
                        if self.settings.click_reactive {
                            self.latches.clicked = true;
                        }
                        self.last_mouse = keys::MOUSE_LEFT.raw();
                        self.press_edge = Some(code);
                    } else if code == keys::MOUSE_RIGHT {
                        self.latches.right_clicked = true;
                        self.last_mouse = keys::MOUSE_RIGHT.raw();
                        self.press_edge = Some(code);
                    } else {
                        self.last_mouse = keys::MOUSE_MIDDLE.raw();
                    }
                } else {
                    self.pending[idx] = false;
                }
            }
            PlatformEvent::Wheel { delta, x, y } => {
                if !self.settings.mouse {
                    return;
                }
                self.update_cursor(x, y);
                if delta == 0 || !self.settings.wheel_reactive {
                    return;
                }
                let code = if delta > 0 {
                    keys::WHEEL_UP
                } else {
                    keys::WHEEL_DOWN
                };
                let idx = code.raw() as usize;
                self.pulse[idx] = true;
                self.pending[idx] = true;
            }
            PlatformEvent::Touch { .. } => {
                // 触摸必须经 `TouchAdapter` 转成鼠标事件；直接推入会被忽略
                // （平台层错误使用会在调试构建中暴露）。
                debug_assert!(false, "PlatformEvent::Touch 需要 TouchAdapter");
            }
            PlatformEvent::FocusLost => {
                self.pending = [false; ButtonCode::VK_COUNT];
                self.pulse = [false; ButtonCode::VK_COUNT];
                self.pad_dir_pending = [[false; 4]; 16];
                self.pad_button_pending = [[false; 32]; 16];
                self.latches.ctrl_held = false;
            }
            PlatformEvent::PadButton {
                pad,
                button,
                pressed,
            } => {
                if !self.settings.pad {
                    return;
                }
                let pad = pad as usize;
                let button = button as usize;
                if pad >= 16 || button >= 32 {
                    return;
                }
                self.connected_pads |= 1 << pad.min(7);
                self.pad_button_pending[pad][button] = pressed;
                if pressed {
                    self.latches.any_pressed = true;
                }
            }
            PlatformEvent::PadAxis {
                pad,
                axis,
                value,
                deadzone,
            } => {
                if !self.settings.pad {
                    return;
                }
                let pad = pad as usize;
                if pad >= 16 {
                    return;
                }
                self.connected_pads |= 1 << pad.min(7);
                let dz = deadzone.max(0.0);
                let dir = match axis {
                    PadAxis::LeftY | PadAxis::DPadY => {
                        if value < -dz {
                            Some(0) // up
                        } else if value > dz {
                            Some(1) // down
                        } else {
                            None
                        }
                    }
                    PadAxis::LeftX | PadAxis::DPadX => {
                        if value < -dz {
                            Some(2) // left
                        } else if value > dz {
                            Some(3) // right
                        } else {
                            None
                        }
                    }
                };
                for i in 0..4 {
                    let should = dir == Some(i);
                    if !should {
                        self.pad_dir_pending[pad][i] = false;
                    }
                }
                if let Some(i) = dir {
                    self.pad_dir_pending[pad][i] = true;
                }
            }
        }
    }

    fn update_cursor(&mut self, x: f64, y: f64) {
        self.cursor_px = (x, y);
        self.cursor = self.viewport.to_logical(x, y);
    }

    fn source_allowed(&self, code: u16) -> bool {
        if code <= keys::MOUSE_MOVE.raw() {
            self.settings.mouse
        } else {
            self.settings.keyboard
        }
    }

    /// 推进一帧（每逻辑帧调用一次，**先于** VM 执行）。
    ///
    /// 顺序：平台事件已推入 → `tick` 提交物理状态 → 脚本在同一帧读取
    /// 到 1 / -1 / 递增计数。
    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        for code in 0..ButtonCode::VK_COUNT {
            if !self.source_allowed(code as u16) {
                continue;
            }
            let down = self.pending[code];
            let pulse = self.pulse[code];
            step_slot(&mut self.buttons[code], down, code as u16 != keys::MOUSE_MOVE.raw(), self.latches.any_pressed);
            if pulse {
                self.pending[code] = false;
                self.pulse[code] = false;
            }
        }
        // `_KEY_ANY`：任一键盘键按住时按“任意键”自己的计数器走。
        let any_down = (keys::BS.raw()..=keys::BACKSLASH.raw())
            .any(|c| self.buttons[c as usize].down);
        self.any_pending = any_down;
        step_slot(&mut self.any, any_down, true, self.latches.any_pressed);

        if self.settings.pad {
            for pad in 0..16 {
                for dir in 0..4 {
                    let down = self.pad_dir_pending[pad][dir];
                    step_slot(&mut self.pad_dirs[pad][dir], down, true, false);
                }
                for b in 0..32 {
                    let down = self.pad_button_pending[pad][b];
                    step_slot(&mut self.pad_buttons[pad][b], down, true, false);
                }
            }
        }
    }

    /// 读 `@_INP(code)`（VK 或手柄码）。
    pub fn inp(&self, code: u16) -> i64 {
        if (code as usize) < ButtonCode::VK_COUNT {
            return self.buttons[code as usize].value;
        }
        if (384..=387).contains(&code) {
            return self.pad_dirs[0][(code - 384) as usize].value;
        }
        if (388..420).contains(&code) {
            return self.pad_buttons[0][(code - 388) as usize].value;
        }
        0
    }

    /// 写 `@_INP(code)`（引擎 `FUN_00447014` 的钳制规则）。
    ///
    /// 返回 `false` = 码空间未知（未写）。
    pub fn write_inp(&mut self, code: u16, v: i64) -> bool {
        if (code as usize) < ButtonCode::VK_COUNT {
            write_clamped(&mut self.buttons[code as usize], v);
            return true;
        }
        if (384..=387).contains(&code) {
            write_clamped(&mut self.pad_dirs[0][(code - 384) as usize], v);
            return true;
        }
        if (388..420).contains(&code) {
            write_clamped(&mut self.pad_buttons[0][(code - 388) as usize], v);
            return true;
        }
        false
    }

    /// 该 id 是否属于引擎输入系统变量（`@_MOUSE_X`、`@_KEY_A`、`@_CODE_*`…）。
    pub fn is_input_sysvar(id: u16) -> bool {
        matches!(
            id,
            133..=166 | 167..=280 | 281..=409 | 410..=414 | 415..=547
        )
    }

    /// 读引擎输入系统变量（`id` = YSCM 系统变量下标）。
    ///
    /// - `133`/`138` = `_MOUSE_X/Y`（主窗口；`134..137`/`139..142` 是
    ///   另外三个窗口对象，本内核只有主窗口 → 0，引擎 `FUN_00403ee0`
    ///   对空 HWND 同样写 0）。
    /// - `144` = `_INP`（数组，必须用 [`inp`](Self::inp)）。
    /// - `145`/`146` = `_KEYLAST` / `_MOUSELAST`。
    /// - `147..166` = 鼠标按键状态（`L/L1..L3` 等同码；引擎是否为多击
    ///   分级另行 Unknown，见 `keys` 模块）。
    /// - `167..275` = 键盘状态（`275` = `_KEY_ANY`）。
    /// - `281..300` / `301..409` = 鼠标/键盘按下次数。
    /// - `410..414` = 手柄次数；`415..547` = `_CODE_*` 常量。
    pub fn read_sysvar(&self, id: u16) -> Option<i64> {
        match id {
            133 => return Some(self.cursor.x),
            134..=137 => return Some(0),
            138 => return Some(self.cursor.y),
            139..=142 => return Some(0),
            143 => return Some(self.connected_pads.count_ones() as i64),
            144 => return None,
            145 => return Some(self.last_key as i64),
            146 => return Some(self.last_mouse as i64),
            147..=166 => {
                let c = mouse_state_code(id)?;
                return Some(self.buttons[c as usize].value);
            }
            167..=274 => {
                let c = key_state_code(id)?;
                return Some(self.buttons[c as usize].value);
            }
            275 => return Some(self.any.value),
            276..=279 => return Some(self.pad_dirs[0][(id - 276) as usize].value),
            280 => return Some(self.pad_buttons[0][0].value),
            281..=300 => {
                let c = mouse_state_code(147 + (id - 281))?;
                return Some(self.buttons[c as usize].count as i64);
            }
            301..=408 => {
                let c = key_state_code(167 + (id - 301))?;
                return Some(self.buttons[c as usize].count as i64);
            }
            409 => return Some(self.any.count as i64),
            410..=413 => return Some(self.pad_dirs[0][(id - 410) as usize].count as i64),
            414 => return Some(
                self.pad_buttons[0]
                    .iter()
                    .map(|b| b.count as i64)
                    .sum::<i64>(),
            ),
            415..=547 => {
                // 脚本可改写（手册 ○；键位设置）。未改写 = 引擎常量。
                if let Some(v) = self.code_overrides.get(&id) {
                    return Some(*v as i64);
                }
                return default_code_constant(id);
            }
            _ => {}
        }
        None
    }

    /// 写引擎输入系统变量。
    ///
    /// 返回 `Some(true)` = 已写；`Some(false)` = 是输入变量但只读；
    /// `None` = 不是输入变量。`@_INP` 必须走 [`write_inp`](Self::write_inp)。
    ///
    /// 手册（`basic/svariable.html`）里带 ○ 的输入变量都可赋值。状态类
    /// （`_MOUSE_L` / `_KEY_*`）按引擎 `FUN_00447014` 的物理态钳制；
    /// 计数类直接写；`_CODE_*` 是可变常量表。
    pub fn write_sysvar(&mut self, id: u16, v: i64) -> Option<bool> {
        match id {
            133 => {
                self.set_cursor_logical(v, self.cursor.y);
                Some(true)
            }
            138 => {
                self.set_cursor_logical(self.cursor.x, v);
                Some(true)
            }
            145 => {
                self.last_key = v.clamp(0, u16::MAX as i64) as u16;
                Some(true)
            }
            146 => {
                self.last_mouse = v.clamp(0, u16::MAX as i64) as u16;
                Some(true)
            }
            147..=166 => {
                let c = mouse_state_code(id)?;
                write_clamped(&mut self.buttons[c as usize], v);
                Some(true)
            }
            167..=274 => {
                let c = key_state_code(id)?;
                write_clamped(&mut self.buttons[c as usize], v);
                Some(true)
            }
            275 => {
                write_clamped(&mut self.any, v);
                Some(true)
            }
            276..=279 => {
                write_clamped(&mut self.pad_dirs[0][(id - 276) as usize], v);
                Some(true)
            }
            280 => {
                write_clamped(&mut self.pad_buttons[0][0], v);
                Some(true)
            }
            281..=300 => {
                let c = mouse_state_code(147 + (id - 281))?;
                self.buttons[c as usize].count = v.max(0).min(u32::MAX as i64) as u32;
                Some(true)
            }
            301..=408 => {
                let c = key_state_code(167 + (id - 301))?;
                self.buttons[c as usize].count = v.max(0).min(u32::MAX as i64) as u32;
                Some(true)
            }
            409 => {
                self.any.count = v.max(0).min(u32::MAX as i64) as u32;
                Some(true)
            }
            410..=413 => {
                let slot = &mut self.pad_dirs[0][(id - 410) as usize];
                slot.count = v.max(0).min(u32::MAX as i64) as u32;
                Some(true)
            }
            414 => {
                // `_PADNUM_BUTTON` 是二维计数（(n,m)）；标量写入语义 Unknown。
                // **2026-10-17 用户要求**:以前这里静默返回 false —— 脚本的
                // 赋值被丢弃且完全无提示。现在大告警一次(按 id 去重)。
                yuris_log::alert_once(
                    module_path!(),
                    format!("input-write-414"),
                    "输入系统变量写入被丢弃\n\
                     id = 414 (_PADNUM_BUTTON)\n\
                     原因: 该变量是二维计数 (n,m),标量写入语义 Unknown(不猜)\n\
                     处置: 脚本此赋值不生效;需要二维下标路径的实机取证"
                        .to_string(),
                );
                Some(false)
            }
            415..=547 => {
                self.code_overrides
                    .insert(id, v.clamp(0, u16::MAX as i64) as u16);
                Some(true)
            }
            134..=137 | 139..=142 | 143 | 144 => {
                // 手册标为只读的输入变量:写入被忽略。这是**引擎语义**,
                // 但脚本作者容易误写 —— 留一条去重告警而不是完全无声。
                // (144 = `_INP` 的**带下标**写入走 `write_inp`,这里是标量
                //  路径,确实是脚本异常。)
                yuris_log::alert_once(
                    module_path!(),
                    format!("input-readonly:{id}"),
                    format!(
                        "向只读输入变量写入被忽略\n\
                         id = {id}\n\
                         处置: 手册标 ○ 的只写集合不含该 id;若脚本依赖写入,需重新取证"
                    ),
                );
                Some(false)
            }
            unknown => {
                // 既不在可写集合也不在只读集合 = **输入层不认识的 id**。
                // 以前返回 None 让调用方静默放弃。
                yuris_log::alert_once(
                    module_path!(),
                    format!("input-unknown-write:{id}"),
                    format!(
                        "输入层不认识的系统变量 id,写入无法落地\n\
                         id = {unknown}\n\
                         处置: 输入层变量表(133..=547)未覆盖该 id —— 需补表"
                    ),
                );
                None
            }
        }
    }

    /// 该 id 是否允许脚本赋值（手册 ○ 的输入变量集合）。
    pub fn is_writable_sysvar(id: u16) -> bool {
        matches!(
            id,
            133 | 138 | 144 | 145 | 146 | 147..=166 | 167..=275 | 276..=280
                | 281..=409 | 410..=414 | 415..=547
        )
    }

    /// 按下次数（诊断/测试）。
    pub fn press_count(&self, code: ButtonCode) -> u32 {
        if code.is_vk() {
            self.buttons[code.raw() as usize].count
        } else {
            0
        }
    }

    /// 生成 VM 侧快照（每帧 tick 之后调用一次）。
    ///
    /// 返回**纯数据**，`yuris-vm` 不需要认识本 crate。字段与引擎 id 空间
    /// 一一对应（SDK 0.495 的 YSCM 系统变量序号）：
    ///
    /// - 标量：133/138 光标、143 手柄数、145/146 last、147..=166 鼠标键、
    ///   167..=275 键盘键、281..=409 计数、410..=414 手柄计数、415..=547 常量。
    /// - `_INP`：0..=255 的完整按钮码表（引擎按 VK 索引的 64 位计数表）。
    pub fn snapshot(&self) -> snapshot::InputSnapshotData {
        use std::collections::{HashMap, HashSet};
        let mut scalar = HashMap::new();
        let mut writable = HashSet::new();
        let mut inp = Vec::with_capacity(ButtonCode::VK_COUNT);
        for code in 0..ButtonCode::VK_COUNT as u16 {
            inp.push(self.buttons[code as usize].value);
        }
        // 光标
        scalar.insert(133, self.cursor.x);
        scalar.insert(134, 0);
        scalar.insert(135, 0);
        scalar.insert(136, 0);
        scalar.insert(137, 0);
        scalar.insert(138, self.cursor.y);
        scalar.insert(139, 0);
        scalar.insert(140, 0);
        scalar.insert(141, 0);
        scalar.insert(142, 0);
        scalar.insert(143, self.connected_pads.count_ones() as i64);
        scalar.insert(145, self.last_key as i64);
        scalar.insert(146, self.last_mouse as i64);
        // 状态/计数/常量：全量展开（547-133+1 ≈ 415 项，每帧一次可接受；
        // 避免 VM 侧再维护一份 id→code 表 —— 单一真值在本模块）。
        for id in 147..=409u16 {
            if let Some(v) = self.read_sysvar(id) {
                scalar.insert(id, v);
                if Self::is_writable_sysvar(id) {
                    writable.insert(id);
                }
            }
        }
        for id in 415..=547u16 {
            if let Some(v) = self.read_sysvar(id) {
                scalar.insert(id, v);
                writable.insert(id);
            }
        }
        // 可写的状态类（引擎手册 ○）
        for id in [133u16, 138, 145, 146] {
            writable.insert(id);
        }
        writable.insert(144);
        // 手柄数组：`_PAD_UP/DOWN/LEFT/RIGHT` 各是一条 16 项向量（按手柄号），
        // `_PAD_BUTTON` 是 16×32 扁平表。
        let mut pad = HashMap::new();
        for dir in 0..4usize {
            pad.insert(
                276 + dir as u16,
                (0..16).map(|p| self.pad_dirs[p][dir].value).collect::<Vec<_>>(),
            );
        }
        let mut buttons = Vec::with_capacity(16 * 32);
        for p in 0..16 {
            for b in 0..32 {
                buttons.push(self.pad_buttons[p][b].value);
            }
        }
        pad.insert(280u16, buttons);
        // 物理按下集合（写回钳制）
        let mut down = HashSet::new();
        for code in 0..ButtonCode::VK_COUNT {
            if self.buttons[code].down {
                down.insert(code as u16);
            }
        }
        snapshot::InputSnapshotData {
            // 输入层按 SDK 0.495 的完整 id 面导出；**是否采纳**由播放器
            // 按引擎版本决定（VM 侧 `InputSnapshot::claimed`）。
            claimed: (133..=547).collect(),
            scalar,
            pad,
            writable,
            down,
            inp,
        }
    }
}

/// 每帧快照（纯数据；`yuris-vm` 的 `InputSnapshot` 由播放器转换）。
pub mod snapshot {
    use std::collections::{HashMap, HashSet};

    /// 见 [`super::InputState::snapshot`]。
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct InputSnapshotData {
        /// 本帧声明的引擎输入系统变量 id 集合（输入层按 SDK 0.495 导出，
        /// 播放器按引擎版本裁剪）。
        pub claimed: HashSet<u16>,
        /// 标量系统变量。
        pub scalar: HashMap<u16, i64>,
        /// 数组型（手柄）。
        pub pad: HashMap<u16, Vec<i64>>,
        /// 可写 id 集合（手册 ○）。
        pub writable: HashSet<u16>,
        /// 物理按住按钮码。
        pub down: HashSet<u16>,
        /// `_INP`（下标 = 按钮码）。
        pub inp: Vec<i64>,
    }
}

/// 单键四态转移（引擎函数表 1:1）。
fn step_slot(slot: &mut ButtonSlot, down: bool, count_edge: bool, _any: bool) {
    let prev = slot.down;
    slot.value = match (prev, down) {
        (false, false) => slot.value.wrapping_sub(1),
        (false, true) => 1,
        (true, false) => -1,
        (true, true) => slot.value.wrapping_add(1),
    };
    if count_edge && down && !prev {
        slot.count = slot.count.saturating_add(1);
    }
    slot.down = down;
}

fn write_clamped(slot: &mut ButtonSlot, v: i64) {
    if !slot.down {
        slot.value = if v >= 0 { 0 } else { v };
    } else {
        slot.value = if v > 0 { v } else { 0 };
    }
}

const MOUSE_STATE_CODES: [u16; 20] = [
    1, 1, 1, 1, // L, L1, L2, L3
    2, 2, 2, 2, // R, R1, R2, R3
    4, 4, 4, 4, // M, M1, M2, M3
    5, 5, 5, 5, // U, U1, U2, U3
    6, 6, 6, 6, // D, D1, D2, D3
];

const KEY_STATE_CODES: [u16; 108] = [
    // A-Z
    0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E,
    0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A,
    // 0-9
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
    // F1-F24
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D,
    0x7E, 0x7F, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    // 小键盘 0-9
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    // 小键盘 + - * / .
    0x6B, 0x6D, 0x6A, 0x6F, 0x6E,
    // BS, TAB, ENTER, SPACE, SHIFT, CTRL, ALT, LEFT, RIGHT, UP, DOWN,
    // INSERT, DELETE, HELP, SCROLL, NUMLOCK, ESC, PAGEUP, PAGEDOWN,
    // HYPHEN, HOME, END, CIRCUMFLEX, YEN, AT, LEFTBRACKET, SEMICOLON,
    // COLON, RIGHTBRACKET, COMMA, PERIOD, SLASH, BACKSLASH
    0x08, 0x09, 0x0D, 0x20, 0x10, 0x11, 0x12, 0x25, 0x27, 0x26, 0x28, 0x2D, 0x2E, 0x2F,
    0x91, 0x90, 0x1B, 0x21, 0x22, 0xBD, 0x24, 0x23, 0xDE, 0xDC, 0xC0, 0xDB, 0xBA, 0xBA,
    0xDD, 0xBC, 0xBE, 0xBF, 0xE2,
];

fn mouse_state_code(id: u16) -> Option<u16> {    if (147..=166).contains(&id) {
        Some(MOUSE_STATE_CODES[(id - 147) as usize])
    } else {
        None
    }
}

fn key_state_code(id: u16) -> Option<u16> {
    if (167..=274).contains(&id) {
        Some(KEY_STATE_CODES[(id - 167) as usize])
    } else {
        None
    }
}

/// `_CODE_*`（415..=547）的引擎缺省值。
fn default_code_constant(id: u16) -> Option<i64> {
    if (415..=434).contains(&id) {
        return mouse_state_code(147 + (id - 415)).map(i64::from);
    }
    if (435..=542).contains(&id) {
        return key_state_code(167 + (id - 435)).map(i64::from);
    }
    if (543..=546).contains(&id) {
        return Some(384 + (id - 543) as i64);
    }
    if id == 547 {
        return Some(388);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Key;

    fn press(state: &mut InputState, key: Key) {
        state.push_event(PlatformEvent::Key {
            key,
            pressed: true,
            repeat: false,
        });
    }

    fn release(state: &mut InputState, key: Key) {
        state.push_event(PlatformEvent::Key {
            key,
            pressed: false,
            repeat: false,
        });
    }

    #[test]
    fn counter_follows_engine_four_state_machine() {
        let mut s = InputState::new();
        // 未按下：每帧 -1。
        s.tick();
        assert_eq!(s.inp(0x41), -1);
        s.tick();
        assert_eq!(s.inp(0x41), -2);
        // 按下帧 = 1。
        press(&mut s, Key::Char('a'));
        s.tick();
        assert_eq!(s.inp(0x41), 1);
        // 持续按住：2、3。
        s.tick();
        assert_eq!(s.inp(0x41), 2);
        s.tick();
        assert_eq!(s.inp(0x41), 3);
        // 抬起帧 = -1。
        release(&mut s, Key::Char('a'));
        s.tick();
        assert_eq!(s.inp(0x41), -1);
        s.tick();
        assert_eq!(s.inp(0x41), -2);
    }

    #[test]
    fn press_count_increments_on_edges_only() {
        let mut s = InputState::new();
        for _ in 0..3 {
            press(&mut s, Key::Char('a'));
            s.tick();
            s.tick();
            release(&mut s, Key::Char('a'));
            s.tick();
        }
        assert_eq!(s.press_count(ButtonCode(0x41)), 3);
        // 系统自动重复不计。
        press(&mut s, Key::Char('a'));
        s.tick();
        s.push_event(PlatformEvent::Key {
            key: Key::Char('a'),
            pressed: true,
            repeat: true,
        });
        s.tick();
        assert_eq!(s.press_count(ButtonCode(0x41)), 4);
    }

    #[test]
    fn script_write_clamps_by_physical_state() {
        let mut s = InputState::new();
        // 未按住：写正数 → 0。
        assert_eq!(s.write_sysvar(147, 5), Some(true)); // _MOUSE_L
        assert_eq!(s.read_sysvar(147), Some(0));
        // 未按住：写负数 → 保留。
        assert_eq!(s.write_sysvar(147, -7), Some(true));
        assert_eq!(s.read_sysvar(147), Some(-7));
        // 按住：写正数 → 保留。
        s.push_event(PlatformEvent::PointerButton {
            button: PointerButton::Left,
            pressed: true,
        });
        s.tick();
        assert_eq!(s.read_sysvar(147), Some(1));
        assert_eq!(s.write_sysvar(147, 2), Some(true));
        assert_eq!(s.read_sysvar(147), Some(2));
        // 按住：写 0 → 0。
        assert_eq!(s.write_sysvar(147, 0), Some(true));
        assert_eq!(s.read_sysvar(147), Some(0));
    }

    #[test]
    fn inp_write_uses_same_clamp() {
        let mut s = InputState::new();
        s.write_inp(0x41, 2);
        assert_eq!(s.inp(0x41), 0);
        press(&mut s, Key::Char('a'));
        s.tick();
        s.write_inp(0x41, 2);
        assert_eq!(s.inp(0x41), 2);
    }

    #[test]
    fn code_constants_match_key_state_ids() {
        let s = InputState::new();
        assert_eq!(s.read_sysvar(435), Some(0x41)); // _CODE_A
        assert_eq!(s.read_sysvar(415), Some(0x01)); // _CODE_MOUSE_L
        assert_eq!(s.read_sysvar(512), Some(0x0D)); // _CODE_ENTER
        assert_eq!(s.read_sysvar(471), Some(0x70)); // _CODE_F1
        assert_eq!(s.read_sysvar(495), Some(0x60)); // _CODE_NUM0
        assert_eq!(s.read_sysvar(542), Some(0xE2)); // _CODE_BACKSLASH
    }

    #[test]
    fn mouse_state_ids_share_button_slot() {
        let mut s = InputState::new();
        s.push_event(PlatformEvent::PointerButton {
            button: PointerButton::Left,
            pressed: true,
        });
        s.tick();
        for id in 147..=150 {
            assert_eq!(s.read_sysvar(id), Some(1), "id {id}");
        }
    }

    #[test]
    fn key_any_tracks_any_keyboard_key() {
        let mut s = InputState::new();
        s.tick();
        assert_eq!(s.read_sysvar(275), Some(-1));
        press(&mut s, Key::Char('a'));
        s.tick();
        assert_eq!(s.read_sysvar(275), Some(1));
        s.tick();
        assert_eq!(s.read_sysvar(275), Some(2));
        release(&mut s, Key::Char('a'));
        s.tick();
        assert_eq!(s.read_sysvar(275), Some(-1));
    }

    #[test]
    fn last_key_and_mouse_follow_edges() {
        let mut s = InputState::new();
        press(&mut s, Key::Enter);
        s.tick();
        assert_eq!(s.read_sysvar(145), Some(0x0D));
        s.push_event(PlatformEvent::PointerButton {
            button: PointerButton::Right,
            pressed: true,
        });
        s.tick();
        assert_eq!(s.read_sysvar(146), Some(2));
        // 脚本清 0（ERIS 取消 skip 的写法）。
        assert_eq!(s.write_sysvar(145, 0), Some(true));
        assert_eq!(s.read_sysvar(145), Some(0));
    }

    #[test]
    fn wheel_is_pulse() {
        let mut s = InputState::new();
        s.push_event(PlatformEvent::Wheel {
            delta: 120,
            x: 10.0,
            y: 10.0,
        });
        s.tick();
        assert_eq!(s.inp(keys::WHEEL_UP.raw()), 1);
        s.tick();
        assert_eq!(s.inp(keys::WHEEL_UP.raw()), -1);
    }

    #[test]
    fn focus_lost_releases_keys() {
        let mut s = InputState::new();
        press(&mut s, Key::Char('a'));
        s.tick();
        assert_eq!(s.inp(0x41), 1);
        s.push_event(PlatformEvent::FocusLost);
        s.tick();
        assert_eq!(s.inp(0x41), -1);
    }

    #[test]
    fn disabled_source_is_frozen() {
        let mut s = InputState::new();
        press(&mut s, Key::Char('a'));
        s.tick();
        assert_eq!(s.inp(0x41), 1);
        s.set_source_enabled(InputSource::Keyboard, false);
        s.tick();
        assert_eq!(s.inp(0x41), 1, "冻结：不递增也不递减");
        s.set_source_enabled(InputSource::Keyboard, true);
        s.tick();
        assert_eq!(s.inp(0x41), 2);
    }

    #[test]
    fn cursor_uses_letterbox_inverse() {        let mut s = InputState::new();
        s.set_viewport(Viewport::new(1280, 960, 1920.0, 1080.0));
        s.push_event(PlatformEvent::PointerMoved { x: 0.0, y: 120.0 });
        assert_eq!(s.cursor(), (0, 0));
        assert!(s.cursor_inside());
        s.push_event(PlatformEvent::PointerMoved { x: 640.0, y: 480.0 });
        assert_eq!(s.cursor(), (960, 540));
    }

    #[test]
    fn snapshot_exposes_engine_ids() {
        let mut s = InputState::new();
        press(&mut s, Key::Char('a'));
        s.tick();
        let snap = s.snapshot();
        assert_eq!(snap.scalar.get(&133), Some(&0));
        assert_eq!(snap.scalar.get(&145), Some(&0x41));
        assert_eq!(snap.scalar.get(&167), Some(&1)); // _KEY_A
        assert_eq!(snap.scalar.get(&435), Some(&0x41)); // _CODE_A
        assert_eq!(snap.inp.get(0x41), Some(&1));
        assert!(snap.writable.contains(&147));
        assert!(!snap.writable.contains(&548)); // 非输入 id
        assert!(snap.down.contains(&0x41));
        assert_eq!(snap.pad.get(&276).map(|v| v.len()), Some(16));
        assert_eq!(snap.pad.get(&280).map(|v| v.len()), Some(512));
    }
}
