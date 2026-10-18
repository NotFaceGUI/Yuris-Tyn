//! HUD 渲染画布:单一管线 + 两种基元(字形图集采样 / 圆角矩形 SDF)。
//!
//! ## Windows:分层窗口(UpdateLayeredWindow)
//!
//! 实测(2026-10-17,本机):wgpu surface 在 Windows 上 `alpha_modes` 只有
//! `[Opaque]` —— DXGI 的 HWND 交换链不支持每像素 alpha,透明清屏会整层
//! 变黑盖住游戏。因此 Windows 走**分层窗口**路径:HUD 离屏渲染到
//! Bgra8Unorm 纹理 → 拷回 CPU → 写入 32bpp 预乘 DIB → `UpdateLayeredWindow`
//! 交给 DWM 做真正的逐像素 alpha 合成。着色器输出**预乘 alpha**,混合用
//! `One / OneMinusSrcAlpha`。
//!
//! ## 其它平台:wgpu surface
//!
//! 清屏为全透明黑,alpha 模式优先 PreMultiplied(不支持时退 Auto)。
//!
//! 两种后端共用同一管线;user32/gdi32 以最小 `extern "system"` 绑定接入,
//! 不引入新依赖(仅 x64:用 `GetWindowLongPtrW`)。

use std::sync::Arc;
use winit::window::Window;

use crate::text::{ATLAS_H, ATLAS_W};

/// 顶点:pos(px)+ uv(字形:图集归一化;矩形:局部中心像素坐标)+
/// col(sRGB 0..1)+ prm(kind, 圆角半径, 半宽, 半高)。
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vx {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub col: [f32; 4],
    pub prm: [f32; 4],
}

const SHADER: &str = r#"
struct U {
    inv: vec2<f32>,
    pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) col: vec4<f32>,
    @location(2) prm: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>,
           @location(2) col: vec4<f32>, @location(3) prm: vec4<f32>) -> VOut {
    // 像素坐标(y 向下)→ 裁剪坐标。
    var o: VOut;
    o.pos = vec4<f32>(
        pos.x * u.inv.x * 2.0 - 1.0,
        1.0 - pos.y * u.inv.y * 2.0,
        0.0,
        1.0
    );
    o.uv = uv;
    o.col = col;
    o.prm = prm;
    return o;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    if (i.prm.x < 0.5) {
        // 字形:图集 coverage × 顶点色(预乘输出)。
        let a = textureSample(tex, samp, i.uv).r * i.col.a;
        return vec4<f32>(i.col.rgb * a, a);
    }
    // 圆角矩形 SDF:uv = 以矩形中心为原点的局部像素坐标,prm.zw = 半宽半高。
    let q = abs(i.uv) - (i.prm.zw - vec2<f32>(i.prm.y));
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - i.prm.y;
    let a = (1.0 - smoothstep(0.0, 1.5, d)) * i.col.a;
    return vec4<f32>(i.col.rgb * a, a);
}
"#;

/// 一帧绘制所需的 GPU 资源组(以独立借用传出,避免 `&mut self` 冲突)。
struct Pass<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    pipeline: &'a wgpu::RenderPipeline,
    bind_group: &'a wgpu::BindGroup,
    uniform_buf: &'a wgpu::Buffer,
}

pub struct Canvas {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    backend: Backend,
    pub size: (u32, u32),
}

enum Backend {
    /// Windows:离屏渲染 + UpdateLayeredWindow(逐像素 alpha)。
    #[cfg(windows)]
    Layered(LayeredCtx),
    /// 其它平台:wgpu surface(透明合成由平台能力决定)。
    #[cfg(not(windows))]
    Surface(SurfaceCtx),
}

#[cfg(not(windows))]
struct SurfaceCtx {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

#[cfg(windows)]
struct LayeredCtx {
    hwnd: isize,
    /// 内存 DC 与 32bpp 预乘 DIB(自上而下)。
    hdc_mem: isize,
    hbm: isize,
    bits: *mut u8,
    /// 离屏渲染目标(Bgra8Unorm)与回读缓冲。
    target: wgpu::Texture,
    readback: wgpu::Buffer,
    row_pitch: u32,
}

// ---- user32 / gdi32 最小绑定(仅本文件使用)----
#[cfg(windows)]
mod ffi {
    #[repr(C)]
    pub struct POINT {
        pub x: i32,
        pub y: i32,
    }
    #[repr(C)]
    pub struct SIZE {
        pub cx: i32,
        pub cy: i32,
    }
    #[repr(C)]
    pub struct BLENDFUNCTION {
        pub blend_op: u8,
        pub blend_flags: u8,
        pub src_constant_alpha: u8,
        pub alpha_format: u8,
    }
    #[repr(C)]
    pub struct BITMAPINFOHEADER {
        pub bi_size: u32,
        pub bi_width: i32,
        pub bi_height: i32,
        pub bi_planes: u16,
        pub bi_bit_count: u16,
        pub bi_compression: u32,
        pub bi_size_image: u32,
        pub bi_x_pels_per_meter: i32,
        pub bi_y_pels_per_meter: i32,
        pub bi_clr_used: u32,
        pub bi_clr_important: u32,
    }
    #[repr(C)]
    pub struct BITMAPINFO {
        pub bmi_header: BITMAPINFOHEADER,
        pub bmi_colors: [u32; 1],
    }

    const AC_SRC_OVER: u8 = 0;
    const AC_SRC_ALPHA: u8 = 1;
    pub const ULW_ALPHA: u32 = 2;
    pub const WS_EX_LAYERED: isize = 0x0008_0000;
    pub const GWL_EXSTYLE: i32 = -20;

    pub fn blend_premult() -> BLENDFUNCTION {
        BLENDFUNCTION { blend_op: AC_SRC_OVER, blend_flags: 0, src_constant_alpha: 255, alpha_format: AC_SRC_ALPHA }
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn GetWindowLongPtrW(hwnd: isize, nindex: i32) -> isize;
        pub fn SetWindowLongPtrW(hwnd: isize, nindex: i32, dwnewlong: isize) -> isize;
        pub fn UpdateLayeredWindow(
            hwnd: isize,
            hdcdst: isize,
            pptdst: *const POINT,
            psize: *const SIZE,
            hdcsrc: isize,
            pptsrc: *const POINT,
            crkey: u32,
            pblend: *const BLENDFUNCTION,
            dwflags: u32,
        ) -> i32;
        pub fn GetDC(hwnd: isize) -> isize;
        pub fn ReleaseDC(hwnd: isize, hdc: isize) -> i32;
        pub fn CreateCompatibleDC(hdc: isize) -> isize;
        pub fn DeleteDC(hdc: isize) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        pub fn CreateDIBSection(
            hdc: isize,
            pbmi: *const BITMAPINFO,
            iusage: u32,
            ppvbits: *mut *mut u8,
            hsection: isize,
            offset: u32,
        ) -> isize;
        pub fn SelectObject(hdc: isize, hgdiobj: isize) -> isize;
        pub fn DeleteObject(hgdiobj: isize) -> i32;
    }
}

impl Canvas {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        use wgpu::util::DeviceExt;
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        // Windows:分层窗口(逐像素 alpha 的唯一可靠路径,见模块文档)。
        #[cfg(windows)]
        let (device, queue, backend) = {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let RawWindowHandle::Win32(h) = window
                .window_handle()
                .map_err(|e| format!("取窗口句柄: {e}"))?
                .as_raw()
            else {
                return Err("非 Win32 窗口".into());
            };
            let hwnd = h.hwnd.get();
            unsafe {
                let style = ffi::GetWindowLongPtrW(hwnd, ffi::GWL_EXSTYLE);
                ffi::SetWindowLongPtrW(hwnd, ffi::GWL_EXSTYLE, style | ffi::WS_EX_LAYERED);
            }
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
            .ok_or_else(|| "request adapter: 无兼容适配器".to_string())?;
            let (device, queue) = pollster::block_on(adapter.request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("yuris-debug device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: Default::default(),
                },
                None,
            ))
            .map_err(|e| format!("request device: {e}"))?;
            let hdc_mem = unsafe { ffi::CreateCompatibleDC(0) };
            if hdc_mem == 0 {
                return Err("CreateCompatibleDC 失败".into());
            }
            let mut ctx = LayeredCtx {
                hwnd,
                hdc_mem,
                hbm: 0,
                bits: std::ptr::null_mut(),
                target: device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("yuris-debug offscreen(占位,recreate 替换)"),
                    size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 4,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                row_pitch: 0,
            };
            Self::recreate_layered(&device, &mut ctx, size.width.max(1), size.height.max(1));
            yuris_log::info!("[debug] HUD 后端 = UpdateLayeredWindow(逐像素 alpha)");
            (device, queue, Backend::Layered(ctx))
        };
        // 其它平台:常规 surface。
        #[cfg(not(windows))]
        let (device, queue, backend) = {
            let surface = instance
                .create_surface(window.clone())
                .map_err(|e| format!("create surface: {e}"))?;
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }))
            .ok_or_else(|| "request adapter: 无兼容适配器".to_string())?;
            let (device, queue) = pollster::block_on(adapter.request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("yuris-debug device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: Default::default(),
                },
                None,
            ))
            .map_err(|e| format!("request device: {e}"))?;
            let caps = surface.get_capabilities(&adapter);
            let format = caps
                .formats
                .iter()
                .copied()
                .find(|f| !f.is_srgb())
                .unwrap_or(caps.formats[0]);
            let alpha = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                wgpu::CompositeAlphaMode::PreMultiplied
            } else {
                wgpu::CompositeAlphaMode::Auto
            };
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::AutoVsync,
                alpha_mode: alpha,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };
            surface.configure(&device, &config);
            (device, queue, Backend::Surface(SurfaceCtx { instance, surface, config }))
        };

        let target_format = match &backend {
            #[cfg(windows)]
            Backend::Layered(_) => wgpu::TextureFormat::Bgra8Unorm,
            #[cfg(not(windows))]
            Backend::Surface(s) => s.config.format,
        };

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuris-debug shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuris-debug bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuris-debug pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuris-debug pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vx>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // uniform(16B 对齐)+ R8 图集 + 线性采样,装进一个 bind group。
        let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuris-debug uniform"),
            contents: bytemuck::cast_slice(&[0.0f32; 4]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuris-debug atlas"),
            size: wgpu::Extent3d { width: ATLAS_W, height: ATLAS_H, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuris-debug sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuris-debug bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniform_buf,
                        offset: 0,
                        size: None,
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &atlas_tex.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group,
            uniform_buf,
            atlas_tex,
            backend,
            size: (size.width, size.height),
        })
    }

    /// (再)建分层窗口的离屏目标 / 回读缓冲 / DIB。
    #[cfg(windows)]
    fn recreate_layered(device: &wgpu::Device, ctx: &mut LayeredCtx, w: u32, h: u32) {
        let pitch = (w * 4).div_ceil(256) * 256;
        if ctx.hbm != 0 {
            unsafe { ffi::DeleteObject(ctx.hbm) };
            ctx.hbm = 0;
            ctx.bits = std::ptr::null_mut();
        }
        let bi = ffi::BITMAPINFO {
            bmi_header: ffi::BITMAPINFOHEADER {
                bi_size: std::mem::size_of::<ffi::BITMAPINFOHEADER>() as u32,
                bi_width: w as i32,
                // 负高 = 自上而下,与 GPU 图像行序一致。
                bi_height: -(h as i32),
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: 0, // BI_RGB
                bi_size_image: 0,
                bi_x_pels_per_meter: 0,
                bi_y_pels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
            bmi_colors: [0; 1],
        };
        let mut bits: *mut u8 = std::ptr::null_mut();
        let hbm = unsafe {
            ffi::CreateDIBSection(ctx.hdc_mem, &bi, 0 /* DIB_RGB_COLORS */, &mut bits, 0, 0)
        };
        if hbm == 0 || bits.is_null() {
            yuris_log::error!("[debug] CreateDIBSection 失败({w}×{h})");
            return;
        }
        unsafe { ffi::SelectObject(ctx.hdc_mem, hbm) };
        ctx.hbm = hbm;
        ctx.bits = bits;
        ctx.target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuris-debug offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        ctx.readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuris-debug readback"),
            size: (pitch as u64) * (h as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        ctx.row_pitch = pitch;
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.size = (width, height);
        match &mut self.backend {
            #[cfg(windows)]
            Backend::Layered(ctx) => Self::recreate_layered(&self.device, ctx, width, height),
            #[cfg(not(windows))]
            Backend::Surface(s) => {
                s.config.width = width;
                s.config.height = height;
                s.surface.configure(&self.device, &s.config);
            }
        }
    }

    /// 全量上传字形图集(R8 coverage)。
    pub fn upload_atlas(&self, bytes: &[u8]) {
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_W),
                rows_per_image: Some(ATLAS_H),
            },
            wgpu::Extent3d { width: ATLAS_W, height: ATLAS_H, depth_or_array_layers: 1 },
        );
    }

    /// 渲染一帧并上屏(surface:present;分层:UpdateLayeredWindow)。
    pub fn draw(&mut self, verts: &[Vx], idx: &[u32], clear: [f64; 4]) {
        if verts.is_empty() || idx.is_empty() || self.size.0 == 0 || self.size.1 == 0 {
            return;
        }
        let p = Pass {
            device: &self.device,
            queue: &self.queue,
            pipeline: &self.pipeline,
            bind_group: &self.bind_group,
            uniform_buf: &self.uniform_buf,
        };
        match &mut self.backend {
            #[cfg(windows)]
            Backend::Layered(ctx) => draw_layered(&p, ctx, self.size, verts, idx, clear),
            #[cfg(not(windows))]
            Backend::Surface(s) => draw_surface(p, s, self.size, verts, idx, clear),
        }
    }
}

/// 共用的编码段:渲染基元到一个目标视图。
fn encode_pass(
    p: &Pass<'_>,
    size: (u32, u32),
    enc: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    verts: &[Vx],
    idx: &[u32],
    clear: [f64; 4],
) {
    use wgpu::util::DeviceExt;
    let vb = p.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("yuris-debug verts"),
        contents: bytemuck::cast_slice(verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ib = p.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("yuris-debug idx"),
        contents: bytemuck::cast_slice(idx),
        usage: wgpu::BufferUsages::INDEX,
    });
    let inv = [1.0 / size.0.max(1) as f32, 1.0 / size.1.max(1) as f32, 0.0, 0.0];
    p.queue.write_buffer(p.uniform_buf, 0, bytemuck::cast_slice(&inv));
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("yuris-debug pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear[0],
                        g: clear[1],
                        b: clear[2],
                        a: clear[3],
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(p.pipeline);
        pass.set_bind_group(0, p.bind_group, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..idx.len() as u32, 0, 0..1);
    }
}

/// 分层窗口路径:离屏渲染 → 回读 → DIB → UpdateLayeredWindow。
#[cfg(windows)]
fn draw_layered(p: &Pass<'_>, ctx: &mut LayeredCtx, size: (u32, u32), verts: &[Vx], idx: &[u32], clear: [f64; 4]) {
    if ctx.bits.is_null() || ctx.row_pitch == 0 {
        return;
    }
    let (w, h) = size;
    let view = ctx.target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = p
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuris-debug enc") });
    encode_pass(p, size, &mut enc, &view, verts, idx, clear);
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &ctx.target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &ctx.readback,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(ctx.row_pitch),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    p.queue.submit([enc.finish()]);
    // 阻塞回读(调试 HUD,60Hz 下 3~4MB 拷贝可接受)。
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), wgpu::BufferAsyncError>>();
    ctx.readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    p.device.poll(wgpu::Maintain::Wait);
    if rx.recv().is_err() {
        return;
    }
    {
        let map = ctx.readback.slice(..).get_mapped_range();
        let stride = (w * 4) as usize;
        unsafe {
            for row in 0..h as usize {
                let src = map.as_ptr().add(row * ctx.row_pitch as usize);
                std::ptr::copy_nonoverlapping(src, ctx.bits.add(row * stride), stride);
            }
        }
    }
    ctx.readback.unmap();
    // DWM 合成:位置由 winit 的 set_outer_position 维护,这里只更新内容。
    let blend = ffi::blend_premult();
    let size = ffi::SIZE { cx: w as i32, cy: h as i32 };
    let src = ffi::POINT { x: 0, y: 0 };
    let ok = unsafe {
        let hdc_screen = ffi::GetDC(0);
        let r = ffi::UpdateLayeredWindow(
            ctx.hwnd,
            hdc_screen,
            std::ptr::null(),
            &size,
            ctx.hdc_mem,
            &src,
            0,
            &blend,
            ffi::ULW_ALPHA,
        );
        ffi::ReleaseDC(0, hdc_screen);
        r
    };
    if ok == 0 {
        yuris_log::warn!("[debug] UpdateLayeredWindow 失败(内容未上屏)");
    }
}

/// surface 路径(非 Windows):透明合成由平台能力决定。
#[cfg(not(windows))]
fn draw_surface(p: Pass<'_>, s: &mut SurfaceCtx, size: (u32, u32), verts: &[Vx], idx: &[u32], clear: [f64; 4]) {
    let frame = match s.surface.get_current_texture() {
        Ok(f) => f,
        Err(_) => {
            s.surface.configure(p.device, &s.config);
            return;
        }
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = p
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuris-debug enc") });
    encode_pass(&p, size, &mut enc, &view, verts, idx, clear);
    p.queue.submit([enc.finish()]);
    frame.present();
}
