//! `yuris-runtime::InputBackend` 实现。
//!
//! L4 的 trait 只有 `wait_click`（“最后一次点击是否仍在等待消费”）。
//! 本层把 [`InputState`] 的帧锁存桥到该语义：有未消费的点击 → `Some(())`。

use yuris_runtime::InputBackend;

use crate::state::{InputSource, InputState, InputSettings};

impl InputBackend for InputState {
    fn wait_click(&mut self) -> Option<()> {
        if self.take_clicked() {
            Some(())
        } else {
            None
        }
    }
}

/// 便于平台壳构造的输入后端：状态机 + 触摸适配器。
///
/// 播放器直接持有 [`InputState`]；本类型是给 `RuntimeApi` 装配用的组合。
#[derive(Debug, Clone, Default)]
pub struct InputBackendImpl {
    /// 引擎输入状态机。
    pub state: InputState,
    /// 触摸适配器（桌面壳可不用）。
    pub touch: crate::touch::TouchAdapter,
}

impl InputBackendImpl {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前设置。
    pub fn settings(&self) -> &InputSettings {
        self.state.settings()
    }

    /// 启用/禁用输入源。
    pub fn set_source_enabled(&mut self, source: InputSource, enabled: bool) {
        self.state.set_source_enabled(source, enabled);
    }

    /// 推入触摸事件（自动经适配器转鼠标）。
    pub fn push_touch(
        &mut self,
        id: u64,
        phase: crate::mapping::TouchPhase,
        x: f64,
        y: f64,
    ) {
        for ev in self.touch.translate(id, phase, x, y) {
            self.state.push_event(ev);
        }
    }
}

impl InputBackend for InputBackendImpl {
    fn wait_click(&mut self) -> Option<()> {
        self.state.wait_click()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{PlatformEvent, PointerButton};

    #[test]
    fn wait_click_consumes_once() {
        let mut input = InputBackendImpl::new();
        assert_eq!(input.wait_click(), None);
        input.state.push_event(PlatformEvent::PointerButton {
            button: PointerButton::Left,
            pressed: true,
        });
        input.state.tick();
        assert_eq!(input.wait_click(), Some(()));
        assert_eq!(input.wait_click(), None);
    }
}
