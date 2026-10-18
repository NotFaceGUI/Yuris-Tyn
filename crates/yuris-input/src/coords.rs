//! 窗口像素 ↔ 逻辑坐标反算。
//!
//! ## 为什么不能照抄引擎的整数缩放
//!
//! 引擎 `FUN_00403ee0` 把客户区像素映射成逻辑坐标时是**逐轴拉伸**：
//!
//! ```text
//! x_logical = x_client * 逻辑宽 / 客户区宽
//! y_logical = y_client * 逻辑高 / 客户区高
//! ```
//!
//! 因为原引擎用 `StretchDIBits` 把后缓冲拉伸铺满窗口（`FUN_004178c4`），
//! 输入与显示用的是同一个逐轴映射。
//!
//! **本内核的渲染路径是 letterbox**（等比缩放 + 居中黑边，
//! `yuris_render::letterbox_matrix`）。输入必须反算**我们自己**的映射，
//! 否则窗口比例与逻辑比例不一致时点击会漂移。故本模块实现的是
//! `letterbox_matrix` 的严格逆映射，而不是引擎的逐轴公式。
//! 两者在窗口比例 == 逻辑比例时完全一致（原版窗口默认就是这种情况）。
//!
//! 证据：`letterbox_matrix` 的 `scale = min(w/lw, h/lh)`、
//! `off = (w - lw*scale)/2`；本模块取其逆。

/// 逻辑视口：窗口尺寸（物理像素）+ 逻辑画布尺寸。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// 窗口客户区宽（物理像素）。
    pub window_w: u32,
    /// 窗口客户区高（物理像素）。
    pub window_h: u32,
    /// 逻辑画布宽（游戏 `yscfg.ybn` 的分辨率）。
    pub logical_w: f32,
    /// 逻辑画布高。
    pub logical_h: f32,
}

/// 反算出的逻辑坐标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalPoint {
    /// 逻辑 X（可为负/越界；引擎对越界坐标也有定义）。
    pub x: i64,
    /// 逻辑 Y。
    pub y: i64,
    /// 是否落在逻辑画布内。
    pub inside: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            window_w: 0,
            window_h: 0,
            logical_w: 1920.0,
            logical_h: 1080.0,
        }
    }
}

impl Viewport {
    /// 新建视口。
    pub fn new(window_w: u32, window_h: u32, logical_w: f32, logical_h: f32) -> Self {
        Self {
            window_w,
            window_h,
            logical_w: logical_w.max(1.0),
            logical_h: logical_h.max(1.0),
        }
    }

    /// 更新窗口尺寸（`Resized` / Android surface 变更）。
    pub fn resize(&mut self, window_w: u32, window_h: u32) {
        self.window_w = window_w;
        self.window_h = window_h;
    }

    /// 更新逻辑画布（启动链读到 `yscfg.ybn` 后）。
    pub fn set_logical(&mut self, logical_w: f32, logical_h: f32) {
        self.logical_w = logical_w.max(1.0);
        self.logical_h = logical_h.max(1.0);
    }

    /// 等比缩放因子（与 `letterbox_matrix` 的 `scale` 同式）。
    pub fn scale(&self) -> f32 {
        if self.window_w == 0 || self.window_h == 0 {
            return 0.0;
        }
        (self.window_w as f32 / self.logical_w).min(self.window_h as f32 / self.logical_h)
    }

    /// 画布在窗口内的左上角偏移（像素）。
    pub fn offset(&self) -> (f32, f32) {
        let s = self.scale();
        (
            (self.window_w as f32 - self.logical_w * s) * 0.5,
            (self.window_h as f32 - self.logical_h * s) * 0.5,
        )
    }

    /// 窗口像素 → 逻辑坐标（letterbox 逆映射）。
    ///
    /// 缩放比取 `scale = min(w/lw, h/lh)`；逆映射用**有理式**计算
    /// `logical = pixel * 逻辑边长 / 受限窗口边长`（而不是
    /// `(pixel - offset) / scale`）—— 后者在 1/3、2/3 这类除不尽的
    /// 比例上会把 540 算成 539.9999，截断后就差 1 像素。
    ///
    /// 取整采用**向零截断**（与引擎整数除法一致）；窗口未初始化时返回
    /// `(0,0,false)`。
    pub fn to_logical(&self, px: f64, py: f64) -> LogicalPoint {
        if self.window_w == 0 || self.window_h == 0 {
            return LogicalPoint {
                x: 0,
                y: 0,
                inside: false,
            };
        }
        let (ox, oy) = self.offset();
        // 受限轴：宽受限时 x 恰为引擎的 x*lw/w，y 也以同一分母换算。
        let width_limited =
            self.window_w as f64 * self.logical_h as f64 <= self.window_h as f64 * self.logical_w as f64;
        let (num, den) = if width_limited {
            (self.logical_w as f64, self.window_w as f64)
        } else {
            (self.logical_h as f64, self.window_h as f64)
        };
        let lx = (px - ox as f64) * num / den;
        let ly = (py - oy as f64) * num / den;
        let x = lx as i64;
        let y = ly as i64;
        LogicalPoint {
            x,
            y,
            inside: x >= 0
                && y >= 0
                && (x as f64) < self.logical_w as f64
                && (y as f64) < self.logical_h as f64,
        }
    }

    /// 逻辑坐标 → 窗口像素（正向映射；测试与光标 warp 用）。
    pub fn to_window(&self, lx: f64, ly: f64) -> (f64, f64) {
        let s = self.scale();
        let (ox, oy) = self.offset();
        (lx * s as f64 + ox as f64, ly * s as f64 + oy as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} != {b}");
    }

    #[test]
    fn exact_aspect_maps_one_to_one() {
        let vp = Viewport::new(1920, 1080, 1920.0, 1080.0);
        let p = vp.to_logical(0.0, 0.0);
        assert_eq!((p.x, p.y, p.inside), (0, 0, true));
        let p = vp.to_logical(1919.0, 1079.0);
        assert_eq!((p.x, p.y, p.inside), (1919, 1079, true));
    }

    #[test]
    fn four_by_three_letterboxes_left_right() {
        // 1280x960 显示 1920x1080：scale = min(1280/1920, 960/1080) = 2/3
        // 显示尺寸 1280x720，上下各 120 黑边。
        let vp = Viewport::new(1280, 960, 1920.0, 1080.0);
        let p = vp.to_logical(0.0, 120.0);
        assert_eq!((p.x, p.y, p.inside), (0, 0, true));
        let p = vp.to_logical(640.0, 480.0);
        assert_eq!((p.x, p.y, p.inside), (960, 540, true));
        // 黑边内的点：逻辑 Y 为负/越界，inside=false。
        let p = vp.to_logical(640.0, 0.0);
        assert!(!p.inside);
        assert!(p.y < 0);
        // 底边黑边。
        let p = vp.to_logical(640.0, 959.0);
        assert!(!p.inside);
        assert!(p.y >= 1080);
    }

    #[test]
    fn portrait_window_pillarboxes() {
        let vp = Viewport::new(600, 1080, 1920.0, 1080.0);
        // scale = min(600/1920, 1080/1080) = 0.3125；显示宽 600，高 337.5，
        // 上下各 371.25 黑边。
        let p = vp.to_logical(300.0, 540.0);
        assert_eq!((p.x, p.y, p.inside), (960, 540, true));
    }

    #[test]
    fn roundtrip() {
        let vp = Viewport::new(1600, 1000, 1280.0, 720.0);
        for (lx, ly) in [(0.0, 0.0), (640.0, 360.0), (1279.0, 719.0)] {
            let (wx, wy) = vp.to_window(lx, ly);
            let p = vp.to_logical(wx, wy);
            assert_eq!((p.x, p.y), (lx as i64, ly as i64));
        }
    }

    #[test]
    fn uninitialized_window_is_outside() {
        let vp = Viewport::default();
        let p = vp.to_logical(10.0, 10.0);
        assert_eq!((p.x, p.y, p.inside), (0, 0, false));
    }
}
