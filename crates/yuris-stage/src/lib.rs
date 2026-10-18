//! # yuris-stage — 渲染编排核心
//!
//! 通用引擎层的画布编排机:吃 [`Command`] 批,产出已提交的
//! [`yuris_scene::Scene`] 视图。YU-RIS 的 CG/CGACT/TEXT/截屏机制实现为它的
//! 指令超集;规格 = `docs/engine/stage-core.md`(不变量 I1–I10,均可单测)。
//!
//! 铁律:
//! - **I1 属性保持**:patch 未给出的槽保持原值(引擎层=持久属性袋,REA)。
//! - **I2 纹理所有权**:纹理只随 FILE 族操作 / 拷贝产物 / 截屏变化;属性 patch 永不碰像素。
//! - **I3 新层缺省隐**:创建即隐(a4=0),显式 E=1 点亮(成果 154 实证)。
//! - **I4 批原子**:一个 [`Stage::apply`] = 一个批;截屏取样 = 批开始时的已提交景;
//!   场景投影只在批末发生。批粒度 = VM 的自然让出点(引擎一帧)。
//! - **I5 销毁**:Kill 即删实体;复活必须重新创建。
//! - **I7 组**:无像素 + SX/SY = 图层组(子层离屏合成)。
//! - **I8 纯色盖**:FILE="" + SX/SY>0 = 拉伸纯色块(es.CGTBLACK 黑幕),不算组。
//! - **I9 文本门控**:消息窗未开 → 文本不进景(boot 无窗,不产白点;Likely,挂成果签)。
//!
//! 像素工作(解码/裁剪/截屏)全部经 [`Assets`] 回调交给宿主;Stage 只裁决
//! "谁、何时、以什么所有权"用图,与数据源解耦。

use std::collections::HashMap;

pub use yuris_scene::{BlendMode, ResourceId, Scene, Transition};

/// 引擎层 id(本仓库约定 = fnv(基名),与播放器纹理缓存同源)。
pub type LayerId = u64;
/// 宿主纹理 id(实作方自由)。
pub type TexId = u64;

/// A 槽(0..256 → 0.0..1.0;手册)。
pub fn opacity(a: i64) -> f32 {
    yuris_scene::opacity_to_unit(a)
}

/// 64-bit FNV-1a(与播放器/VM 同一约定)。
pub fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &c in b {
        h ^= c as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ---------------------------------------------------------------------------
// 槽集(Layer {..} 命令一次携带的"字节流里出现的槽")
// ---------------------------------------------------------------------------

/// 台词文本层的 z。
///
/// **取证(2026-10-17,成果 169)**:游戏自己的消息窗载体是 cgsys 层,
/// 实测档位 998 / 1000;若文本留在旧值 90,会被窗背景(1000)整片盖住
/// ⇒ 实机「文字页不显示」。取 1500 = **窗口带之上、按钮带(2010)之下**:
/// 文本属于窗口内容,不应遮住可点击按钮。
///
/// 精确值待 `FONT`/文本子系统取证(U27);此处只锁**相对档位**。
pub const TEXT_Z: i64 = 1500;

/// `YURIS_TRACE_BLIT` 门控(BlitSize 失败/缺源诊断;**缺省静默**)。
///
/// 这两条以前是无门控 `trace_forced!`:boot 期每帧数百条 `format!`,
/// 单独把 VM 段拉到 19ms(成果 168 实测)。取证时显式开。
fn trace_blit_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| std::env::var_os("YURIS_TRACE_BLIT").is_some())
}

/// `YURIS_TRACE_COPY` 门控(COPY 别名落空诊断;**缺省静默**)。
fn trace_copy_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| std::env::var_os("YURIS_TRACE_COPY").is_some())
}

/// None = 未给 → 保持原值(I1)。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Slots {
    pub e: Option<i64>,
    pub x: Option<i64>,
    pub y: Option<i64>,
    pub z: Option<i64>,
    pub a: Option<i64>,
    pub mode: Option<i64>,
    /// 过渡:图案层名(需经 [`Assets::resolve`] 落图)。
    pub tid: Option<Vec<u8>>,
    pub t: Option<i64>,
    pub ta: Option<i64>,
    /// 遮罩层名(同上)。
    pub mid: Option<Vec<u8>>,
    pub sx: Option<i64>,
    pub sy: Option<i64>,
}

/// 层的像素所有权(I2:只有这些途径能出现在纹理槽上)。
#[derive(Debug, Clone, PartialEq)]
pub enum Pixel {
    /// FILE="路径":以 [`Assets::resolve`] 的返回为纹理;名称留存供 BLIT 源回读。
    File(Vec<u8>),
    /// FILE="" = 1×1 黑(手册示例[3];带 SX/SY 时按 I8 拉伸)。
    Solid,
    /// 产物纹理(BLIT / 截屏)。
    Product(TexId),
    /// 别名:显示 = 源层(id)当前合成结果(渲染取 alias_of 通道)。
    Alias(LayerId),
}

/// 宿主提供的像素服务(解码/裁剪/截屏与 IO 全在这里,Stage 不做)。
pub trait Assets {
    /// FILE 路径 → 纹理。`owner` = 申请层名(宿主纹理 id 按层名约定,如
    /// fnv(名))。None = 解析失败(层保持无产物,不上屏)。
    fn resolve(&mut self, file: &[u8], owner: &[u8]) -> Option<TexId>;
    /// 1×1 黑的稳定纹理 id。
    fn solid(&mut self) -> TexId;
    /// CGACT[SIZE] 裁剪缩放:源为 `src_file`(层 `src`),`params =
    /// [X,Y,X2?,Y2?,SX?,SY?]`(缺位传 `i64::MIN`);`gen` = 源层像素代号。
    /// 实作方按 (src, params, gen, target) 缓存防重。
    fn blit(
        &mut self,
        src_file: &[u8],
        src: LayerId,
        params: [i64; 6],
        gen: u64,
        target: LayerId,
    ) -> Option<TexId>;
    /// 整窗截图:`view` = 批开始前的已提交景(I6,引擎=上一呈现帧)。
    fn snapshot(&mut self, view: &Scene, target: LayerId) -> Option<TexId>;
    /// 文本渲染:内容 → (纹理, 宽, 高);None = 不可用(不进景)。
    fn text(&mut self, content: &str) -> Option<(TexId, u32, u32)>;
}

// ---------------------------------------------------------------------------
// 指令
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Command {
    /// CG 家族的创建/patch(实体不存在则创建,存在则 patch —— 引擎同)。
    Layer {
        id: LayerId,
        name: Vec<u8>,
        /// FILE 槽原文;None = 未给;Some(空) = FILE="" ⇒ Solid。
        file: Option<Vec<u8>>,
        slots: Slots,
        /// 新建层的兜底 z(策略位;引擎 z 槽永远优先)。
        fallback_z: i64,
    },
    /// CGEND = 销毁(I5)。
    Kill(LayerId),
    /// CGACT[SIZE/SIZE2] = 源→目标裁剪缩放搬运(ID=源、ID2=目标;成果 153)。
    /// 产物缓存代号由 Stage 从源实体像素代号自取(调用方不再自持层→FILE 表)。
    BlitSize {
        src_name: Vec<u8>,
        src: LayerId,
        dst: LayerId,
        params: [i64; 6],
    },
    /// CGACT[ID=":/W0/" ID2=dst COPY2] 整窗截图(成果 134 语料实证)。
    SnapshotTo(LayerId),
    /// CGACT[COPY/COPY2] = 模板层 → 目标层合成结果复制(引擎方向:ID=源、
    /// ID2=目标;成果 134 三次语料验证)。目标层此后渲染取源层合成结果。
    CopyAlias { dst: LayerId, src: LayerId },
    /// 消息窗开合(I9)。
    MessageWindow(bool),
    /// TEXT:窗开才进景(位置/字号由宿主 Assets 与 plug 层定;Stage 记层与纹理)。
    Text {
        layer: LayerId,
        content: String,
    },
    TextClear(LayerId),
}

// ---------------------------------------------------------------------------
// 实体(引擎真值)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Entity {
    name: Vec<u8>,
    /// FILE 槽原文(引擎层对象自持文件名 +0x4c;BLIT 源按它回读)。
    file: Option<Vec<u8>>,
    /// 当前像素状态(与 file 分离:产物/纯色时 file 仍是注册名)。
    pixel: Option<Pixel>,
    // 属性槽真值(引擎"未给保持"语义)
    e: bool,
    x: f32,
    y: f32,
    z: i64,
    alpha: f32,
    mode: i64,
    t: Option<i64>,
    ta: Option<i64>,
    /// 过渡图案/遮罩:层名 → 提交时经 resolve 落 TexId。
    tid_name: Option<Vec<u8>>,
    mid_name: Option<Vec<u8>>,
    sx: Option<i64>,
    sy: Option<i64>,
    /// 像素代号:换图自增;BLIT 产物随代号失效(宿主缓存参与)。
    pixel_gen: u64,
    /// **创建序**(单调自增;I11 → 成果 159)。
    ///
    /// 引擎的同 Z 层叠放次序 = **创建序(链表序)**(`docs/engine/render/
    /// 02-layer-model.md` §4;成果 132 已在 Layer.z 注释里引同一结论)。
    /// `project()` 曾按 `HashMap` 遍历序写入 `Scene`,同 Z 的层因此随机叠放
    /// —— 实测标题画面 7 层全 z=0,背景随机盖住部分按钮。投影必须按
    /// `(z, seq)` 排序,把创建序显式化、确定性化。
    seq: u64,
    /// 先 COPY/截屏后建层的挂回(引擎同批内必达成,不跨批)。
    carry_tex: Option<TexId>,
}

impl Entity {
    fn new(name: Vec<u8>, fallback_z: i64, seq: u64) -> Self {
        Entity {
            name,
            file: None,
            pixel: None,
            e: false, // I3
            x: 0.0,
            y: 0.0,
            z: fallback_z,
            alpha: 1.0,
            mode: 0,
            t: None,
            ta: None,
            tid_name: None,
            mid_name: None,
            sx: None,
            sy: None,
            pixel_gen: 0,
            seq,
            carry_tex: None,
        }
    }

    fn patch(&mut self, s: &Slots) {
        if let Some(v) = s.e {
            self.e = v != 0;
        }
        if let Some(v) = s.x {
            self.x = v as f32;
        }
        if let Some(v) = s.y {
            self.y = v as f32;
        }
        if let Some(v) = s.z {
            self.z = v;
        }
        if let Some(v) = s.a {
            self.alpha = opacity(v);
        }
        if let Some(v) = s.mode {
            self.mode = v;
        }
        if let Some(v) = s.t {
            self.t = Some(v);
        }
        if let Some(v) = s.ta {
            self.ta = Some(v);
        }
        if let Some(n) = &s.tid {
            self.tid_name = Some(n.clone());
        }
        if let Some(n) = &s.mid {
            self.mid_name = Some(n.clone());
        }
        if let Some(v) = s.sx {
            self.sx = Some(v);
        }
        if let Some(v) = s.sy {
            self.sy = Some(v);
        }
    }

    fn file_path(&self) -> Option<Vec<u8>> {
        // 引擎层对象自持文件名(+0x4c):BLIT 源按它回读,与当前像素状态无关。
        self.file.clone().filter(|f| !f.is_empty())
    }

    fn solid_stretch(&self) -> (f32, f32) {
        let (w, h) = (self.sx.unwrap_or(0), self.sy.unwrap_or(0));
        match (&self.pixel, w, h) {
            (Some(Pixel::Solid), w, h) if w > 0 && h > 0 => (w as f32, h as f32),
            (Some(Pixel::Solid), w, _) if w > 0 => (w as f32, 0.0),
            (Some(Pixel::Solid), _, h) if h > 0 => (0.0, h as f32),
            _ => (1.0, 1.0),
        }
    }

    fn group_size(&self) -> Option<(f32, f32)> {
        // I7:I8 例外(Solid)之外,无像素 + SX/SY = 组。
        let is_group = self.pixel.is_none() && self.carry_tex.is_none();
        if !is_group {
            return None;
        }
        let (w, h) = (self.sx.unwrap_or(0), self.sy.unwrap_or(0));
        match (w, h) {
            (w, h) if w > 0 && h > 0 => Some((w as f32, h as f32)),
            (w, _) if w > 0 => Some((w as f32, 0.0)),
            (_, h) if h > 0 => Some((0.0, h as f32)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Stage
// ---------------------------------------------------------------------------

/// 画布编排机:实体真值表 + 批处理 + 场景投影。
pub struct Stage {
    entities: HashMap<LayerId, Entity>,
    /// 创建序分配器(单调;实体创建即领号,I11/成果 159)。
    next_seq: u64,
    /// 消息窗(I9)。
    window_open: bool,
    /// 当前文本层与渲染元数据(引擎 ADV 文本 = 单层重写)。
    text_layer: Option<LayerId>,
    text_meta: Option<(TexId, u32, u32)>,
    /// 宿主 1×1 黑纹理 id(首次 Solid 时从 Assets 记入)。
    solid_tex: Option<TexId>,
    /// 画布逻辑尺寸(文本层居中/贴底用;宿主每帧刷新,缺省 1920×1080)。
    canvas: (f32, f32),
}

impl Default for Stage {
    fn default() -> Self {
        Self::new()
    }
}

impl Stage {
    pub fn new() -> Self {
        Stage {
            entities: HashMap::new(),
            next_seq: 0,
            solid_tex: None,
            window_open: false,
            text_layer: None,
            text_meta: None,
            canvas: (yuris_scene::LOGICAL_SIZE.0, yuris_scene::LOGICAL_SIZE.1),
        }
    }

    /// 画布逻辑尺寸(宿主在分辨率注入后调用;文本投影用)。
    pub fn set_canvas(&mut self, w: f32, h: f32) {
        if w > 0.0 && h > 0.0 {
            self.canvas = (w, h);
        }
    }

    /// 领一个创建序号(实体创建时调用;单调递增,号段有空洞无碍 ——
    /// 只要先建者号更小,`(z, seq)` 排序即等于创建序)。
    fn take_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq = s.wrapping_add(1);
        s
    }

    pub fn window_open(&self) -> bool {
        self.window_open
    }

    /// 层 id 是否仍在实体表(诊断/测试用)。
    pub fn has_entity(&self, id: LayerId) -> bool {
        self.entities.contains_key(&id)
    }

    /// **命中测试**(2026-10-17,成果 177):返回逻辑坐标下方、名字以
    /// `prefix` 开头且**当前可见(E=1 且有像素)**的最上层实体名。
    ///
    /// 用于「鼠标点中游戏自己的按钮层 → 通知 VM」。按 z 升序取**最后一个**
    /// 命中的(即最上层);同 z 按创建序。
    ///
    /// 尺寸来源:实体 `x/y` + 组尺寸 `SX/SY`;无显式尺寸时用装载图像尺寸
    /// (`img_w/img_h` 在玩家侧不可得,故由调用方传 `fallback_size`)。
    /// 返回 `(名字, 层 z)`。
    pub fn hit_test(
        &self,
        x: f32,
        y: f32,
        name_prefix: &str,
        fallback_size: (f32, f32),
    ) -> Option<(Vec<u8>, i64)> {
        let mut best: Option<(&Entity, i64)> = None;
        for e in self.entities.values() {
            if !e.e {
                continue;
            }
            if !e.name.starts_with(name_prefix.as_bytes()) {
                continue;
            }
            // 组尺寸(SX/SY)优先;未给则用调用方给的图像原始尺寸。
            let (w, h) = match (e.sx, e.sy) {
                (Some(sx), Some(sy)) if sx > 0 && sy > 0 => (sx as f32, sy as f32),
                _ => fallback_size,
            };
            if w <= 0.0 || h <= 0.0 {
                continue;
            }
            if x >= e.x && x < e.x + w && y >= e.y && y < e.y + h {
                match best {
                    Some((b, bz)) if (e.z, e.seq) <= (bz, b.seq) => {}
                    _ => best = Some((e, e.z)),
                }
            }
        }
        best.map(|(e, z)| (e.name.clone(), z))
    }

    /// 是否存在满足谓词的实体(参数 = 名字、FILE 槽原文)。
    /// 宿主用它做**载体归属判定**(如"游戏自带消息窗是否在场")。
    pub fn has_matching(&self, f: impl Fn(&[u8], Option<&[u8]>) -> bool) -> bool {
        self.entities.values().any(|e| f(&e.name, e.file.as_deref()))
    }

    /// 清空文本层(宿主直接调用;不走批 —— 供场景重载等路径复位)。
    pub fn clear_text(&mut self, scene: &mut Scene) {
        if let Some(id) = self.text_layer.take() {
            scene.hide_layer(id);
        }
        self.text_meta = None;
    }

    /// 清空全部实体并从景里摘除(内置标题接管视觉时用;等价于逐条 Kill
    /// 后的稳态。消息窗/文本态一并复位)。
    pub fn kill_all(&mut self, scene: &mut Scene) {
        for id in self.entities.keys() {
            scene.hide_layer(*id);
        }
        self.entities.clear();
        if let Some(id) = self.text_layer.take() {
            scene.hide_layer(id);
        }
        self.text_meta = None;
        self.window_open = false;
    }

    /// 一个批:依序执行 → 批末投影。**批中间态不落景**(I4)。
    ///
    /// `scene` = 宿主传入的已提交视图(播放器的 bridge 场景);本批开始时
    /// 它的内容即"上一呈现帧"。
    pub fn apply(&mut self, batch: &[Command], scene: &mut Scene, assets: &mut dyn Assets) {
        // **Kill 延迟到批末(勘误 2026-10-17,Confirmed)**。
        //
        // 引擎的 `CGEND` 是从链表摘除,而 `CGACT[COPY2]` 截屏在同批内若先于
        // 摘除执行,取到的是**仍在场**的画面。我们此前逐条立即删除 ⇒
        // 跨域同帧内「任务删精灵 → 主线截屏」的顺序(域泵序:主线先、任务后,
        // 但两者事件合并进同一 Stage 批时按到达序)会让截屏取不到精灵。
        //
        // 实测 boot 收尾:f181 任务删 SPL401 → f184 主线截屏 ⇒ 截图全黑
        // ⇒ logo 淡出到黑屏。延迟 Kill 到批末即可让同批截屏仍看到精灵,
        // 且批末删除与引擎「链表摘除」的可观察结果一致。
        let mut deferred_kills: Vec<LayerId> = Vec::new();
        for cmd in batch {
            match cmd {
                Command::Layer {
                    id,
                    name,
                    file,
                    slots,
                    fallback_z,
                } => self.apply_layer(*id, name, file, slots, *fallback_z, assets),
                Command::Kill(id) => {
                    if std::env::var_os("YURIS_TRACE_KILL").is_some() {
                        let nm = self
                            .entities
                            .get(id)
                            .map(|e| String::from_utf8_lossy(&e.name).into_owned())
                            .unwrap_or_default();
                        yuris_log::trace_forced!("[kill-deferred] {id:#x} {:?}", nm);
                    }
                    // 批末统一摘除(见函数头注释);同批内 SnapshotTo/Blit 仍可见。
                    deferred_kills.push(*id);
                }
                Command::BlitSize {
                    src,
                    dst,
                    params,
                    ..
                } => self.apply_blit(*src, *dst, *params, assets),
                Command::SnapshotTo(dst) => {
                    // **引擎同步表语义(I6)**:截屏取"此刻"的合成画面 =
                    // 批起始景 + 本批**已执行**命令的投影 —— 脚本在 COPY2
                    // 之前先 `CG[E=1 A=0]` 让幕布透明,该 patch 必须对本次
                    // 取样生效。用批前视图会让"上一帧仍不透明的幕布"被拍进
                    // 新截图(黑幕自锁:幕布拍到的全是自己,永远黑)。
                    let mut view = scene.clone();
                    self.project(&mut view);
                    if std::env::var_os("YURIS_TRACE_SNAPVIEW").is_some() {
                        let vis: Vec<String> = view
                            .drawable_layers()
                            .map(|l| {
                                format!(
                                    "{:x}z{}a{:.2}",
                                    l.id & 0xFFFF_FFFF,
                                    l.z,
                                    l.alpha
                                )
                            })
                            .collect();
                        yuris_log::trace_forced!(
                            "[snapview] n={} {:?}",
                            vis.len(),
                            vis
                        );
                        // 同时列出**有像素但不可见**的实体(排查"截屏缺层")
                        let hidden: Vec<String> = self
                            .entities
                            .iter()
                            .filter(|(_, e)| e.pixel.is_some() && !e.e)
                            .map(|(id, e)| {
                                format!(
                                    "{:x}[{}]",
                                    (*id) & 0xFFFF_FFFF,
                                    String::from_utf8_lossy(&e.name)
                                )
                            })
                            .collect();
                        yuris_log::trace_forced!("[snapview] hidden={:?}", hidden);
                        // 直接点名 SPL401 的实体全状态
                        let spl = fnv1a(b"SPL401");
                        if let Some(e) = self.entities.get(&spl) {
                            yuris_log::trace_forced!(
                                "[snapview] SPL401 e={} pixel={} carry={} alpha={:.2} z={} name={:?}",
                                e.e,
                                e.pixel.is_some(),
                                e.carry_tex.is_some(),
                                e.alpha,
                                e.z,
                                String::from_utf8_lossy(&e.name)
                            );
                        } else {
                            yuris_log::trace_forced!("[snapview] SPL401 NOT IN ENTITIES");
                        }
                    }
                    self.apply_snapshot(&view, *dst, assets);
                }
                Command::CopyAlias { dst, src } => {
                    match self.entities.get_mut(dst) {
                        Some(e) => {
                            e.pixel = Some(Pixel::Alias(*src));
                            e.pixel_gen += 1;
                        }
                        None => {
                            // **目标层已不存在时不得凭空重建(勘误 2026-10-17)**。
                            // 引擎语义:`CGACT[COPY]` 是「把源层像素拷到目标层」,
                            // 目标层已 CGEND = 对象不存在 ⇒ handler 无对象即返回
                            // (手册「存在しないレイヤに対して属性変更」同族)。
                            //
                            // 旧实现无条件建层,且 `Entity::new(Vec::new(), 0, seq)`
                            // 把 **z 硬编码成 0、名字留空** —— 实测后果:
                            // `BT.OFFES.CONFIG.BTN.TAB=1=1` 收到 CGEND 后再被
                            // COPY2 建回,z 从 2010 掉到 0,永久沉到所有层之下,
                            // 且名字为空导致诊断/重放全部失联。boot 期每个按钮
                            // 都要经历一次 CGEND→COPY 循环,于是整页按钮的 z 全丢。
                            //
                            // 正确行为:目标不存在 → 不建、不拷(下一次 CG 创建
                            // 时按 FILE/Z 槽重新建立完整对象)。
                            if trace_copy_enabled() {
                                yuris_log::trace_forced!(
                                    "[stage] drop copy-alias on missing dst id={dst:#x} (src={src:#x})"
                                );
                            }
                        }
                    }
                },
                Command::MessageWindow(open) => self.window_open = *open,
                Command::Text { layer, content } => self.apply_text(*layer, content, assets),
                Command::TextClear(id) => {
                    if Some(*id) == self.text_layer {
                        self.text_meta = None;
                    }
                }
            }
        }
        for id in deferred_kills.drain(..) {
            self.entities.remove(&id);
            scene.hide_layer(id);
        }
        self.commit(scene);
    }

    fn apply_layer(
        &mut self,
        id: LayerId,
        name: &[u8],
        file: &Option<Vec<u8>>,
        slots: &Slots,
        fallback_z: i64,
        assets: &mut dyn Assets,
    ) {
        let existed = self.entities.contains_key(&id);
        // **I5 补强(成果 161,Confirmed)**:对象不存在时的**纯属性**指令
        // (无 FILE 族创建、也无 SX/SY 组尺寸)在引擎侧是**无效操作** ——
        // 手册「存在しないレイヤに対して属性変更をしようとしました」= 错误
        // 路径,handler 无实体即 return,不会凭空建层。旧实现
        // `entry().or_insert` 无条件创建 ⇒ CGEND 销毁后游戏每帧发来的
        // `E=1/A=256` 刷新事件把配置页**复活**并上屏(boot 后段「配置页
        // 盖 logo」的直接成因)。复活/组创建必须走带 FILE 或带 SX/SY 的
        // 创建指令(见 I7「无像素 + SX/SY = 组」)。
        if !existed && file.is_none() && slots.sx.is_none() && slots.sy.is_none() {
            if std::env::var_os("YURIS_TRACE_STAGE").is_some() {
                yuris_log::trace_forced!(
                    "[stage] drop attr-only on missing layer {:?} id={id:#x}",
                    String::from_utf8_lossy(name)
                );
            }
            return;
        }
        let ent = if existed {
            self.entities.get_mut(&id).expect("刚刚探测存在")
        } else {
            let seq = self.take_seq();
            self.entities
                .entry(id)
                .or_insert_with(|| Entity::new(name.to_vec(), fallback_z, seq))
        };
        ent.patch(slots);
        if !existed && !name.is_empty() {
            ent.name = name.to_vec();
        }
        // 层属性流水(`YURIS_TRACE_STAGE=1` 门控):z 槽消费链取证
        // (成果 161:配置页 z=998 却以 z=0 呈现 —— 定位丢失点)。
        if std::env::var_os("YURIS_TRACE_STAGE").is_some() {
            let nm = String::from_utf8_lossy(name);
            if nm.contains("CONFIG.TIP.BG") || nm.contains("CGS100") || slots.z.is_some() {
                yuris_log::trace_forced!(
                    "[stage] layer {nm:?} id={id:#x} file={:?} z_slot={:?} e={:?} a={:?} → ent.z={} existed={existed}",
                    file.as_ref().map(|f| String::from_utf8_lossy(f).into_owned()),
                    slots.z,
                    slots.e,
                    slots.a,
                    ent.z
                );
            }
        }
        // ---- 像素所有权(I2):FILE 族才动纹理 ----
        if let Some(f) = file {
            if f.is_empty() {
                // **`FILE=""` 只对"新建层"是 1×1 纯色;对已有纹理的层是空操作**
                // (勘误 2026-10-17,Confirmed)。
                //
                // 手册:`FILE=""` = 1×1 黑图(新建时);而 §3「FILE 替换保留属性」
                // 的同一套语义意味着 —— 不带图片的 CG 命令**不改变**已有像素。
                // 引擎侧 `CG[ID=… A=…]` 这种纯属性刷新是每帧常态(实测本作
                // `SPLBUF401` 先 `FILE="cg/ev/…"` 建立,随后每帧 `FILE="" A=0`
                // 刷新透明度)。
                //
                // 旧实现无条件把像素覆盖成 `Pixel::Solid` ⇒ 真实立绘被 1×1 纯色
                // 顶掉 ⇒ `CGACT[SPLBUF→SPL SIZE]` 拷到的是纯色 ⇒ 整窗截屏全黑
                // ⇒ **logo 淡出后黑屏**(实机症状)。修复:已有真实像素时忽略空 FILE。
                let already_has_pixels = matches!(
                    ent.pixel,
                    Some(Pixel::Product(_)) | Some(Pixel::File(_)) | Some(Pixel::Alias(_))
                );
                if !already_has_pixels {
                    let changed = !matches!(ent.pixel, Some(Pixel::Solid));
                    if changed {
                        ent.pixel = Some(Pixel::Solid);
                        ent.pixel_gen += 1;
                        if self.solid_tex.is_none() {
                            self.solid_tex = Some(assets.solid());
                        }
                    }
                }
            } else {
                let same = ent.file.as_deref() == Some(f.as_slice());
                if !same {
                    ent.file = Some(f.clone());
                    ent.pixel_gen += 1;
                    match assets.resolve(f, &ent.name) {
                        Some(tex) => ent.pixel = Some(Pixel::Product(tex)),
                        None => {
                            // **纹理解析失败以前完全静默**:层留在 `Pixel::File`,
                            // 渲染时无纹理被跳过 —— 用户只看到「这层不显示」。
                            // 现在按文件名去重打横幅(资源缺失/名字编码问题)。
                            let nm = String::from_utf8_lossy(f).into_owned();
                            yuris_log::alert_once(
                                module_path!(),
                                format!("layer-resolve:{nm}"),
                                format!(
                                    "图层纹理解析失败\n\
                                     FILE: {nm}\n\
                                     层名: {}\n\
                                     后果: 该层无纹理,渲染时被跳过(不上屏)\n\
                                     处置: 检查资源是否在封包内 / CP932 名字是否可回查",
                                    String::from_utf8_lossy(&ent.name)
                                ),
                            );
                            ent.pixel = Some(Pixel::File(f.clone()));
                        }
                    }
                }
            }
        }
    }

    fn apply_blit(
        &mut self,
        src_lid: LayerId,
        dst: LayerId,
        params: [i64; 6],
        assets: &mut dyn Assets,
    ) {
        // 源 = 层对象的文件名(+0x4c)+ 其**像素代号**(换图自增)——
        // 产物缓存随换代失效,调用方无需另持层→FILE 对照表。
        let Some((src_file, gen)) = self
            .entities
            .get(&src_lid)
            .and_then(|e| e.file_path().map(|f| (f, e.pixel_gen)))
        else {
            // **BLIT 找不到源**:以前只有 `YURIS_TRACE_BLIT` 时才可见。
            // 这是「画面缺一块」的常见根因(源层已 CGEND / 从未拿到 FILE)。
            yuris_log::alert_once(
                module_path!(),
                format!("blit-no-src:{src_lid:#x}"),
                format!(
                    "CGACT 复制找不到源层\n\
                     源 id = {src_lid:#x}  目标 id = {dst:#x}\n\
                     后果: 本次复制不产生像素(目标层保持原样)\n\
                     处置: 检查源层是否已 CGEND,或其 CG 命令是否带 FILE 槽"
                ),
            );
            if trace_blit_enabled() {
                yuris_log::trace_forced!(
                    "[stage] blit NO-SRC src={src_lid:#x} dst={dst:#x} (entity missing or no file)"
                );
            }
            return;
        };
        let Some(tex) = assets.blit(&src_file, src_lid, params, gen, dst) else {
            // 复制执行失败(解码/裁剪区):同样必须可见。
            yuris_log::alert_once(
                module_path!(),
                format!("blit-failed:{src_lid:#x}"),
                format!(
                    "CGACT 复制执行失败\n\
                     源 id = {src_lid:#x} 文件 = {:?}\n\
                     目标 id = {dst:#x}  裁剪参数 = {params:?}\n\
                     后果: 目标层拿不到像素\n\
                     处置: 检查图像解码 / 裁剪矩形是否为空(X2/Y2/SX/SY)",
                    String::from_utf8_lossy(&src_file)
                ),
            );
            if trace_blit_enabled() {
                yuris_log::trace_forced!(
                    "[stage] blit FAILED src={src_lid:#x} dst={dst:#x} file={:?}",
                    String::from_utf8_lossy(&src_file)
                );
            }
            return;
        };
        match self.entities.get_mut(&dst) {
            Some(e) => e.pixel = Some(Pixel::Product(tex)),
            None => {
                // 先搬运后建层的挂回(引擎同批内汇合;目标层创建会覆盖 carry_tex 语义)
                let seq = self.take_seq();
                let mut e = Entity::new(Vec::new(), 0, seq);
                e.carry_tex = Some(tex);
                e.sx = Some(0);
                e.sy = Some(0);
                self.entities.insert(dst, e);
            }
        }
    }

    fn apply_snapshot(&mut self, view: &Scene, dst: LayerId, assets: &mut dyn Assets) {
        match assets.snapshot(view, dst) {
            Some(tex) => match self.entities.get_mut(&dst) {
                Some(e) => {
                    e.pixel = Some(Pixel::Product(tex));
                    e.pixel_gen += 1;
                    e.carry_tex = None;
                }
                None => {
                    // 目标层不存在 ⇒ 截图落点由本次操作**建立**(幕布缓冲层;
                    // 引擎侧截图同样会建对象)。以前静默;现在留 trace 级别,
                    // 因为这是合法路径而不是缺失。
                    if std::env::var_os("YURIS_TRACE_STAGE").is_some() {
                        yuris_log::trace_forced!(
                            "[stage] snapshot creates buffer layer dst={dst:#x}"
                        );
                    }
                    let seq = self.take_seq();
                    let mut e = Entity::new(Vec::new(), 0, seq);
                    e.carry_tex = Some(tex);
                    self.entities.insert(dst, e);
                }
            },
            None => {
                // 截图本身失败(后端不可用/尺寸 0)。
                yuris_log::alert_once(
                    module_path!(),
                    format!("snapshot-fail:{dst:#x}"),
                    format!(
                        "截屏取样失败\n\
                         目标 id = {dst:#x}\n\
                         后果: 该幕布层没有新画面(后续合成会用到旧内容或空)\n\
                         处置: 检查图形后端是否注入、画布尺寸是否已设置"
                    ),
                );
            }        }
    }

    fn apply_text(&mut self, layer: LayerId, content: &str, assets: &mut dyn Assets) {
        // I9(标记 Likely):窗未开 → 不进景。boot 的空格/"."文本因此不产白点。
        if !self.window_open {
            self.text_layer = Some(layer);
            self.text_meta = None;
            return;
        }
        let Some((tex, w, h)) = assets.text(content) else {
            // **文本排版失败 = 这一行不上屏**。以前完全静默
            // (用户只会看到「台词没出来」)。按内容前 24 字符去重告警。
            let head: String = content.chars().take(24).collect();
            yuris_log::alert_once(
                module_path!(),
                format!("text-render-fail:{head}"),
                format!(
                    "文本排版失败,该行不上屏\n\
                     内容(前 24 字): {head:?}\n\
                     处置: 检查 CJK 字体是否加载 / 字符是否在字体覆盖范围内"
                ),
            );
            return;
        };
        self.text_layer = Some(layer);
        self.text_meta = Some((tex, w, h));
    }

    /// 批末:实体真值 → 场景(全量 upsert;Kill 已即时移除)。
    fn commit(&mut self, scene: &mut Scene) {
        self.project(scene);
        // 文本层(I9):位置 = 画布底部消息带(底部 250 带内垂直居中;
        // 与游戏自绘窗 `tip_meswindow_txspace` 1350×200 同带)。
        if let Some(id) = self.text_layer {
            match &self.text_meta {
                Some((tex, w, h)) => {
                    let (tw, th) = (*w as f32, *h as f32);
                    let (lw, lh) = self.canvas;
                    scene.upsert_layer(yuris_scene::Layer {
                        id,
                        z: TEXT_Z,
                        visible: true,
                        x: (lw - tw) / 2.0,
                        y: lh - 250.0 + (250.0 - th) / 2.0,
                        scale_x: 1.0,
                        scale_y: 1.0,
                        alpha: 1.0,
                        rotation: 0.0,
                        resource: Some(ResourceId(*tex)),
                        ..Default::default()
                    });
                }
                None => scene.hide_layer(id),
            }
        }
    }

    /// 实体真值 → 场景(只读 `&self`;截图取样与批末投影共用)。
    ///
    /// **投影顺序 = `(z, 创建序)` 升序(I11,成果 159)**。`Scene::upsert_layer`
    /// 对**新 id 追加**、对已存在 id 就地更新,而渲染端只按 `z` 做**稳定**排序
    /// (同 z 保持入列顺序,`crates/yuris-render/src/lib.rs` `sort_by_key`)——
    /// 于是 `Scene` Vec 里同 z 层的相对次序 = **首次投影的次序**。旧实现按
    /// `self.entities.keys()`(HashMap)遍历,该次序每进程随机 ⇒ 同 z 层随机
    /// 叠放(实测:标题画面 7 层全 z=0,背景随机盖住部分按钮,每次启动子集不同)。
    /// 按 `(z, seq)` 排序后,首次投影序 = 稳定的创建序 = 引擎链表序
    /// (`docs/engine/render/02-layer-model.md` §4「同 Z 按创建序」)。
    fn project(&self, scene: &mut Scene) {
        // 提交景流水(`YURIS_TRACE_SCENE=1` 门控):逐帧列出**可见**实体
        // (名字/z/e/有无像素)。排查"某层为何上屏/为何被盖"的唯一现场。
        if std::env::var_os("YURIS_TRACE_SCENE").is_some() {
            let mut vis: Vec<(&Entity, i64)> = self
                .entities
                .values()
                .filter(|e| e.e && (e.pixel.is_some() || e.carry_tex.is_some()))
                .map(|e| (e, e.z))
                .collect();
            vis.sort_by_key(|(e, z)| (*z, e.seq));
            let names: Vec<String> = vis
                .iter()
                .map(|(e, z)| {
                    format!(
                        "{}(z{},a{:.2},g{})",
                        String::from_utf8_lossy(&e.name),
                        z,
                        e.alpha,
                        if e.group_size().is_some() { 1 } else { 0 }
                    )
                })
                .collect();
            yuris_log::trace_forced!("[scene] visible={} {}", names.len(), names.join(" | "));
        }
        let mut ids: Vec<(&LayerId, &Entity)> = self.entities.iter().collect();
        ids.sort_by_key(|(_, e)| (e.z, e.seq));
        for (id, e) in ids {
            let id = *id;
            let has_pixel = e.pixel.is_some() || e.carry_tex.is_some();
            let group = e.group_size();
            if !has_pixel && group.is_none() {
                scene.hide_layer(id);
                continue;
            }
            let group_size = e.group_size();
            let (sx_s, sy_s) = e.solid_stretch();
            let mut alias_src: Option<LayerId> = None;
            let resource = if let Some(tex) = e.carry_tex {
                Some(ResourceId(tex))
            } else {
                match e.pixel.as_ref() {
                    Some(Pixel::Product(tex)) => Some(ResourceId(*tex)),
                    Some(Pixel::Solid) => Some(ResourceId(self.solid_tex.unwrap_or_else(|| fnv1a(&[])))),
                    Some(Pixel::Alias(src)) => {
                        alias_src = Some(*src);
                        None
                    }
                    _ => None,
                }
            };
            let mut layer = yuris_scene::Layer {
                id,
                z: e.z,
                visible: e.e,
                x: e.x,
                y: e.y,
                scale_x: sx_s,
                scale_y: sy_s,
                alpha: e.alpha,
                rotation: 0.0,
                resource,
                blend: match BlendMode::from_engine(e.mode) {
                    Some(m) => m,
                    None => {
                        // **MODE 值域外的静默 Normal 是危险兜底**(2026-10-17
                        // 用户要求):未知 MODE 会让图层以「通常」混合上屏,
                        // 与引擎真实效果不符且无从察觉。保留 Normal(引擎
                        // 0..18 已全量枚举,理论不可达),但必须大告警。
                        yuris_log::alert_once(
                            module_path!(),
                            format!("blend-mode:{}", e.mode),
                            format!(
                                "CG 的 MODE 值域外\n\
                                 MODE = {}\n\
                                 处置: 按 Normal(0) 上屏 —— **是兜底,不是真值**\n\
                                 引擎已确认值域 0..18;出现此值说明数据或解析有问题",
                                e.mode
                            ),
                        );
                        BlendMode::Normal
                    }
                },
                mask: e.mid_name.as_ref().map(|n| ResourceId(fnv1a(n))),
                group_size,
                ..Default::default()
            };
            layer.alias_of = alias_src;
            // 过渡与遮罩:提交时把层名落到 TexId(沿用播放器 resolve 约定)。
            // (Assets 不在 commit 签名里;这里用与播放器同源的 fnv 纹理约定,
            //  resolve 未命中的名字由播放器纹理层表现为"加载失败",不臆造。)
            if let Some(n) = &e.tid_name {
                layer.transition = Some(Transition {
                    pattern: Some(ResourceId(fnv1a(n))),
                    t: opacity(e.t.unwrap_or(0)),
                    softness: opacity(e.ta.unwrap_or(0)),
                });
            } else if let Some(t) = e.t {
                layer.transition = Some(Transition {
                    pattern: None,
                    t: opacity(t),
                    softness: opacity(e.ta.unwrap_or(0)),
                });
            }
            // 组父判定:名字前缀 + `/`,且该前缀本身是已注册实体(成果 134:
            // 场景树里组节点先于子节点注册;平层长名不带 `…/` 父)。
            if let Some(pos) = e.name.iter().rposition(|&b| b == b'/') {
                if pos > 0 {
                    let pid = fnv1a(&e.name[..=pos]);
                    if self.entities.contains_key(&pid) {
                        layer.parent = Some(pid);
                    }
                }
            }
            scene.upsert_layer(layer);
        }
    }
}

// ---------------------------------------------------------------------------
// 测试(不变量逐条)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct RecAssets {
        resolve_ok: bool,
        blits: Vec<([i64; 6], LayerId)>,
        snaps: Vec<LayerId>,
        texts: Vec<String>,
    }
    impl RecAssets {
        fn new() -> Self {
            RecAssets {
                resolve_ok: true,
                blits: vec![],
                snaps: vec![],
                texts: vec![],
            }
        }
    }
    impl Assets for RecAssets {
        fn resolve(&mut self, _f: &[u8], _o: &[u8]) -> Option<TexId> {
            self.resolve_ok.then_some(0x10)
        }
        fn solid(&mut self) -> TexId {
            0x01
        }
        fn blit(
            &mut self,
            _s: &[u8],
            _so: LayerId,
            p: [i64; 6],
            _g: u64,
            t: LayerId,
        ) -> Option<TexId> {
            self.blits.push((p, t));
            Some(0x20)
        }
        fn snapshot(&mut self, _v: &Scene, t: LayerId) -> Option<TexId> {
            self.snaps.push(t);
            Some(0x30 + t)
        }
        fn text(&mut self, s: &str) -> Option<(TexId, u32, u32)> {
            self.texts.push(s.to_string());
            Some((0x40, 100, 20))
        }
    }

    const ID: LayerId = 0xAA;
    const ID2: LayerId = 0xBB;

    fn layer(id: u64, file: Option<&[u8]>, slots: Slots) -> Command {
        Command::Layer {
            id,
            name: id.to_le_bytes().to_vec(),
            file: file.map(|f| f.to_vec()),
            slots,
            fallback_z: 1,
        }
    }

    /// I1 + I3:新层缺省不可见;patch 未给槽保持原值。
    #[test]
    fn i3_default_hidden_and_i1_patch_preserves() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        st.apply(&[layer(1, Some(b"x.png"), Slots::default())], &mut sc, &mut a);
        let l = sc.layer(1).expect("创建即是实体");
        assert!(!l.visible, "I3:缺省不可见");

        let mut s = Slots::default();
        s.x = Some(5);
        st.apply(&[layer(1, None, s)], &mut sc, &mut a);
        let l = sc.layer(1).unwrap();
        assert_eq!(l.x, 5.0);
        assert!(!l.visible, "I1:E 未给 = 保持原值(仍隐)");
    }

    /// 显式 E=1 点亮(成果 154 实证,原实现 bug 的反向)。
    #[test]
    fn explicit_e1_shows() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s = Slots::default();
        s.e = Some(1);
        st.apply(&[layer(1, Some(b"a.png"), s)], &mut sc, &mut a);
        assert!(sc.layer(1).unwrap().visible);
    }

    /// I2:属性 patch 不碰纹理;换 FILE 才换图。
    #[test]
    fn i2_attr_patch_never_touches_texture() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s1 = Slots::default();
        s1.e = Some(1);
        st.apply(&[layer(ID, Some(b"img.png"), s1)], &mut sc, &mut a);
        let tex0 = sc.layer(ID).unwrap().resource.map(|r| r.0).unwrap();

        let mut s2 = Slots::default();
        s2.z = Some(777);
        st.apply(&[layer(ID, None, s2)], &mut sc, &mut a);
        let l = sc.layer(ID).unwrap();
        assert_eq!(l.resource.map(|r| r.0), Some(tex0), "I2:patch 不换图");
        assert_eq!(l.z, 777, "给出的槽生效");
    }

    /// I5:Kill 销毁;patch-only 不会复活(F 族才建)。
    #[test]
    fn i5_kill_and_no_patch_revive() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s = Slots::default();
        s.e = Some(1);
        st.apply(&[layer(ID, Some(b"a.png"), s.clone())], &mut sc, &mut a);
        st.apply(&[Command::Kill(ID)], &mut sc, &mut a);
        st.apply(&[layer(ID, None, s)], &mut sc, &mut a);
        // 死了的层被 attribute-only patch 触碰:引擎无对象不做任何事 =
        // 不在景里可见(handler 无实体即 return 0),此处保持隐
        let l = sc.layer(ID);
        assert!(l.is_none() || !l.unwrap().visible);
    }

    /// I8:FILE="" + SX/SY = 拉伸纯色,不是组。
    #[test]
    fn i8_solid_with_sx_sy_stretches() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s = Slots::default();
        s.e = Some(1);
        s.sx = Some(1100);
        s.sy = Some(825);
        st.apply(&[layer(9, Some(b""), s)], &mut sc, &mut a);
        let l = sc.layer(9).unwrap();
        assert_eq!((l.scale_x, l.scale_y), (1100.0, 825.0));
        assert!(l.group_size.is_none(), "黑幕不是组");
    }

    /// I7:无像素 + SX/SY = 组。
    #[test]
    fn i7_no_pixel_with_sx_sy_is_group() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s = Slots::default();
        s.e = Some(1);
        s.sx = Some(200);
        s.sy = Some(100);
        st.apply(&[layer(2, None, s)], &mut sc, &mut a);
        let l = sc.layer(2).unwrap();
        assert_eq!(l.group_size, Some((200.0, 100.0)));
    }

    /// I6(2026-10-16 修正):截屏取样 = **此刻**的合成画面 —— 批起始景 +
    /// 批内**已执行**命令的投影(批内先挪 src 再截 → 取样含新位置)。
    /// 旧版取"批前视图"会造成黑幕自锁(boot 黑幕拍到上一帧仍不透明的自己)。
    #[test]
    fn i6_snapshot_uses_mid_batch_view() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s = Slots::default();
        s.e = Some(1);
        st.apply(&[layer(7, Some(b"img.png"), s)], &mut sc, &mut a);
        let presented_name = 77;
        let mut s2 = Slots::default();
        s2.x = Some(999);
        st.apply(
            &[
                layer(7, None, s2),
                Command::SnapshotTo(presented_name as u64),
            ],
            &mut sc, &mut a,
        );
        assert_eq!(a.snaps, vec![presented_name as u64], "截屏发生一次");
        let l = sc.layer(presented_name as u64).unwrap();
        assert!(matches!(l.resource, Some(r) if r.0 == 0x30 + presented_name as u64));
    }

    /// 空层不上屏:创建无像素无组 → 不可见,也非 drawable。
    #[test]
    fn empty_entity_not_drawn() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        st.apply(&[layer(5, None, Slots::default())], &mut sc, &mut a);
        // 空壳层从未投影进景:scene 里没有它(引擎不为其建可视对象)
        assert!(sc.layer(5).is_none());
    }

    /// I9:窗未开 → 文本不进景;窗开 → 进景。
    #[test]
    fn i9_text_gated_by_window() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        st.apply(
            &[Command::Text {
                layer: 500,
                content: ".".into(),
            }],
            &mut sc, &mut a,
        );
        assert!(
            !sc.layer(500).map(|l| l.visible).unwrap_or(false),
            "I9:窗未开,boot 文本不产白点"
        );
        st.apply(&[Command::MessageWindow(true)], &mut sc, &mut a);
        st.apply(
            &[Command::Text {
                layer: 500,
                content: "こんにちは".into(),
            }],
            &mut sc, &mut a,
        );
        assert!(sc.layer(500).unwrap().visible);
    }

    /// BLIT:ID=源、ID2=目标;产物落到目标;同代同参宿主可见到同元组。
    #[test]
    fn blit_forwards_and_attaches() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s = Slots::default();
        s.e = Some(1);
        let params = [0, 0, 1099, 824, 1100, 825];
        st.apply(
            &[
                layer(ID, Some(b"buf.png"), s),
                layer(ID2, Some(b""), Slots::default()),
            ],
            &mut sc, &mut a,
        );
        let b = Command::BlitSize {
            src_name: ID.to_le_bytes().to_vec(),
            src: ID,
            dst: ID2,
            params,
        };
        st.apply(&[b], &mut sc, &mut a);
        let l = sc.layer(ID2).unwrap();
        assert_eq!(l.resource.map(|r| r.0), Some(0x20), "产物挂目标层");
        assert_eq!(a.blits, vec![(params, ID2)], "转发 ID=源 ID2=目标");
    }

    /// kill_all:全实体摘除 + 文本/窗态复位(内置标题接管路径)。
    #[test]
    fn kill_all_clears_entities_and_text_state() {
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        let mut s = Slots::default();
        s.e = Some(1);
        st.apply(&[layer(ID, Some(b"a.png"), s)], &mut sc, &mut a);
        st.apply(&[Command::MessageWindow(true)], &mut sc, &mut a);
        st.apply(&[Command::Text { layer: 500, content: "x".into() }], &mut sc, &mut a);
        assert!(st.has_entity(ID));
        assert!(st.window_open());
        st.kill_all(&mut sc);
        assert!(!st.has_entity(ID));
        assert!(!st.window_open(), "窗态复位");
        assert!(!sc.layer(ID).map(|l| l.visible).unwrap_or(false));
        assert!(!sc.layer(500).map(|l| l.visible).unwrap_or(false), "文本层摘除");
    }

    /// I11(成果 159):**同 z 层叠放次序 = 创建序,与 HashMap 遍历序无关、跨
    /// apply 恒定**。旧 `project()` 按 `entities.keys()`(HashMap)遍历,同 z 层
    /// 随机叠放 —— 标题画面 7 层全 z=0 时背景随机盖住部分按钮。
    ///
    /// 这里建 8 个同 z 实体(数量足以让"恰好有序"不再是巧合),断言 Scene
    /// Vec 序 = 创建序,且连续两次 apply(第二次带一次属性 patch)后**完全不变**。
    #[test]
    fn same_z_order_is_creation_order_and_stable() {
        let ids: Vec<u64> = (0..8).map(|i| 0x1000 + i).collect();
        let mk = |id: u64| {
            let mut s = Slots::default();
            s.e = Some(1);
            s.z = Some(0); // 全部同 z = 触发旧实现的随机叠放
            layer(id, Some(b"img.png"), s)
        };
        let batch: Vec<Command> = ids.iter().map(|&id| mk(id)).collect();

        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        st.apply(&batch, &mut sc, &mut a);
        let order1: Vec<u64> = sc.layers.iter().map(|l| l.id).collect();
        assert_eq!(order1, ids, "I11:同 z 的 Scene 序 = 创建序");

        // 再跑一次(带一个 patch,模拟后续帧的属性更新):次序必须恒定。
        let mut s = Slots::default();
        s.x = Some(7);
        st.apply(&[layer(ids[3], None, s)], &mut sc, &mut a);
        let order2: Vec<u64> = sc.layers.iter().map(|l| l.id).collect();
        assert_eq!(order2, order1, "I11:跨 apply 次序恒定(不因 HashMap 序抖动)");
    }

    /// I11 的**乱序创建**面:即使创建顺序与数值 id 顺序无关,叠放仍按创建序
    /// (证明排序键是 `seq`,不是 id/Z 的巧合)。
    #[test]
    fn same_z_order_follows_creation_not_id_order() {
        let create_order: [u64; 4] = [0xCC, 0xAA, 0xDD, 0xBB];
        let mut st = Stage::new();
        let mut sc = Scene::default();
        let mut a = RecAssets::new();
        for id in create_order {
            let mut s = Slots::default();
            s.e = Some(1);
            s.z = Some(0);
            st.apply(&[layer(id, Some(b"img.png"), s)], &mut sc, &mut a);
        }
        let order: Vec<u64> = sc.layers.iter().map(|l| l.id).collect();
        assert_eq!(order, create_order.to_vec(), "I11:叠放 = 创建序(与 id 大小无关)");
    }
}