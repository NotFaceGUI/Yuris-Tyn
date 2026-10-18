//! 缓动曲线(缓入缓出)与数值平滑。
//!
//! HUD 的全部动画都走这里:整层显隐、波形新样本进场、徽章数值(临界阻尼
//! 弹簧)、状态灯呼吸、扫描线往复。曲线是纯函数,单测锁定端点与单调性。

use std::time::{Duration, Instant};

/// 三次缓入缓出:`t∈[0,1] → [0,1]`,端点锁定,两端慢中间快。
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// 三次缓出:快起缓收(用于新样本进场)。
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 正弦缓入缓出(用于扫描线的往复运动)。
pub fn ease_in_out_sine(t: f32) -> f32 {
    -(std::f32::consts::PI * t.clamp(0.0, 1.0)).cos() / 2.0 + 0.5
}

/// 标量补间:`from → to`,`dur` 时长,缓入缓出。
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
}

impl Tween {
    pub fn new(from: f32, to: f32, dur: Duration, now: Instant) -> Self {
        Self { from, to, start: now, dur }
    }

    pub fn value(&self, now: Instant) -> f32 {
        let t = now.duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32();
        self.from + (self.to - self.from) * ease_in_out_cubic(t)
    }

    pub fn done(&self, now: Instant) -> bool {
        now >= self.start + self.dur
    }
}

/// 临界阻尼弹簧:数值徽章(FPS / 帧耗时)的平滑趋近,无过冲不抖动。
pub struct Damped {
    value: f32,
    target: f32,
    vel: f32,
}

impl Damped {
    pub fn new(initial: f32) -> Self {
        Self { value: initial, target: initial, vel: 0.0 }
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// 半隐式欧拉积分;`dt` 由调用方钳制(≤50ms)防止停帧后爆冲。
    pub fn step(&mut self, dt: f32) {
        // ω=10:约 0.3s 收敛到目标,临界阻尼(ζ=1)无过冲。
        const OMEGA: f32 = 10.0;
        let acc = OMEGA * OMEGA * (self.target - self.value) - 2.0 * OMEGA * self.vel;
        self.vel += acc * dt;
        self.value += self.vel * dt;
    }

    pub fn value(&self) -> f32 {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_endpoints_locked() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
    }

    #[test]
    fn cubic_monotonic() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = ease_in_out_cubic(i as f32 / 100.0);
            assert!(v >= prev, "曲线必须单调不减 @ {i}");
            prev = v;
        }
    }

    #[test]
    fn tween_reaches_target() {
        let t0 = Instant::now();
        let tw = Tween::new(0.0, 1.0, Duration::from_millis(100), t0);
        assert!((tw.value(t0) - 0.0).abs() < 1e-6);
        let later = t0 + Duration::from_millis(150);
        assert!((tw.value(later) - 1.0).abs() < 1e-6);
        assert!(tw.done(later));
    }

    #[test]
    fn damped_converges() {
        let mut d = Damped::new(0.0);
        d.set_target(60.0);
        let dt = 1.0 / 60.0;
        for _ in 0..60 {
            d.step(dt);
        }
        assert!((d.value() - 60.0).abs() < 1.0, "1s 后应收敛到 ±1:{}", d.value());
    }
}
