//! yuris-scene
//!
//! L5:Scene / Layer / Animation **纯状态**。不触碰任何平台 API,
//! 不依赖 yuris-runtime(依赖方向见 `docs/02-workspace-design.md` §3.8)。
//!
//! P3 起步(成果 48):落地 [`Scene`] / [`Layer`] / [`TextLayout`] /
//! [`ResourceId`] 数据类型。字段语义分两级:
//! - 结构本身 = 实现选择(渲染对象的容器);
//! - 字段到引擎行为的对应关系 = 映射草案,**标注等级**见
//!   `yuris-vm::bridge`(`Cg` 事件的槽位映射)。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

/// crate 版本(与 workspace 同步)
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 资源句柄:图像/音频等已加载资源的稳定标识。
///
/// 由桥接层从引擎侧标识(如 CG id 字符串)派生;派生策略为实现选择
/// (如 FNV-1a 哈希),非引擎行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceId(pub u64);


/// 混合模式:引擎 `CG[MODE=0..18]` 的 19 种官方命名。
///
/// 名称与取值范围来自官方手册(`cg/cg.html`),见
/// `旁线仓库/docs/render-semantics.md` §1.1。
///
/// ⚠ **公式未定**(同上 §3):RE 尚未取到各模式的混合方程,故本枚举只承载
/// **语义标识**。渲染器不得按 Photoshop 定义自行推导 —— 手册明确要求
/// "公式一律以二进制为准"。未实现的模式由 [`BlendMode::is_implemented`]
/// 显式区分,渲染器遇未实现模式应报错而非近似。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    /// 0 通常(Normal)。
    Normal,
    /// 1 加算(Add)。
    Add,
    /// 2 減算(Subtract)。
    Subtract,
    /// 3 乗算(Multiply)。
    Multiply,
    /// 4 除算(Divide)。
    Divide,
    /// 5 スクリーン(Screen)。
    Screen,
    /// 6 比較(明)(Lighten)。
    Lighten,
    /// 7 比較(暗)(Darken)。
    Darken,
    /// 8 差分(Difference)。
    Difference,
    /// 9 除外(Exclusion)。
    Exclusion,
    /// 10 オーバーレイ(Overlay)。
    Overlay,
    /// 11 覆い焼きカラー(Color Dodge)。
    ColorDodge,
    /// 12 焼き込みカラー(Color Burn)。
    ColorBurn,
    /// 13 焼き込みリニア(Linear Burn)。
    LinearBurn,
    /// 14 ハードライト(Hard Light)。
    HardLight,
    /// 15 ソフトライト(Soft Light)。
    SoftLight,
    /// 16 ビビットライト(Vivid Light)。
    VividLight,
    /// 17 リニアライト(Linear Light)。
    LinearLight,
    /// 18 ピンライト(Pin Light)。
    PinLight,
}

impl Default for BlendMode {
    fn default() -> Self {
        Self::Normal
    }
}

impl BlendMode {
    /// 由引擎 `MODE` 值(0..18)构造。未知值 → `None`(不猜)。
    pub fn from_engine(v: i64) -> Option<Self> {
        Some(match v {
            0 => Self::Normal,
            1 => Self::Add,
            2 => Self::Subtract,
            3 => Self::Multiply,
            4 => Self::Divide,
            5 => Self::Screen,
            6 => Self::Lighten,
            7 => Self::Darken,
            8 => Self::Difference,
            9 => Self::Exclusion,
            10 => Self::Overlay,
            11 => Self::ColorDodge,
            12 => Self::ColorBurn,
            13 => Self::LinearBurn,
            14 => Self::HardLight,
            15 => Self::SoftLight,
            16 => Self::VividLight,
            17 => Self::LinearLight,
            18 => Self::PinLight,
            _ => return None,
        })
    }

    /// 引擎 `MODE` 值。
    pub fn to_engine(self) -> i64 {
        match self {
            Self::Normal => 0,
            Self::Add => 1,
            Self::Subtract => 2,
            Self::Multiply => 3,
            Self::Divide => 4,
            Self::Screen => 5,
            Self::Lighten => 6,
            Self::Darken => 7,
            Self::Difference => 8,
            Self::Exclusion => 9,
            Self::Overlay => 10,
            Self::ColorDodge => 11,
            Self::ColorBurn => 12,
            Self::LinearBurn => 13,
            Self::HardLight => 14,
            Self::SoftLight => 15,
            Self::VividLight => 16,
            Self::LinearLight => 17,
            Self::PinLight => 18,
        }
    }

    /// 该模式是否已被本内核**精确**实现(可由 GPU 混合态逐通道表达)。
    ///
    /// 依据 = SDK 0.495 二进制状态序列(`docs/engine/render/03-blend-modes.md` §2,
    /// 成果 126 反编译实测):引擎对 MODE 0/1/2/3/5/6/7/9 各下发一套
    /// `SRCBLEND/DESTBLEND/BLENDOP` 固定管线状态,wgpu 的 BlendFactor/
    /// BlendOperation 可逐通道镜像 ⇒ **精确**,不是按运算名的近似:
    /// - 0 通常 `SRCALPHA/INVSRCALPHA/ADD`、1 加算 `SRCALPHA/ONE/ADD`、
    ///   2 減算 `SRCALPHA/ONE/REVSUBTRACT`、3 乗算 `ZERO/SRCCOLOR/ADD`、
    ///   5 スクリーン `INVDESTCOLOR/ONE/ADD`、6 比較(明) `ONE/ONE/MAX`、
    ///   7 比較(暗) `ONE/ONE/MIN`、9 除外 `INVDESTCOLOR/INVSRCCOLOR/ADD`。
    ///
    /// 其余(4 除算、8 差分、10..18 ライト族)在引擎里走**像素着色器**路径
    /// (同文档 §4;着色器本体未取到,挂号 G11)⇒ `false`,渲染器遇 `false`
    /// 记录后**跳过**,不做近似(铁律 2)。
    ///
    /// ## 三态清单(T9/T26,2026-10-16 补齐;`docs/engine/render/03-blend-modes.md` §2/§4)
    ///
    /// | MODE | 手册名 | 状态 |
    /// |---|---|---|
    /// | 0 通常 / 1 加算 / 2 減算 / 3 乗算 | | **已实装**(精确状态序列) |
    /// | 4 除算 | | Unsupported(着色器路径,§4) |
    /// | 5 スクリーン / 6 比較(明) / 7 比較(暗) / 9 除外 | | **已实装** |
    /// | 8 差分 | | Unsupported(着色器路径) |
    /// | 10 オーバーレイ … 18 ピンライト(9 档) | | Unsupported |
    ///
    /// 19 档全覆盖:**8 已实装 + 11 `Unsupported`(显式跳过,不近似)**。
    /// 引擎侧 D3D 状态序列对 0..9 已 **Confirmed**(§2 反编译原值);
    /// 10..18 待着色器取证(§4,挂号 G11)。
    pub fn is_implemented(self) -> bool {
        matches!(
            self,
            Self::Normal
                | Self::Add
                | Self::Subtract
                | Self::Multiply
                | Self::Screen
                | Self::Lighten
                | Self::Darken
                | Self::Exclusion
        )
    }

    /// 该 MODE 的**三态归属**(T9/T26 可观测面)。
    ///
    /// - [`BlendState::Implemented`] = 固定管线状态序列已 Confirmed 且 wgpu 逐通道镜像;
    /// - [`BlendState::Unsupported`] = 引擎走像素着色器路径、本体未取证(G11)
    ///   ⇒ 渲染侧**显式跳过**,不做近似(铁律 2)。
    ///
    /// 无 `Unknown` 分支:`MODE` 值域 0..18 已全量枚举 —— 也就是说这里的
    /// `Unsupported` 是**已定性但未实装**,与「行为未知」是两回事。
    pub fn state(self) -> BlendState {
        if self.is_implemented() {
            BlendState::Implemented
        } else {
            BlendState::Unsupported
        }
    }
}

/// 混合模式的三态归属(T9/T26 清单;见 [`BlendMode::state`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendState {
    /// 已实装(精确:wgpu 逐通道镜像引擎下发的状态序列)。
    Implemented,
    /// 引擎侧走着色器路径、本体未取证 ⇒ 渲染侧显式跳过(不近似)。
    Unsupported,
}

/// 引擎不透明度单位:`A` 的取值域是 **0..=256**(不是 0..=255)。
///
/// 出处:官方手册 `cg/cg.html`(见 `render-semantics.md` §1.2)。
/// 渲染器最后一步转归一化 alpha:`a / 256.0`,以保持 1:1。
pub const OPACITY_MAX: i64 = 256;

/// 引擎不透明度 → 归一化 f32(1:1:除以 256,不是 255)。
pub fn opacity_to_unit(a: i64) -> f32 {
    (a.clamp(0, OPACITY_MAX) as f32) / (OPACITY_MAX as f32)
}

/// 引擎变换参数集(`CG` 的几何槽;手册 + `YSCom.ycd` 参数表,§1.2)。
///
/// 字段名即引擎槽名,便于与 `CG`/`CGACT` 的窗口一一对照。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// 位置 X(`X`)。
    pub x: f32,
    /// 位置 Y(`Y`)。
    pub y: f32,
    /// Z 序(`Z`)。
    pub z: i32,
    /// 旋转中心 X(`FX`)。
    pub fx: f32,
    /// 旋转中心 Y(`FY`)。
    pub fy: f32,
    /// 绕 X 轴旋转(`FRX`)。
    pub frx: f32,
    /// 绕 Y 轴旋转(`FRY`)。
    pub fry: f32,
    /// 绕 Z 轴旋转(`FRZ`)。
    pub frz: f32,
    /// 缩放锚点 X(`FCX`)。
    pub fcx: f32,
    /// 缩放锚点 Y(`FCY`)。
    pub fcy: f32,
    /// 缩放 X(引擎侧对应槽未在手册单列;占位)。
    pub scale_x: f32,
    /// 缩放 Y(占位)。
    pub scale_y: f32,
    /// `FSD`(语义 Unknown —— 疑似剪切)。
    pub fsd: f32,
    /// `FMD`(语义 Unknown)。
    pub fmd: f32,
    /// `FQU`(语义 Unknown —— 疑似四分变换)。
    pub fqu: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0,
            fx: 0.0,
            fy: 0.0,
            frx: 0.0,
            fry: 0.0,
            frz: 0.0,
            fcx: 0.0,
            fcy: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            fsd: 0.0,
            fmd: 0.0,
            fqu: 0.0,
        }
    }
}

/// 纹理尺寸基准(引擎 `CG` 的 `SX`/`SY`,见 §1.2「其它已确证的渲染参数」)。
pub const LOGICAL_SIZE: (f32, f32) = (1920.0, 1080.0);

/// 过渡(transition)状态 —— `CG[T=… TID=… TA=…]` 的渲染落点。
///
/// 手册 `cg/cg.html` 关键字表(原文):
/// - `T = 値(0～256)` **トランジション進行度**;
/// - `TID = 文字列` **トランジションレイヤＩＤ**(图案来源);
/// - `TA = 値(0～256)` **トランジション精度**(边缘软化宽度)。
///
/// 即:图层按 `TID` 所指图层的图案、以 `T` 的进度逐像素显现/消隐,
/// `TA` 控制过渡边缘的柔和度。本作实际用法(语料实证,s0 g3019):
/// `CG[ID="BT.OFF"+$v, TID="BTNTR", TA=16, T=<变量>]` —— 标题按钮的
/// 逐帧显现动画(变量在 LOOP 里递增)。
///
/// **二进制公式(成果 126;`docs/engine/render/04-alpha-transition-offscreen.md` §2)**:
/// SDK 绘制函数向 HLSL effect 上传 `YS_FadeVal =
/// T / (256/(256/TA + 128)) × 1/256`(常量 256.0/128.0/1/256 实测于 .rdata
/// `0x58ec08/10/00`)—— `TA` 是**离散步长**而非边缘宽度:TA 越大淡入越慢。
/// 渲染器消费 `fade = 该公式(T,TA)`;图案合成仍取软阈值形状(**UNVERIFIED**:
/// 图案→alpha 的映射曲线未取到,挂号 G11)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// 图案纹理(`TID` 图层的图像;`None` = 无图案 ⇒ 纯 alpha 渐变)。
    pub pattern: Option<ResourceId>,
    /// 进行度 `T/256`:0 = 全隐,1 = 全显。
    pub t: f32,
    /// 精度 `TA/256`:过渡边缘的软化宽度(0 = 硬边)。
    pub softness: f32,
}

/// 引擎 `YS_FadeVal` 公式(成果 126;SDK 绘制函数 HLSL uniform,原式反编译)。
///
/// `T`/`TA` 均为 0..256 域的引擎值;返回 0..1 的淡值。
/// **Likely**:`+0x82`=`T`、`+0x84`=`TA` 的槽位指认与手册对位互证,待对拍转正。
pub fn fade_val(t: f32, ta: f32) -> f32 {
    // fade = T / (256/(256/TA + 128)) / 256;TA=0 时分母退化为 256/128=2(除零保护:取 TA 极小值方向)。
    let ta = if ta > 0.0 { ta } else { 1.0 };
    let denom = 256.0 / (256.0 / ta + 128.0);
    t / denom / 256.0
}

/// 一个可渲染图层(P3 映射草案;字段与引擎槽位的对应关系见桥接层标注)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// 图层标识(桥接层由 CG id 派生;同 id 重复出现 = 更新而非新建)。
    pub id: u64,
    /// z 序(引擎侧 Z 槽)。
    ///
    /// **为何是 i64(成果 132)**:引擎的 Z 是脚本侧任意整数,本作与官方样例
    /// 都用**超大值做分层**(`es.Z.BT = 99000000*@es.ZEX` → z=900000000000010;
    /// 调试层 z=`4551686018427387903` ≈ i64 近顶)。旧实现按 i32 存,
    /// 截断后变**负数** ⇒ 排序把顶层甩到最底 —— 实机「调试网格盖在标题画面上」
    /// 的直接成因之一。排序键必须保真到 i64(REA `02-layer-model.md` §4:
    /// 引擎按 Z 排序、同 Z 按链表序)。
    pub z: i64,
    /// 可见性。
    pub visible: bool,
    /// X 坐标(引擎 SX 槽;Likely)。
    pub x: f32,
    /// Y 坐标(引擎 SY 槽;Likely)。
    pub y: f32,
    /// X 缩放(引擎侧对应槽未定性;占位)。
    pub scale_x: f32,
    /// Y 缩放(占位)。
    pub scale_y: f32,
    /// 不透明度 0.0-1.0(引擎侧对应槽未定性;占位)。
    pub alpha: f32,
    /// 旋转角(引擎 RZ 槽;Likely)。
    pub rotation: f32,
    /// 关联资源(未加载时 None)。
    pub resource: Option<ResourceId>,
    /// 描画模式(`CG[MODE=0..18]`;手册关键字表)。
    #[serde(default)]
    pub blend: BlendMode,
    /// 过渡状态(`CG[T/TID/TA]`;None = 无过渡)。
    #[serde(default)]
    pub transition: Option<Transition>,
    /// 外部遮罩(`CG[MID=…]` = 掩码图层 ID 的图像)。
    #[serde(default)]
    pub mask: Option<ResourceId>,
    /// **所属图层组**(`CG[ID="ABC/"]` 建立的组 id;`None` = 顶层层)。
    ///
    /// 引擎侧依据(REA `docs/engine/render/02-layer-model.md` §2):场景是
    /// 节点链表,组节点有子节点;绘制顺序 = 链表顺序,`Z` 只影响入链位置。
    /// 组 = **先把子层画进组自己的离屏目标,再把组当普通层画** —— 组的
    /// `A`/`MODE`/`E` 作用于「子层合成后的位图」,不下推到子层。
    ///
    /// 命名约定(手册 `ABC/` 示例 + 语料):子层 id = `组id + 子名`
    /// (实测 `:/W1/` 组下有子层 `:/W1/DBG`)。
    #[serde(default)]
    pub parent: Option<u64>,
    /// **本层是组时的合成画布尺寸**(`CG[ID="ABC/" SX=… SY=…]`;手册:
    /// 「レイヤセットのサイズを設定することで範囲マスクとしても使えたり」)。
    /// `None` = 未设定(渲染器回退到逻辑画布)。
    #[serde(default)]
    pub group_size: Option<(f32, f32)>,
    /// **内容来源 = 另一层的渲染结果**(`CGACT[COPY]`/`COPY2`)。
    ///
    /// 手册「スクリーンショット機能」:把源层的合成结果复制给目标层。
    /// 与 [`Layer::resource`] 的区别:`resource` 是封包图像,本字段是
    /// **运行期已合成位图** —— 渲染器据此把源层(含其子树)单独画进离屏
    /// 目标并采样,而不是去找一张不存在的纹理。
    ///
    /// 实测方向(全语料 228722 次真值,成果 134):`ID` = **源**、`ID2` = **目标**
    /// (`CGACT[ID="BT.OFF.TMP" ID2="BT.NAES.CONFIG.…" COPY2=1]` 把模板层的图
    /// 拷到逻辑按钮;`CGACT[ID=":/W0/" ID2="CGS100" COPY2=1]` 把整窗截到 CGS100)。
    #[serde(default)]
    pub alias_of: Option<u64>,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            id: 0,
            z: 0,
            visible: true,
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            alpha: 1.0,
            rotation: 0.0,
            resource: None,
            blend: BlendMode::Normal,
            transition: None,
            mask: None,
            parent: None,
            group_size: None,
            alias_of: None,
        }
    }
}

impl Layer {
    /// 以默认变换创建图层(x/y/z/资源由调用方给定)。
    pub fn new(id: u64, z: i64, x: f32, y: f32, resource: Option<ResourceId>) -> Self {
        Self {
            id,
            z,
            visible: true,
            x,
            y,
            scale_x: 1.0,
            scale_y: 1.0,
            alpha: 1.0,
            rotation: 0.0,
            resource,
            blend: BlendMode::Normal,
            transition: None,
            mask: None,
            parent: None,
            group_size: None,
            alias_of: None,
        }
    }

    /// 本层是否**图层组**(`CG[ID="ABC/"]`;组本身没有纹理,内容是子层合成)。
    ///
    /// 判据 = 桥接层显式设了 [`Layer::group_size`](`CG[ID="ABC/" SX=… SY=…]`
    /// 的尺寸槽;组恒随该槽建立)。**不**按「没有 resource」推断 —— 顶层空壳层
    /// 也会没有 resource,两者语义不同。
    pub fn is_group(&self) -> bool {
        self.group_size.is_some()
    }

    /// 本层是否有可绘制内容(直接纹理 / 别名源的合成结果)。
    pub fn has_content(&self) -> bool {
        self.resource.is_some() || self.alias_of.is_some()
    }
}

/// 一帧文本布局(TEXT 命令的落点;SJIS 原文由上层解码)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextLayout {
    /// SJIS 原始字节(TEXT 事件原样透传,解码交给字体后端)。
    pub sjis: Vec<u8>,
    /// 是否清除既有文本(TEXT 的 let/clear 旗标族;Likely)。
    pub clear: bool,
}

/// 场景快照:图层的有序集合 + 当前文本。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scene {
    /// 图层集合(绘制顺序 = Vec 顺序;z 排序策略归后端)。
    ///
    /// ⚠ 直接改动本 Vec(增删、重排)会与 [`Scene::index`] 失配 ——
    /// 增删请走 [`Scene::upsert_layer`]/[`Scene::hide_layer`]/[`Scene::retain`];
    /// 就地改字段(`scene.layers[i].x = …`)是安全的。
    pub layers: Vec<Layer>,
    /// 当前帧文本(无则 None)。
    pub text: Option<TextLayout>,
    /// `Layer.id → layers 下标`(成果 124)。
    ///
    /// **为什么需要**:VM 每帧产出成千上万条 CG 事件,每条都
    /// `upsert_layer`/`hide_layer`;旧实现每次线性扫描 `layers` ⇒ 图层数
    /// (本作实测 571)与事件数相乘是 O(n²),是实机「卡顿严重」的 CPU 侧
    /// 主项之一。索引把每次查找降到 O(1)。
    ///
    /// 不进序列化:它是派生数据,反序列化后由 [`Scene::reindex`] 重建。
    #[serde(skip)]
    index: std::collections::HashMap<u64, usize>,
    /// **媒体模式**(T23b,2026-10-16):影片/Flash 播放期间**屏蔽其他图层**。
    ///
    /// 引擎语义(官方手册逐字,SDK 0.495):
    /// - `movie/movie.html` 注[3]:「停止しても、**MOVIEEND 命令でムービー再生を
    ///   終了しない限り**他のレイヤは一切表示されません。」
    /// - `movie/movieend.html`:「ムービー再生モードを抜けます。…終了する
    ///   ことにより、**他のレイヤが見えるようになります**。」
    /// - `flash/flash.html`:「FLASHファイル再生モードになると、**他のどの
    ///   レイヤより最前面**になります。」
    ///
    /// 即:进入模式(`MOVIE[PLAY=1]` / `FLASH[… PLAY=1]`)后直到
    /// `MOVIEEND`/`FLASHEND`,**其他图层一概不绘制**;`MOVIE[PLAY=0]`
    /// 只停播放、**不退模式**(手册注[3] 的原话就是"停止しても…不显示")。
    ///
    /// 本内核无视频/Flash 解码器 ⇒ 媒体画面本身画不出(那是解码器的事),
    /// 但**屏蔽语义**照实复制:[`Scene::drawable_layers`] 在置位期间恒返回空。
    /// 于是上层看到的图层集合与引擎在该状态下**一致**(差别仅在媒体表面本身,
    /// 显式 Unknown,不近似)。
    #[serde(default)]
    pub media_mode: bool,
}

impl Scene {
    /// 重建 id→下标 索引(反序列化后 / 外部直接改过 `layers` 之后调用)。
    pub fn reindex(&mut self) {
        self.index.clear();
        self.index.reserve(self.layers.len());
        for (i, l) in self.layers.iter().enumerate() {
            self.index.insert(l.id, i);
        }
    }

    /// 按 id 查图层。
    pub fn layer(&self, id: u64) -> Option<&Layer> {
        self.index.get(&id).and_then(|&i| self.layers.get(i))
    }

    /// 按 id 查图层(可改)。
    pub fn layer_mut(&mut self, id: u64) -> Option<&mut Layer> {
        self.index.get(&id).and_then(|&i| self.layers.get_mut(i))
    }

    /// 按 id upsert 图层:存在则更新字段,不存在则追加。
    pub fn upsert_layer(&mut self, layer: Layer) {
        let id = layer.id;
        if let Some(&i) = self.index.get(&id) {
            if let Some(slot) = self.layers.get_mut(i) {
                *slot = layer;
                return;
            }
            // 索引失配(外部动过 `layers`):自愈重建后再试一次。
            self.reindex();
            if let Some(&i) = self.index.get(&id) {
                if let Some(slot) = self.layers.get_mut(i) {
                    *slot = layer;
                    return;
                }
            }
        }
        self.index.insert(id, self.layers.len());
        self.layers.push(layer);
    }

    /// 按 id 隐藏图层(找不到则无操作)。
    pub fn hide_layer(&mut self, id: u64) {
        if let Some(l) = self.layer_mut(id) {
            l.visible = false;
        }
    }

    // ---- 图层组(レイヤセット;REA `02-layer-model.md` §2)----

    /// 一个组的直接子层(**按 z 升序;同 z 保持创建序** —— 引擎的
    /// 「链表序 + Z 决定入链位置」;稳定排序保证同 z 不乱)。
    ///
    /// 返回下标(不是引用):调用方多半要同时索引别的层,拿下标可绕开借用。
    pub fn children_of(&self, group: u64) -> Vec<usize> {
        let mut idx: Vec<usize> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.parent == Some(group))
            .map(|(i, _)| i)
            .collect();
        idx.sort_by_key(|&i| self.layers[i].z);
        idx
    }

    /// 组的子层里出现过的最大嵌套深度(叶子 = 0)。
    ///
    /// 渲染器据此决定离屏 pass 的**执行次序**:先画最深的组,再画它的父组
    /// (父组把子组的合成结果当普通纹理采样)。带环保护(自引用 id)。
    pub fn group_depth(&self, group: u64) -> usize {
        let mut seen = std::collections::HashSet::new();
        fn walk(s: &Scene, id: u64, seen: &mut std::collections::HashSet<u64>) -> usize {
            if !seen.insert(id) {
                return 0; // 自引用 / 环:按叶子处理(不猜、不崩)
            }
            let mut best = 0usize;
            for i in s.children_of(id) {
                let cid = s.layers[i].id;
                let d = if s.layers[i].is_group() { 1 + walk(s, cid, seen) } else { 1 };
                best = best.max(d);
            }
            best
        }
        walk(self, group, &mut seen)
    }

    /// 场景里全部的组 id(按 z 升序,稳定)。
    pub fn groups(&self) -> Vec<u64> {
        let mut v: Vec<(i64, u64)> = self
            .layers
            .iter()
            .filter(|l| l.is_group())
            .map(|l| (l.z, l.id))
            .collect();
        v.sort_by_key(|(z, _)| *z);
        v.into_iter().map(|(_, id)| id).collect()
    }

    /// 顶层(不属于任何组)的层下标,按 `z` 升序稳定排序。
    pub fn roots(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.parent.is_none())
            .map(|(i, _)| i)
            .collect();
        idx.sort_by_key(|&i| self.layers[i].z);
        idx
    }

    /// **应绘制的图层**(渲染入口的唯一口径;T23b)。
    ///
    /// 与直接遍历 `layers` 的区别:媒体模式置位([`Scene::media_mode`])时
    /// **返回空** —— 引擎在 `MOVIE`/`FLASH` 播放期间屏蔽其他所有层
    /// (手册原文见该字段文档)。渲染器与离屏回读都经本方法取图层,
    /// 保证"屏蔽"只有一处实现。
    ///
    /// 未置位时 = `layers` 中 `visible` 的子序列(顺序即绘制顺序)。
    pub fn drawable_layers(&self) -> impl Iterator<Item = &Layer> {
        let masked = self.media_mode;
        self.layers
            .iter()
            .filter(move |l| !masked && l.visible)
    }

    /// 媒体模式置位/清除。返回是否**发生变化**(供调用方决定要不要刷新)。
    ///
    /// 语义见 [`Scene::media_mode`]:`MOVIE[PLAY=1]`/`FLASH[… PLAY=1]` 置位,
    /// `MOVIEEND`/`FLASHEND` 清除;`PLAY=0` **不动**该状态(手册注[3])。
    pub fn set_media_mode(&mut self, on: bool) -> bool {
        let changed = self.media_mode != on;
        self.media_mode = on;
        changed
    }
}

#[cfg(test)]
mod blend_state_tests {
    use super::{BlendMode, BlendState};

    /// **T9/T26 三态清单**:MODE 0..18 共 19 档,**8 已实装 + 11 Unsupported**,
    /// 且**无缺口**(每档都有明确归属,不存在未定性的值)。
    #[test]
    fn every_mode_has_exactly_one_state() {
        let mut implemented = Vec::new();
        let mut unsupported = Vec::new();
        for v in 0..=18i64 {
            let m = BlendMode::from_engine(v).unwrap_or_else(|| panic!("MODE {v} 应有枚举"));
            match m.state() {
                BlendState::Implemented => implemented.push(v),
                BlendState::Unsupported => unsupported.push(v),
            }
            assert_eq!(m.is_implemented(), m.state() == BlendState::Implemented);
        }
        assert_eq!(
            implemented,
            vec![0, 1, 2, 3, 5, 6, 7, 9],
            "已实装档位 = 引擎固定管线状态序列已 Confirmed 的 8 档"
        );
        assert_eq!(
            unsupported,
            vec![4, 8, 10, 11, 12, 13, 14, 15, 16, 17, 18],
            "Unsupported = 4 除算 / 8 差分 / 10..18 ライト族(着色器路径,G11)"
        );
        assert_eq!(implemented.len() + unsupported.len(), 19);
    }

    /// 值域外不得枚举(不猜):`MODE=19` 与负数 → `None`。
    #[test]
    fn out_of_range_modes_are_none() {
        assert_eq!(BlendMode::from_engine(19), None);
        assert_eq!(BlendMode::from_engine(-1), None);
    }
}

#[cfg(test)]
mod media_mode_tests {
    use super::{Layer, Scene};

    fn visible_layer(id: u64, z: i64) -> Layer {
        let mut l = Layer::new(id, z, 0.0, 0.0, None);
        l.visible = true;
        l
    }

    /// **T23b 核心**:媒体模式置位 → 所有图层停止绘制;清除 → 恢复。
    ///
    /// 引擎语义(官方手册逐字):`movie/movie.html` 注[3]「停止しても、
    /// MOVIEEND 命令でムービー再生を終了しない限り他のレイヤは一切表示され
    /// ません」;`movieend.html`「終了することにより、他のレイヤが見える
    /// ようになります」。
    #[test]
    fn media_mode_masks_and_restores_every_layer() {
        let mut s = Scene::default();
        s.upsert_layer(visible_layer(0xA, 0));
        s.upsert_layer(visible_layer(0xB, 1));
        assert_eq!(s.drawable_layers().count(), 2, "未置位时全部可绘制");

        assert!(s.set_media_mode(true), "首次置位应报告变化");
        assert_eq!(
            s.drawable_layers().count(),
            0,
            "媒体模式置位 ⇒ 其他图层一概不绘制(手册语义)"
        );
        // 幂等:重复置位不报变化(上层据此避免无谓重绘)
        assert!(!s.set_media_mode(true), "重复置位不应报告变化");

        assert!(s.set_media_mode(false), "清除应报告变化");
        assert_eq!(s.drawable_layers().count(), 2, "退出媒体模式 ⇒ 其他层重新可见");
        assert!(!s.set_media_mode(false), "重复清除不应报告变化");
    }

    /// 屏蔽**不改变 `visible` 字段本身** —— 与引擎的"持久对象"一致:
    /// 退出媒体模式后原样恢复,不需要重新注册图层。
    #[test]
    fn media_mode_does_not_mutate_layer_visibility() {
        let mut s = Scene::default();
        s.upsert_layer(visible_layer(0xC, 0));
        s.set_media_mode(true);
        assert!(
            s.layers.iter().all(|l| l.visible),
            "屏蔽是绘制口径,不是把 visible 改掉"
        );
        s.set_media_mode(false);
        assert_eq!(s.drawable_layers().count(), 1);
    }

    /// 未置位时 `drawable_layers` 与旧口径(`visible` 过滤)等价。
    #[test]
    fn drawable_layers_matches_visible_filter_when_inactive() {
        let mut s = Scene::default();
        s.upsert_layer(visible_layer(0xD, 0));
        let mut hidden = visible_layer(0xE, 1);
        hidden.visible = false;
        s.upsert_layer(hidden);
        let got = s.drawable_layers().count();
        let want = s.layers.iter().filter(|l| l.visible).count();
        assert_eq!(got, want);
        assert_eq!(got, 1);
    }
}
