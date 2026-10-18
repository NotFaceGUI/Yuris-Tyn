//! 调试 HUD 覆盖层:一个**贴合游戏客户区**的无边框透明窗。
//!
//! - 透明 + 鼠标直通(`set_cursor_hittest(false)`)+ 不抢焦点(`with_active(false)`)
//!   + 置顶 ⇒ 视觉上就是「盖在游戏窗口内容上」,而游戏接收全部输入;
//! - 位置/尺寸由调用方在游戏窗口 `Moved` / `Resized` / 启动时用
//!   [`DebugOverlay::follow_window`] 同步;
//! - 不依赖 yuris-render:游戏画面管线零改动,覆盖层由 DWM 与游戏画面合成。
//!
//! 显隐走 360ms 缓入缓出补间(alpha + 上滑);淡出完成后自动隐藏窗口,
//! 不再产生任何重绘开销。

use std::sync::Arc;
use std::time::Instant;
use winit::window::Window;

use crate::ease::ease_in_out_cubic;
use crate::gfx::Canvas;
use crate::hud::{self, HudState, Painter};
use crate::telemetry::Telemetry;
use crate::text::{load_font_from_candidates, TextCache};

/// 显隐补间时长(缓入缓出)。
const SHOW_SECS: f32 = 0.36;

struct Win {
    window: Arc<Window>,
    canvas: Canvas,
}

pub struct DebugOverlay {
    tele: Telemetry,
    visible: bool,
    anim: f32,
    anim_from: f32,
    anim_to: f32,
    anim_t0: Instant,
    tc: TextCache,
    hud: HudState,
    win: Option<Win>,
    last_draw: Instant,
}

impl DebugOverlay {
    pub fn new(tele: Telemetry, font_bytes: Vec<u8>) -> Self {
        let font = fontdue::Font::from_bytes(
            font_bytes,
            fontdue::FontSettings { collection_index: 0, scale: 40.0, load_substitutions: true },
        )
        .ok();
        if font.is_none() {
            yuris_log::error!("[debug] HUD 字体解析失败:仅形状,无文本");
        }
        Self {
            tele,
            visible: false,
            anim: 0.0,
            anim_from: 0.0,
            anim_to: 0.0,
            anim_t0: Instant::now(),
            tc: TextCache::new(font),
            hud: HudState::default(),
            win: None,
            last_draw: Instant::now(),
        }
    }

    /// 创建覆盖层窗(无边框/透明/直通/不抢焦点/置顶,初始隐藏)。
    pub fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.win.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("YurisKernel 调试 HUD")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_visible(false);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                yuris_log::error!("[debug] 创建覆盖层窗失败: {e}");
                return;
            }
        };
        // 鼠标直通:点击全部落到下面的游戏窗口。
        window.set_cursor_hittest(false).ok();
        match Canvas::new(window.clone()) {
            Ok(canvas) => {
                yuris_log::info!("[debug] HUD 覆盖层就绪({}×{})", canvas.size.0, canvas.size.1);
                self.win = Some(Win { window, canvas });
                self.last_draw = Instant::now();
            }
            Err(e) => yuris_log::error!("[debug] 覆盖层 wgpu 初始化失败,HUD 不可用: {e}"),
        }
    }

    /// 贴合游戏窗口客户区(位置 + 尺寸;仅在变化时落窗,避免逐帧扰动)。
    pub fn follow_window(&mut self, game: &Window) {
        let Some(w) = self.win.as_ref() else { return };
        let pos = game.inner_position().ok();
        let size = game.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if let Some(p) = pos {
            if w.window.outer_position().ok() != Some(p) {
                w.window.set_outer_position(p);
            }
        }
        if w.window.inner_size() != size {
            let _ = w.window.request_inner_size(size);
        }
    }

    /// 该 WindowId 是否归本覆盖层(调用方事件分流用)。
    pub fn handles(&self, id: winit::window::WindowId) -> bool {
        self.win.as_ref().is_some_and(|w| w.window.id() == id)
    }

    pub fn toggle(&mut self) {
        if self.visible {
            self.visible = false;
        } else {
            self.visible = true;
            if let Some(w) = self.win.as_ref() {
                w.window.set_visible(true);
                w.window.request_redraw();
            }
        }
    }

    pub fn show(&mut self) {
        if !self.visible {
            self.toggle();
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// 跟随游戏帧节拍的重绘请求(仅需要动画时申请,避免忙转)。
    pub fn sync_redraw(&self) {
        if self.wants_frame() {
            if let Some(w) = self.win.as_ref() {
                w.window.request_redraw();
            }
        }
    }

    fn wants_frame(&self) -> bool {
        self.visible || self.anim_value(Instant::now()) > 0.003
    }

    fn anim_value(&self, now: Instant) -> f32 {
        let t = (now.duration_since(self.anim_t0).as_secs_f32() / SHOW_SECS).clamp(0.0, 1.0);
        self.anim_from + (self.anim_to - self.anim_from) * ease_in_out_cubic(t)
    }

    /// 覆盖层窗尺寸变化(surface 重配置)。
    pub fn resized(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if let Some(w) = self.win.as_mut() {
            w.canvas.resize(size.width, size.height);
        }
    }

    /// 绘制一帧 HUD(由调用方事件循环在覆盖层 RedrawRequested 时调用)。
    pub fn redraw(&mut self, now: Instant) {
        let target = if self.visible { 1.0 } else { 0.0 };
        if target != self.anim_to {
            self.anim_from = self.anim_value(now);
            self.anim_to = target;
            self.anim_t0 = now;
        }
        self.anim = self.anim_value(now);
        let Some(w) = self.win.as_mut() else { return };
        if !self.visible && self.anim <= 0.004 {
            // 淡出完成:藏窗并停帧。
            w.window.set_visible(false);
            return;
        }
        let dt = now.duration_since(self.last_draw).as_secs_f32().min(0.05);
        self.last_draw = now;
        let scale = w.window.scale_factor() as f32;
        let size = w.window.inner_size();
        let samples = self.tele.snapshot();
        let mut painter = Painter::new();
        hud::build(
            &mut painter,
            &mut self.tc,
            &mut self.hud,
            hud::FrameCtx {
                samples: &samples,
                now,
                anim: self.anim,
                scale,
                w: size.width as f32,
                h: size.height as f32,
                dt,
            },
        );
        if self.tc.dirty {
            w.canvas.upload_atlas(&self.tc.atlas);
            self.tc.dirty = false;
        }
        w.canvas.draw(&painter.verts, &painter.idx, hud::CLEAR);
    }
}

/// HUD 默认字体候选(与播放器字体链同优先序:繁中优先)。
pub const FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\msjh.ttc",
    r"C:\Windows\Fonts\msjhbd.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
];

/// 按候选序装一个可用字体(独立小助手;播放器侧的 `load_cjk_font` 属于
/// player-core,本 crate 不依赖它)。
pub fn load_default_font() -> Option<fontdue::Font> {
    load_font_from_candidates(FONT_CANDIDATES)
}
