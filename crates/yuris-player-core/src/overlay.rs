//! 崩坏级错误的画布中央错误窗（数据源：[`yuris_log::FatalReport`]）。
//!
//! 打开方式：`Player::tick` 检测到 [`yuris_log::has_fatal`] 后调用
//! [`PlayerCore::update_fatal_overlay`](crate::PlayerCore::update_fatal_overlay)，
//! 本模块把报告排版成一张**输出分辨率**的 RGBA 位图（含全屏压暗 + 居中面板），
//! 由播放器当作最高 z 图层提交。这样错误窗与游戏画面走**同一条渲染路径**，
//! 不需要在 wgpu 后端里再开一条 UI 通道，也不会随分辨率/letterbox 漂移。
//!
//! ## 为什么不按逻辑画布分辨率排版
//!
//! 早期实现按逻辑画布（如 1100×825）排版，再由 letterbox 放大到窗口
//! （如 2560×1440）⇒ 文字被**双线性放大 2.3 倍**，实机观察即「模糊」。
//! 现在由调用方传入 `scale = 窗口像素 / 逻辑像素`：位图按物理像素排版、
//! 字号同比放大，图层 `scale = 1/scale` 把纹理映射回逻辑坐标 —— 屏幕上
//! 每个纹理像素恰好对应一个物理像素。
//!
//! ## 滚动
//!
//! 回溯日志可能远高于一屏。面板不再「截断 + 省略提示」，而是保留全部行，
//! 由 [`FatalPanel::max_scroll`] 给出可滚动范围；调用方在滚轮/触摸拖动时
//! 重新调用本函数（`scroll` 变化才重排），滚动条指示当前位置。
//!
//! 排版刻意只依赖 `fontdue`：错误窗口必须在图形后端刚初始化完、字体链可能
//! 尚未就绪的情况下也能画——字体由调用方保证（启动期即加载）。

use fontdue::Font;
use yuris_log::{FatalReport, Level};

/// 错误窗配色（RGBA）。
const DIM: [u8; 4] = [0, 0, 0, 170];
const PANEL: [u8; 4] = [30, 32, 38, 255];
const BORDER: [u8; 4] = [190, 54, 54, 255];
const TITLE_BG: [u8; 4] = [156, 38, 38, 255];
const TITLE_FG: [u8; 4] = [255, 245, 245, 255];
const BODY_FG: [u8; 4] = [228, 230, 234, 255];
const TAIL_FG: [u8; 4] = [166, 170, 178, 255];
const HINT_FG: [u8; 4] = [140, 144, 152, 255];
const SCROLL_TRACK: [u8; 4] = [58, 61, 70, 255];
const SCROLL_THUMB: [u8; 4] = [150, 156, 168, 255];

/// 标题字号（逻辑像素，按 `scale` 放大）。
pub const TITLE_PX: f32 = 26.0;
/// 正文字号（逻辑像素）。
pub const BODY_PX: f32 = 18.0;
/// 回溯日志字号（逻辑像素；略小于正文）。
pub const TAIL_PX: f32 = 16.5;
/// 提示字号（逻辑像素）。
pub const HINT_PX: f32 = 16.0;

/// 一次排版的结果。
///
/// `width/height` 是**物理像素**（= 逻辑画布 × `scale`）；`max_scroll` 是
/// 内容可滚动上限（物理像素），`scroll` 是夹紧后的实际滚动量。
#[derive(Debug, Clone)]
pub struct FatalPanel {
    /// RGBA 位图（`width * height * 4`）。
    pub rgba: Vec<u8>,
    /// 位图宽（物理像素）。
    pub width: u32,
    /// 位图高（物理像素）。
    pub height: u32,
    /// 最大滚动量（物理像素；0 = 一屏放得下）。
    pub max_scroll: i32,
    /// 已应用的滚动量（夹紧到 `0..=max_scroll`）。
    pub scroll: i32,
}

/// 已排版的一行（文本 + 颜色 + 字号）。
struct Line {
    text: String,
    color: [u8; 4],
    px: f32,
}

/// 把致命报告排成画布尺寸的 RGBA 位图。
///
/// - `width/height`：输出位图尺寸（**物理像素**；调用方传逻辑尺寸 × `scale`）；
/// - `scale`：窗口物理像素 / 逻辑画布像素（1.0 = 不放大）；
/// - `scroll`：内容滚动量（物理像素；会被夹紧到合法范围）。
///
/// 公开是为了让平台壳 / 取证工具能复用同一套排版（`yuris-cli` 走
/// `PlayerCore::update_fatal_overlay`，示例见 `examples/fatal_panel.rs`）。
pub fn build_fatal_panel(
    font: &Font,
    report: &FatalReport,
    width: u32,
    height: u32,
    scale: f32,
    scroll: i32,
) -> FatalPanel {
    let s = scale.clamp(1.0, 4.0);
    let w = width.max(64);
    let h = height.max(64);
    let mut canvas = vec![0u8; (w as usize) * (h as usize) * 4];
    fill_rect(&mut canvas, w, 0, 0, w as i32, h as i32, DIM);

    // ---- 几何（全部按 scale 放大，保证物理像素下字号不变形）----
    let title_px = (TITLE_PX * s).min(64.0);
    let body_px = (BODY_PX * s).min(44.0);
    let tail_px = (TAIL_PX * s).min(40.0);
    let hint_px = (HINT_PX * s).min(38.0);
    let pad = ((22.0 * s) as i32).max(10);
    let margin = ((20.0 * s) as i32).max(6);
    let panel_w = ((w as f32 * 0.90) as i32)
        .min((1000.0 * s) as i32)
        .max((420.0 * s) as i32)
        .min((w as i32 - 2 * margin).max(120));
    let title_line_h = line_height(font, title_px);
    let hint_line_h = line_height(font, hint_px);
    let body_line_h = line_height(font, body_px);
    let inner_w = (panel_w - pad * 3).max(body_px as i32);

    // ---- 全部内容行（不截断；超出由滚动解决）----
    let mut lines: Vec<Line> = Vec::new();
    push_wrapped(font, &mut lines, &report.message, body_px, BODY_FG, inner_w);
    if !report.tail.is_empty() {
        lines.push(Line {
            text: String::new(),
            color: BODY_FG,
            px: body_px,
        });
        lines.push(Line {
            text: format!("—— 最近日志（{} 条，最新在下）——", report.tail.len()),
            color: TAIL_FG,
            px: body_px,
        });
        for r in &report.tail {
            push_wrapped(
                font,
                &mut lines,
                &r.format_panel(),
                tail_px,
                level_color(r.level),
                inner_w,
            );
        }
    }
    let content_h: i32 = lines.iter().map(|l| line_height(font, l.px)).sum();

    // ---- 面板尺寸与视口 ----
    // `gap` = 正文视口与底部提示之间的留白：滚动时最后一行总会被裁在
    // 视口边缘，没有留白时那半截字形会贴着提示行，看起来像叠字。
    let gap = (pad / 3).max(2);
    let chrome = title_line_h + pad * 2 + gap + hint_line_h;
    let max_panel_h = (h as i32 - margin * 2).max(60);
    let max_view = (max_panel_h - chrome).max(body_line_h);
    let view_h = content_h.min(max_view).max(body_line_h);
    let panel_h = (chrome + view_h).min(max_panel_h).max(1);
    let max_scroll = (content_h - view_h).max(0);
    let scroll = scroll.clamp(0, max_scroll);
    let px0 = (w as i32 - panel_w) / 2;
    let py0 = (h as i32 - panel_h) / 2;
    let body_top = py0 + title_line_h + pad;
    let hint_y = py0 + panel_h - pad - hint_line_h;

    // ---- 面板 + 标题栏 ----
    fill_rect(
        &mut canvas,
        w,
        px0 - 2,
        py0 - 2,
        panel_w + 4,
        panel_h + 4,
        BORDER,
    );
    fill_rect(&mut canvas, w, px0, py0, panel_w, panel_h, PANEL);
    fill_rect(&mut canvas, w, px0, py0, panel_w, title_line_h, TITLE_BG);

    // ---- 标题（单行，超宽截断）----
    draw_text(
        &mut canvas,
        w,
        h,
        font,
        &truncate(font, &report.title, title_px, panel_w - pad * 2),
        px0 + pad,
        py0 + (title_line_h - line_height(font, title_px)) / 2 + 2,
        title_px,
        TITLE_FG,
        None,
    );

    // ---- 正文（裁剪在视口内，按 scroll 偏移）----
    let scroll_w = if max_scroll > 0 {
        ((6.0 * s) as i32).max(3)
    } else {
        0
    };
    let clip = (
        px0 + pad,
        body_top,
        px0 + panel_w - pad - scroll_w,
        body_top + view_h,
    );
    let mut y = body_top - scroll;
    for line in &lines {
        let lh = line_height(font, line.px);
        if y + lh <= body_top {
            y += lh;
            continue;
        }
        if y >= body_top + view_h {
            break;
        }
        draw_text(
            &mut canvas,
            w,
            h,
            font,
            &line.text,
            px0 + pad,
            y,
            line.px,
            line.color,
            Some(clip),
        );
        y += lh;
    }

    // ---- 滚动条（有溢出才画）----
    if max_scroll > 0 {
        let bar_w = scroll_w;
        let track_x = px0 + panel_w - pad / 2 - bar_w;
        let track_y = body_top + 2;
        let track_h = (view_h - 4).max(8);
        fill_rect(&mut canvas, w, track_x, track_y, bar_w, track_h, SCROLL_TRACK);
        let denom = content_h.max(1);
        let thumb_h = ((track_h as f32 * view_h as f32 / denom as f32) as i32)
            .clamp(12, track_h);
        let travel = (track_h - thumb_h).max(0);
        let thumb_y = track_y
            + ((scroll as f32 / max_scroll as f32) * travel as f32).round() as i32;
        fill_rect(&mut canvas, w, track_x, thumb_y, bar_w, thumb_h, SCROLL_THUMB);
    }

    // ---- 底部提示 ----
    //
    // **不写滚动提示**(实机要求):用户需要知道的是「游戏已经停了、
    // 现在只能关窗口」,而不是滚动操作说明 —— 滚动照旧可用(滚轮/触摸拖动),
    // 但不占据唯一的提示位。也不再写「点按关闭」:错误窗**不提供**在
    // 游戏内恢复的路径,点了只会退出。
    let hint = format!(
        "游戏已停止推进 · 此面板无法关闭 · 只能退出游戏 · 报告 #{} · 回溯 {} 条",
        report.seq,
        report.tail.len()
    );
    draw_text(
        &mut canvas,
        w,
        h,
        font,
        &truncate(font, &hint, hint_px, panel_w - pad * 2),
        px0 + pad,
        hint_y,
        hint_px,
        HINT_FG,
        None,
    );

    FatalPanel {
        rgba: canvas,
        width: w,
        height: h,
        max_scroll,
        scroll,
    }
}

/// 日志等级 → 错误窗内回溯行的颜色（与终端配色同一语义，但不依赖 ANSI）。
fn level_color(level: Level) -> [u8; 4] {
    match level {
        Level::Trace => [128, 132, 140, 255],
        Level::Debug => [120, 180, 196, 255],
        Level::Info => [186, 198, 208, 255],
        Level::Warn => [235, 190, 90, 255],
        Level::Error => [240, 120, 110, 255],
        Level::Fatal => [255, 140, 140, 255],
        Level::Off => TAIL_FG,
    }
}

/// 按宽度换行（CJK 逐字符；保留显式换行）。
fn push_wrapped(font: &Font, out: &mut Vec<Line>, text: &str, px: f32, color: [u8; 4], max_w: i32) {
    let max_w = max_w.max(px as i32);
    for raw in text.split('\n') {
        if raw.is_empty() {
            out.push(Line {
                text: String::new(),
                color,
                px,
            });
            continue;
        }
        let mut line = String::new();
        let mut width = 0i32;
        for ch in raw.chars() {
            let adv = advance(font, ch, px);
            if width + adv > max_w && !line.is_empty() {
                out.push(Line {
                    text: std::mem::take(&mut line),
                    color,
                    px,
                });
                width = 0;
            }
            line.push(ch);
            width += adv;
        }
        if !line.is_empty() {
            out.push(Line {
                text: line,
                color,
                px,
            });
        }
    }
}

/// 字形前进宽度（**换行与绘制必须共用**，否则会算窄/算宽导致越界或大空隙）。
fn advance(font: &Font, ch: char, px: f32) -> i32 {
    font.metrics(ch, px).advance_width.max(0.0).round() as i32
}

/// 超宽则截断并追加 `…`。
fn truncate(font: &Font, text: &str, px: f32, max_w: i32) -> String {
    let mut out = String::new();
    let mut width = 0i32;
    for ch in text.chars() {
        let adv = advance(font, ch, px);
        if width + adv > max_w {
            out.push('…');
            break;
        }
        out.push(ch);
        width += adv;
    }
    out
}

fn line_height(font: &Font, px: f32) -> i32 {
    match font.horizontal_line_metrics(px) {
        Some(hm) => ((hm.ascent - hm.descent).max(px * 0.8)).ceil() as i32 + 2,
        None => (px * 1.3) as i32,
    }
}

/// 画一行文本（字体位图按 alpha 混合到画布）。
///
/// `y` = 行顶。字形按**基线**定位：`fontdue::Metrics::ymin` 是基线到字形
/// 底部的偏移（descender 为负），故位图顶 = 基线 − (`ymin` + `height`)。
/// 逐字形贴行顶会让拉丁小写/标点各自上下跳动（实测「Yuris-q;i」式错位）。
///
/// `clip` = 可选的裁剪矩形 `(x0, y0, x1, y1)`（半开区间，物理像素）。
#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut [u8],
    w: u32,
    h: u32,
    font: &Font,
    text: &str,
    x: i32,
    y: i32,
    px: f32,
    color: [u8; 4],
    clip: Option<(i32, i32, i32, i32)>,
) {
    let baseline = y + font
        .horizontal_line_metrics(px)
        .map(|m| m.ascent as i32)
        .unwrap_or((px * 0.8) as i32);
    let mut pen_x = x;
    for ch in text.chars() {
        let adv = advance(font, ch, px);
        if ch == ' ' {
            pen_x += adv.max((px * 0.3) as i32);
            continue;
        }
        let (m, bitmap) = font.rasterize(ch, px);
        let glyph_x = pen_x + m.xmin;
        let glyph_y = baseline - (m.ymin + m.height as i32);
        let gw = m.width.max(1);
        for i in 0..bitmap.len() {
            let a = bitmap[i];
            if a < 8 {
                continue;
            }
            let gx = glyph_x + (i % gw) as i32;
            let gy = glyph_y + (i / gw) as i32;
            blend_px(canvas, w, h, gx, gy, color, a, clip);
        }
        pen_x += adv.max(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn blend_px(
    canvas: &mut [u8],
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    color: [u8; 4],
    a: u8,
    clip: Option<(i32, i32, i32, i32)>,
) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    if let Some((x0, y0, x1, y1)) = clip {
        if x < x0 || y < y0 || x >= x1 || y >= y1 {
            return;
        }
    }
    let o = ((y as usize) * (w as usize) + x as usize) * 4;
    let sa = a as u32;
    let da = canvas[o + 3] as u32;
    let out_a = sa + da * (255 - sa) / 255;
    if out_a == 0 {
        return;
    }
    for c in 0..3 {
        let src = color[c] as u32;
        let dst = canvas[o + c] as u32;
        // 直通 alpha 合成（src 覆盖 dst）。
        canvas[o + c] = ((src * sa + dst * da * (255 - sa) / 255) / out_a) as u8;
    }
    canvas[o + 3] = out_a as u8;
}

fn fill_rect(canvas: &mut [u8], w: u32, x: i32, y: i32, rw: i32, rh: i32, color: [u8; 4]) {
    let h = canvas.len() as u32 / (w.max(1) * 4);
    for yy in y..y + rh {
        for xx in x..x + rw {
            let o = ((yy.max(0) as usize) * (w as usize) + xx.max(0) as usize) * 4;
            if yy < 0 || xx < 0 || xx >= w as i32 || yy >= h as i32 || o + 3 >= canvas.len() {
                continue;
            }
            canvas[o..o + 4].copy_from_slice(&color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceId;
    use yuris_log::Record;

    fn font() -> Font {
        // 测试用字体：优先系统 CJK 字体，缺则用注入的空字体不可行；
        // 本仓库测试环境已有 Windows/macOS/Linux 候选路径。
        let bytes = crate::load_cjk_font().expect("系统 CJK 字体");
        Font::from_bytes(
            bytes,
            fontdue::FontSettings {
                collection_index: 0,
                scale: 40.0,
                load_substitutions: true,
            },
        )
        .unwrap()
    }

    const TARGET: &str = "test";

    fn report() -> FatalReport {
        FatalReport {
            seq: 7,
            title: "致命错误 / FATAL ERROR".into(),
            message: "无法初始化图形后端: adapter 不可用".into(),
            tail: vec![Record {
                seq: 7,
                level: yuris_log::Level::Fatal,
                target: TARGET,
                message: "无法初始化图形后端".into(),
                elapsed_ms: 12,
                unix_ms: 0,
            }],
            unix_ms: 0,
        }
    }

    #[test]
    fn panel_is_canvas_sized_and_has_center_content() {
        let p = build_fatal_panel(&font(), &report(), 640, 480, 1.0, 0);
        assert_eq!((p.width, p.height), (640, 480));
        assert_eq!(p.rgba.len(), 640 * 480 * 4);
        // 面板中心应为不透明像素（PANEL 或文字）。
        let cx = (p.width / 2) as usize;
        let cy = (p.height / 2) as usize;
        let o = (cy * p.width as usize + cx) * 4;
        assert!(p.rgba[o + 3] > 200, "画布中心应有不透明错误面板");
        // 四角只有压暗层（半透明）。
        assert!(p.rgba[3] < 220, "角落应为半透明压暗");
    }

    #[test]
    fn overlay_layer_is_topmost_and_origin_aligned() {
        let l = crate::fatal_overlay_layer(ResourceId(crate::SC_FATAL_TEX), 1.0);
        assert_eq!(l.id, crate::SC_FATAL);
        assert_eq!(l.z, i64::MAX, "错误窗必须压过任何脚本层");
        assert_eq!((l.x, l.y, l.scale_x, l.scale_y), (0.0, 0.0, 1.0, 1.0));
        assert!(l.visible);
        assert_eq!(l.resource, Some(ResourceId(crate::SC_FATAL_TEX)));

        // 物理分辨率纹理:scale = 1/1.75 才能回到逻辑画布尺寸。
        let hi = crate::fatal_overlay_layer(ResourceId(crate::SC_FATAL_TEX), 1.75);
        assert!((hi.scale_x - 1.0 / 1.75).abs() < 1e-6);
        assert!((hi.scale_y - 1.0 / 1.75).abs() < 1e-6);
    }

    #[test]
    fn long_message_scrolls_instead_of_being_truncated() {
        let mut r = report();
        r.message = "很长的错误信息。".repeat(400);
        let p = build_fatal_panel(&font(), &r, 320, 200, 1.0, 0);
        assert_eq!(p.rgba.len(), 320 * 200 * 4);
        assert!(
            p.max_scroll > 0,
            "长内容必须可滚动,得到 max_scroll={}",
            p.max_scroll
        );
        assert_eq!(p.scroll, 0);

        // 滚到底:scroll 被夹紧且画面与顶部不同(内容确实移动了)。
        let bottom = build_fatal_panel(&font(), &r, 320, 200, 1.0, p.max_scroll * 10);
        assert_eq!(bottom.scroll, p.max_scroll);
        assert_ne!(p.rgba, bottom.rgba, "滚动后内容必须变化");
    }

    #[test]
    fn short_report_has_no_scroll() {
        let p = build_fatal_panel(&font(), &report(), 640, 480, 1.0, 0);
        assert_eq!(p.max_scroll, 0);
    }

    #[test]
    fn huge_scale_still_fits_canvas() {
        // 物理分辨率排版(1100×825 @2.0)不得越界/panic。
        // 注意:高 DPI 下视口也同比放大,同样的文本反而更容易一屏放下 ——
        // 故这里用足够长的内容才让滚动路径真正被走到。
        let mut r = report();
        r.message = "运行时缺失。".repeat(1500);
        let p = build_fatal_panel(&font(), &r, 2200, 1650, 2.0, 0);
        assert_eq!((p.width, p.height), (2200, 1650));
        assert!(
            p.max_scroll > 0,
            "长内容即使在高 DPI 画布上也必须可滚动,得到 max_scroll={}",
            p.max_scroll
        );
        assert_eq!(p.rgba.len(), 2200 * 1650 * 4);
    }
}
