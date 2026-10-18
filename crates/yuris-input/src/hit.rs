//! 命中测试（按钮/图层）。
//!
//! ## 引擎语义（Confirmed：`FUN_0045aa44`）
//!
//! 引擎在 `WM_LBUTTONDOWN` 里不是只做矩形判定：它取窗口的**遮罩位图**
//! （`DAT_00801578` → 位图对象 `+0x90` 像素缓冲），按光标位置读一个 24 位
//! 像素值，**值 > 0 才算命中**。这就是 `CGINFO ONMOUSE/ONMOUSE2` 的底层
//! 语义：按钮只有非透明像素区域可点。
//!
//! 本模块提供：
//! - [`Rect`] 矩形判定（脚本侧按钮的粗命中；与引擎的包围盒一致）；
//! - [`AlphaMask`] 由调用方提供像素探针（Stage/渲染侧持有纹理时实现）；
//! - [`hit_topmost`] 按 z 从高到低找最上面的命中目标。
//!
//! 没有遮罩时只做矩形判定 —— 这是**显式降级**：调用方可用
//! [`HitTarget::mask`] 为 `None` 表示“该目标没有遮罩数据”，
//! 结果不应被当作引擎 ONMOUSE 的像素级等价。

use std::cmp::Ordering;

/// 逻辑坐标矩形（左上角 + 尺寸）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// 左上角 X。
    pub x: f32,
    /// 左上角 Y。
    pub y: f32,
    /// 宽。
    pub w: f32,
    /// 高。
    pub h: f32,
}

impl Rect {
    /// 构造。
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// 点是否在矩形内（左闭右开，与引擎 `x < sx && y < sy` 一致）。
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// 像素遮罩探针（引擎 `CGINFO ONMOUSE` 的底层面）。
pub trait AlphaMask {
    /// 逻辑坐标处的非透明强度；0 = 透明（不可点）。
    fn alpha_at(&self, x: i64, y: i64) -> u8;
}

/// 一个命中目标。
pub struct HitTarget<'a> {
    /// 包围盒（逻辑坐标）。
    pub rect: Rect,
    /// 层序（大者在上）。
    pub z: i64,
    /// 像素遮罩；`None` = 只有矩形（显式降级）。
    pub mask: Option<&'a dyn AlphaMask>,
}

impl<'a> HitTarget<'a> {
    /// 仅矩形的目标。
    pub const fn rect_only(rect: Rect, z: i64) -> Self {
        Self {
            rect,
            z,
            mask: None,
        }
    }

    /// 带遮罩的目标。
    pub const fn masked(rect: Rect, z: i64, mask: &'a dyn AlphaMask) -> Self {
        Self {
            rect,
            z,
            mask: Some(mask),
        }
    }

    fn hit(&self, x: i64, y: i64) -> bool {
        if !self.rect.contains(x as f32, y as f32) {
            return false;
        }
        match self.mask {
            Some(mask) => mask.alpha_at(x, y) > 0,
            None => true,
        }
    }
}

/// 按 z 从高到低返回最上面的命中目标下标。
pub fn hit_topmost(targets: &[HitTarget<'_>], x: i64, y: i64) -> Option<usize> {
    targets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.hit(x, y))
        .max_by(|(ia, a), (ib, b)| {
            a.z.cmp(&b.z)
                .then_with(|| ia.cmp(ib)) // 同 z：后加入者优先（上层重放）
                .then(Ordering::Equal)
        })
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mask;
    impl AlphaMask for Mask {
        fn alpha_at(&self, x: i64, _y: i64) -> u8 {
            if x < 10 {
                0
            } else {
                255
            }
        }
    }

    #[test]
    fn rect_half_open() {
        let r = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(39.9, 59.9));
        assert!(!r.contains(40.0, 20.0));
        assert!(!r.contains(10.0, 60.0));
    }

    #[test]
    fn topmost_wins() {
        let a = HitTarget::rect_only(Rect::new(0.0, 0.0, 100.0, 100.0), 10);
        let b = HitTarget::rect_only(Rect::new(0.0, 0.0, 100.0, 100.0), 20);
        let targets = [a, b];
        assert_eq!(hit_topmost(&targets, 50, 50), Some(1));
    }

    #[test]
    fn mask_blocks_transparent_pixels() {
        let mask = Mask;
        let a = HitTarget::masked(Rect::new(0.0, 0.0, 100.0, 100.0), 10, &mask);
        let targets = [a];
        assert_eq!(hit_topmost(&targets, 5, 5), None);
        assert_eq!(hit_topmost(&targets, 15, 5), Some(0));
    }
}
