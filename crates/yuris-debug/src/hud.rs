//! HUD 布局与绘制:FPS 徽章(左上)、状态/目标芯片(右上)、
//! 底部居中「VM 執行時序」波形(时钟 / VM tick / 渲染三条数字迹 +
//! 模拟耗时条 + 预算网格 + 扫描线 + 播放头)。
//!
//! 动画全部走 `ease` 曲线:整层缓入缓出显隐、新样本从右缘滑入并 ease-out
//! 长入、徽章数值用临界阻尼弹簧、状态灯呼吸、扫描线 ease-in-out 往复 ——
//! 目标是真实仪表的观感;展示的数字全部来自遥测实测,无装饰性假数据。

use crate::ease::{ease_in_out_sine, ease_out_cubic, Damped};
use crate::gfx::Vx;
use crate::telemetry::{FrameSample, VmRunState};
use crate::text::{TextCache, ATLAS_H, ATLAS_W};
use std::time::Instant;

// ---- 配色(sRGB 0..1;深色仪表主题)----
const PANEL: [f32; 4] = [0.066, 0.078, 0.098, 0.86];
const BORDER: [f32; 4] = [0.22, 0.28, 0.36, 0.85];
const TXT: [f32; 4] = [0.90, 0.94, 0.98, 1.00];
const TXT_DIM: [f32; 4] = [0.56, 0.63, 0.72, 1.00];
const TRACK: [f32; 4] = [0.30, 0.36, 0.44, 0.35];
const GRID: [f32; 4] = [0.45, 0.55, 0.68, 0.22];
const GRID_V: [f32; 4] = [0.45, 0.55, 0.68, 0.10];
const CLK: [f32; 4] = [0.58, 0.68, 0.82, 1.00];
const CYAN: [f32; 4] = [0.30, 0.84, 1.00, 1.00];
const AMBER: [f32; 4] = [1.00, 0.77, 0.42, 1.00];
const OK: [f32; 4] = [0.36, 0.87, 0.48, 1.00];
const WARN: [f32; 4] = [1.00, 0.72, 0.20, 1.00];
const BAD: [f32; 4] = [1.00, 0.36, 0.36, 1.00];
const IDLE_C: [f32; 4] = [0.55, 0.60, 0.68, 1.00];

/// 清屏色:全透明(覆盖层靠 DWM 与游戏画面合成)。
pub const CLEAR: [f64; 4] = [0.0, 0.0, 0.0, 0.0];

// ---- 基元绘制 ----

/// 累积顶点/索引的画笔;每帧重建,`Canvas::draw` 一次提交。
pub struct Painter {
    pub verts: Vec<Vx>,
    pub idx: Vec<u32>,
}

impl Painter {
    pub fn new() -> Self {
        Self { verts: Vec::with_capacity(4096), idx: Vec::with_capacity(6144) }
    }

    fn push_quad(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, u0: f32, v0: f32, u1: f32, v1: f32, col: [f32; 4], prm: [f32; 4]) {
        let base = self.verts.len() as u32;
        let cx = (x0 + x1) * 0.5;
        let cy = (y0 + y1) * 0.5;
        for (px, py, uu, vv) in [(x0, y0, u0, v0), (x1, y0, u1, v0), (x0, y1, u0, v1), (x1, y1, u1, v1)] {
            // 矩形基元(prm.x≥0.5)的 uv 携带「相对中心的局部像素坐标」(SDF 需要);
            // 字形基元(prm.x=0)的 uv 携带图集归一化坐标。
            let uv = if prm[0] >= 0.5 { [px - cx, py - cy] } else { [uu, vv] };
            self.verts.push(Vx { pos: [px, py], uv, col, prm });
        }
        self.idx.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 1, base + 3]);
    }

    /// 实心(圆角)矩形。
    pub fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, col: [f32; 4]) {
        let r = r.min(w * 0.5).min(h * 0.5);
        self.push_quad(x, y, x + w, y + h, 0.0, 0.0, 0.0, 0.0, col, [1.0, r, w * 0.5, h * 0.5]);
    }

    /// 圆点。
    pub fn dot(&mut self, cx: f32, cy: f32, r: f32, col: [f32; 4]) {
        self.rrect(cx - r, cy - r, r * 2.0, r * 2.0, r, col);
    }

    /// 字形(图集采样)。
    fn glyph(&mut self, gx: f32, gy: f32, w: f32, h: f32, u0: f32, v0: f32, u1: f32, v1: f32, col: [f32; 4]) {
        self.push_quad(gx, gy, gx + w, gy + h, u0, v0, u1, v1, col, [0.0, 0.0, 0.0, 0.0]);
    }

    /// 一行文本(左对齐,`baseline` 为基线 y);返回结束笔位。
    pub fn text(&mut self, tc: &mut TextCache, x: f32, baseline: f32, px: f32, col: [f32; 4], s: &str) -> f32 {
        let mut pen = x;
        let size = px.round().max(6.0) as u16;
        for ch in s.chars() {
            match tc.slot(ch, size) {
                Some(sl) => {
                    if sl.w > 0 && sl.h > 0 {
                        let gx = pen + sl.xmin as f32;
                        let gy = baseline - sl.ymin as f32 - sl.h as f32;
                        self.glyph(
                            gx,
                            gy,
                            sl.w as f32,
                            sl.h as f32,
                            sl.x as f32 / ATLAS_W as f32,
                            sl.y as f32 / ATLAS_H as f32,
                            (sl.x + sl.w) as f32 / ATLAS_W as f32,
                            (sl.y + sl.h) as f32 / ATLAS_H as f32,
                            col,
                        );
                    }
                    pen += sl.advance;
                }
                None => pen += px * 0.55,
            }
        }
        pen
    }

    /// 右对齐文本。
    pub fn text_r(&mut self, tc: &mut TextCache, x_right: f32, baseline: f32, px: f32, col: [f32; 4], s: &str) -> f32 {
        let w = tc.measure(s, px);
        self.text(tc, x_right - w, baseline, px, col, s)
    }
}

impl Default for Painter {
    fn default() -> Self {
        Self::new()
    }
}

// ---- 颜色工具 ----

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// 负载色:rel = 耗时 / 帧预算;≤0.85 绿,过渡琥珀,>1.4 红。
fn load_color(rel: f32) -> [f32; 4] {
    let t1 = ease_out_cubic((rel - 0.85) / 0.30);
    let t2 = ease_out_cubic((rel - 1.40) / 0.30);
    lerp4(lerp4(OK, WARN, t1), BAD, t2)
}

/// 脉宽比:该段耗时 / 帧间隔,钳到 [0.04, 1](0 宽会闪,留最小可见)。
pub(crate) fn pulse_ratio(ns: u64, gap_ns: f64) -> f32 {
    if gap_ns <= 0.0 {
        0.0
    } else {
        (ns as f64 / gap_ns).clamp(0.04, 1.0) as f32
    }
}

// ---- 布局与逐帧构建 ----

/// 跨帧状态(数值弹簧 / 时钟)。
pub struct HudState {
    pub fps: Damped,
    pub ft_ms: Damped,
    pub t: f32,
}

impl Default for HudState {
    fn default() -> Self {
        Self { fps: Damped::new(0.0), ft_ms: Damped::new(0.0), t: 0.0 }
    }
}

pub struct FrameCtx<'a> {
    pub samples: &'a [FrameSample],
    pub now: Instant,
    /// 整层显隐补间值 0..1(缓入缓出已作用)。
    pub anim: f32,
    /// 窗口 DPI 缩放。
    pub scale: f32,
    /// 客户区物理像素。
    pub w: f32,
    pub h: f32,
    /// 本帧步长(已钳制)。
    pub dt: f32,
}

pub fn build(p: &mut Painter, tc: &mut TextCache, st: &mut HudState, f: FrameCtx) {
    let ga = f.anim.clamp(0.0, 1.0);
    if ga <= 0.004 {
        return;
    }
    st.t += f.dt;
    let s = f.scale;
    // 整层缓入:alpha 随补间,位置自下方 18px 缓入(缓出曲线收尾)。
    let oy = (1.0 - ease_out_cubic(ga)) * 18.0 * s;
    let a = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * ga];

    // ---- 实测数据(全部来自遥测)----
    let mut fps_now = 0.0f32;
    let mut ft_now = 0.0f32;
    if f.samples.len() >= 2 {
        let tail = &f.samples[f.samples.len().saturating_sub(46)..];
        let span = (tail.last().unwrap().at - tail.first().unwrap().at).as_secs_f32();
        if span > 1e-4 {
            fps_now = (tail.len() - 1) as f32 / span;
        }
        ft_now = (f.samples.last().unwrap().at - f.samples[f.samples.len() - 2].at)
            .as_secs_f32()
            * 1000.0;
    }
    st.fps.set_target(fps_now);
    st.fps.step(f.dt);
    st.ft_ms.set_target(ft_now);
    st.ft_ms.step(f.dt);
    let latest = f.samples.last().copied();
    let target = latest.map(|x| x.fps_target).unwrap_or(0);
    let budget_ms = if target > 0 { 1000.0 / target as f32 } else { 1000.0 / 60.0 };

    // ---- 呼吸相位(所有活性光效共用)----
    let breath = 0.5 + 0.5 * (st.t * 2.4).sin();

    // ================= 左上:FPS 徽章 =================
    let bx = 12.0 * s;
    let by = 12.0 * s + oy;
    let bw = 184.0 * s;
    let bh = 86.0 * s;
    p.rrect(bx, by, bw, bh, 12.0 * s, a(BORDER));
    p.rrect(bx + 1.0 * s, by + 1.0 * s, bw - 2.0 * s, bh - 2.0 * s, 11.0 * s, a(PANEL));
    // 呼吸状态点:帧耗在预算内绿、超预算琥珀/红。
    let rel = st.ft_ms.value() / budget_ms;
    let dot_col = if fps_now > 0.0 { load_color(rel) } else { IDLE_C };
    p.dot(bx + 24.0 * s, by + 22.0 * s, (4.2 + 0.9 * breath) * s, a(dot_col));
    p.text(tc, bx + 38.0 * s, by + 26.0 * s, 12.0 * s, a(TXT_DIM), "FPS");
    let fps_txt = if fps_now > 0.0 { format!("{:.1}", st.fps.value()) } else { "--".to_string() };
    p.text_r(tc, bx + bw - 12.0 * s, by + 34.0 * s, 26.0 * s, a(TXT), &fps_txt);
    p.text(
        tc,
        bx + 14.0 * s,
        by + 56.0 * s,
        11.0 * s,
        a(TXT_DIM),
        &format!("幀 {:.1} ms / 預算 {:.1}", st.ft_ms.value(), budget_ms),
    );
    // 帧耗预算条(满刻度 = 1.5×预算)。
    let bar_y = by + bh - 16.0 * s;
    let bar_w = bw - 28.0 * s;
    p.rrect(bx + 14.0 * s, bar_y, bar_w, 5.0 * s, 2.5 * s, a(TRACK));
    let fill = (rel / 1.5).clamp(0.0, 1.0);
    if fill > 0.004 {
        p.rrect(bx + 14.0 * s, bar_y, bar_w * fill, 5.0 * s, 2.5 * s, a(load_color(rel)));
    }

    // ================= 右上:状态 / 目标芯片 =================
    let (st_txt, st_col) = match latest.map(|x| x.vm_state).unwrap_or(VmRunState::Idle) {
        VmRunState::Running => ("執行中", OK),
        VmRunState::Idle => ("閒置", IDLE_C),
        VmRunState::Finished => ("完成", CYAN),
        VmRunState::Error => ("掛起", BAD),
    };
    let tg_txt = if target > 0 { format!("目標 {} FPS", target) } else { "目標 --".to_string() };
    let chips: [(&str, [f32; 4], bool); 2] = [
        (&tg_txt, CYAN, false),
        (st_txt, st_col, latest.map(|x| x.vm_state) == Some(VmRunState::Running)),
    ];
    let mut rx = f.w - 12.0 * s;
    let chip_h = 26.0 * s;
    let chip_bl = 12.0 * s + oy + chip_h * 0.5 + tc.h_metrics(12.0 * s).0 * 0.5;
    for (txt, col, glow) in chips {
        let tw = tc.measure(txt, 12.0 * s);
        let cw = tw + 30.0 * s;
        let cy0 = 12.0 * s + oy;
        if glow {
            // 執行中:呼吸辉光(两层近似 bloom,缓入缓出包络)。
            let g = 0.10 + 0.14 * breath;
            p.rrect(rx - cw - 2.0 * s, cy0 - 2.0 * s, cw + 4.0 * s, chip_h + 4.0 * s, 15.0 * s, a([col[0], col[1], col[2], g]));
            p.rrect(rx - cw - 5.0 * s, cy0 - 5.0 * s, cw + 10.0 * s, chip_h + 10.0 * s, 18.0 * s, a([col[0], col[1], col[2], g * 0.4]));
        }
        p.rrect(rx - cw, cy0, cw, chip_h, 13.0 * s, a(BORDER));
        p.rrect(rx - cw + 1.0 * s, cy0 + 1.0 * s, cw - 2.0 * s, chip_h - 2.0 * s, 12.0 * s, a(PANEL));
        p.dot(rx - cw + 15.0 * s, cy0 + chip_h * 0.5, 3.2 * s, a(col));
        p.text(tc, rx - cw + 24.0 * s, chip_bl, 12.0 * s, a(TXT), txt);
        rx -= cw + 8.0 * s;
    }

    // ================= 底部居中:VM 執行時序 =================
    let pw = (f.w - 24.0 * s).min(700.0 * s);
    let ph = 150.0 * s;
    let px = (f.w - pw) * 0.5;
    let py = f.h - ph - 12.0 * s + oy;
    p.rrect(px, py, pw, ph, 12.0 * s, a(BORDER));
    p.rrect(px + 1.0 * s, py + 1.0 * s, pw - 2.0 * s, ph - 2.0 * s, 11.0 * s, a(PANEL));

    // 头行:标题 + 图例。
    let head_bl = py + 10.0 * s + tc.h_metrics(13.0 * s).0;
    p.text(tc, px + 14.0 * s, head_bl, 13.0 * s, a(TXT), "VM 執行時序");
    let legend = [(CLK, "時鐘"), (CYAN, "VM"), (AMBER, "繪製")];
    let mut lx = px + pw - 14.0 * s;
    for (col, label) in legend.iter().rev() {
        let tw = tc.measure(label, 11.0 * s);
        lx -= tw;
        p.text(tc, lx, head_bl, 11.0 * s, a(TXT_DIM), label);
        lx -= 15.0 * s;
        p.rrect(lx, head_bl - 8.0 * s, 8.0 * s, 8.0 * s, 2.0 * s, a(*col));
        lx -= 16.0 * s;
    }

    let lane_x0 = px + 14.0 * s;
    let lane_w = pw - 28.0 * s;
    let lanes_y = py + 32.0 * s;
    let lane_h = 11.0 * s;
    let lane_gap = 6.0 * s;
    let slot = 6.0 * s;
    let ana_y = lanes_y + 3.0 * (lane_h + lane_gap) + 8.0 * s;
    let ana_h = 38.0 * s;

    // 竖向刻度网格(固定等距,示波器栅格观感)。
    let n_slots = (lane_w / slot).floor().max(1.0) as usize;
    let mut k = 0u32;
    while (k as f32) * slot < lane_w {
        let gx = lane_x0 + lane_w - k as f32 * slot;
        p.rrect(gx, lanes_y, 1.0 * s, ana_y + ana_h - lanes_y, 0.0, a(GRID_V));
        k += 12;
    }

    let Some(hist_all) = (!f.samples.is_empty()).then_some(f.samples) else {
        let bl = py + ph * 0.5 + tc.h_metrics(12.0 * s).0 * 0.5;
        p.text(tc, px + pw * 0.5 - tc.measure("等待幀資料…", 12.0 * s) * 0.5, bl, 12.0 * s, a(TXT_DIM), "等待幀資料…");
        return;
    };
    let take = hist_all.len().min(n_slots);
    let hist = &hist_all[hist_all.len() - take..];
    let last = hist[hist.len() - 1];
    // 滚动相位:最新样本随其到达后的时间从右缘连续左移(帧间隔归一)。
    let gap_last = if take >= 2 {
        (hist[take - 1].at - hist[take - 2].at).as_secs_f64().max(1e-4)
    } else {
        1.0 / 60.0
    };
    let frac = ((f.now - last.at).as_secs_f64() / gap_last).clamp(0.0, 1.0) as f32;
    // 新样本 ease-out 长入。
    let grow = 0.35 + 0.65 * ease_out_cubic(frac);
    let xr = lane_x0 + lane_w;
    let x_of = |i: usize| xr - ((take - 1 - i) as f32 + frac) * slot - slot * 0.5;
    let gap_ns_of = |i: usize| -> f64 {
        if i > 0 {
            (hist[i].at - hist[i - 1].at).as_secs_f64().max(1e-4) * 1e9
        } else {
            1.0e9 / 60.0
        }
    };

    // 三条数字迹:时钟(整帧)/ VM tick / 渲染。高电平宽度 ∝ 耗时/帧间隔。
    let lanes: [([f32; 4], fn(&FrameSample) -> u64); 3] =
        [(CLK, |x| x.total_ns), (CYAN, |x| x.vm_ns), (AMBER, |x| x.render_ns)];
    for (li, (col, get)) in lanes.iter().enumerate() {
        let y0 = lanes_y + li as f32 * (lane_h + lane_gap);
        p.rrect(lane_x0, y0 + lane_h - 1.0 * s, lane_w, 1.0 * s, 0.0, a(TRACK));
        for (i, sm) in hist.iter().enumerate() {
            let ns = get(sm);
            if ns <= 50_000 {
                continue;
            }
            let mut wpx = pulse_ratio(ns, gap_ns_of(i)) * slot;
            if i == take - 1 {
                wpx *= grow;
            }
            let x = x_of(i);
            let x_left = x - slot * 0.5;
            let cap = [col[0], col[1], col[2], col[3] * 0.85];
            p.rrect(x_left, y0, wpx.max(1.2 * s), lane_h - 1.0 * s, 0.0, a(*col));
            p.rrect(x_left, y0, wpx.max(1.2 * s), 1.6 * s, 0.0, a(cap));
        }
    }

    // 模拟耗时条 + 预算网格(1 帧 / 2 帧;满高 = 2×帧间隔)。
    let denom = (2.0 * gap_last as f32).max(1.0);
    for mult in [1.0f32, 2.0] {
        let gy = ana_y + ana_h * (1.0 - mult / 2.0);
        p.rrect(lane_x0, gy, lane_w, 1.0 * s, 0.0, a(GRID));
        let ms = mult * gap_last as f32 * 1000.0;
        p.text_r(tc, lane_x0 + lane_w, gy - 3.0 * s, 9.0 * s, a(TXT_DIM), &format!("{:.0}ms", ms));
    }
    for (i, sm) in hist.iter().enumerate() {
        let mut r = (sm.total_ns as f32 / denom).clamp(0.0, 1.0);
        if i == take - 1 {
            r *= grow;
        }
        if r < 0.004 {
            continue;
        }
        let rel = sm.total_ns as f32 / (gap_ns_of(i) as f32);
        let x_left = x_of(i) - slot * 0.5 + 1.0 * s;
        let bw_slot = slot - 2.0 * s;
        let bh = r * ana_h;
        p.rrect(x_left, ana_y + ana_h - bh, bw_slot, bh, 0.0, a(load_color(rel)));
    }
    // 底轨基线。
    p.rrect(lane_x0, ana_y + ana_h, lane_w, 1.0 * s, 0.0, a(TRACK));

    // 扫描线:ease-in-out 往复(7s 周期),低亮度,示波器质感。
    let phase = (st.t / 7.0) % 1.0;
    let tri = if phase < 0.5 {
        ease_in_out_sine(phase * 2.0)
    } else {
        1.0 - ease_in_out_sine((phase - 0.5) * 2.0)
    };
    let sx = lane_x0 + tri * lane_w;
    p.rrect(sx, lanes_y, 1.2 * s, ana_y + ana_h - lanes_y, 0.0, a([0.65, 0.82, 1.0, 0.12]));
    p.rrect(sx - 2.0 * s, lanes_y, 4.0 * s, ana_y + ana_h - lanes_y, 0.0, a([0.65, 0.82, 1.0, 0.05]));

    // 播放头(右缘)+ 余辉。
    p.rrect(lane_x0 + lane_w - 1.5 * s, lanes_y, 1.5 * s, ana_y + ana_h - lanes_y, 0.0, a([0.88, 0.96, 1.0, 0.55]));
    p.rrect(lane_x0 + lane_w - 9.0 * s, lanes_y, 7.5 * s, ana_y + ana_h - lanes_y, 0.0, a([0.88, 0.96, 1.0, 0.06]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_ratio_clamped() {
        assert!((pulse_ratio(0, 16.6e6) - 0.04).abs() < 1e-6, "零宽钳到最小可见");
        assert!((pulse_ratio(u64::MAX, 1.0) - 1.0).abs() < 1e-6);
        assert_eq!(pulse_ratio(8_300_000, 16.6e6), 0.5);
        assert_eq!(pulse_ratio(1, 0.0), 0.0);
    }

    #[test]
    fn load_color_within_budget_is_greenish() {
        let c = load_color(0.5);
        assert!(c[1] > c[0], "预算内应偏绿:{c:?}");
        let over = load_color(2.0);
        assert!(over[0] > over[1], "两倍预算应偏红:{over:?}");
    }
}
