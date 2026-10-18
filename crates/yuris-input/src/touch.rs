//! 触摸 → 鼠标语义适配（Android / wasm）。
//!
//! 原引擎只有鼠标/键盘。移动端壳用这一层把触摸合成成引擎能理解的事件；
//! 映射表来自 `旁线仓库/docs/architecture.md` §8（**平台约定**，非引擎真值）：
//!
//! | 触摸 | 鼠标 |
//! |---|---|
//! | 单指按下 | 左键按下 |
//! | 单指移动 | 光标移动（按住期间是拖动） |
//! | 单指抬起 | 左键抬起 |
//! | 双指轻点 | 右键（取消/菜单） |
//! | 双指上下滑 | 滚轮 |
//!
//! 引擎只看结果（`_MOUSE_L` 计数、`_MOUSE_U/D` 脉冲），所以这一层不承载
//! 任何“兼容”承诺 —— 换壳可以换表。

use std::collections::HashMap;

use crate::mapping::{PlatformEvent, PointerButton, TouchPhase};

/// 触摸点状态。
#[derive(Debug, Clone, Copy)]
struct TouchPoint {
    start_px: (f64, f64),
    last_px: (f64, f64),
}

/// 触摸适配器：把 [`PlatformEvent::Touch`] 合成为鼠标事件。
#[derive(Debug, Clone)]
pub struct TouchAdapter {
    active: HashMap<u64, TouchPoint>,
    /// 轻点容差（像素）。
    pub tap_slop_px: f64,
    /// 双指上下滑每多少像素产生一格滚轮。
    pub wheel_scale_px: f64,
    /// 本次手势期间是否出现过第二根手指（用于双指轻点判定）。
    two_finger: bool,
    /// 本次手势已产生右键，抑制左键抬起。
    consumed: bool,
    /// 左键当前是否由触摸保持按住（第二指出现时要撤销）。
    left_down: bool,
    /// 双指滑动累计（像素；正 = 向下）。
    wheel_accum: f64,
}

impl Default for TouchAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl TouchAdapter {
    /// 新适配器（容差 16px、滚轮 60px/格）。
    pub fn new() -> Self {
        Self {
            active: HashMap::new(),
            tap_slop_px: 16.0,
            wheel_scale_px: 60.0,
            two_finger: false,
            consumed: false,
            left_down: false,
            wheel_accum: 0.0,
        }
    }

    /// 正在跟踪的触摸点数（诊断）。
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// 转换一条触摸事件为 0..n 条鼠标事件。
    pub fn translate(&mut self, id: u64, phase: TouchPhase, x: f64, y: f64) -> Vec<PlatformEvent> {
        let mut out = Vec::new();
        match phase {
            TouchPhase::Started => {
                self.active.insert(
                    id,
                    TouchPoint {
                        start_px: (x, y),
                        last_px: (x, y),
                    },
                );
                if self.active.len() == 1 {
                    // 第一根手指 = 光标 + 左键。
                    out.push(PlatformEvent::PointerMoved { x, y });
                    out.push(PlatformEvent::PointerButton {
                        button: PointerButton::Left,
                        pressed: true,
                    });
                    self.left_down = true;
                } else {
                    // 第二根手指出现 = 手势待判定。撤销第一指的误触左键，
                    // 否则双指轻点结束后左键会一直处于按住状态。
                    if self.left_down {
                        out.push(PlatformEvent::PointerButton {
                            button: PointerButton::Left,
                            pressed: false,
                        });
                        self.left_down = false;
                    }
                    self.two_finger = true;
                }
            }
            TouchPhase::Moved => {
                let Some(pt) = self.active.get_mut(&id) else {
                    return out;
                };
                let last_y = pt.last_px.1;
                pt.last_px = (x, y);
                if self.two_finger {
                    // 双指竖直滑 → 滚轮。累计到整格才发，避免抖动误触。
                    self.wheel_accum += y - last_y;
                    while self.wheel_accum.abs() >= self.wheel_scale_px {
                        let up = self.wheel_accum < 0.0;
                        out.push(PlatformEvent::Wheel {
                            delta: if up { 120 } else { -120 },
                            x,
                            y,
                        });
                        self.wheel_accum -= self.wheel_accum.signum() * self.wheel_scale_px;
                    }
                } else {
                    out.push(PlatformEvent::PointerMoved { x, y });
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some(pt) = self.active.remove(&id) else {
                    return out;
                };
                if self.two_finger && !self.consumed {
                    // 双指轻点：抬起时所有点都未超出容差、且期间没有滑动。
                    let moved = (x - pt.start_px.0).abs() > self.tap_slop_px
                        || (y - pt.start_px.1).abs() > self.tap_slop_px
                        || self.active.values().any(|o| {
                            (o.last_px.0 - o.start_px.0).abs() > self.tap_slop_px
                                || (o.last_px.1 - o.start_px.1).abs() > self.tap_slop_px
                        });
                    if !moved {
                        out.push(PlatformEvent::PointerButton {
                            button: PointerButton::Right,
                            pressed: true,
                        });
                        out.push(PlatformEvent::PointerButton {
                            button: PointerButton::Right,
                            pressed: false,
                        });
                        self.consumed = true;
                    }
                }
                if self.active.is_empty() {
                    // 单指手势的抬起：只有在左键仍由触摸按住时才发抬起。
                    if self.left_down {
                        out.push(PlatformEvent::PointerButton {
                            button: PointerButton::Left,
                            pressed: false,
                        });
                        self.left_down = false;
                    }
                    self.two_finger = false;
                    self.consumed = false;
                    self.wheel_accum = 0.0;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_touch_is_left_click() {
        let mut a = TouchAdapter::new();
        let ev = a.translate(1, TouchPhase::Started, 10.0, 20.0);
        assert_eq!(ev.len(), 2);
        assert!(matches!(ev[0], PlatformEvent::PointerMoved { .. }));
        assert!(matches!(
            ev[1],
            PlatformEvent::PointerButton {
                button: PointerButton::Left,
                pressed: true
            }
        ));
        let ev = a.translate(1, TouchPhase::Ended, 10.0, 20.0);
        assert!(matches!(
            ev[0],
            PlatformEvent::PointerButton {
                button: PointerButton::Left,
                pressed: false
            }
        ));
    }

    #[test]
    fn drag_moves_cursor() {
        let mut a = TouchAdapter::new();
        a.translate(1, TouchPhase::Started, 10.0, 20.0);
        let ev = a.translate(1, TouchPhase::Moved, 30.0, 40.0);
        assert!(matches!(ev[0], PlatformEvent::PointerMoved { x, y } if x == 30.0 && y == 40.0));
    }

    #[test]
    fn two_finger_slide_wheels() {
        let mut a = TouchAdapter::new();
        a.translate(1, TouchPhase::Started, 10.0, 20.0);
        let cancel = a.translate(2, TouchPhase::Started, 50.0, 20.0);
        // 第二指落下必须先撤销第一指产生的左键，否则左键会一直按住。
        assert!(cancel.iter().any(|e| matches!(
            e,
            PlatformEvent::PointerButton {
                button: PointerButton::Left,
                pressed: false
            }
        )));
        let ev = a.translate(1, TouchPhase::Moved, 10.0, -80.0);
        assert!(ev
            .iter()
            .any(|e| matches!(e, PlatformEvent::Wheel { delta, .. } if *delta > 0)));
        // 全部抬起后不再产生任何左键事件。
        let mut rest = a.translate(1, TouchPhase::Ended, 10.0, -80.0);
        rest.extend(a.translate(2, TouchPhase::Ended, 50.0, 20.0));
        assert!(!rest.iter().any(|e| matches!(
            e,
            PlatformEvent::PointerButton {
                button: PointerButton::Left,
                ..
            }
        )));
    }

    #[test]
    fn two_finger_tap_is_right_click_and_suppresses_left_release() {
        let mut a = TouchAdapter::new();
        a.translate(1, TouchPhase::Started, 10.0, 20.0);
        a.translate(2, TouchPhase::Started, 14.0, 22.0);
        let ev = a.translate(1, TouchPhase::Ended, 10.0, 20.0);
        assert!(ev.iter().any(|e| matches!(
            e,
            PlatformEvent::PointerButton {
                button: PointerButton::Right,
                pressed: true
            }
        )));
        let ev = a.translate(2, TouchPhase::Ended, 14.0, 22.0);
        assert!(
            !ev.iter().any(|e| matches!(
                e,
                PlatformEvent::PointerButton {
                    button: PointerButton::Left,
                    ..
                }
            )),
            "双指轻点不应再发左键抬起"
        );
    }
}
