//! 调试遥测:每帧一个样本,由播放器外壳(调用方)在帧边界推入。
//!
//! 有意**不依赖** yuris-vm / yuris-player-core:VM 状态由调用方从引擎状态
//! 映射成 [`VmRunState`],耗时由调用方用墙钟实测后填入。本 crate 与内核
//! 的并行改动因此完全解耦 —— 调试框架只消费数字,不解释引擎语义。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// VM 运行状态(调用方映射;HUD 只负责展示配色与文案)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmRunState {
    /// 尚未启动。
    Idle,
    /// 可继续执行(配额内)。
    Running,
    /// 脚本执行完毕。
    Finished,
    /// 遇到未证实/未实现语义,已挂起。
    Error,
}

/// 一帧的运行样本。`*_ns` 均为该帧各段的墙钟实测耗时。
#[derive(Debug, Clone, Copy)]
pub struct FrameSample {
    /// 帧起工时刻(RedrawRequested 起点)。
    pub at: Instant,
    /// VM tick(yuris-vm 执行段)耗时。
    pub vm_ns: u64,
    /// 渲染(present 前)耗时。
    pub render_ns: u64,
    /// 整帧耗时(含以上两段与间隙)。
    pub total_ns: u64,
    /// 本帧末的 VM 状态。
    pub vm_state: VmRunState,
    /// 引擎设定的目标帧率(`FPS[SET=n]`);0 = 未配置。
    pub fps_target: u32,
}

/// 环形容量:600 帧 ≈ 10 秒 @60fps。
const CAP: usize = 600;

/// 遥测句柄:`Clone` 共享环形缓冲;推送方与 HUD 同线程(当前接线形态),
/// 内部互斥锁使其对未来的跨线程喂数也安全。
#[derive(Clone)]
pub struct Telemetry {
    inner: Arc<Mutex<VecDeque<FrameSample>>>,
}

impl Telemetry {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(VecDeque::with_capacity(CAP))) }
    }

    /// 推入一帧样本;超出容量则挤掉最旧的一帧。
    pub fn push_frame(&self, sample: FrameSample) {
        let mut q = self.lock();
        if q.len() >= CAP {
            q.pop_front();
        }
        q.push_back(sample);
    }

    /// 当前缓冲快照(时间升序拷贝;HUD 每帧取一次)。
    pub fn snapshot(&self) -> Vec<FrameSample> {
        self.lock().iter().copied().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<FrameSample>> {
        // 毒化即调试会话已坏:取回数据继续,不让调试层拖垮游戏进程。
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample(at: Instant, vm_ns: u64) -> FrameSample {
        FrameSample { at, vm_ns, render_ns: 0, total_ns: vm_ns, vm_state: VmRunState::Running, fps_target: 60 }
    }

    #[test]
    fn ring_keeps_recent_in_order() {
        let t = Telemetry::new();
        let t0 = Instant::now();
        for i in 0..(CAP + 50) {
            t.push_frame(sample(t0 + Duration::from_millis(i as u64), i as u64));
        }
        let snap = t.snapshot();
        assert_eq!(snap.len(), CAP);
        assert_eq!(snap.first().unwrap().vm_ns, 50);
        assert_eq!(snap.last().unwrap().vm_ns, (CAP + 49) as u64);
        // 升序
        assert!(snap.windows(2).all(|w| w[0].at <= w[1].at));
    }
}
