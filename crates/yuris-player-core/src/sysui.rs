//! 播放器**内置兜底 UI** 的标定 profile(集中地)。
//!
//! # 定性(先读这个)
//!
//! 本模块全部常量都是**播放器侧兜底实现**的取值,**不是引擎常量**:
//!
//! - 本引擎的正规 UI 数据源是游戏自己的系统剧本(es.BT.* 子程序)经 VM
//!   CG 通道下发的层与按钮注册(见 `docs/in-game-ui-plan.md`);es.BT
//!   尚未接管前,标题/LOAD/EXTRA 鉴赏/END 确认等画面由播放器自绘兜底
//!   (PROGRESS 成果 76–83)。
//! - 因此这里的**素材名**取自标定样本系统剧本的字面量(es.BT.CG.SET /
//!   es.BT.SE.SET 等,或按其运行期拼串规律的产物),**坐标/容量**是对照
//!   实机截图与底图的实测标定。它们**随作品可变**:换了游戏,名字与
//!   布局都可能对不上 —— 届时应换掉/覆写本表,而不是把新作品的名字再
//!   抄散进逻辑代码。
//! - **层 id 命名空间(`0x5C_xxxx_xxxx`)是播放器自造的**,与游戏脚本
//!   无关(游戏层的 id = `fnv1a(层名)`,不会落在该段)。当前划分:
//!   `0x5C_0000_xxxx` 合成小资源(白板/黑板)、`0x5C_7000_*` 标题画面、
//!   `0x5C_A000_*` 图集裁剪、`0x5C_8000_*` LOAD 屏、`0x5C_9000_*` ADV
//!   主按钮行 + EXTRA 入口、`0x5C_B000_*` CG 鉴赏、`0x5C_D000_*` BGM
//!   鉴赏、`0x5C_E000_*` END 确认。
//! - 长期方向 = es.BT 系统链接管这些画面后**整表删除**;短期价值 =
//!   集中一处、逐项留痕(证据 = 成果编号),换皮/数据化时只动本模块。

/// 按钮三态下标(素材后缀 `_off`/`_on`/`_over`;成果 78 图像实证)。
pub const TITLE_BTN_OFF: usize = 0;
/// 悬停/聚焦高亮态。
pub const TITLE_BTN_ON: usize = 1;
/// 按下态。
pub const TITLE_BTN_OVER: usize = 2;

/// 标题按钮悬停音(sysse/sse02)。es.BT.SE.SET 第一参数全语料 444 组恒定,
/// 时长 0.086s 显著短促(其余 0.30~0.39s);成果 80。
pub const TITLE_SE_HOVER: &str = "sse02";
/// 标题按钮决定音(sysse/sse03)。es.BT.SE.SET 第二参数(普通按钮 316 组);
/// BACK 型取消钮为 sse06(128 组)—— 参数 2 随按钮语境变化,标题六钮均普通型。
pub const TITLE_SE_DECIDE: &str = "sse03";
/// 取消/戻る音(sysse/sse06)。es.BT.SE.SET 第二参数的 BACK 型取值(成果 80)。
pub const TITLE_SE_CANCEL: &str = "sse06";

/// 子画面层 id 基(P3:LOAD / EXTRA / CG 鉴赏 / BGM 鉴赏 / END 确认;
/// 与标题 0x5C_7000_* 与台词窗 SC_* 错开;z 在 50..=61 段)。
pub const UI_LOAD_BASE: u64 = 0x5C_8000_0000;
/// EXTRA(鉴赏入口)子画面层 id 基。
pub const UI_EXTRA_BASE: u64 = 0x5C_9000_0000;
/// EXTRA CG 鉴赏子画面层 id 基。
pub const UI_CG_BASE: u64 = 0x5C_B000_0000;
/// BGM 鉴赏子画面层 id 基。
pub const UI_BGM_BASE: u64 = 0x5C_D000_0000;
/// END 确认对话框层 id 基。
pub const UI_CONFIRM_BASE: u64 = 0x5C_E000_0000;

/// 子画面路由钮 id(常量而非算术 —— match 模式需要)。
pub const UI_LOAD_BACK: u64 = UI_LOAD_BASE + 1;
/// CG 鉴赏:返回。
pub const UI_CG_BACK: u64 = UI_CG_BASE + 1;
/// CG 鉴赏:上一页。
pub const UI_CG_PREV: u64 = UI_CG_BASE + 0x90;
/// CG 鉴赏:下一页。
pub const UI_CG_NEXT: u64 = UI_CG_BASE + 0x91;
/// CG 鉴赏:切到 BGM 页签。
pub const UI_CG_TAB_BGM: u64 = UI_CG_BASE + 0xA1;
/// BGM 鉴赏:返回。
pub const UI_BGM_BACK: u64 = UI_BGM_BASE + 1;
/// BGM 鉴赏:上一页。
pub const UI_BGM_PREV: u64 = UI_BGM_BASE + 0x90;
/// BGM 鉴赏:下一页。
pub const UI_BGM_NEXT: u64 = UI_BGM_BASE + 0x91;
/// BGM 鉴赏:切到 CG 页签。
pub const UI_BGM_TAB_CG: u64 = UI_BGM_BASE + 0xA0;
/// END 确认:是。
pub const UI_CONFIRM_YES: u64 = UI_CONFIRM_BASE + 2;
/// END 确认:否。
pub const UI_CONFIRM_NO: u64 = UI_CONFIRM_BASE + 3;

/// 标题全屏分层素材(path, x, y, 层 id)。素材 = 标题 eyecatch/st 族,
/// 布局对齐真机截图(成果 78:银发左 / 双马尾中 / logo 右上)。
pub const TITLE_ST_LAYERS: [(&str, i64, i64, u64); 5] = [
    ("eyecatch/st/bg01a", 0, 0, 0x5C_7000_0000),
    ("eyecatch/st/tet", 0, 0, 0x5C_7000_0001),
    ("eyecatch/st/sir", -1058, 0, 0x5C_7000_0002),
    ("eyecatch/st/han", -86, 0, 0x5C_7000_0003),
    ("eyecatch/st/logo", 1046, -255, 0x5C_7000_0004),
];

/// 标题按钮列(path, x, y, 层 id)。三态素材 `cgsys/title/btn_{名}_{off,on,over}`
/// (成果 78 图像实证);坐标/绑定 = 原生系统剧本 es.BT.XY.SET/es.BT.SET
/// 真值(成果 79:arasuji→BTN.START(2)、start→BTN.START(1)、
/// extra(1314)/config(1477,未实装)/end(1640) 同排 y=745)。
pub const TITLE_BUTTONS: [(&str, i64, i64, u64); 6] = [
    ("cgsys/title/btn_arasuji", 1393, 384, 0x5C_7000_000A),
    ("cgsys/title/btn_start", 1393, 439, 0x5C_7000_0005),
    ("cgsys/title/btn_load", 1393, 533, 0x5C_7000_0006),
    ("cgsys/title/btn_lastload", 1393, 627, 0x5C_7000_0007),
    ("cgsys/title/btn_extra", 1314, 745, 0x5C_7000_0008),
    ("cgsys/title/btn_end", 1640, 745, 0x5C_7000_0009),
];

/// ADV 主按钮行(layout-fix-plan P2-2):参考图 1920×1080 模板匹配标定
/// (相关系数 0.73~0.99;HOME 0.44 待实机复核)。
/// 字段:(层 id, x, y, 宽, 高, 素材路径, 图集段数)。
/// 素材 = `cgsys\main\button\type1\btn_*.png` 横排图集,首段 = OFF。
pub const ADV_BUTTONS: &[(u64, f32, f32, u32, u32, &str, usize)] = &[
    (
        0x5C_9000_0001,
        1048.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_backlog_bt3",
        3,
    ),
    (
        0x5C_9000_0002,
        1148.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_auto_bt4",
        4,
    ),
    (
        0x5C_9000_0003,
        1248.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_skip_bt4n",
        5,
    ),
    (
        0x5C_9000_0004,
        1348.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_save_bt3n",
        4,
    ),
    (
        0x5C_9000_0005,
        1448.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_load_bt3n",
        4,
    ),
    (
        0x5C_9000_0006,
        1548.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_qsave_bt3n",
        4,
    ),
    (
        0x5C_9000_0007,
        1648.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_qload_bt3n",
        4,
    ),
    (
        0x5C_9000_0008,
        1748.0,
        1040.0,
        69,
        39,
        "cgsys/main/button/type1/btn_volume_bt3",
        3,
    ),
    (
        0x5C_9000_0009,
        1803.0,
        1040.0,
        69,
        39,
        "cgsys/main/button/type1/btn_config_bt3",
        3,
    ),
    (
        0x5C_9000_000A,
        1856.0,
        1040.0,
        64,
        39,
        "cgsys/main/button/type1/btn_menulock_bt4",
        4,
    ),
    (
        0x5C_9000_000B,
        1882.0,
        952.0,
        31,
        31,
        "cgsys/main/button/type1/btn_title_bt3",
        3,
    ),
    (
        0x5C_9000_000C,
        1882.0,
        989.0,
        31,
        31,
        "cgsys/main/button/type1/btn_end_bt3",
        3,
    ),
];

// ---- 子画面素材名(EXTRA/LOAD/确认框/台词窗;均为标定样本 es.BT 字面量)----

/// EXTRA 页签:CG 鉴赏活动态单图。
pub const IMG_TAB_CG_ON: &str = "cgsys/extra/btn_tab_cgmode_on";
/// EXTRA 页签:CG 鉴赏非活动态(4 段横排图集,第 0 段 = 常态)。
pub const IMG_TAB_CG_OFF: &str = "cgsys/extra/btn_tab_cgmode_bt3n";
/// EXTRA 页签:RP(SCENE)禁用态(4 段图集,第 3 段 = `_na` 暗段)。
pub const IMG_TAB_RP_OFF: &str = "cgsys/extra/btn_tab_rpmode_bt3n";
/// EXTRA 页签:MV 禁用态(同上)。
pub const IMG_TAB_MV_OFF: &str = "cgsys/extra/btn_tab_mvmode_bt3n";
/// EXTRA 页签:BGM 鉴赏活动态单图。
pub const IMG_TAB_BGM_ON: &str = "cgsys/extra/btn_tab_bgmmode_on";
/// EXTRA 页签:BGM 鉴赏非活动态(4 段图集)。
pub const IMG_TAB_BGM_OFF: &str = "cgsys/extra/btn_tab_bgmmode_bt3n";
/// bt3 三态返回钮图集(蓝=常态/淡=悬停/橙=按下;3 段)。
pub const IMG_BACK_BT3: &str = "cgsys/extra/btn_back_bt3";
/// LOAD 整屏底图。
pub const IMG_SAVELOAD_BACK: &str = "cgsys/saveload/back_load";
/// LOAD 戻る钮(三态)。
pub const IMG_SAVELOAD_BTN_BACK: &str = "cgsys/saveload/btn_back";
/// CG 鉴赏底图(3×3 画格)。
pub const IMG_CGMODE_BACK: &str = "cgsys/extra/cgmode/back";
/// BGM 鉴赏底图(EXTRAS 双列列表)。
pub const IMG_EXTRAS_BACK: &str = "cgsys/extra/back";
/// END 确认对话框底图。
pub const IMG_DIALOG_END: &str = "cgsys/confirm/dialog_end";
/// END 确认「是」钮(三态)。
pub const IMG_BTN_YES: &str = "cgsys/confirm/btn_yes";
/// END 确认「否」钮(三态)。
pub const IMG_BTN_NO: &str = "cgsys/confirm/btn_no";
/// 台词窗底框拉伸素材(1350×200 文本区)。
pub const IMG_MESWINDOW_TXSPACE: &str = "cgsys/main/button/type1/tip_meswindow_txspace";

// ---- 子画面布局(实测标定;换作品即失效,逐项见成果)----

/// EXTRA 页签行几何(x0, y, 单钮宽, 高, 步距)。
/// 原生 7 tab @ y=10;本实现布 4 钮等距 247px 防重叠(原生 95px 步距的
/// 图集裁剪方式未取证,布点为 Likely)。
pub const TAB_BAR: (f32, f32, f32, f32, f32) = (531.0, 10.0, 237.0, 53.0, 247.0);

/// CG 鉴赏每页画格数(原生 3×3;cgmode/back 白格实测:列 277/736/1192 起、
/// 行 141/435/733 起,格 ~458×294,缩略图内缩 ~7px)。
pub const CG_PER_PAGE: usize = 9;
/// CG 画格首格左上 x。
pub const CG_GRID_X0: f32 = 284.0;
/// CG 画格首格左上 y(白格 141 起 + 内缩)。
pub const CG_GRID_Y0: f32 = 148.0;
/// CG 画格横向步距。
pub const CG_GRID_DX: f32 = 458.0;
/// CG 画格纵向步距。
pub const CG_GRID_DY: f32 = 296.0;
/// CG 缩略图宽(格内缩后)。
pub const CG_THUMB_W: f32 = 445.0;
/// CG 缩略图高。
pub const CG_THUMB_H: f32 = 284.0;

/// BGM 鉴赏每页曲目数(2 列 × 11 行,对齐底图画线;extra/back 双列列表底)。
pub const BGM_PER_PAGE: usize = 22;
/// BGM 列表首项左上 x。
pub const BGM_X0: f32 = 368.0;
/// BGM 列表首项左上 y。
pub const BGM_Y0: f32 = 250.0;
/// BGM 列距。
pub const BGM_DX: f32 = 600.0;
/// BGM 行距。
pub const BGM_DY: f32 = 56.0;

/// 鉴赏屏「上一页」钮命中区(CG/BGM 共用)。
pub const PAGE_PREV_RECT: [f32; 4] = [250.0, 950.0, 220.0, 100.0];
/// 鉴赏屏「下一页」钮命中区(CG/BGM 共用)。
pub const PAGE_NEXT_RECT: [f32; 4] = [1350.0, 950.0, 220.0, 100.0];
/// 鉴赏屏 bt3 返回钮命中区(CG/BGM 共用)。
pub const SUBUI_BACK_RECT: [f32; 4] = [1620.0, 930.0, 237.0, 80.0];
/// LOAD 屏戻る钮命中区(btn_back 素材尺寸独立标定)。
pub const LOAD_BACK_RECT: [f32; 4] = [1660.0, 940.0, 186.0, 87.0];

/// LOAD 存档行区 x。
pub const SAVE_ROW_X: f32 = 320.0;
/// LOAD 存档行首行 y。
pub const SAVE_ROW_Y0: f32 = 200.0;
/// LOAD 存档行距。
pub const SAVE_ROW_DY: f32 = 92.0;
/// LOAD 存档行宽。
pub const SAVE_ROW_W: f32 = 1280.0;
/// LOAD 存档行高。
pub const SAVE_ROW_H: f32 = 80.0;
/// LOAD 存档行上限。
pub const SAVE_ROW_MAX: usize = 8;

/// END 确认对话框底图左上(x, y)(原生 dialog_end 素材锚点)。
pub const CONFIRM_DIALOG_POS: (f32, f32) = (694.0, 380.0);
/// END 确认「是」钮命中区。
pub const CONFIRM_YES_RECT: [f32; 4] = [740.0, 470.0, 146.0, 45.0];
/// END 确认「否」钮命中区。
pub const CONFIRM_NO_RECT: [f32; 4] = [1040.0, 470.0, 146.0, 45.0];

/// 台词窗底框素材声明尺寸(w, h)(txspace 拉伸基准)。
pub const MESWINDOW_TEX_SIZE: (u32, u32) = (1350, 200);
