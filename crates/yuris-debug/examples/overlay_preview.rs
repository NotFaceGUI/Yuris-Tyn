//! 调试 HUD 实机预览(无需样本游戏;成果 163 的验证工具)。
//!
//! 开一个纯色"假游戏窗"作为底,`DebugOverlay` 覆盖层贴合其客户区,
//! 以合成遥测(含周期性卡顿簇与 WAIT 空闲段)驱动 HUD 动画。
//!
//! ```bash
//! cargo run -p yuris-debug --example overlay_preview
//! ```
//!
//! 只依赖本 crate 与 winit/wgpu,不触碰其它模块。

use std::sync::Arc;
use std::time::{Duration, Instant};

use yuris_debug::{DebugOverlay, FrameSample, Telemetry, VmRunState};

struct Preview {
    base: Option<Arc<winit::window::Window>>,
    base_canvas: Option<BaseCanvas>,
    overlay: Option<DebugOverlay>,
    tele: Telemetry,
    frame_no: u64,
    next_tick: Instant,
}

/// 假游戏窗的最小 wgpu 清屏画布(充当"游戏画面"底色)。
struct BaseCanvas {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl BaseCanvas {
    fn new(window: Arc<winit::window::Window>) -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("preview base"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            },
            None,
        ))
        .expect("device");
        let caps = surface.get_capabilities(&adapter);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: caps.formats[0],
            width: 1280,
            height: 720,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        Self { surface, device, queue, config }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn draw(&mut self) {
        let Ok(frame) = self.surface.get_current_texture() else { return };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.10,
                            g: 0.16,
                            b: 0.28,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit([enc.finish()]);
        frame.present();
    }
}

impl winit::application::ApplicationHandler for Preview {
    fn resumed(&mut self, el: &winit::event_loop::ActiveEventLoop) {
        if self.base.is_some() {
            return;
        }
        let attrs = winit::window::Window::default_attributes()
            .with_title("overlay_preview 假游戏窗")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let base = Arc::new(el.create_window(attrs).expect("base window"));
        self.base_canvas = Some(BaseCanvas::new(base.clone()));
        self.base = Some(base.clone());
        self.base_canvas.as_mut().unwrap().draw();
        // HUD:字体按候选序本机加载(失败时 HUD 仍出形状,文本空)。
        let font_path = yuris_debug::view::FONT_CANDIDATES
            .iter()
            .find(|p| std::path::Path::new(p).is_file());
        let font_bytes = font_path
            .map(std::fs::read)
            .unwrap_or_else(|| Err(std::io::Error::new(std::io::ErrorKind::NotFound, "无候选字体")))
            .unwrap_or_default();
        let mut overlay = DebugOverlay::new(self.tele.clone(), font_bytes);
        overlay.resumed(el);
        overlay.follow_window(&base);
        // OVERLAY=0:对照实验 —— 只看底窗,验证覆盖层是否挡住了它。
        if std::env::var("OVERLAY").as_deref() != Ok("0") {
            overlay.show();
        }
        self.overlay = Some(overlay);
        self.next_tick = Instant::now();
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let now = Instant::now();
        if now < self.next_tick {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(self.next_tick));
            return;
        }
        while self.next_tick <= now {
            self.next_tick += Duration::from_micros(16_667);
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(self.next_tick));
        self.push_synthetic(now);
        if let Some(b) = self.base.as_ref() {
            if let Some(o) = self.overlay.as_mut() {
                o.follow_window(b);
            }
        }
        if let Some(o) = self.overlay.as_ref() {
            o.sync_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        if self.overlay.as_mut().is_some_and(|o| o.handles(window_id)) {
            let o = self.overlay.as_mut().unwrap();
            match event {
                winit::event::WindowEvent::RedrawRequested => o.redraw(Instant::now()),
                winit::event::WindowEvent::Resized(s) => o.resized(s),
                _ => {}
            }
            return;
        }
        match event {
            winit::event::WindowEvent::CloseRequested => event_loop.exit(),
            winit::event::WindowEvent::Resized(s) => {
                if let Some(c) = self.base_canvas.as_mut() {
                    c.resize(s.width, s.height);
                    c.draw();
                }
                if let (Some(b), Some(o)) = (self.base.as_ref(), self.overlay.as_mut()) {
                    o.follow_window(b);
                }
            }
            winit::event::WindowEvent::Moved(_) => {
                if let (Some(b), Some(o)) = (self.base.as_ref(), self.overlay.as_mut()) {
                    o.follow_window(b);
                }
            }
            winit::event::WindowEvent::KeyboardInput { event, .. } => {
                if event.state == winit::event::ElementState::Pressed {
                    match event.logical_key {
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::F3) => {
                            if let Some(o) = self.overlay.as_mut() {
                                o.toggle();
                            }
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) => {
                            event_loop.exit()
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl Preview {
    /// 合成遥测:典型 1~2ms tick,每 ~1.6s 一次 5× 卡顿簇,周期 WAIT 空闲段。
    fn push_synthetic(&mut self, now: Instant) {
        self.frame_no += 1;
        let n = self.frame_no;
        let busy = n % 97 < 8;
        let mut vm_ns: u64 = if n % 3 == 0 {
            1_400_000 + (n % 7) as u64 * 180_000
        } else {
            900_000
        };
        if busy {
            vm_ns *= 5;
        }
        let render_ns = 2_100_000 + (n % 11) as u64 * 90_000;
        let state = if !busy && n % 150 >= 10 && n % 150 <= 25 {
            VmRunState::Idle
        } else {
            VmRunState::Running
        };
        self.tele.push_frame(FrameSample {
            at: now,
            vm_ns,
            render_ns,
            total_ns: vm_ns + render_ns,
            vm_state: state,
            fps_target: 60,
        });
    }
}

fn main() {
    let tele = Telemetry::new();
    let mut preview = Preview {
        base: None,
        base_canvas: None,
        overlay: None,
        tele,
        frame_no: 0,
        next_tick: Instant::now(),
    };
    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    event_loop.run_app(&mut preview).unwrap();
}
