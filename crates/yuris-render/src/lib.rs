//! yuris-render
//!
//! L6:GraphicsBackend 实现 —— winit + wgpu(P8.1/P8.2,成果 65;渲染对齐
//! 成果 124/125)。
//!
//! - **P8.1 窗口/设备初始化**:winit 窗口 + wgpu surface/adapter/device。
//! - **P8.2 图层合成**:逻辑画布(**每款作品自带 `yscfg.ybn` 的分辨率**,
//!   成果 121/124)→ 视口 letterbox 缩放;Layer 的 x/y/scale/alpha/z 合成
//!   (z 升序绘制;预乘 alpha 混合)。
//! - **成果 125(手册 `cg/cg.html` 对齐)**:
//!   * `MODE` 0..18 → 管线混合态(可精确表达的 5 种);
//!   * `T`/`TID`/`TA` 过渡(图案软阈值逐像素显现);
//!   * `MID` 外部遮罩(遮罩图层相乘)。
//! - **成果 124(性能)**:uniform 单次批量上传(旧实现每图层一次
//!   `write_buffer`)、纹理按 id 去重(`has_image`)、纹理绑定组缓存。
//! - `load_image`:PNG/JPG/BMP/GIF(image crate)→ wgpu 纹理。
//!
//! 坐标系:引擎逻辑坐标(画布像素,原点左上)`Layer.x/y` = 图层左上角;
//! `scale` 以图像原始像素为基准。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use yuris_runtime::{BackendError, GraphicsBackend, Result};
use yuris_scene::{BlendMode, Layer, ResourceId, Scene, TextLayout};

/// crate 版本（与 workspace 同步）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 引擎逻辑分辨率宽 —— **默认值**(最初样本 v555 的 `WINDOWINFO SX` oracle = 1920)。
///
/// **勘误(2026-10-13,成果 121)**:逻辑画布**不是常量**。真值 = 每款作品自带
/// `ysbin\yscfg.ybn`(YSCF)的 `screen_width/height`。本作 <game-b>(引擎 494)
/// 实测 **1100×825**;写死 1920×1080 会让所有图层坐标被按错误画布缩放/加黑边
/// (实机报告:「分辨率有问题」「CG 混乱」)。运行期用 [`set_logical_canvas`] 设置、
/// [`logical_w`]/[`logical_h`] 读取。
pub const LOGICAL_W: f32 = 1920.0;
/// 逻辑分辨率高(默认;见 [`LOGICAL_W`])。
pub const LOGICAL_H: f32 = 1080.0;

/// 运行期逻辑画布(应等于游戏自身 `yscfg.ybn` 的分辨率)。
///
/// 进程级单例:**一次只跑一款作品**(CLI/播放器),由启动链按游戏数据设置。
/// 彻底做法是把画布穿进渲染上下文(待做),此处先保证「不再写死」。
static LOGICAL_W_BITS: AtomicU32 = AtomicU32::new(LOGICAL_W.to_bits());
static LOGICAL_H_BITS: AtomicU32 = AtomicU32::new(LOGICAL_H.to_bits());

/// 设定逻辑画布(取游戏自身 `yscfg.ybn` 的 `screen_width/height`)。
pub fn set_logical_canvas(w: f32, h: f32) {
    if w > 0.0 && h > 0.0 {
        LOGICAL_W_BITS.store(w.to_bits(), Ordering::Relaxed);
        LOGICAL_H_BITS.store(h.to_bits(), Ordering::Relaxed);
    }
}

/// 当前逻辑画布宽(默认 [`LOGICAL_W`])。
pub fn logical_w() -> f32 {
    f32::from_bits(LOGICAL_W_BITS.load(Ordering::Relaxed))
}

/// 当前逻辑画布高(默认 [`LOGICAL_H`])。
pub fn logical_h() -> f32 {
    f32::from_bits(LOGICAL_H_BITS.load(Ordering::Relaxed))
}

const UNIFORM_STRIDE: u64 = 256;
const MAX_LAYERS: usize = 2048;
/// uniform 内层结构大小:transform(64)+ color(16)+ params(16)= 96。
const UNIFORM_SIZE: u64 = 96;
/// 图层组离屏目标的纹理格式(成果 134)。
///
/// **为什么固定 sRGB**:组纹理还要被父层当普通纹理采样;素材纹理是
/// `Rgba8UnormSrgb`(采样时做 sRGB→线性),组纹理若用线性格式就会在
/// 「组内合成 → 再采样」之间少一次/多一次 gamma 往返,同 z 的组与非组层
/// 会出现亮度差。取同一格式 ⇒ 两条路径数值一致。
///
/// 与 `offscreen`(CLI 截图目标,`Rgba8Unorm`)格式不同是**有意的**:
/// 截图是回读给人看的字节,不需要再做一次采样的色彩往返;
/// 渲染侧管线按 (MODE, 格式) 缓存,两者互不干扰。
const GROUP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// 离屏渲染目标(成果 127):把场景画进**逻辑画布分辨率**的内部纹理。
///
/// 用途:CLI `--shot`(截图落 PNG,对拍/排错证)、未来的 parity 测试与
/// 图层组(レイヤセット)合成。分辨率 = 逻辑画布(不 letterbox)——
/// 截图与引擎同坐标系,才能逐像素比。
struct Offscreen {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// 回读缓冲(bytes_per_row 256 对齐;宽高随 texture)。
    readback: wgpu::Buffer,
    padded_bytes_per_row: u32,
    width: u32,
    height: u32,
}

const SHADER: &str = r#"
struct Uniforms {
    transform: mat4x4<f32>,
    // rgb = 颜色乘数; a = 不透明度(已含 A 槽)
    color: vec4<f32>,
    // x = 过渡进行度 T/256(0 隐 1 显)
    // y = 过渡精度 TA/256(边缘软化宽度)
    // z = 是否带过渡图案(0/1)
    // w = 是否带外部遮罩(0/1)
    params: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
@group(2) @binding(0) var pat: texture_2d<f32>;
@group(2) @binding(1) var msk: texture_2d<f32>;
@group(2) @binding(2) var samp2: sampler;

struct VSOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) xy: vec2<f32>, @location(1) uv: vec2<f32>) -> VSOut {
    var out: VSOut;
    out.pos = u.transform * vec4<f32>(xy, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    var a = c.a * u.color.a;
    // —— 过渡 T/TID/TA(手册 `cg/cg.html` keyword 表)—— 
    // 图案亮度 p 与进行度 t 的**软阈值**:t 推进 ⇒ 图案暗处先显、亮处后显。
    // UNVERIFIED(边缘曲线):手册只给关键字语义,没给合成公式;本内核取
    // 「软阈值」形状(见 yuris_scene::Transition 文档),待二进制公式替换。
    if (u.params.z > 0.5) {
        let p4 = textureSample(pat, samp2, in.uv);
        let p = dot(p4.rgb, vec3<f32>(0.299, 0.587, 0.114)) * p4.a;
        let soft = max(u.params.y, 1.0 / 256.0);
        let k = clamp((u.params.x - p) / soft + 0.5, 0.0, 1.0);
        a = a * k;
    }
    // —— 外部遮罩 MID(手册遮罩示例:图层与遮罩图合成后显示)—— 
    if (u.params.w > 0.5) {
        a = a * textureSample(msk, samp2, in.uv).a;
    }
    let rgb = c.rgb * u.color.rgb;
    return vec4<f32>(rgb * a, a);
}
"#;

/// 单个已加载纹理。
struct GpuImage {
    bind_group: wgpu::BindGroup,
    /// 同一纹理的独立 view(供 `pattern`/`mask` 绑定组使用)。
    aux_view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// **图层组的离屏合成目标**(成果 134;REA `docs/engine/render/02-layer-model.md` §2)。
///
/// 引擎模型:组 = 先把子层画进组自己的 RT,再把组当普通层画 ——
/// 组的 `A`/`MODE`/`E` 作用于「子层合成后的位图」,**不下推到子层**。
/// 本内核按同一语义建 wgpu 渲染目标。
///
/// 格式 = `Rgba8UnormSrgb`:与普通素材纹理同一色彩空间(采样时同样做
/// sRGB→线性),这样「组内合成 ⇒ 组纹理再被采样」与「直接采样素材」的
/// 数值路径一致,不引入一次多余的 gamma 往返。
struct GroupTarget {
    view: wgpu::TextureView,
    /// 供上层 pass 采样的绑定组(与普通纹理同布局)。
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

/// 一层的纹理来源:直接素材纹理 / 另一层(组)的合成结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TexSrc {
    /// 封包素材纹理(资源 id)。
    Image(u64),
    /// 图层组 / `COPY` 别名的**合成结果**(按源层 id 索引)。
    Group(u64),
}

/// wgpu 图形后端(P8.1/P8.2;渲染对齐成果 124/125)。
pub struct WgpuBackend {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// (混合模式, 目标格式) → 管线(**按需建**;成果 127 勘误:旧实现只按
    /// MODE 缓存,离屏目标是 Rgba8Unorm 而 surface 是 sRGB ⇒ 管线与 pass
    /// 不兼容,wgpu 校验直接 panic —— `--shot` 一开就炸)。
    pipelines: HashMap<(i64, wgpu::TextureFormat), wgpu::RenderPipeline>,
    /// 建管线所需的三份布局/着色器(懒建时用)。
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    color_format: wgpu::TextureFormat,
    texture_layout: wgpu::BindGroupLayout,
    aux_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// 动态 uniform(256B 对齐;每图层一个槽位)。
    uniform_buf: wgpu::Buffer,
    /// 覆盖整个 uniform 缓冲的**唯一**绑定组(配动态偏移复用)。
    uniform_bg: wgpu::BindGroup,
    /// 每帧 uniform 暂存(成果 124:单次 `write_buffer`,旧实现每层一次)。
    staging: Vec<u8>,
    /// 单位方块顶点缓冲(共享)。
    quad_buf: wgpu::Buffer,
    /// 每帧收集的待绘制图层(P8.2:z 升序合成)。
    frame_layers: Vec<Layer>,
    images: HashMap<u64, GpuImage>,
    /// (图案, 遮罩) → 绑定组缓存(成果 124:免去每帧每层重建)。
    aux_groups: HashMap<(u64, u64), wgpu::BindGroup>,
    /// 1×1 不透明白纹理(无图案/无遮罩时的占位,避免空绑定)。
    white_view: wgpu::TextureView,
    surface_size: (u32, u32),
    /// 离屏目标(成果 127;`render_scene_to_rgba` 时使用)。
    offscreen: Option<Offscreen>,
    /// 本帧画到离屏(`true`)还是 surface(`false`)。
    offscreen_active: bool,
    /// 图层组的离屏合成目标缓存(成果 134;键 = 源层 id)。
    group_targets: HashMap<u64, GroupTarget>,
    /// 组的图像尺寸缓存(供 `draw_layer` 之后按组尺寸算变换)。
    ///
    /// **当前无读者**(2026-10-18 清理告警时发现):组的变换改由 `group_targets` 承担。
    #[allow(dead_code)]
    group_dims: HashMap<u64, (u32, u32)>,
}

impl WgpuBackend {
    /// 从 winit 窗口创建后端(阻塞式初始化)。
    ///
    /// 窗口尺寸变化时调用 [`Self::resize`]。
    pub fn new(window: Arc<winit::window::Window>) -> Result<Self> {
        use wgpu::util::DeviceExt;
        let err = |msg: String| BackendError::Other(msg);

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let size = window.inner_size();
        let surface = instance
            .create_surface(window)
            .map_err(|e| err(format!("create surface: {e}")))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| err("request adapter: 无兼容适配器".into()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("yuris-render device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            }, None))
            .map_err(|e| err(format!("request device: {e}")))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = winit::dpi::PhysicalSize {
            width: size.width.max(1),
            height: size.height.max(1),
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // **present 模式可切换**(2026-10-17,性能取证用)。
            //
            // 缺省 `AutoVsync` = 与垂直同步对齐(正常游玩语义)。
            // `YURIS_UNLOCK_FPS=1` 切 `AutoNoVsync` —— 解除 vsync 阻塞,
            // 用于区分「VM 慢」与「present 阻塞」两个来源。
            // 代码里**不改变**渲染结果,只改变上屏节拍。
            present_mode: if std::env::var_os("YURIS_UNLOCK_FPS").is_some() {
                wgpu::PresentMode::AutoNoVsync
            } else {
                wgpu::PresentMode::AutoVsync
            },
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuris-render shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    // **动态偏移**:整个 uniform 缓冲只建 **一个** bind group,
                    // 每图层用 `set_bind_group(0, &bg, &[i * STRIDE])` 指定槽位。
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // 图案(TID)+ 遮罩(MID)+ 采样器:两者共用同一个 sampler。
        let aux_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aux layout (pattern/mask)"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
            label: Some("yuris-render pipeline layout"),
            bind_group_layouts: &[&uniform_layout, &texture_layout, &aux_layout],
            push_constant_ranges: &[],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuris-render sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuris-render uniforms"),
            size: UNIFORM_STRIDE * MAX_LAYERS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuris-render uniform bg (dynamic)"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform_buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(UNIFORM_SIZE),
                }),
            }],
        });

        // 1×1 白纹理:无图案/无遮罩时的占位(着色器用 params 开关短路,
        // 但绑定组必须齐备 —— WebGPU 不允许缺绑定)。
        let white = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuris-render white 1x1"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255u8, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let white_view = white.create_view(&wgpu::TextureViewDescriptor::default());

        // 单位方块顶点(位置 0..1,uv 0..1;两三角形)
        const QUAD: &[f32] = &[
            0.0, 0.0, 0.0, 0.0, //
            1.0, 0.0, 1.0, 0.0, //
            0.0, 1.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 1.0, //
            1.0, 0.0, 1.0, 0.0, //
            1.0, 1.0, 1.0, 1.0,
        ];
        let quad_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuris-render quad"),
            contents: bytemuck::cast_slice(QUAD),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipelines: HashMap::new(),
            pipeline_layout,
            shader,
            color_format: format,
            texture_layout,
            aux_layout,
            sampler,
            uniform_buf,
            uniform_bg,
            staging: Vec::new(),
            quad_buf,
            frame_layers: Vec::new(),
            images: HashMap::new(),
            aux_groups: HashMap::new(),
            white_view,
            surface_size: (size.width, size.height),
            offscreen: None,
            offscreen_active: false,
            group_targets: HashMap::new(),
            group_dims: HashMap::new(),
        })
    }

    /// 窗口尺寸变化(surface 重配置)。
    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface_size = (width, height);
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// 当前 surface 像素尺寸(播放器光标换算用)。
    pub fn surface_size(&self) -> (u32, u32) {
        self.surface_size
    }

    /// 已加载图像的原始尺寸(居中/布局计算用;未加载 → None)。
    pub fn image_size(&self, id: u64) -> Option<(u32, u32)> {
        self.images.get(&id).map(|i| (i.width, i.height))
    }

    /// 已上传纹理数(诊断用)。
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// 该资源 id 是否已上传过纹理(**去重入口**:调用方据此免去重复解码/上传)。
    ///
    /// 成果 124:旧实现每次 `load_image_rgba` 都新建纹理 + 绑定组 ——
    /// 台词层每帧重建、CG 每帧重载时每秒产生上百个 GPU 资源。
    pub fn has_image(&self, id: u64) -> bool {
        self.images.contains_key(&id)
    }

    /// 取(或懒建)某个(混合模式 × 目标格式)的渲染管线。
    fn pipeline_for(
        &mut self,
        blend: BlendMode,
        target_format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        let key = (blend.to_engine(), target_format);
        if !self.pipelines.contains_key(&key) {
            let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("yuris-render pipeline"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: 16,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(blend_state(blend)),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
            self.pipelines.insert(key, p);
        }
        self.pipelines.get(&key).unwrap()
    }

    /// (图案, 遮罩) 的绑定组(缓存;缺位用 1×1 白)。
    ///
    /// 需要 `&mut self`(首次创建后落缓存)⇒ **渲染通道开始前**批量预热
    /// (成果 124),通道内只做查表,避免与 `pass` 的借用冲突。
    fn aux_group(&mut self, pattern: Option<u64>, mask: Option<u64>) -> wgpu::BindGroup {
        let key = (pattern.unwrap_or(0), mask.unwrap_or(0));
        if let Some(g) = self.aux_groups.get(&key) {
            return g.clone();
        }
        let pat_view = pattern
            .and_then(|id| self.images.get(&id))
            .map(|i| i.aux_view.clone())
            .unwrap_or_else(|| self.white_view.clone());
        let msk_view = mask
            .and_then(|id| self.images.get(&id))
            .map(|i| i.aux_view.clone())
            .unwrap_or_else(|| self.white_view.clone());
        let g = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuris-render aux bg (pattern/mask)"),
            layout: &self.aux_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&pat_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&msk_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.aux_groups.insert(key, g.clone());
        g
    }

    /// 便捷入口:渲染整个 Scene(z 升序合成 + present)。
    ///
    /// 图层经 [`yuris_scene::Scene::drawable_layers`] 取 —— 媒体模式
    /// (`MOVIE`/`FLASH` 播放期间)会**屏蔽其他所有图层**(T23b,手册语义);
    /// 此时本入口只 present 空画面(媒体表面本身需解码器,显式 Unknown)。
    pub fn render(&mut self, scene: &Scene) -> Result<()> {
        self.begin_frame()?;
        for layer in scene.drawable_layers() {
            self.draw_layer(layer)?;
        }
        // 文本同属"其他图层"——媒体模式下一并屏蔽(手册:「他のレイヤは
        // 一切表示されません」)。
        if !scene.media_mode {
            if let Some(text) = &scene.text {
                self.draw_text(text)?;
            }
        }
        self.end_frame()
    }

    /// 当前逻辑坐标 → NDC 的 letterbox 映射矩阵(列主序)。
    ///
    /// **画布尺寸必须是运行期值**(`logical_w()/logical_h()`;成果 124)。
    fn logical_to_ndc(&self) -> [[f32; 4]; 4] {
        let (w, h) = self.surface_size;
        letterbox_matrix(logical_w(), logical_h(), w as f32, h as f32)
    }
}

/// 逻辑坐标(`lw`×`lh`)→ NDC 的 letterbox 映射矩阵(列主序;纯函数可单测)。///
/// **各向同性**:像素空间缩放因子对 x/y 相同
/// (`scale = min(w/lw, h/lh)`),窗口任意改比例只会改变信箱黑边,
/// 不会拉伸素材(实机「拉伸」排查锚点)。
///
/// 画布尺寸**由参数给出**(不读全局)—— 单测可覆盖多款作品的分辨率;
/// 调用方传 [`logical_w`]/[`logical_h`]。
pub fn letterbox_matrix(lw: f32, lh: f32, w: f32, h: f32) -> [[f32; 4]; 4] {
    let (lw, lh) = (lw.max(1.0), lh.max(1.0));
    let scale = (w / lw).min(h / lh);
    let sx = 2.0 * scale / w;
    let sy = 2.0 * scale / h;
    let off_x = -1.0 + (w - lw * scale) / w;
    let off_y = -1.0 + (h - lh * scale) / h;
    // 列主序;Y 翻转(逻辑 Y 向下,NDC Y 向上)
    [
        [sx, 0.0, 0.0, 0.0],
        [0.0, -sy, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [off_x, -off_y, 0.0, 1.0],
    ]
}

/// 引擎 `MODE`(0..18)→ GPU 混合态(**逐通道镜像 D3D9 状态序列**)。
///
/// 依据 = SDK 0.495 绘制函数 `FUN_00414ba4` 下发的原始状态
/// (`docs/engine/render/03-blend-modes.md` §2;成果 126 反编译)。
/// 映射规则(**约定,自洽且对齐 D3D9 语义**):
/// - D3D9 颜色因子 `SRCALPHA` 作用于 RGB = 直通色 × α;本后端 fragment 输出
///   **预乘 RGB**(`rgb*a`),故 D3D 颜色侧 `SRCALPHA` ≡ wgpu `One`。
/// - D3D9 因子作用于 **alpha 通道**用分量值(`SRCALPHA`→`SrcAlpha`、
///   `SRCCOLOR`→`SrcAlpha`、`INVDESTCOLOR`→`OneMinusDstAlpha`),**原样镜像**
///   —— 引擎 alpha 通道确实出现 `sa²` 这类形状(D3D9 无分离 alpha 时
///   SRCBLEND/DESTBLEND 同时作用于 alpha);背板不透明时不可见,但离屏
///   (图层组/CGACT[COPY] 后续接入)必须一致,故不"美化"。
/// - 表中未列的模式(4/8/10..18)在引擎里走像素着色器(挂号 G11),
///   **不得**落 `Normal` 近替;调用侧以 [`yuris_scene::BlendMode::is_implemented`] 拦截。
#[rustfmt::skip]
fn blend_state(blend: BlendMode) -> wgpu::BlendState {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation as Op};
    let st = |sc: BlendFactor, dc: BlendFactor, op: Op,
              sa: BlendFactor, da: BlendFactor| wgpu::BlendState {
        color: BlendComponent { src_factor: sc, dst_factor: dc, operation: op },
        alpha: BlendComponent { src_factor: sa, dst_factor: da, operation: op },
    };
    //              ┌─ color(src,dst,op)          ┌─ alpha(D3D 因子的 alpha 分量语义)
    match blend {
        // 0 通常: SRCBLEND=SRCALPHA(5) DESTBLEND=INVSRCALPHA(6) ADD(1)
        BlendMode::Normal   => st(BlendFactor::One, BlendFactor::OneMinusSrcAlpha, Op::Add,
                                  BlendFactor::SrcAlpha, BlendFactor::OneMinusSrcAlpha),
        // 1 加算: SRCALPHA(5) ONE(2) ADD(1)
        BlendMode::Add      => st(BlendFactor::One, BlendFactor::One, Op::Add,
                                  BlendFactor::SrcAlpha, BlendFactor::One),
        // 2 減算: SRCALPHA(5) ONE(2) REVSUBTRACT(3)  ⇒ dst − src
        BlendMode::Subtract => st(BlendFactor::One, BlendFactor::One, Op::ReverseSubtract,
                                  BlendFactor::SrcAlpha, BlendFactor::One),
        // 3 乗算: ZERO(1) SRCCOLOR(3) ADD(1)  ⇒ dst × srcColor
        BlendMode::Multiply => st(BlendFactor::Zero, BlendFactor::Src, Op::Add,
                                  BlendFactor::Zero, BlendFactor::SrcAlpha),
        // 5 スクリーン: INVDESTCOLOR(10) ONE(2) ADD(1)  ⇒ src(1−dst) + dst
        BlendMode::Screen   => st(BlendFactor::OneMinusDst, BlendFactor::One, Op::Add,
                                  BlendFactor::OneMinusDstAlpha, BlendFactor::One),
        // 6 比較(明): ONE(2) ONE(2) MAX(5)
        BlendMode::Lighten  => st(BlendFactor::One, BlendFactor::One, Op::Max,
                                  BlendFactor::One, BlendFactor::One),
        // 7 比較(暗): ONE(2) ONE(2) MIN(4)
        BlendMode::Darken   => st(BlendFactor::One, BlendFactor::One, Op::Min,
                                  BlendFactor::One, BlendFactor::One),
        // 9 除外: INVDESTCOLOR(10) INVSRCCOLOR(4) ADD(1)
        BlendMode::Exclusion => st(BlendFactor::OneMinusDst, BlendFactor::OneMinusSrc, Op::Add,
                                   BlendFactor::OneMinusDstAlpha, BlendFactor::OneMinusSrcAlpha),
        // 4/8/10..18:着色器路径未实装(调用侧拦截;此处仅兜底,不被选中)。
        // 万一被调用侧漏过,**绝不静默** —— 打横幅并说明用了哪种替代状态。
        other => {
            yuris_log::alert_once(
                module_path!(),
                format!("blend-fallback:{}", other.to_engine()),
                format!(
                    "混合模式走兜底状态(不应发生)\n\
                     MODE {} 未实装,渲染器已用 PREMULTIPLIED_ALPHA 顶替\n\
                     处置: 调用侧应以 is_implemented() 拦截;此处出现即为调用侧缺陷",
                    other.to_engine()
                ),
            );
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
        }
    }
}#[cfg(test)]
mod letterbox_tests {
    use super::*;

    /// 应用列主序矩阵到 (x,y)。
    fn apply(m: &[[f32; 4]; 4], x: f32, y: f32) -> (f32, f32) {
        (m[0][0] * x + m[3][0], m[1][1] * y + m[3][1])
    }

    #[test]
    fn exact_fit_and_offsets() {
        // 16:9 精确匹配(画布 1920×1080 → 窗口 2560×1440)
        let m = letterbox_matrix(1920.0, 1080.0, 2560.0, 1440.0);
        assert!((apply(&m, 0.0, 0.0).0 + 1.0).abs() < 1e-4);
        assert!((apply(&m, 0.0, 0.0).1 - 1.0).abs() < 1e-4);
        assert!((apply(&m, 1920.0, 1080.0).0 - 1.0).abs() < 1e-4);
        assert!((apply(&m, 1920.0, 1080.0).1 + 1.0).abs() < 1e-4);
        // 4:3 窗口 → 上下黑边 0.25
        let m = letterbox_matrix(1920.0, 1080.0, 1920.0, 1440.0);
        assert!((apply(&m, 0.0, 0.0).1 - 0.75).abs() < 1e-4);
        assert!((apply(&m, 1920.0, 1080.0).1 + 0.75).abs() < 1e-4);
        assert!((apply(&m, 0.0, 0.0).0 + 1.0).abs() < 1e-4);
        // 竖窗 → 左右黑边
        let m = letterbox_matrix(1920.0, 1080.0, 1080.0, 1440.0);
        assert!((apply(&m, 0.0, 0.0).0 + 1.0).abs() < 1e-4);
        assert!((apply(&m, 1920.0, 1080.0).0 - 1.0).abs() < 1e-4);
    }

    /// **回归(成果 124)**:画布必须来自参数。
    ///
    /// 旧实现内部用编译期常量 1920×1080,而本作画布是 1100×825 ⇒ 画面
    /// 只占窗口 `1100/1920 ≈ 57%` 且贴左上(实机截图)。此用例把「窗口
    /// 尺寸 == 画布尺寸」时画布必须**正好铺满**钉死。
    #[test]
    fn canvas_fills_window_when_sizes_match() {
        for (lw, lh) in [
            (1100.0f32, 825.0f32),
            (1280.0, 720.0),
            (800.0, 600.0),
            (1920.0, 1080.0),
        ] {
            let m = letterbox_matrix(lw, lh, lw, lh);
            let (x0, y0) = apply(&m, 0.0, 0.0);
            let (x1, y1) = apply(&m, lw, lh);
            assert!(
                (x0 + 1.0).abs() < 1e-4 && (y0 - 1.0).abs() < 1e-4,
                "画布 {lw}×{lh} 左上角应落在 NDC (-1,1),实为 ({x0},{y0})"
            );
            assert!(
                (x1 - 1.0).abs() < 1e-4 && (y1 + 1.0).abs() < 1e-4,
                "画布 {lw}×{lh} 右下角应落在 NDC (1,-1),实为 ({x1},{y1})"
            );
        }
    }

    /// 回归:旧 bug 下逻辑 (1100,825) 只映射到像素 (630,472);
    /// 现在必须铺满 1100×825 窗口。
    #[test]
    fn canvas_pixel_span_matches_canvas_size() {
        let (lw, lh) = (1100.0f32, 825.0f32);
        let (w, h) = (1100.0f32, 825.0f32);
        let m = letterbox_matrix(lw, lh, w, h);
        let px = |lx: f32, ly: f32| {
            let (nx, ny) = apply(&m, lx, ly);
            ((nx + 1.0) * 0.5 * w, (1.0 - ny) * 0.5 * h)
        };
        let (x, y) = px(lw, lh);
        assert!((x - w).abs() < 0.5, "x 跨度 {x} ≠ 窗口宽 {w}");
        assert!((y - h).abs() < 0.5, "y 跨度 {y} ≠ 窗口高 {h}");
    }

    #[test]
    fn pixel_space_is_isotropic() {
        // 任意画布/窗口比例:逻辑 (dx,dy) 在像素空间的缩放因子相同(不拉伸)
        for (lw, lh) in [(1920.0f32, 1080.0f32), (1100.0, 825.0), (800.0, 600.0)] {
            for (w, h) in [
                (1280.0, 720.0),
                (1920.0, 1440.0),
                (1080.0, 1440.0),
                (2560.0, 1080.0),
            ] {
                let m = letterbox_matrix(lw, lh, w, h);
                let (x0, y0) = apply(&m, 0.0, 0.0);
                let (x1, _y1) = apply(&m, 100.0, 0.0);
                let (_x2, y2) = apply(&m, 0.0, 100.0);
                let px_x = (x1 - x0) * w / 2.0;
                let px_y = (y2 - y0) * h / 2.0;
                // Y 翻转 → 比较幅值
                assert!(
                    (px_x.abs() - px_y.abs()).abs() < 1e-2,
                    "lw={lw} lh={lh} w={w} h={h} px_x={px_x} px_y={px_y}"
                );
            }
        }
    }

    /// 混合态覆盖范围(成果 127):引擎固定管线 8 种模式必须「已实现」;
    /// 其余 11 种(着色器路径,挂号 G11)必须**未实现**(不得近似顶替)。
    #[test]
    fn implemented_blend_modes_match_the_fixed_function_subset() {
        let implemented: Vec<i64> = (0..=18)
            .filter(|v| BlendMode::from_engine(*v).unwrap().is_implemented())
            .collect();
        assert_eq!(
            implemented,
            vec![0, 1, 2, 3, 5, 6, 7, 9],
            "固定管线模式集变了(对照 docs/engine/render/03-blend-modes.md §2)"
        );
    }

    /// **混合态表钉死(成果 127)**:每个已实现模式的 GPU 混合因子/运算必须与
    /// 引擎下发的 D3D9 状态逐通道一致(03 §2 原始序列)。改这张表 = 改渲染
    /// 语义,必须先改文档再改这里。
    #[test]
    fn blend_states_mirror_the_d3d9_state_table() {
        use yuris_scene::BlendMode as B;
        let f = |m: B| {
            let s = blend_state(m);
            (
                s.color.src_factor,
                s.color.dst_factor,
                s.color.operation,
                s.alpha.src_factor,
                s.alpha.dst_factor,
                s.alpha.operation,
            )
        };
        use wgpu::BlendFactor as F;
        use wgpu::BlendOperation as O;
        // (mode, color src/dst/op, alpha src/dst/op) —— 03 §2 的 RS(19/20/171) 序列。
        let cases: [(B, F, F, O, F, F, O); 8] = [
            (B::Normal, F::One, F::OneMinusSrcAlpha, O::Add, F::SrcAlpha, F::OneMinusSrcAlpha, O::Add),
            (B::Add, F::One, F::One, O::Add, F::SrcAlpha, F::One, O::Add),
            (B::Subtract, F::One, F::One, O::ReverseSubtract, F::SrcAlpha, F::One, O::ReverseSubtract),
            (B::Multiply, F::Zero, F::Src, O::Add, F::Zero, F::SrcAlpha, O::Add),
            (B::Screen, F::OneMinusDst, F::One, O::Add, F::OneMinusDstAlpha, F::One, O::Add),
            (B::Lighten, F::One, F::One, O::Max, F::One, F::One, O::Max),
            (B::Darken, F::One, F::One, O::Min, F::One, F::One, O::Min),
            (B::Exclusion, F::OneMinusDst, F::OneMinusSrc, O::Add, F::OneMinusDstAlpha, F::OneMinusSrcAlpha, O::Add),
        ];
        for (m, cs, cd, co, asrc, adst, ao) in cases {
            assert_eq!(
                f(m),
                (cs, cd, co, asrc, adst, ao),
                "MODE {} 的混合态偏离引擎状态表",
                m.to_engine()
            );
        }
    }

    /// 引擎 `YS_FadeVal` 公式(成果 126;.rdata 常量 256/128/1⁄256)。
    ///
    /// TA 大 → 淡入慢(TA=256 时 T=256 才到 ≈0.5?——按公式 FA=256:分母
    /// 256/(1+128)=1.984,fade(256,256)=0.504;TA=16:分母 256/(16+128)=1.778,
    /// fade(256,16)=0.5625)。**注意**:这条公式在 TA=256 时 T=256 仍到不了 1.0
    /// —— 引擎语义就是「TA 大 = 粗步长、淡不满」,与手册「精度」相符;
    /// 对拍若证伪再修(挂号 G11)。
    #[test]
    fn fade_val_matches_binary_formula_shape() {
        let f = yuris_scene::fade_val;
        assert!((f(256.0, 16.0) - 0.5625).abs() < 1e-4);
        assert!((f(256.0, 256.0) - 0.504).abs() < 1e-3);
        assert!(f(0.0, 16.0) == 0.0);
        // TA=0:除零保护(取 1)→ fade(256,1) = 256×(256/1+128)/65536 = 1.5(渲染侧 clamp)
        assert!((f(256.0, 0.0) - 1.5).abs() < 1e-4);
    }
}

impl GraphicsBackend for WgpuBackend {
    fn begin_frame(&mut self) -> Result<()> {
        self.frame_layers.clear();
        Ok(())
    }

    fn draw_layer(&mut self, layer: &Layer) -> Result<()> {
        if self.frame_layers.len() >= MAX_LAYERS {
            return Err(BackendError::Other("frame layer overflow".into()));
        }
        self.frame_layers.push(layer.clone());
        Ok(())
    }

    fn draw_text(&mut self, _text: &TextLayout) -> Result<()> {
        // 台词由播放器光栅化为纹理图层(`PlayerCore::render_text_layer`),
        // 不走本接口 —— 引擎的 YSTCH 字体子系统未实装。
        yuris_log::debug!("draw_text: 文本由播放器图层路径渲染,此处跳过");
        Ok(())
    }

    fn end_frame(&mut self) -> Result<()> {
        let err = |msg: String| BackendError::Other(msg);
        // 图层表由 `draw_layer` 累积;就地取走 —— 组离屏 pass 要 `&mut self`
        // 建目标纹理,与借用 `self.frame_layers` 冲突(成果 134)。
        let mut layers: Vec<Layer> = std::mem::take(&mut self.frame_layers);
        // z 升序、同 z 保持入列顺序(引擎:同优先级按创建序;02-layer-model §4)。
        layers.sort_by_key(|l| l.z);
        // 画布 → NDC 基准矩阵(画布尺寸取运行期 `logical_w/h`;成果 124)。
        let base = self.logical_to_ndc();

        // ---- 0) 图层组的离屏目标(成果 134)—— **尺寸 = 逻辑画布** ----
        //
        // 引擎的组 RT 就是屏幕尺寸:离屏目标由 `FUN_00462e40` 族创建
        // (`CreateTexture(W=[0x80194c], H=[0x801950], … RENDERTARGET)`,
        // 两个全局量 = 屏幕宽高;REA `04-alpha-transition-offscreen.md` §3)。
        // 组层 `/ SX=/SY=` 的语义是**范围遮罩**(手册:「レイヤセットのサイズを
        // 設定することで範囲マスクとしても使えたり」),**不是** RT 尺寸 ——
        // 按 RT 尺寸实现会让子层坐标系整体错位(≈ 把子层当组局部坐标)。
        // 因此目标恒 = 逻辑画布 ⇒ 组的子层 pass 与顶层 pass **共用同一个
        // 基准矩阵**,uniform 可以共享(一次 write_buffer 仍然成立)。
        // SX/SY 的范围遮罩语义未取证到像素级(挂号 G13),此处不近似。
        let mut group_ids: Vec<u64> = Vec::new();
        let push_g = |v: &mut Vec<u64>, id: u64| {
            if !v.contains(&id) {
                v.push(id);
            }
        };
        for l in layers.iter() {
            if l.is_group() {
                push_g(&mut group_ids, l.id);
            }
            // 别名目标(`COPY`/`COPY2`)的**源**也要有合成目标:
            // 源可能是一张普通素材层(不需要 RT,直接采样素材)或另一组。
            if let Some(a) = l.alias_of {
                if layers.iter().any(|x| x.id == a && x.is_group()) {
                    push_g(&mut group_ids, a);
                }
            }
            if let Some(p) = l.parent {
                push_g(&mut group_ids, p);
            }
        }
        let (gw, gh) = (logical_w().max(1.0) as u32, logical_h().max(1.0) as u32);
        for gid in &group_ids {
            self.ensure_group_target(*gid, gw, gh)?;
        }
        // 不再使用的组目标回收(长跑不累积纹理)。
        self.group_targets.retain(|k, _| group_ids.contains(k));

        // ---- 1) 组装本帧全部 uniform(单次 write_buffer;成果 124)----
        self.staging.clear();
        // 一层的绘制任务(槽 = uniform 动态偏移号;`tex` = 纹理来源)。
        struct Draw {
            slot: usize,
            tex: TexSrc,
            blend: BlendMode,
            pattern: Option<u64>,
            mask: Option<u64>,
        }
        let mut draws: Vec<Draw> = Vec::new();
        // 图层下标 → draw 下标(同层在「组 pass」与「顶层 pass」复用它)。
        let mut draw_of: HashMap<usize, usize> = HashMap::new();
        let mut skipped_blend = 0usize;
        let skipped_no_tex = 0usize;
        let mut slot = 0usize;
        for (li, layer) in layers.iter().enumerate() {
            // 纹理来源:别名(COPY/COPY2)> 直接素材 > 组的合成结果。
            // `visible=false` 的组仍要建 RT(子层可能要参与别的组),但不上屏;
            // 这里只决定"本层要不要进 draws"。
            let src: Option<TexSrc> = if let Some(a) = layer.alias_of {
                // 别名源:组 → 采样其 RT;普通素材层 → 直接采样该素材。
                if self.group_targets.contains_key(&a) {
                    Some(TexSrc::Group(a))
                } else {
                    layers
                        .iter()
                        .find(|x| x.id == a)
                        .and_then(|x| x.resource)
                        .filter(|r| self.images.contains_key(&r.0))
                        .map(|r| TexSrc::Image(r.0))
                }
            } else if let Some(res) = layer.resource {
                self.images.contains_key(&res.0).then_some(TexSrc::Image(res.0))
            } else if layer.is_group() {
                Some(TexSrc::Group(layer.id))
            } else {
                None
            };
            let Some(src) = src else { continue };
            if !layer.visible {
                continue;
            }
            // 纹理尺寸:素材取像素;组取 RT(逻辑画布)。
            let size = match src {
                TexSrc::Image(id) => {
                    let Some(img) = self.images.get(&id) else { continue };
                    (img.width as f32, img.height as f32)
                }
                TexSrc::Group(_) => (gw as f32, gh as f32),
            };
            if !layer.blend.is_implemented() {
                // 手册「公式一律以二进制为准」:未取到公式的模式**不近似**。
                skipped_blend += 1;
                continue;
            }
            // 图层变换:逻辑坐标 (x,y) 起,w×scale_x × h×scale_y
            let qw = size.0 * layer.scale_x;
            let qh = size.1 * layer.scale_y;
            let m = mul(&base, &mul(&translate(layer.x, layer.y), &scale(qw, qh)));
            // 过渡(成果 126/127):`fade` = 引擎 `YS_FadeVal = T/(256/(256/TA+128))/256`
            // (scene::fade_val;Likely 待对拍)。**合并规则**:手册 Confirmed
            // 「T=256 = 完成」,而二进制公式在 TA=256 时 T=256 只到 ≈0.504
            // (公式→FadeVal 的槽位指认本身是 Likely,挂号 G11)⇒
            // T 满 = 全显(手册),未满段用引擎 ramp 形状(公式)。
            // 边缘宽度 params.y 取近硬边(**UNVERIFIED**:图案→alpha 曲线未取到)。
            let (pt, soft, has_pat) = match &layer.transition {
                Some(tr) => {
                    let fade = yuris_scene::fade_val(tr.t * 256.0, tr.softness * 256.0);
                    let pt = if tr.t >= 1.0 { 1.0 } else { fade.clamp(0.0, 1.0) };
                    (pt, 2.0 / 256.0, tr.pattern.is_some())
                }
                None => (1.0, 0.0, false),
            };
            let mut u = [0f32; 24];
            for c in 0..4 {
                for r in 0..4 {
                    u[c * 4 + r] = m[c][r];
                }
            }
            u[16..20].copy_from_slice(&[1.0, 1.0, 1.0, layer.alpha.clamp(0.0, 1.0)]);
            u[20..24].copy_from_slice(&[
                pt,
                soft,
                if has_pat { 1.0 } else { 0.0 },
                if layer.mask.is_some() { 1.0 } else { 0.0 },
            ]);
            self.staging
                .extend_from_slice(bytemuck::cast_slice::<f32, u8>(&u));
            // 补齐到 **256B 槽位**(成果 127 根因修复):旧实现只 +16(96→112),
            // 而动态偏移按 256 步进 ⇒ 第 1 层起读到的都是相邻层矩阵碎片。
            let padded = self.staging.len().div_ceil(UNIFORM_STRIDE as usize)
                * UNIFORM_STRIDE as usize;
            self.staging.resize(padded, 0);
            draws.push(Draw {
                slot,
                tex: src,
                blend: layer.blend,
                pattern: layer.transition.as_ref().and_then(|t| t.pattern).map(|r| r.0),
                mask: layer.mask.map(|r| r.0),
            });
            draw_of.insert(li, draws.len() - 1);
            slot += 1;
        }
        if skipped_blend > 0 {
            // **醒目告警(2026-10-17 用户要求)**:未实装混合模式此前只有一行
            // warn —— 每帧刷一次、且不告诉用户是哪种 MODE。现在按模式去重
            // 打横幅:缺哪几个公式一目了然(挂号 G11 / U29)。
            let mut modes: Vec<i64> = layers
                .iter()
                .map(|l| l.blend)
                .filter(|b| !b.is_implemented())
                .map(|b| b.to_engine())
                .collect();
            modes.sort_unstable();
            modes.dedup();
            yuris_log::alert_once(
                module_path!(),
                format!("blend-modes:{modes:?}"),
                format!(
                    "未实装的混合模式,图层已被跳过\n\
                     模式码: {modes:?}(引擎 4/8/10..18 走着色器路径,G11)\n\
                     本帧图层数: {skipped_blend}\n\
                     处置: **不近似**(不落 Normal);这些层当前不上屏"
                ),
            );
        }
        if skipped_no_tex > 0 {
            yuris_log::alert_once(
                module_path!(),
                "no-tex",
                format!(
                    "图层无可用纹理,已跳过\n\
                     本帧图层数: {skipped_no_tex}\n\
                     处置: 检查 CG 的 FILE 槽/资源装载链(缺资源不会静默成背景)"
                ),
            );
        }
        if !self.staging.is_empty() {
            self.queue.write_buffer(&self.uniform_buf, 0, &self.staging);
        }

        // ---- 2) 预热管线 / aux 绑定组 / 纹理绑定组(须在通道之前)----
        //
        // 通道内 `pass` 只借 `encoder`(局部),而 `set_pipeline`/`set_bind_group`
        // 要借 `self.*` —— 通道内建资源就要 `&mut self`,与 `pass` 冲突。
        // 离屏目标现在与素材同为 sRGB(成果 179);色彩空间一致才不会有
        // 半透明叠加偏色。
        let target_format = if self.offscreen_active {
            GROUP_FORMAT
        } else {
            self.color_format
        };
        let mut used_modes: Vec<(BlendMode, wgpu::TextureFormat)> = Vec::new();
        let mut used_aux: Vec<(u64, u64)> = Vec::new();
        // 两种目标格式都要预热:组 pass 恒画 GROUP_FORMAT,顶层画 target_format。
        let formats: Vec<wgpu::TextureFormat> = if group_ids.is_empty() {
            vec![target_format]
        } else if target_format == GROUP_FORMAT {
            vec![target_format]
        } else {
            vec![target_format, GROUP_FORMAT]
        };
        for fmt in formats {
            for d in &draws {
                if !used_modes.contains(&(d.blend, fmt)) {
                    used_modes.push((d.blend, fmt));
                    self.pipeline_for(d.blend, fmt);
                }
            }
        }
        for d in &draws {
            let key = (d.pattern.unwrap_or(0), d.mask.unwrap_or(0));
            if !used_aux.contains(&key) {
                used_aux.push(key);
                self.aux_group(d.pattern, d.mask);
            }
        }
        // 纹理绑定组提前取好(code>通道借用:与 `pass` 同为不可变借用,
        // 但为免混用 `&mut self` 与通道,统一在通道前 clone 出来)。
        let tex_binds: Vec<wgpu::BindGroup> = draws
            .iter()
            .map(|d| match d.tex {
                TexSrc::Image(id) => self.images.get(&id).map(|i| i.bind_group.clone()),
                TexSrc::Group(g) => self.group_targets.get(&g).map(|t| t.bind_group.clone()),
            })
            .collect::<Vec<_>>()
            .into_iter()
            .enumerate()
            .map(|(i, b)| {
                b.unwrap_or_else(|| {
                    // 不该发生:建 draws 时已过滤;兜底用白纹理(可见即知)。
                    let _ = i;
                    self.images
                        .values()
                        .next()
                        .map(|x| x.bind_group.clone())
                        .unwrap_or_else(|| self.aux_group(None, None))
                })
            })
            .collect();
        let aux_binds: Vec<wgpu::BindGroup> = draws
            .iter()
            .map(|d| {
                self.aux_groups
                    .get(&(d.pattern.unwrap_or(0), d.mask.unwrap_or(0)))
                    .cloned()
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();

        // 绘制顺序 = z 升序、同 z 创建序(= `layers` 的排序;见上)。
        // 组内 pass 的次序另按「最深组优先」(子组先合成,父组再采样它)。

        // ---- 3) 录制 ----
        let frame = if self.offscreen_active {
            None
        } else {
            Some(
                self.surface
                    .get_current_texture()
                    .map_err(|e| err(format!("get_current_texture: {e}")))?,
            )
        };
        let view = match &frame {
            Some(f) => f.texture.create_view(&wgpu::TextureViewDescriptor::default()),
            None => self
                .offscreen
                .as_ref()
                .ok_or_else(|| err("离屏目标未创建".into()))?
                .view
                .clone(),
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("yuris-render encoder"),
            });

        // 组 pass 执行次序:嵌套深的先画(父组采样子组的 RT)。
        let mut groups_sorted: Vec<(usize, u64)> = group_ids
            .iter()
            .map(|g| (layer_group_depth(&layers, *g), *g))
            .collect();
        groups_sorted.sort_by(|a, b| b.0.cmp(&a.0));

        let record = |pass: &mut wgpu::RenderPass<'_>,
                      indices: &[usize],
                      uniform_bg: &wgpu::BindGroup,
                      pipes: &HashMap<(i64, wgpu::TextureFormat), wgpu::RenderPipeline>,
                      fmt: wgpu::TextureFormat| {
            let mut last_pipe: Option<i64> = None;
            let mut last_tex: Option<usize> = None;
            let mut last_aux: Option<usize> = None;
            for &di in indices {
                let d = &draws[di];
                let Some(pip) = pipes.get(&(d.blend.to_engine(), fmt)) else { continue };
                if last_pipe != Some(d.blend.to_engine()) {
                    pass.set_pipeline(pip);
                    last_pipe = Some(d.blend.to_engine());
                }
                pass.set_bind_group(
                    0,
                    uniform_bg,
                    &[(d.slot as u32) * UNIFORM_STRIDE as u32],
                );
                if last_tex != Some(di) {
                    pass.set_bind_group(1, &tex_binds[di], &[]);
                    last_tex = Some(di);
                }
                if last_aux != Some(di) {
                    pass.set_bind_group(2, &aux_binds[di], &[]);
                    last_aux = Some(di);
                }
                pass.draw(0..6, 0..1);
            }
        };

        for (_depth, gid) in groups_sorted {
            let Some(gt) = self.group_targets.get(&gid) else { continue };
            let gview = gt.view.clone();
            // 直接子层(draws 序里取自 `layers` 的 z 序)。
            let child: Vec<usize> = layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.parent == Some(gid))
                .filter_map(|(li, _)| draw_of.get(&li).copied())
                .collect();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuris-render group pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gview,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // 透明底:组的合成结果要能被父层按 alpha 叠加。
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_vertex_buffer(0, self.quad_buf.slice(..));
            record(
                &mut pass,
                &child,
                &self.uniform_bg,
                &self.pipelines,
                GROUP_FORMAT,
            );
        }

        // 顶层 pass:不属于任何组的层(组本身作为普通层画,`A`/`MODE` 作用于
        // 子层合成结果 —— 引擎语义,手册「レイヤセット」段)。
        let roots: Vec<usize> = layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.parent.is_none())
            .filter_map(|(li, _)| draw_of.get(&li).copied())
            .collect();
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuris-render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_vertex_buffer(0, self.quad_buf.slice(..));
            record(
                &mut pass,
                &roots,
                &self.uniform_bg,
                &self.pipelines,
                target_format,
            );
        }
        self.queue.submit([encoder.finish()]);
        // 离屏目标不 present(它不是交换链图像;回读走 `readback_offscreen`)。
        if let Some(f) = frame {
            f.present();
        }
        // 归还图层表(诊断读取本帧内容;`begin_frame` 会清)。
        self.frame_layers = layers;
        Ok(())
    }

    fn load_image(&mut self, id: ResourceId, data: &[u8]) -> Result<()> {
        let err = |msg: String| BackendError::Other(msg);
        let img = image::load_from_memory(data)
            .map_err(|e| err(format!("decode image: {e}")))?
            .to_rgba8();
        let (w, h) = img.dimensions();
        self.load_image_rgba(id, &img, w, h)
    }
}

/// 组 `gid` 在**本帧图层表**里的嵌套深度(叶子组 = 1;含组子层 +1)。
///
/// 组 pass 的执行次序必须「深的先画」—— 父组采样子组的合成结果,
/// 子组不先落纹理,父组就会采到上一帧(或空)的内容。
///
/// 带环保护(自引用 `parent`)。
fn layer_group_depth(layers: &[Layer], gid: u64) -> usize {
    fn walk(layers: &[Layer], id: u64, seen: &mut std::collections::HashSet<u64>) -> usize {
        if !seen.insert(id) {
            return 0; // 环:按叶子处理(不猜、不崩)
        }
        let mut best = 1usize;
        for l in layers.iter().filter(|l| l.parent == Some(id)) {
            if l.is_group() {
                best = best.max(1 + walk(layers, l.id, seen));
            }
        }
        best
    }
    walk(layers, gid, &mut std::collections::HashSet::new())
}

/// 列主序矩阵乘法。
fn mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0f32; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            out[c][r] = a[0][r] * b[c][0]
                + a[1][r] * b[c][1]
                + a[2][r] * b[c][2]
                + a[3][r] * b[c][3];
        }
    }
    out
}

/// 平移矩阵(列主序)。
fn translate(x: f32, y: f32) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [x, y, 0.0, 1.0],
    ]
}

/// 缩放矩阵(列主序)。
fn scale(sx: f32, sy: f32) -> [[f32; 4]; 4] {
    [
        [sx, 0.0, 0.0, 0.0],
        [0.0, sy, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

impl WgpuBackend {
    /// 原始 RGBA 上传(P7.3 台词纹理等运行期生成图;非编码数据)。
    pub fn load_image_rgba(
        &mut self,
        id: ResourceId,
        rgba: &[u8],
        w: u32,
        h: u32,
    ) -> Result<()> {
        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuris-render texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba[..],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuris-render texture bind"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        // 覆写同 id 时旧 (图案,遮罩) 绑定组会指向已释放的纹理 ⇒ 清缓存。
        if self.images.contains_key(&id.0) {
            self.aux_groups.clear();
        }
        self.images.insert(
            id.0,
            GpuImage {
                bind_group,
                width: w,
                height: h,
                aux_view: view,
            },
        );
        Ok(())
    }
}

impl WgpuBackend {
    /// 确保(或按需要的尺寸重建)某个**图层组**的离屏合成目标(成果 134)。
    ///
    /// 尺寸恒 = 逻辑画布(见 `end_frame` §0 的依据);因此同一帧里所有组
    /// 目标同尺寸,只有首次进入(或画布变更)时才建纹理。
    fn ensure_group_target(&mut self, id: u64, w: u32, h: u32) -> Result<()> {
        if let Some(t) = self.group_targets.get(&id) {
            if t.width == w && t.height == h {
                return Ok(());
            }
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuris-render group target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GROUP_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuris-render group target bind"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.group_targets.insert(
            id,
            GroupTarget { view, bind_group, width: w, height: h },
        );
        Ok(())
    }

    /// 已建目标的组 id(诊断/测试)。
    pub fn group_target_count(&self) -> usize {
        self.group_targets.len()
    }
}

impl WgpuBackend {
    /// 确保(或按新画布重建)离屏目标 = **逻辑画布分辨率**。
    fn ensure_offscreen(&mut self) -> Result<()> {
        let w = (logical_w() as u32).max(1);
        let h = (logical_h() as u32).max(1);
        if let Some(o) = &self.offscreen {
            if o.width == w && o.height == h {
                return Ok(());
            }
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuris-render offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // **色彩空间必须与素材一致(成果 179)**。
            //
            // 素材纹理是 `Rgba8UnormSrgb`(采样时 GPU 做 sRGB→线性),
            // shader 在线性空间做混合并输出**预乘** `(rgb*a, a)`。
            // 若目标是非 sRGB 的 `Rgba8Unorm`,线性值被**直接写成字节**
            // ⇒ 半透明叠加偏亮/偏色(实机:`--shot` 截出的半透明 UI
            // 呈蓝色泛光,而窗口画面正常 —— 因为 surface 是 sRGB)。
            // 取 sRGB 后,写入时 GPU 做线性→sRGB,回读字节与窗口一致。
            format: GROUP_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // 回读缓冲:bytes_per_row 须 256 对齐(WebGPU copy 约束)。
        let padded = (w * 4).div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuris-render offscreen readback"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        self.offscreen = Some(Offscreen {
            texture,
            view,
            readback,
            padded_bytes_per_row: padded,
            width: w,
            height: h,
        });
        Ok(())
    }

    /// 把整个 Scene 画进离屏目标并回读 RGBA(**逻辑画布分辨率**,无 letterbox)。
    ///
    /// CLI `--shot` 与 parity 对拍的取像管:与引擎同坐标系,截图可逐像素比。
    pub fn render_scene_to_rgba(&mut self, scene: &Scene) -> Result<(Vec<u8>, u32, u32)> {
        self.ensure_offscreen()?;
        self.offscreen_active = true;
        let rendered = (|| -> Result<()> {
            self.begin_frame()?;
            // 与 `render` 同口径:媒体模式屏蔽其他层(T23b)。
            for layer in scene.drawable_layers() {
                self.draw_layer(layer)?;
            }
            self.end_frame()
        })();
        let out = self.readback_offscreen();
        self.offscreen_active = false;
        rendered?;
        out
    }

    /// 回读最近一帧离屏内容(`render_scene_to_rgba` 之后有效)。
    fn readback_offscreen(&mut self) -> Result<(Vec<u8>, u32, u32)> {
        let err = |msg: String| BackendError::Other(msg);
        let (padded, w, h) = {
            let Some(o) = self.offscreen.as_ref() else {
                return Err(err("离屏目标未创建".into()));
            };
            (o.padded_bytes_per_row, o.width, o.height)
        };
        let texture = self.offscreen.as_ref().unwrap().texture.clone();
        let readback = self.offscreen.as_ref().unwrap().readback.clone();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("yuris-render offscreen copy"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let done = self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::Maintain::wait_for(done));
        pollster::block_on(async {
            rx.recv()
                .map_err(|e| err(format!("readback channel: {e}")))?
                .map_err(|e| err(format!("map_async: {e}")))
        })?;
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
        drop(data);
        readback.unmap();
        Ok((out, w, h))
    }
}
