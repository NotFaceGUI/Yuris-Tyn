//! winit 0.30 事件适配（`feature = "winit"`）。
//!
//! 只做**翻译**：把 `WindowEvent` 变成 [`PlatformEvent`]。窗口创建、事件循环
//! 与 surface 生命周期留在平台壳（`yuris-cli` / Android）。
//!
//! 坐标来自 `CursorMoved` / `MouseInput` 携带的位置（winit 0.30 的
//! `MouseInput` 本身不带坐标，调用方应把最近一次 `CursorMoved` 的像素坐标
//! 传进来 —— 触屏合成点击就是这种情况）。

use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WinitKey, NamedKey};

use crate::keys::Key;
use crate::mapping::{PlatformEvent, PointerButton, TouchPhase};

/// winit 逻辑键 → 引擎键。
pub fn map_key(key: &WinitKey) -> Option<Key> {
    Some(match key {
        WinitKey::Named(named) => match named {
            NamedKey::Enter => Key::Enter,
            NamedKey::Escape => Key::Escape,
            NamedKey::Space => Key::Space,
            NamedKey::Tab => Key::Tab,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Shift => Key::Shift,
            NamedKey::Control => Key::Control,
            NamedKey::Alt => Key::Alt,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::Insert => Key::Insert,
            NamedKey::Delete => Key::Delete,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::NumLock => Key::NumLock,
            NamedKey::ScrollLock => Key::ScrollLock,
            NamedKey::Help => Key::Help,
            NamedKey::F1 => Key::F(1),
            NamedKey::F2 => Key::F(2),
            NamedKey::F3 => Key::F(3),
            NamedKey::F4 => Key::F(4),
            NamedKey::F5 => Key::F(5),
            NamedKey::F6 => Key::F(6),
            NamedKey::F7 => Key::F(7),
            NamedKey::F8 => Key::F(8),
            NamedKey::F9 => Key::F(9),
            NamedKey::F10 => Key::F(10),
            NamedKey::F11 => Key::F(11),
            NamedKey::F12 => Key::F(12),
            NamedKey::F13 => Key::F(13),
            NamedKey::F14 => Key::F(14),
            NamedKey::F15 => Key::F(15),
            NamedKey::F16 => Key::F(16),
            NamedKey::F17 => Key::F(17),
            NamedKey::F18 => Key::F(18),
            NamedKey::F19 => Key::F(19),
            NamedKey::F20 => Key::F(20),
            NamedKey::F21 => Key::F(21),
            NamedKey::F22 => Key::F(22),
            NamedKey::F23 => Key::F(23),
            NamedKey::F24 => Key::F(24),
            _ => return None,
        },
        WinitKey::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // 多字符 = 输入法/组合键中间态；引擎没有对应键。
                return None;
            }
            Key::Char(c)
        }
        _ => return None,
    })
}

/// 鼠标按钮映射。
pub fn map_button(button: MouseButton) -> PointerButton {
    match button {
        MouseButton::Left => PointerButton::Left,
        MouseButton::Right => PointerButton::Right,
        MouseButton::Middle => PointerButton::Middle,
        MouseButton::Back => PointerButton::Other(3),
        MouseButton::Forward => PointerButton::Other(4),
        MouseButton::Other(n) => PointerButton::Other(n as u16),
    }
}

/// 把一条 `WindowEvent` 翻译成 0..n 条 [`PlatformEvent`]。
///
/// `cursor_px` 是壳维护的最近光标像素坐标（`MouseInput` 不带位置）。
pub fn translate(event: &WindowEvent, cursor_px: (f64, f64)) -> Vec<PlatformEvent> {
    let mut out = Vec::new();
    match event {
        WindowEvent::KeyboardInput { event, .. } => {
            let Some(key) = map_key(&event.logical_key) else {
                return out;
            };
            out.push(PlatformEvent::Key {
                key,
                pressed: event.state == ElementState::Pressed,
                repeat: event.repeat,
            });
        }
        WindowEvent::CursorMoved { position, .. } => {
            out.push(PlatformEvent::PointerMoved {
                x: position.x,
                y: position.y,
            });
        }
        WindowEvent::MouseInput { state, button, .. } => {
            out.push(PlatformEvent::PointerButton {
                button: map_button(*button),
                pressed: *state == ElementState::Pressed,
            });
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (*x as f64, *y as f64),
                MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
            };
            let _ = dx;
            if dy != 0.0 {
                let step: f64 = if dy.abs() >= 1.0 {
                    // 行增量：winit 正 = 向上/远离用户，与 WHEEL_DELTA 同号。
                    if dy > 0.0 { 120.0 } else { -120.0 }
                } else {
                    // 像素增量：按 1 格 / 60px 折算，至少 1。
                    let units = (dy.abs() / 60.0).max(1.0);
                    (if dy > 0.0 { 1.0 } else { -1.0 }) * 120.0 * units
                };
                out.push(PlatformEvent::Wheel {
                    delta: step as i32,
                    x: cursor_px.0,
                    y: cursor_px.1,
                });
            }
        }
        WindowEvent::Focused(false) => out.push(PlatformEvent::FocusLost),
        WindowEvent::Touch(touch) => {
            let phase = match touch.phase {
                winit::event::TouchPhase::Started => TouchPhase::Started,
                winit::event::TouchPhase::Moved => TouchPhase::Moved,
                winit::event::TouchPhase::Ended => TouchPhase::Ended,
                winit::event::TouchPhase::Cancelled => TouchPhase::Cancelled,
            };
            out.push(PlatformEvent::Touch {
                id: touch.id,
                phase,
                x: touch.location.x,
                y: touch.location.y,
            });
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_named_keys() {
        assert_eq!(map_key(&WinitKey::Named(NamedKey::Enter)), Some(Key::Enter));
        assert_eq!(map_key(&WinitKey::Named(NamedKey::F9)), Some(Key::F(9)));
        assert_eq!(
            map_key(&WinitKey::Named(NamedKey::ArrowLeft)),
            Some(Key::Left)
        );
    }

    #[test]
    fn maps_character_keys() {
        assert_eq!(map_key(&WinitKey::Character("a".into())), Some(Key::Char('a')));
        assert_eq!(map_key(&WinitKey::Character("A".into())), Some(Key::Char('A')));
        assert_eq!(map_key(&WinitKey::Character("ab".into())), None);
    }

    #[test]
    fn maps_mouse_buttons() {
        assert_eq!(map_button(MouseButton::Left), PointerButton::Left);
        assert_eq!(map_button(MouseButton::Right), PointerButton::Right);
        assert_eq!(map_button(MouseButton::Middle), PointerButton::Middle);
        assert_eq!(map_button(MouseButton::Back), PointerButton::Other(3));
    }

    #[test]
    fn maps_focus_loss() {
        let out = translate(&WindowEvent::Focused(false), (0.0, 0.0));
        assert!(matches!(out.as_slice(), [PlatformEvent::FocusLost]));
    }
}
