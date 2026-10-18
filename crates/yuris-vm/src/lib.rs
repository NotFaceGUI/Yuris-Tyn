//! yuris-vm
//!
//! L3:组级执行 VM / PC / 挂起恢复。**绝不阻塞**。
//!
//! ## 执行模型(2026-09-03 引擎逆向,**Confirmed**)
//!
//! 证据:`docs/engine/command-layer.md`(kemonomichi2.exe 逆向 + 全语料断言)。
//!
//! - 执行单元 = **命令组**([`CommandGroup`]):part1 的一条 u32
//!   (byte0=YSCM 命令下标,byte1=窗口数,gparam u16)
//! - 主循环(引擎 FUN_0040449c):`pc = obj->pc++; yield = handler[pc]();`
//!   —— PC **先取后增**,跳转处理器直接写目标组号
//! - 处理器返回非零 = **让出**(挂起);本 VM 以 `VmSuspend` 表达
//! - GO(0x2a):标签名 → 标签表(YSLB)→ {目标组号, 脚本号};跨脚本 = 多脚本
//!   重绑,单脚本 VM 显式报 Unsupported(不猜)
//! - GOSUB(0x2b):条件真 → 压帧{return_pc=pc+1} → 跳标签
//! - RETURN(0x4f):弹帧;帧空 = 脚本结束(引擎 depth==0 路径)
//! - IF(0x2c,3 窗):求值 w0 条件;假 → 跳 `w1.len ?: w2.len`
//!   (**len 域存编译期组号**);嵌套栈记录 end 目标
//! - ELSE(0x2d):跳嵌套栈顶的 end;IFEND(0x30):弹嵌套栈
//! - 其余命令:语义未逆向 → [`VmEvent::Unsupported`](VmEvent::Unsupported)
//!   事件;`strict` 模式(默认)挂起,`trace` 模式记录后继续 —— **不猜语义**

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

pub mod audio_probe;
pub mod boot;
pub mod bridge;
pub mod commands;
pub mod event;
pub mod exec;
pub mod host;
pub mod input;
pub mod math;
/// 官方任务调度器重建(**未接线**,差异审阅:`docs/engine/task-scheduler-rebuild.md`;
/// 现行调度 = `pump_frame`/`step_tasks_at`,保持原样)。
pub mod task;
mod driver;
pub mod scenario;
pub mod util;
pub mod vfs;

use host::{ScriptCtx, ScriptHost};
use yuris_core::{Error, Result};
use yuris_format::ystb::YstbFile;
use yuris_script::Evaluator;
use yuris_value::{Value, VariableStore};

pub use commands::{CmdKind, CommandDispatch, DeclElem};
pub use input::{EngineInputSettings, InputSnapshot};
pub(crate) use event::GoOutcome;
pub use event::{event_json, EffectValue, JumpKind, ResumeResponse, VmEvent, VmState, VmSuspend};
pub use util::consume_declarations_into;
pub(crate) use util::mstring;
pub(crate) use util::VarTarget;
#[allow(unused_imports)]
pub(crate) use util::{
    extract_var_target, sjis_char_len, value_summary, value_truthy, varact_ascii_case,
    varact_byte_to_char, varact_char_to_byte,
};

/// crate 版本(与 workspace 同步)
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// **默认 profile 的序号常量**（引擎 555）。
///
/// ⚠️ 本模块**不参与分派**：运行期分派一律走 [`CommandDispatch`]，序号来自
/// 游戏自身的 YSCM（见 [`crate::commands`] 与 `docs/cross-version-commands.md`）。
/// 这里保留常量只为：(a) 构造[`yuris_core::CommandProfile::v555`]兜底 profile；
/// (b) 既有测试/工具按名取号。**新代码不得用它做 `match`**。
pub mod cmd {
    /// GO。
    pub const GO: u8 = 0x2a;
    /// GOSUB。
    pub const GOSUB: u8 = 0x2b;
    /// IF。
    pub const IF: u8 = 0x2c;
    /// ELSE(0x0b;带可选条件,求值后决定进 else 块或跳过)。
    pub const ELSE: u8 = 0x0b;
    /// IFBLEND(0x2d;无条件跳到嵌套栈顶 else 块的结束)。
    pub const IFBLEND: u8 = 0x2d;
    /// RETURN。
    pub const RETURN: u8 = 0x4f;
    /// LET。
    pub const LET: u8 = 0x35;
    /// IFEND。
    pub const IFEND: u8 = 0x30;
    /// LOOP。
    pub const LOOP: u8 = 0x37;
    /// LOOPBREAK。
    pub const LOOPBREAK: u8 = 0x38;
    /// LOOPCONTINUE。
    pub const LOOPCONTINUE: u8 = 0x39;
    /// LOOPEND。
    pub const LOOPEND: u8 = 0x3a;
    /// END(脚本结束;可选返回码)。
    pub const END: u8 = 0x0d;
    /// WAIT。
    pub const WAIT: u8 = 0x68;
    /// TEXT。
    pub const TEXT: u8 = 0x62;
    /// CG。
    pub const CG: u8 = 0x01;
    /// SOUND。
    pub const SOUND: u8 = 0x59;
    /// VARACT(字符串变量操作;只读槽可执行,写回槽 Unsupported)。
    pub const VARACT: u8 = 0x66;
    /// VARINFO(变量属性查询;只读槽可执行,写回槽 Unsupported)。
    pub const VARINFO: u8 = 0x67;
    /// CGINFO(CG 图层属性查询;无图形后端走「CG 不存在」路径,LET=0)。
    pub const CGINFO: u8 = 0x04;
    /// LABELINFO(标签存在性查询;EXIST → LET=0/1,引擎 murmur2 查表 Confirmed)。
    pub const LABELINFO: u8 = 0x34;
    /// CGACT(CGM 图层操作;事件化记录关键字段)。
    pub const CGACT: u8 = 0x02;
    /// LOAD(文件/变量装载;事件化)。
    pub const LOAD: u8 = 0x36;
    /// SAVE(存档数据位图标记;语料 298 组,非声明族最高频)。
    pub const SAVE: u8 = 0x56;
    /// CGEND(CG 显示结束通知;语料 241 组)。
    pub const CGEND: u8 = 0x03;
    /// TASK(命名任务创建/重配置;引擎 0044fe40)。
    pub const TASK: u8 = 0x5f;
    /// TASKINFO(任务状态查询;引擎 00451838,EXIST → LET=0/1)。
    pub const TASKINFO: u8 = 0x61;
}
/// GOSUB 调用帧(引擎帧的子集)。
///
/// 帧局部变量(引擎 frame 0x328B:INT 局部 +8 每槽 8B、FLT +0x90、STR +0x120,
/// 计数在 +0x2bc/+0x2cd/+0x2de;gparam 解码 int=(u16&0xff)>>3、flt=(u16&7)*4+
/// (u16>>14)、str=(u16>>9)&0x1f)。本实现用帧上独立的
/// [`VariableStore`](yuris_value::VariableStore) 承载,键 = (前缀字节, id)
/// —— 与全局存储列分离,弹帧即丢弃(引擎语义:Return 丢弃局部)。
#[derive(Debug, Clone)]
pub struct GosubFrame {
    /// 返回组下标(GOSUB 的 pc+1)。
    pub return_pc: usize,
    /// 返回时所在脚本号(跨脚本 RETURN 恢复;引擎帧含脚本号字段)。
    pub script_id: u16,
    /// 帧局部变量栈(按 id 存;声明命令 id = 0x32-0x35/0x46/0x75+ 帧局部族)。
    pub locals: VariableStore,
    /// GOSUB 时的 IF 嵌套栈深(引擎:帧 +1 字节存栈深,RETURN 恢复 ——
    /// 成果 51.5;子程序内遗留的未弹 IF 项随 RETURN 整体丢弃)。
    pub if_nest_depth: usize,
    /// GOSUB 时的循环栈深(引擎:循环记录栈增长随 RETURN 整体丢弃,成果 51.5)。
    pub loop_depth: usize,
}

/// 循环帧(引擎 obj+0x248 记录块的子集;LOOP 处理器 CMDH_00445a00)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopFrame {
    /// 循环体顶部 = LOOP 的 pc+1(LOOPEND/LOOPCONTINUE 跳回)。
    pub body_start: usize,
    /// 退出点 = LOOP w1.len(编译期组号 = 配对 LOOPEND 组;
    /// 引擎 rec+8,LOOPBREAK/limit=0 跳此,LOOPCONTINUE 经此中转)。
    pub exit_target: usize,
    /// 计数上限(引擎 +0x18/+0x1c 64 位;`None` = 引擎 0xffffffff/-1 无限标记)。
    pub limit: Option<u64>,
    /// 已完成次数(引擎 +0x10/+0x14;LOOP 置 1 → LOOPEND 检查后 +1)。
    pub counter: u64,
    /// **进入本循环时的 `if_nest` 深度**(成果 133)。
    ///
    /// **为什么必须有**:`LOOPBREAK`/`LOOPCONTINUE` 会**跳过循环体内剩余的
    /// `IFEND`**(实测 `es.SP.FINISH.SET`:`g4765 IF / g4766 LOOPBREAK /
    /// g4767 IFEND` —— LOOPBREAK 直接跳到循环外的 LOOPEND,`g4767 IFEND`
    /// 永不执行)⇒ 体内 IF 帧全部残留 ⇒ 之后的 `IFBLEND`/`ELSE` 读到**错误的
    /// 嵌套栈顶**(实测跳到 4767 而不是 4822,绕回循环尾 → `LOOPEND 无配对
    /// LOOP`)。
    ///
    /// 引擎同一族语义已逆向:**块记录随执行位恢复**(`RETURN` 处理器
    /// `0x44b418` 用帧里记录的深度 truncate `if_nest`/`loops`,成果 51.5);
    /// 循环退出同理 —— 丢弃循环体内的嵌套块记录。
    pub if_nest_depth: usize,
}

/// IF 嵌套帧(引擎 obj[0x40+level*4] 块的子集)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IfFrame {
    /// IF 组下标(诊断用)。
    pub if_pc: usize,
    /// 块结束目标(IF 的 w2.len)。
    pub end_target: u32,
}

/// **一个可切换的执行位**(脚本 + 组下标 + 三层栈)。
///
/// `TASK` 建立的任务体是**独立的执行位**(引擎侧任务对象带
/// `SCRIPTPOS`/`TEXTPOS`/`GOSUBLABEL`/`GOSUBNEST` —— 见 YSCM 的
/// `TASK`/`TASKINFO` 参数表),与主线**并行**推进。本内核用"整体取出 /
/// 装回"实现上下文切换:`store`(全局变量)留在 [`GroupVm`] 里共用,
/// 只有这五项属于执行位。
///
/// 证据等级:**Confirmed(T4/T6 取证,2026-10-16,成果 148)** —— 引擎命令
/// 处理器表(`0x7aae40`,119 项,初始化器 `FUN_0045cd30`)与执行链
/// (帧驱动 → `FUN_00464fa4` 每任务一步 → `FUN_0045177d` 分派)均已 dump;
/// v555 侧「任务对象 + 入口标签取自 `#` 槽」(`0x4506f4`)与之互印。
/// 表见 `docs/engine/v494-handler-table.md`。
#[derive(Debug, Clone)]
pub struct SavedExec {
    /// 当前脚本上下文。
    pub ctx: crate::host::ScriptCtx,
    /// 当前组下标。
    pub pc: usize,
    /// GOSUB 帧栈(末位 = 当前)。
    pub frames: Vec<GosubFrame>,
    /// IF 嵌套栈。
    pub if_nest: Vec<IfFrame>,
    /// 循环栈。
    pub loops: Vec<LoopFrame>,
}

/// WAIT 挂起流水(`YURIS_TRACE_WAIT=1` 门控;高等级诊断,缺省静默)。
/// 每次让出(主线/任务)打一行:s/pc/计数/时刻 —— boot 相位差(#1/#2,成果 159)
/// 的节拍对拍仪器。主线行在 [`GroupVm::run`],任务行在 `step_tasks` 的 Wait 分支。
fn wait_trace_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| std::env::var("YURIS_TRACE_WAIT").is_ok())
}

/// 单次 `TASK` 调度里给一个任务体的**组配额**。
///
/// **勘误(2026-10-13,成果 120)**:旧值 256 **太小**,实测后果 ——
/// 场景任务 `es.ScenarioScript` 的 `es.SCENARIO.FLOW` 体在 3000 拍里都没能
/// 走出开头的 `\BTDEF.LOAD`(按钮定义批量装载):每拍只有 256 组额度,
/// 而该函数内部大量 `WAIT`/子调用把它切碎 ⇒ **有效推进 ≈ 0.5 组/拍**,
/// 标题永不可达。
///
/// 引擎语义是「**跑到让出为止**」(任务体自己用 `WAIT` 让出),量子只是
/// 失控保护。现取 65536:实测 `es.SCENARIO.FLOW` 在 300 拍内走完
/// g0..g8(含 `GO[#=$es.STARTLABEL]`),标题脚本(s142 `SCENARIO_START`)开始执行。
/// **等级:Confirmed(T6 取证,2026-10-16,成果 148)** —— 引擎步进器
/// `FUN_00464fa4`(每任务一步)每次**只调分派一次**(取任务 → 写 `task+0x3c` →
/// 执行一条命令 → 回写记录),**全程无指令/命令计数器** ⇒ 引擎确实没有
/// 「N 条后抢占」的量子,让出只由命令语义决定。本常量因此确认是**工程
/// 失控保护值**(非引擎常量);运行期可用 `YURIS_TASK_QUANTUM` 覆盖做对拍。
pub const TASK_QUANTUM: usize = 65536;

/// 运行期可调的量子(环境变量 `YURIS_TASK_QUANTUM`,诊断/对拍用)。
/// 引擎语义「跑到让出为止」的工程近似值 —— 默认取 [`TASK_QUANTUM`]。
pub fn task_quantum_from_env() -> usize {
    std::env::var("YURIS_TASK_QUANTUM")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(TASK_QUANTUM)
}

/// `YURIS_DEBUG_FRAME` 门控快照(每组/每读一次 `env::var_os` 的替代)。
fn debug_frame_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| std::env::var_os("YURIS_DEBUG_FRAME").is_some())
}

/// 任务表**规模上限**(防爆:每个 [`VmTask`] 持一份执行位,含整份已解析
/// 脚本 —— 脚本失控反复建名会线性吃内存)。超出即拒建并留事件。
pub const MAX_TASKS: usize = 128;

/// 渲染侧在「脚本从未设置过 `FONT SX/SY`」时使用的缺省字号(像素,游戏画布)。
///
/// 与 `yuris_player_core::text_px_or_default` 的缺省常量必须一致:
/// `FONTINFO SX/SY` 查询要回读**当前生效值**,脚本的「暂存→改→还原」
/// 惯用法(`s70 g290-299`)才构成恒等还原。
pub const DEFAULT_FONT_PX: i64 = 40;

/// 一帧域泵的报告(P10 域调度,成果 161;诊断与测试断言用)。
#[derive(Debug, Clone, PartialEq)]
pub struct PumpReport {
    /// 主线域本帧是否推进(预算耗尽的中途暂停也算)。
    pub main_ran: bool,
    /// 主线域本帧的真实让出(预算耗尽为 `None`)。
    pub main_suspend: Option<VmSuspend>,
    /// 本帧推进过的任务域个数。
    pub tasks_ran: usize,
    /// 本帧结束时仍在休息(未到期/未结束)的任务域个数。
    ///
    /// 手册真值:「**所有任务都休息时引擎才绘制一帧**」—— 呈现侧的
    /// 采样不必等它为 0(引擎绘制也是按帧率采样),但它是帧内作业
    /// 管线是否清空的诊断信号(boot 相位对拍,成果 160)。
    pub tasks_resting: usize,
    /// 主线域本帧实际执行组数(S 用:时间片→组数标定)。
    pub groups_ran: usize,
}

/// 一个 `TASK` 执行体(引擎"任务对象"的最小语义模型)。
#[derive(Debug, Clone)]
pub struct VmTask {
    /// `ID` 槽(任务名,SJIS 字节;`TASKINFO EXIST` 按此匹配)。
    /// 引擎:名字是**普通字符串**、非"脚本自备槽"族(实测 494 的
    /// 12 个自备槽不含此名,与 `SAVE`/`LOAD` 的引用族不同)。
    pub id: Vec<u8>,
    /// 执行位(`None` = 正在被步进,取出中)。
    pub state: Option<SavedExec>,
    /// 让出剩余帧数(任务体内 `WAIT` 的计数)。
    pub wait_frames: u32,
    /// 让出到期时刻(`WAIT TIME` 的毫秒期限;P10 域调度,成果 161)。
    ///
    /// 引擎真值(`VmSuspend::Wait` 注释):TIME 参数存 `obj+0x1c =
    /// timeGetTime()+ms`,**未到期不醒**。旧实现任务侧丢弃 time_ms、把
    /// TIME 型让出也压成 1 帧(`unwrap_or(1)`,D2)—— 本字段即修正位:
    /// `step_tasks`/`pump_frame` 判「`wait_frames` 用尽 **且** `resume_at`
    /// 已到」才可步进(与主线 tick 的双条件同构)。
    pub resume_at: Option<std::time::Instant>,
    /// 已结束(任务体 `RETURN` 落到基帧 / `END`)。
    pub finished: bool,
    /// `E` 槽:0 = 不参与调度(任务保留但暂停)。
    pub enabled: bool,
    /// 已执行的调度次数(`TASKINFO`/诊断用)。
    pub steps: u64,
}

/// CG 状态注册表条目(P7.1,成果 62;P1 收尾定稿)。
///
/// 引擎侧 CG 对象(kemonomichi2.exe 处理器 0x423864 写 X→+0x70 /
/// Y→+0x78 / E→+0xab 等)的最小语义模型:CG 命令按名 upsert,
/// CGINFO 查询 SX(槽13)/SY(槽14)/COLOR(槽24)。
///
/// **槽13/14 = 装载图像的真实宽/高(Confirmed,引擎 watch oracle)**:
/// script9 g1089 CGINFO 写 @1705:occ1 = 1.0(= `cgsys\dummy.png`,1×1
/// 占位图)、occ2 = 1350.0(= `tip_meswindow_txspace.png`,1350×200 消息窗
/// 纹理);两图均经封包扫描确认存在(`yuris-resource` dims_probe)。
/// g1092 IF `SX==1 && SY==1 && COLOR==0x808080` = 「是 1×1 占位图」判定。
/// 本模型:CG 命令 FILE 槽(46)非空 → 经 `PacFileIndex::image_dims` 解析
/// 图像头填 [`img_w`](CgState::img_w)/[`img_h`](CgState::img_h);
/// 不可解析(无探针/非 stored PNG)→ 0(偏离引擎处逐项定性)。
/// 缩放乘积(若有)未验证 —— 应答值与图像尺寸逐字节吻合,乘数 = 1。
///
/// sx/sy(CG 槽9/10)与图像尺寸**无关**(旧「默认 SX=1/SY=1 应答」模型
/// 作废:那只是 occ1 恰好命中 1×1 占位图的巧合)。语义 = Likely(缩放),
/// 不参与 CGINFO 应答。
///
/// COLOR(槽24)= 0x808080:occ1 实证(引擎写回 0x808080);「新建 CG
/// 未指定 COLOR 槽 ⇒ 取 0x808080」的一般化规则为 Likely。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgState {
    /// X(CG 槽 4;引擎 +0x70)。未执行查询,模型 Likely。
    pub x: i64,
    /// Y(CG 槽 5;引擎 +0x78)。未执行查询,模型 Likely。
    pub y: i64,
    /// Z(CG 槽 6)。未执行查询,模型 Likely。
    pub z: i64,
    /// 缩放 X(CG 槽 9;引擎语义 Likely,不参与 CGINFO 应答)。
    pub sx: i64,
    /// 缩放 Y(CG 槽 10;引擎语义 Likely,不参与 CGINFO 应答)。
    pub sy: i64,
    /// 装载图像宽(CGINFO 槽 13 应答;watch oracle 实证)。
    pub img_w: i64,
    /// 装载图像高(CGINFO 槽 14 应答;watch oracle 实证)。
    pub img_h: i64,
    /// 最近一次非空 FILE 槽(纹理路径;P8.2b 重放按路径分 z 带用)。
    pub file: Option<Vec<u8>>,
    /// 色(COLOR;CGINFO 槽 24 查询,CG 命令无对应槽)。
    pub color: i64,
    /// **E = 显隐**(本作 CG 槽 2;官方手册语义:该 CG 是否显示)。
    ///
    /// **勘误(2026-10-13,成果 121)**:旧实现没有该字段 —— 游戏用 `E=0`
    /// 关掉的层在播放器侧仍可见,实测三套按钮状态图(`*NAES`/`*OFFES`/`*ONES`)
    /// 同时上屏。默认 1(新建即显示):`CG[ID=x FILE=y]` 不带 E 时引擎会显示它。
    pub e: i64,
    /// **T = 过渡进行度 0〜256**(手册;本作槽 37)。
    pub t: Option<i64>,
    /// **TID = 过渡图案图层 ID**(手册;本作槽 38)。
    pub tid: Option<Vec<u8>>,
    /// **TA = 过渡精度 0〜256**(手册;本作槽 40)。
    pub ta: Option<i64>,
    /// **MID = 外部遮罩图层 ID**(手册;本作槽 35)。
    pub mid: Option<Vec<u8>>,
    /// **MODE = 描画模式 0〜18**(手册;本作槽 41)。
    pub mode: Option<i64>,
    /// `SX`/`SY` 是否由脚本**显式给出**（手册：レイヤセットの幅/高さ）。
    ///
    /// `CGINFO ONMOUSE*` 的命中矩形需要区分「显式尺寸」与「图像原始尺寸」：
    /// `CgState` 的 `sx/sy` 缺省 1 表示未设置，若直接当尺寸用，按钮命中区
    /// 会退化成 1×1。
    pub size_explicit: bool,
    /// **`Z` 是否由脚本显式给出**(2026-10-17 勘误)。
    ///
    /// `z` 缺省 0 是「未设置」的占位,不是引擎语义上的 z=0。旧实现让
    /// [`GroupVm::cg_z`] 无条件返回 `Some(st.z)` ⇒ 任何**从未给过 Z** 的层
    /// 都以 `Some(0)` 覆盖播放器的档位兜底(`vm_ui_z_for_path`:按钮 2010 /
    /// 其它 1000)⇒ 标题六钮/系统栏全部沉到 z=0 被背景盖住。
    ///
    /// 实测(`YURIS_TRACE_Z=1`):`BT.OFFES.TITLE.BTN.START=1=1` 建层事件
    /// `evt=None`,注册表却回 `Some(0)` ⇒ `→ Some(0) fallback=2010`。
    ///
    /// 引擎语义(成果 161 同一依据):`z=None` 只表示「本命令不改 z」;
    /// 新建对象的 z 由子系统档位决定,不是 0。故只有脚本给过 Z 才可作为
    /// 继承值下发。
    pub z_explicit: bool,
}

/// 一次求值前从 VM 侧取出的上下文快照(见 [`GroupVm::eval_ctx`])。
///
/// 存在的理由:`Evaluator::with_locals` 可变借走 `store`,构造后无法再读
/// VM 的其它字段 —— 求值相关的注入值必须先快照、再一次性 `apply`。
struct EvalCtx {
    loop_counter: i64,
    timer_ms: i64,
    cursor: (i64, i64),
    screen: (i64, i64),
    input: Option<std::sync::Arc<yuris_script::eval::InputVars>>,
}

impl EvalCtx {
    /// 注入到求值器(四处求值点统一;T20)。
    fn apply(&self, ev: &mut yuris_script::Evaluator<'_>) {
        ev.set_cursor(self.cursor.0, self.cursor.1);
        ev.set_loop_counter(self.loop_counter);
        ev.set_timer_ms(self.timer_ms);
        ev.set_screen_size(self.screen.0, self.screen.1);
        if let Some(input) = &self.input {
            ev.set_input(std::sync::Arc::clone(input));
        }
    }
}

impl Default for CgState {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            z: 0,
            sx: 1,
            sy: 1,
            img_w: 0,
            img_h: 0,
            file: None,
            color: 0x80_80_80,
            e: 1,
            // 过渡/遮罩/描画模式(成果 125):未指定 = None(引擎「未给槽保持原值」)。
            t: None,
            tid: None,
            ta: None,
            mid: None,
            mode: None,
            size_explicit: false,
            z_explicit: false,
        }
    }
}

/// 一条被跳过的 Unsupported 命令的聚合记录(见 `GroupVm::unsup_summary`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedHit {
    /// 命中次数。
    pub count: u64,
    /// 首次命中的脚本号。
    pub script_id: u16,
    /// 首次命中的组号。
    pub pc: usize,
    /// 首次命中的原因。
    pub reason: String,
}

/// YU-RIS 组级 VM(单脚本)。
pub struct GroupVm {
    /// 当前脚本上下文(组表 + 窗口起始)。
    ctx: ScriptCtx,
    /// 跨脚本宿主(按脚本号取上下文);None = 单脚本模式。
    host: Option<Box<dyn ScriptHost>>,
    /// 程序计数器 = 组下标。
    pc: usize,
    state: VmState,
    events: Vec<VmEvent>,
    /// 与 [`GroupVm::events`] **平行**的域标记(0 = 主线,1..= 任务创建序;
    /// P10 域调度,成果 161)。引擎真值(成果 159):主线与任务在 worker 线程
    /// 上**并发**泵进,同帧事件序 = 域泵序(主线先、任务按创建序)——消费侧
    /// 按域合成呈现时靠它区分来源。不进 golden JSONL(golden 序列化只吃
    /// `VmEvent`,本 vec 不可见)。
    event_domains: Vec<u8>,
    /// 当前事件发射域(步进任务期间在 `step_tasks`/`pump_frame` 里切换)。
    cur_domain: u8,
    /// **脚本设定的帧率**(`FPS[SET=n]`;`None` = 未设定)。
    ///
    /// 手册 `fps/fps.html`:游戏的帧推进基准,影响 `WAIT` 的 FRAME 计数与
    /// 动画速率。引擎在 `FUN_0041c2f0` 族按此限帧(实测本作 `FPS[SET=60]`,
    /// s0 g318 / s142 等)。旧实现把 `FPS` 归 Subsystem 只记录不消费 ⇒
    /// 内核恒以 60fps 之外的节奏跑,且脚本无从查询。
    ///
    /// 消费侧:`Player::tick` 的帧间隔按它定(缺省 60)。
    pub fps: Option<u32>,
    /// 标签表(name → (目标组号, 脚本号));由外部注入(来自 YSLB)。
    labels: HashMap<Vec<u8>, (u32, u16)>,
    /// strict = 遇 Unsupported 命令挂起(默认);false = 记录后继续。
    strict: bool,
    store: VariableStore,
    frames: Vec<GosubFrame>,
    if_nest: Vec<IfFrame>,
    loops: Vec<LoopFrame>,
    executed_groups: usize,
    executed_windows: usize,
    /// `RETURNCODE` 执行次数（调用点返回标记；语义取证中，计数供对拍）。
    returncode_hits: u64,
    /// 已消费声明组的脚本号(每脚本只全扫一次;见 `switch_script`)。
    declared_scripts: std::collections::HashSet<u16>,
    /// 播放器注入的光标逻辑坐标(@133/@138;P9.2)。
    input_cursor: (i64, i64),
    /// YSVR kind2(按脚本初值)条目暂存,首载时应用(成果 59h)。
    ysvr_kind2: Vec<yuris_format::ysvr::YsvrEntry>,
    /// YSVR kind1(全局初值)条目**快照**(boot 应用完留档)。
    ///
    /// 运行期 `VARACT[INIT/G_*]` 恢复组的镜像(旁线仓库 Round20 Confirmed:
    /// 引擎 `0x454e00 → 0x455b60`,恢复 = 按 `(ty, category)` 12 位掩码重放
    /// kind1 记录; 没 G_* 槽 = 全 0 掩码 = 什么都不恢复 —— 见
    /// [`GroupVm::varact_restore_globals`])。
    ysvr_globals: Vec<yuris_format::ysvr::YsvrEntry>,
    /// 已应用 kind2 的脚本号。
    ysvr_kind2_applied: std::collections::HashSet<u16>,
    /// **引擎毫秒计时的起点**(`@1[1]` 系统变量)。
    ///
    /// 引擎侧:`00447ebc`/`00447d4c` 的 id<1000 二次派发,case 0x01 = timeGetTime
    /// 派生计时(`docs/engine/command-layer.md` §9c)。脚本侧恒按**时间差**用
    /// (`@1[1] - 起始` 再除时长 = 进度),全语料 1504 处。VM 用单调时钟取
    /// 「自 VM 建立以来的毫秒数」—— 与 timeGetTime 同为单调毫秒,差值与绝对值
    /// 在脚本可观测面上等价。
    epoch: std::time::Instant,
    /// 游戏虚拟 FS 存在性索引(FILEINFO EXIST;见 `host::PacFileIndex`)。
    file_probe: Option<std::sync::Arc<crate::host::PacFileIndex>>,
    /// **音频状态查询通道**（`SOUNDINFO`；由宿主注入，VM 不依赖音频层）。
    audio_probe: Option<std::sync::Arc<dyn crate::audio_probe::SoundStateProbe>>,
    /// **官方任务调度器**(单一扁平链;主线也是链中一员)。
    scheduler: Option<crate::task::TaskScheduler>,
    /// 任务执行位(按任务名;主线直接跑在 `self` 上)。
    task_slots: HashMap<Vec<u8>, SavedExec>,
    /// 任务名表(TASK 注册序;TASKINFO/重配置/诊断用)。**原始字节**。
    task_names: Vec<Vec<u8>>,
    /// 调度副作用队列(TASK/TASKEND 写入;调度器每条命令后取走)。
    pending_effects: Vec<crate::task::Effect>,
    /// 窗口切片复用缓冲(稳态零分配)。
    windows_buf: Vec<yuris_format::ystb::CommandSlot>,
    /// 窗口字节复用缓冲。
    bytes_buf: Vec<u8>,
    /// **求值槽位缓冲**（跨调用存活 ⇒ 稳态零分配）。
    ///
    /// `Evaluator` 是每次求值临时构造的，它的内部 `slot_buf` 活不过一次
    /// 调用；条件窗口求值每帧约 70 万次，把缓冲提到 VM 上才是真复用。
    slot_scratch: Vec<yuris_script::eval::Slot>,
    /// **指令解码缓存**:`(offset,len)` → 已解码指令(脚本装载后窗口不变)。
    /// 窗口解码缓存。**FxHashMap**：键是整数元组，每帧约 70 万次查表，
    /// SipHash 的抗 DoS 特性在这里没有安全收益（键不来自外部输入）。
    insn_cache: rustc_hash::FxHashMap<(u16, u32, u32), std::sync::Arc<[yuris_script::insn::Instruction]>>,
    /// 当前 vsync 链的起点时钟(u32 绝对毫秒;WAIT TIME 换算用)。
    chain_now_ms: u32,
    /// **统一文本通道**(v49x 内联 WORD/TEXT + v555 明文剧本共用)。
    text_channel: crate::scenario::TextChannel,
    /// **字体子系统状态**(`FONT` 命令;SX/SY = 字号)。
    font_px: Option<(i64, i64)>,
    font_color: Option<(i64, i64)>,
    font_bold: Option<i64>,
    font_drawmode: Option<i64>,
    font_sel: Option<(i64, i64)>,
    /// `FONT` 的 `ANTIALIASING` 槽(供 `FONTINFO` 查询回读)。
    font_aa: Option<i64>,
    /// `FONT` 的 `GRADATION` 槽(`FONTINFO` 查询回读)。
    font_gradation: Option<i64>,
    /// `FONT` 的 `ROTATION` 槽(`FONTINFO` 查询回读)。
    font_rotation: Option<i64>,
    /// **错误输出级别配置**(`ERROR` 命令;按**命令参数名** `COM_<CMD>` 索引)。
    ///
    /// 取证:YSCM 的 `ERROR` 参数表是 `NO / SET / COM_CG / COM_CGACT / …`
    /// —— 每个 `COM_*` 对应一条命令;SDK 自带 `es_ERIS.yst` 第 33 行
    /// `ERROR[COM_CG=0]` 是**启动配置**,不是错误触发。
    /// 引擎处理体(0x4411f8)只把值写进引擎侧错误号/节流位,**不动脚本 pc、
    /// 变量与场景** ⇒ 本内核按「记录配置 + 推进 pc」实现,不走挂起路径。
    error_levels: HashMap<String, i64>,
    /// `ERROR[NO=…]` 的错误号(引擎侧写进进程级错误码字节)。
    error_code: Option<i64>,
    /// `SYSTEM[SCREENSAVER=1]` 的屏保抑制标志（引擎 `0x5b4748`）。
    ///
    /// 引擎消费者 `0x45ad22`：非 0 时直接返回，跳过屏保/挂起判定。
    screensaver_suppressed: bool,
    /// `SYSTEM[BACKGROUNDEXEC=1]` 的后台执行标志（引擎 `0x5b4700`）。
    ///
    /// 引擎只在非前台任务（任务对象 `+0x2a8 == 1`）时应用。
    background_exec: bool,
    /// **`yst_list.ybn` 脚本名表**（YSTL）—— `SYSTEMINFO` 的真值来源。
    ///
    /// `None` = 该包没有该文件（此时 `SCRIPTNUM`/`TEXTNUM` 答 0、
    /// `SCRIPTFILE` 答空串，与引擎「表未建立」的行为一致）。
    ystl: Option<yuris_format::ystl::YstlTable>,
    /// **E-mote 运行时可用性**（引擎 `0x802a28`：「`YSEmote.dll` 装载成功」）。
    ///
    /// 缺省 `false`：本内核不装载 `YSEmote.dll`，也不实现 E-mote 渲染面 ⇒
    /// 与「无 DLL 的引擎」行为一致（`EMOTE` 为空操作）。
    /// 取证与实现见 `exec::emote` 模块文档。
    emote_runtime: bool,
    /// `SYSTEM[TIMECOUNT*]` 的四个累计毫秒计数（引擎 `0x802934/38/3c/40`）。
    timecount_total: [i64; 4],
    /// 同上的「置 1 时刻」起点（引擎 `0x802508/10/18/20`；`None` = 未在计时）。
    timecount_start: [Option<u32>; 4],
    /// **逻辑时钟**:`now_ms = 上屏帧数 × 1000/fps`(确定性)。
    logical_clock: bool,
    /// 逻辑帧计数。
    logical_frames: u64,
    /// **Unsupported 命令汇总**(命令名 → 次数/首现场/原因)。
    pub unsup_summary: std::collections::BTreeMap<String, UnsupportedHit>,
    /// 是否发射逐组执行事件(诊断面;播放器关闭)。
    emit_group_events: bool,
    /// **热点组直方图**(`YURIS_TRACE_HOT=1`):(脚本,pc) → 执行次数。
    /// 定位「哪几组吃掉每帧时间」的唯一直接手段。
    hot_hist: HashMap<(u16, usize), u64>,
    /// **命令类型直方图**(同为 `YURIS_TRACE_HOT`):command_type → 次数。
    /// 与 `hot_hist` 分开累加,避免「只看收尾时所在脚本」的漏计
    /// (成果 184:链内跨脚本泵时,按 ctx.script_id 反查会全落空)。
    hot_cmd: HashMap<u8, u64>,
    /// **按脚本的组数直方图**(同门控):script_id → 组数。
    hot_script: HashMap<u16, u64>,
    /// **当前分辨率**(引擎 `@_SCREEN_SIZE_X/Y` 与 `WINDOWINFO NO=0 SX/SY` 的真值)。
    ///
    /// **勘误(2026-10-13,成果 123)**:旧实现把 `WINDOWINFO SX/SY` 与
    /// `@115/@116` **写死 1920×1080**(最初样本 v555 的值)。而游戏
    /// `es_First` 读 `WINDOWINFO NO=0 SX/SY` → `@es.WSX/WSY` → **整套布局**
    /// (`@es.GSX/GSY`,立绘/UI 坐标)都按它算 ⇒ 本作 1100×825 时**画面整体错位**。
    /// 真值来自游戏自身 `ysbin\yscfg.ybn`(启动链注入)。
    screen_w: i64,
    screen_h: i64,
    /// CG 状态注册表(CG 名(SJIS 字节)→ 状态;P7.1,成果 62)。
    cg_registry: HashMap<Vec<u8>, CgState>,
    /// **运行期命令分派**:**当前脚本**头版本对应的序号 → [`CmdKind`]。
    ///
    /// **引擎只有一张表**（`0x7aae40`，119 项，由 `FUN_0045cd30` 按游戏自己的
    /// `ysc.ybn` 填充）。脚本头版本**不参与**选表 —— 实测该作 160 个脚本里
    /// 5 个是 v479 头，但它们在 YSLB 里零标签引用（引擎从不加载），
    /// 因此「按头版本切表」既无必要也无依据。
    /// 取值顺序见 [`Self::dispatch_for_version`]。
    dispatch: std::sync::Arc<CommandDispatch>,
    /// 游戏自身 `ysc.ybn` 建的 profile（**唯一权威表**）。
    ///
    /// `None` = 尚未装载 YSCM（工具/合成脚本场景）：`load()` 已按脚本头版本
    /// 建立了内置表，此后的切脚本**不会**把它覆盖掉。
    game_profile: Option<yuris_core::CommandProfile>,
    /// 已解析的分派表缓存（有游戏表 / 无表 各一格）。
    ///
    /// **共享而不是克隆（2026-10-18 性能修复）**：`CommandDispatch` 内含
    /// 121 条命令名 + 每命令的参数名表（约 1100 个 `String`）。旧实现每次
    /// `dispatch_for_version` 都 `clone()` 整张表 —— 切脚本（`GO`/`GOSUB`
    /// 跨脚本、任务调度）时每帧数次，实测是「窗口无响应」级的热点。
    /// 现在缓存与生效字段都指向同一个 `Arc`，切换只剩引用计数 + 一次哈希查找。
    dispatch_cache: HashMap<u32, std::sync::Arc<CommandDispatch>>,
    /// **正在执行的命令下标**(`run_main` 在每次 `dispatch` 前写入)。
    ///
    /// **为什么要这一项(2026-10-15,勘误)**:各处理器调 `halted`/`halt_err` 时
    /// 传的是 `cmd::*` 常量 —— 那批常量是**最初样本 v555 的序号留存**
    /// (见 `cmd` 模块文档),在别的版本上指向完全不同的命令。诊断文本若按它
    /// 反查名字,就会打出**错误的命令名**:实测 v494 语料 s142 的真实命令是
    /// `GO`,而 `cmd::GO = 0x2a` 在 v494 的 YSCM 里是 `G_VAR`,于是日志写成
    /// `未实现/未命中 G_VAR(0x2a idx42) … :: GO 标签未命中` —— 名字与原因
    /// 互相矛盾,排障时会去查一条根本没执行的命令。
    ///
    /// 统一诊断改读本字段(`Some` = 分派期,`None` = 分派外),并把调用方传的
    /// 常量仅在**与真实值不一致**时作为附加信息附上。
    cur_command: Option<u8>,
    /// 「无参数表 → v555 槽号回落」的累计次数(T3;`Cell` 便于 `&self` 的
    /// `slot_is` 族登记)。生产 profile(四张内置表 + 游戏 YSCM)恒为 0;
    /// 非 0 = 分派表按名解析不可用、参数槽读到的是 v555 号(UNVERIFIED)。
    slot_fallback_uses: std::cell::Cell<u64>,
    /// 已打过的回落告警 `(命令下标, 参数名)` 去重集(首次才打 stderr)。
    slot_fallback_warned: std::cell::RefCell<std::collections::HashSet<(u8, String)>>,
    /// Error 态**第一次进入**时的根因快照(`run_main` 的「VM 处于 Error
    /// 态」挡回消息带它;2026-10-17 之前该消息不带任何原因 —— 实机排查
    /// 时用户只能看到无穷尽的同名刷屏,根因一行都翻不到)。
    ///
    /// 填充点:有明确原因的处理器(`halt_or_record`/`halted` 族)**先写**,
    /// [`Self::set_error_state`] 只在缺失时按现场兜底。**只记第一次**:
    /// 之后同一 Error 态里的再入都已不是根因。
    err_cause: Option<String>,
    /// run_main 错误日志的**同因去重**缓存+计数(分派与履约见
    /// [`Self::log_run_error`];60fps 刷屏时同一文本打满一屏无信息量)。
    err_log_last: Option<String>,
    err_log_repeat: u64,
    /// [`Self::set_error_state`] 现场告警的去重集:每个 `(脚本, pc)` 只打一次
    /// (实测同一 spot 每帧重入,不去了重同样会刷满一屏)。
    err_site_warned: std::cell::RefCell<std::collections::HashSet<(u16, usize)>>,
    /// [`Self::note_swallowed`](吞错继续告警)的去重集(同样每 `(脚本, pc)` 一次)。
    swallow_warned: std::cell::RefCell<std::collections::HashSet<(u16, usize)>>,
    /// **Unsupported 大告警去重集**:每 `(脚本, pc, 命令下标)` 只打一次横幅。
    ///
    /// 与 `unsup_summary`(计数)互补:前者控制实机输出量,后者供收尾汇总。
    unsup_alerted: std::cell::RefCell<std::collections::HashSet<(u16, usize, u8)>>,
    /// **标签未命中去重**（同一 `(脚本, pc, 标签)` 只打一次横幅）。
    ///
    /// 必须按实例去重：`GO` 未命中会**每帧重入**（脚本停在原 pc 上继续跑），
    /// 旧实现每次 `alert!` 都同步写 stderr —— 一次 3.3MB 包的启动链里
    /// 实测把帧率压到个位数（每帧几十条横幅 × 终端写锁）。
    // **当前无读者**(2026-10-18 清理告警时发现):label 未命中去重当前由上方几个
    // 集合承担。保留字段与上述帧率依据,待确认后删除。
    #[allow(dead_code)]
    label_miss_alerted: std::cell::RefCell<std::collections::HashSet<(u16, usize)>>,
    /// **未建模参数槽告警去重集**:每 `(命令下标, 槽号)` 只打一次。
    unknown_slot_warned: std::cell::RefCell<std::collections::HashSet<(u8, u8)>>,
    /// **strict 模式下把未实现升级为致命错误窗**(默认 `false` = 只挂起;
    /// 播放器/CLI 启动时置 `true`)。
    ///
    /// 库/测试默认关:避免全局致命状态污染同进程的其它用例;真正的运行时
    /// (有画布、有错误窗)必须在启动链显式打开 —— 这正是用户要求的
    /// 「未知命令 → 中断 → 崩溃界面盖住」。
    fatal_reporting: bool,
    /// **缺失子系统/命令**的告警去重集(按命令名)。
    missing_feature_warned: std::cell::RefCell<std::collections::HashSet<String>>,
    /// `MATH RND` 的随机源(MT19937;T22)。
    ///
    /// 种子来源**未取证**(引擎 `0x4211c2` 初始化路径未读完)→ 缺省用时间派生
    /// 种子,`UNVERIFIED`;确定性对拍用 [`GroupVm::set_math_seed`] 注入。
    /// 取值范围语义与引擎一致(`lo + draw % span`),逐位序列待种子取证后对齐。
    math_rng: crate::math::Mt19937,
    /// 本帧输入快照(播放器每帧推进;`None` = 无输入面,见
    /// [`GroupVm::set_input_snapshot`])。
    input: Option<crate::input::InputSnapshot>,
    /// 求值器看到的输入快照(与 [`Self::input`] 同步;写入时经
    /// `Arc::make_mut` 更新,避免每次求值重建)。
    input_vars: Option<std::sync::Arc<yuris_script::eval::InputVars>>,
    /// 本帧脚本对输入系统变量的写入 overlay
    /// (`(id, 首下标)` → 值);同帧后续读取看到新值。
    input_overlay: HashMap<(u16, i64), Value>,
    /// 本帧脚本写入 outbox(帧末由播放器回灌输入状态机)。
    input_writes: Vec<(u16, Option<i64>, Value)>,
    /// `INPUT` / `MOUSE` 命令累计设置。
    input_settings: crate::input::EngineInputSettings,
}

impl GroupVm {
    /// 设定**当前分辨率**(应来自游戏自身 `yscfg.ybn` 的 `screen_width/height`)。
    ///
    /// 影响:`WINDOWINFO NO=0 SX/SY` 应答、`@_SCREEN_SIZE_X/Y`(`@115/@116`)。
    /// 引擎真值来自显示初始化;本内核无显示栈,故由启动链显式注入(成果 123)。
    pub fn set_screen_size(&mut self, w: i64, h: i64) {
        if w > 0 && h > 0 {
            self.screen_w = w;
            self.screen_h = h;
        }
    }

    /// 当前分辨率(诊断用)。
    pub fn screen_size(&self) -> (i64, i64) {
        (self.screen_w, self.screen_h)
    }

    /// 注入 `yst_list.ybn` 脚本名表（`SYSTEMINFO` 的 `SCRIPTFILE` /
    /// `SCRIPTNUM` / `TEXTNUM` 真值来源）。
    pub fn set_ystl_table(&mut self, table: Option<yuris_format::ystl::YstlTable>) {
        self.ystl = table;
    }

    /// 当前脚本名表（诊断/查询）。
    pub fn ystl_table(&self) -> Option<&yuris_format::ystl::YstlTable> {
        self.ystl.as_ref()
    }

    /// 「无参数表 → v555 槽号回落」累计次数(T3 可观测面)。
    ///
    /// 生产路径(四张内置表 / 游戏 YSCM 注入)**恒为 0**;非 0 表示该脚本
    /// 的参数是按 v555 槽号读的(UNVERIFIED)—— 对拍/探针据此判定结果可信度。
    pub fn slot_fallback_uses(&self) -> u64 {
        self.slot_fallback_uses.get()
    }

    /// 注入 `MATH RND` 的随机种子(确定性对拍用;T22)。
    ///
    /// 引擎的种子来源未取证 —— 本内核缺省时间派生(UNVERIFIED)。
    /// 对拍「取值分布」而非逐位序列时不需要本方法;对拍逐位序列必须在
    /// 种子取证后另行对齐。
    pub fn set_math_seed(&mut self, seed: u32) {
        self.math_rng = crate::math::Mt19937::new(seed);
    }

    /// CG 注册表快照(P8.2b 重放:名称 SJIS 字节 + 注册 x/y + 最近 FILE)。
    /// 播放器进游戏时按此重建 VM UI 层(纹理已在播放器侧按 fnv(名) 预载)。
    pub fn cg_registry_snapshot(&self) -> Vec<(Vec<u8>, i64, i64, Option<Vec<u8>>)> {
        self.cg_registry
            .iter()
            .map(|(k, v)| (k.clone(), v.x, v.y, v.file.clone()))
            .collect()
    }

    /// 注册表快照(**含 E 显隐与 Z**)——重放必须保真层状态。
    ///
    /// **为什么不能只用旧快照(勘误 2026-10-17)**:旧快照只带 `x/y/file`,
    /// 重放侧只好把每个注册件都当成"要点亮"(`e: Some(1)`)。而引擎的
    /// 注册表是**对象属性表**:`CG[E=0]` 的对象照样在表里,只是不显示。
    /// 结果 config 页(注册时 `E=0`)在重放时被强制点亮 ⇒ **开屏就是
    /// 配置页**。本函数把真实 `e`/`z` 一并带出,重放按原值恢复。
    ///
    /// `z` 返回 `Option`:只有脚本显式给过 Z 的对象才带值,否则 `None`
    /// (重放侧落到档位兜底,见 [`Self::cg_z`] 的勘误说明)。
    #[allow(clippy::type_complexity)]
    pub fn cg_registry_snapshot_full(
        &self,
    ) -> Vec<(Vec<u8>, i64, i64, Option<i64>, i64, Option<Vec<u8>>)> {
        self.cg_registry
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.x,
                    v.y,
                    if v.z_explicit { Some(v.z) } else { None },
                    v.e,
                    v.file.clone(),
                )
            })
            .collect()
    }

    /// 按名称查注册表当前 x/y(es.BT.XY.SET 经 CGACT 落库后的权威位置;
    /// ASCII 名精确匹配,SJIS 名经 lossy 往返可能失配 → None)。
    pub fn cg_position(&self, name: &str) -> Option<(i64, i64)> {
        self.cg_registry.get(name.as_bytes()).map(|st| (st.x, st.y))
    }

    /// 按名称查注册表**当前 z**(成果 161 修正:CGEND 后同名重建事件不带
    /// Z 槽时,属性应沿用 CG 注册表里该对象的 z —— 引擎的 CG 对象按名持
    /// 有属性,重建命令的 `z=None` 只表示「本命令不改 z」,不表示 z=0)。
    ///
    /// 取证:`<game-b>_win_trace.log` 的 `BT.OFFES.CONFIG.TIP.BG=1=1` 首次建层
    /// `z=Some(998)`,其后所有 E 翻转/CGEND 后重建行均 `z=None`;引擎画面
    /// 该层始终处于 998 档(配置页在其按钮带之下),⇒ 属性保留。
    /// 引擎语义(成果 161 同一依据):`z=None` 只表示「本命令不改 z」;
    /// 新建对象的 z 取**子系统缺省档位**,不是 0。故只有脚本给过 Z 才可
    /// 作为继承值下发 —— 从未给过 Z 的对象返回 `None`,让播放器落到自己的
    /// 档位兜底(`vm_ui_z_for_path`)。
    pub fn cg_z(&self, name: &str) -> Option<i64> {
        self.cg_registry
            .get(name.as_bytes())
            .filter(|st| st.z_explicit)
            .map(|st| st.z)
    }

    /// 注册表条数(诊断日志用)。
    pub fn cg_registry_len(&self) -> usize {
        self.cg_registry.len()
    }

    /// 装载脚本。组模型校验失败(非 v555 形态)→ `Err`。
    ///
    /// 帧栈预置**基帧**(引擎 record[0],GOSUB 反编译 004428c0/0044b418
    /// 定性:任务创建即有 depth=0 的当前帧记录,顶层脚本的帧局部/返回值
    /// 区都落在它上面;RETURN 于基帧 = depth 0 → 脚本结束路径)。
    pub fn load(script: YstbFile) -> Result<Self> {
        // 运行期命令分派按**脚本自己声明的引擎版本**选内置表；版本未知则置为
        // 「未解析」（任何命令都走未实现路径），由启动链用游戏自身的 YSCM 覆盖。
        // 绝不按一个无关版本的内置表静默执行错误语义。
        let engine_version = script.header().version;
        let dispatch = CommandDispatch::for_engine_version(engine_version)
            .unwrap_or_else(CommandDispatch::unresolved);
        let ctx = ScriptCtx::parse(0, script)?;
        Ok(Self {
            ctx,
            host: None,
            pc: 0,
            state: VmState::Idle,
            events: Vec::new(),
            event_domains: Vec::new(),
            cur_domain: 0,
            fps: None,
            labels: HashMap::new(),
            strict: true,
            store: VariableStore::new(),
            frames: vec![GosubFrame {
                return_pc: 0,
                script_id: 0,
                locals: VariableStore::new(),
                if_nest_depth: 0,
                loop_depth: 0,
            }],
            if_nest: Vec::new(),
            loops: Vec::new(),
            executed_groups: 0,
            executed_windows: 0,
            returncode_hits: 0,
            declared_scripts: std::collections::HashSet::new(),
            input_cursor: (0, 0),
            ysvr_kind2: Vec::new(),
            ysvr_globals: Vec::new(),
            ysvr_kind2_applied: std::collections::HashSet::new(),
            epoch: std::time::Instant::now(),
            file_probe: None,
            audio_probe: None,
            scheduler: Some({
                let mut sch = crate::task::TaskScheduler::from_env();
                sch.spawn(crate::task::TaskSeed {
                    id: crate::task::MAIN_TASK_ID.to_vec(),
                    script_id: 0,
                    pc: 0,
                    enabled: true,
                    is_main: true,
                });
                sch
            }),
            task_slots: HashMap::new(),
            task_names: Vec::new(),
            pending_effects: Vec::new(),
            windows_buf: Vec::new(),
            bytes_buf: Vec::new(),
            slot_scratch: Vec::new(),
            insn_cache: rustc_hash::FxHashMap::default(),
            chain_now_ms: 0,
            text_channel: crate::scenario::TextChannel::new(),
            font_px: None,
            font_color: None,
            font_bold: None,
            font_drawmode: None,
            font_sel: None,
            font_aa: None,
            font_gradation: None,
            font_rotation: None,
            error_levels: HashMap::new(),
            error_code: None,
            ystl: None,
            emote_runtime: false,
            screensaver_suppressed: false,
            background_exec: false,
            timecount_total: [0; 4],
            timecount_start: [None; 4],
            logical_clock: false,
            logical_frames: 0,
            unsup_summary: std::collections::BTreeMap::new(),
            emit_group_events: true,
            hot_hist: HashMap::new(),
            hot_cmd: HashMap::new(),
            hot_script: HashMap::new(),
            screen_w: 1920,
            screen_h: 1080,
            cg_registry: HashMap::new(),
            dispatch: std::sync::Arc::new(dispatch),
            game_profile: None,
            dispatch_cache: HashMap::new(),
            cur_command: None,
            err_cause: None,
            err_log_last: None,
            err_log_repeat: 0,
            err_site_warned: std::cell::RefCell::new(std::collections::HashSet::new()),
            swallow_warned: std::cell::RefCell::new(std::collections::HashSet::new()),
            unsup_alerted: std::cell::RefCell::new(std::collections::HashSet::new()),
            label_miss_alerted: std::cell::RefCell::new(std::collections::HashSet::new()),
            unknown_slot_warned: std::cell::RefCell::new(std::collections::HashSet::new()),
            fatal_reporting: false,
            missing_feature_warned: std::cell::RefCell::new(std::collections::HashSet::new()),
            slot_fallback_uses: std::cell::Cell::new(0),
            slot_fallback_warned: std::cell::RefCell::new(std::collections::HashSet::new()),
            // 种子 = 时间派生(引擎种子未取证;见字段文档,UNVERIFIED)
            math_rng: crate::math::Mt19937::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
                    .unwrap_or(0),
            ),
            input: None,
            input_vars: None,
            input_overlay: HashMap::new(),
            input_writes: Vec::new(),
            input_settings: crate::input::EngineInputSettings::default(),
        })
    }

    /// 设置游戏自身 YSCM 建的命令 profile（**权威表**）。
    ///
    /// 启动链在装载 `ysc.ybn` 后调用。此后每个脚本按**自己头版本**取表：
    /// - 头版本 == 本 profile 版本 → 用本表（游戏自己的 YSCM，权威）；
    /// - 头版本不同 → 用该版本的内置表（官方 SDK 表），并落一条去重告警
    ///   （混版本封包是事实，不是猜测：脚本头自己声明了版本）。
    ///
    /// 传 `None` = 无 YSCM：所有脚本按头版本取内置表，未知版本走「未解析」。
    pub fn set_command_profile(&mut self, profile: Option<yuris_core::CommandProfile>) {
        if let Some(p) = &profile {
            self.dispatch_cache.insert(
                p.engine_version(),
                std::sync::Arc::new(CommandDispatch::new(p.clone())),
            );
        }
        self.game_profile = profile;
        self.refresh_dispatch();
    }

    /// 直接注入已建好的分派表（测试/工具用；不登记为游戏 profile）。
    pub fn set_command_dispatch(&mut self, dispatch: CommandDispatch) {
        self.dispatch = std::sync::Arc::new(dispatch);
    }

    /// **取生效分派表**（分派语义的唯一定义点）。
    ///
    /// ## 引擎真值（勘误 2026-10-18）
    ///
    /// 引擎只装载**一张**命令表：启动链读游戏自己的 `ysc.ybn`，填进
    /// `0x7aae40` 的 119 项分派表（初始化器 `FUN_0045cd30`），分派是
    /// `CALL [handler[cmd_index]]`。**没有第二张表，也没有按脚本头版本
    /// 切换表的逻辑。**
    ///
    /// 因此本函数：
    /// 1. **有游戏 YSCM → 一律用它**（与当前脚本的头版本无关）；
    /// 2. 没有游戏 YSCM → **未解析空表**（任何命令显式报未实现）。
    ///
    /// ## 为什么不能拿内置 SDK 表兜底
    ///
    /// `SDK_479/495/V555` 那几张表是**给人查命令清单的参考资料**
    /// （`docs/opcode/yscm-commands-*.json` 的镜像），不是运行期语义。
    /// 拿它兜底有两个硬伤：
    /// - 同一封包里不同头版本的脚本会被两套序号解释，语义不一致；
    /// - 内置表覆盖不到商业游戏私有的命令增删 ⇒ **静默错解**。
    ///
    /// 实测该作 160 个脚本里 5 个是 v479 头，但它们在 YSLB 里
    /// **一条标签都没有**（139/160 个脚本被引用，这 5 个全不在其中）
    /// ⇒ 引擎从不加载它们（见 boot 侧的「声明按需消费」）。
    pub fn dispatch_for_version(&mut self, _version: u32) -> std::sync::Arc<CommandDispatch> {
        // 缓存槽：有游戏表 / 无表 各一格（与任何真实版本号区分）。
        const GAME_SLOT: u32 = u32::MAX;
        const NONE_SLOT: u32 = u32::MAX - 1;
        let slot = if self.game_profile.is_some() {
            GAME_SLOT
        } else {
            NONE_SLOT
        };
        if let Some(d) = self.dispatch_cache.get(&slot) {
            return std::sync::Arc::clone(d);
        }
        let d = std::sync::Arc::new(match &self.game_profile {
            Some(p) => CommandDispatch::new(p.clone()),
            None => CommandDispatch::unresolved(),
        });
        self.dispatch_cache.insert(slot, std::sync::Arc::clone(&d));
        d
    }

    /// 刷新生效分派表（当前脚本切换时调用）。
    ///
    /// - **有游戏 YSCM** → 一律换成游戏表（与脚本头版本无关，引擎语义）；
    /// - **无游戏 YSCM**（工具/测试/内存宿主）→ **保持 `load()` 已建立的表**
    ///   不动。否则切脚本会把该表覆盖成「未解析空表」，把本来能跑的分派
    ///   清空 —— 实测导致跨脚本 GOSUB 在目标脚本首条命令就报未实现。
    fn refresh_dispatch(&mut self) {
        if self.game_profile.is_some() {
            let version = self.ctx.script.header().version;
            self.dispatch = self.dispatch_for_version(version);
        }
    }

    /// 当前分派表（= 当前脚本头版本对应的表）。
    pub fn command_dispatch(&self) -> &CommandDispatch {
        self.dispatch.as_ref()
    }

    /// **游戏引擎版本号**（YSCM 版本；无 YSCM 时回落当前脚本头版本）。
    ///
    /// 与 [`Self::command_dispatch`] 的 `engine_version()` 不同：后者是
    /// **当前脚本**的编译器版本，混版本封包里会随脚本切换。需要"引擎本身"
    /// 版本的调用方（输入子系统映射、诊断）必须用本方法。
    pub fn engine_version(&self) -> u32 {
        self.game_profile
            .as_ref()
            .map(|p| p.engine_version())
            .unwrap_or_else(|| self.ctx.script.header().version)
    }

    /// 分派表是否已解析（版本未知且未注入 YSCM 时为 `false`）。
    pub fn is_dispatch_resolved(&self) -> bool {
        self.dispatch.is_resolved()
    }

    /// 当前分派的来源描述（诊断/日志用）。
    pub fn dispatch_source(&self) -> &str {
        self.dispatch.profile().source()
    }

    /// 序号是否为声明族（含局部 `FLT`/`INT`/`STR` 与 `F_*`/`G_*`/`S_*`/`VAR`）。
    ///
    /// 取代旧的 `is_declaration(cmd: u8)` 自由函数 —— 旧实现内嵌某一版的
    /// 序号区间，跨版本会误判。
    pub fn is_declaration(&self, index: u8) -> bool {
        self.kind_of(index).is_declaration()
    }

    /// 序号 → 命令种类（运行期分派入口）。
    pub fn kind_of(&self, index: u8) -> CmdKind {
        self.dispatch.kind(index as u16)
    }

    /// 置错误态(**唯一入口**)。
    ///
    /// 存在的理由:有若干处理器设了错误态却不把信息冒泡出去(事件流里看不
    /// 出是哪一组),排查时只能靠"状态突然变 Error"猜。集中到这里后……
    ///
    /// **可观测性(2026-10-17,strict 挂起「根因哪儿都翻不到」的修复)**:
    /// 每个首次到达的 `(脚本, pc)` 现场**无条件**落一条 warn(旧的
    /// `YURIS_LOG_ERR` 门控默认开 — 该环境变量仍可强制全量重打)。实测
    /// 某款游戏在同一个现场**每帧重入**(60fps ⇒ 每帧一条),不去重就会
    /// 刷满一屏;同时把现场快照进 `err_cause`,供 `run_main` 的挡回消息
    /// 携带根因 —— 旧版这条消息不带任何原因,用户只能看到同名刷屏。
    fn set_error_state(&mut self) {
        if self.state != VmState::Error {
            let site = (self.ctx.script_id, self.pc);
            let is_first_here = self.err_site_warned.borrow_mut().insert(site);
            if is_first_here {
                self.log_error_site();
                if self.err_cause.is_none() {
                    self.err_cause = Some(self.error_site_cause(
                        "现场未被处理器注明原因(见同刻的 [vm-err];run_main 挡回根因唯一入口)".into(),
                    ));
                }
            } else if std::env::var("YURIS_LOG_ERR").is_ok() {
                self.log_error_site();
            }
        }
        self.state = VmState::Error;
    }

    /// `set_error_state` 的现场告警一行(`warn` 级)。
    fn log_error_site(&self) {
        let kind = self.kind_of(
            self.ctx
                .groups
                .get(self.pc)
                .map(|g| g.command_type)
                .unwrap_or(0),
        );
        yuris_log::warn!(
            "[vm-err] s{} pc={} cmd={:?} 帧深={} if栈={} 循环栈={}",
            self.ctx.script_id,
            self.pc,
            kind,
            self.frames.len(),
            self.if_nest.len(),
            self.loops.len(),
        );
    }

    /// Error 现场快照文本(`run_main` 挡回消息与 [`Self::log_error_site`]
    /// 共用;`detail` 由调用方在**已知道原因**时给)。
    fn error_site_cause(&self, detail: String) -> String {
        format!(
            "s{} pc={} {}",
            self.ctx.script_id, self.pc, detail
        )
    }

    /// **吞错继续**场景的统一告警(每 `(脚本, pc)` 只一次)。
    ///
    /// 引擎若干处理器(子系统参数/TASK 参数/声明初值非严格路径等)对参数
    /// 求值失败**不传播**:按未知值继续(实测引擎行为)。旧实现下这些
    /// 调用方把 `eval_window_condition` 的 Err 吞进事件流文本(`err:…`),
    /// strict 挂起时根因一行都不见(2026-10-17 s66 pc5355 实测)。现统一
    /// 落 warn(一次性)+ 根因快照。
    fn note_swallowed(&mut self, pc: usize, command: u8, what: &str, e: &Error) {
        if self
            .swallow_warned
            .borrow_mut()
            .insert((self.ctx.script_id, pc))
        {
            let cname = self
                .dispatch
                .name_of(command as u16)
                .map(|n| n.to_string())
                .unwrap_or_else(|| format!("<未命名 cmd{command}>"));
            yuris_log::alert!(
                "参数求值失败但被吞掉(引擎容忍路径)\n\
                 命令: {cname} (idx {command})\n\
                 上下文: {what}\n\
                 现场: script {} pc {pc}\n\
                 错误: {e}\n\
                 处置: {}",
                self.ctx.script_id,
                if self.strict {
                    "strict —— 根因已记录,命令继续按引擎容忍语义执行(不是静默:本条即证据)"
                } else {
                    "lenient —— 记录后继续"
                }
            );
        }
        if self.err_cause.is_none() {
            self.err_cause = Some(format!(
                "{what} 参数求值失败(吞错继续) s{} pc={pc}: {e}",
                self.ctx.script_id
            ));
        }
    }

    /// run 错误的**同因限频**落日志(主线/任务域统一入口)。
    ///
    /// 实测 strict 挂起后每帧 60fps 生成一条同名 ERROR(「VM 处于 Error
    /// 态」),数分钟即数千行,把真根因淹在刷屏里(2026-10-17 用户实机报告)。
    /// 规则:**首次** error 级直落;同文本重复只计数;每满 120 次(60fps ≈ 2s)
    /// 用 warn 提醒一次累计量。不同因 → 重新计。
    pub fn log_run_error(&mut self, reason: &str) {
        if self.err_log_last.as_deref() == Some(reason) {
            self.err_log_repeat += 1;
            if self
                .err_log_repeat
                .checked_sub(1)
                .filter(|c| *c % 120 == 0)
                .is_some()
            {
                let n = self.err_log_repeat - 1;
                yuris_log::warn!("[vm] 同因错误已连续 {} 次(限频中): {reason}", n + 1);
            }
            return;
        }
        self.err_log_last = Some(reason.to_string());
        self.err_log_repeat = 0;
        yuris_log::error!("[vm] 错误: {reason}");
    }

    /// **参数名 → 槽位**（当前命令分派表；`None` = 无参数表或无此参数名）。
    ///
    /// YSTB 参数窗口的 `tag & 0xff` 即此槽位，而槽位号**逐版本漂移**（实测
    /// `CG.FILE`：v555 槽 46 / v494 槽 33 / v479 槽 29）。处理器一律经此取槽：
    /// 写死数字会让该参数在别的版本上**永远读不到**（实测后果 = CG 的 `FILE`
    /// 为空 → 纹理不装载 → 全黑）。
    fn slot_of_param(&self, command_index: u8, param: &str) -> Option<u8> {
        self.dispatch.slot(command_index, param)
    }

    /// 槽位是否命中参数 `param`。
    ///
    /// 无参数表（旧式 profile / 合成脚本）→ 回落 `fallback`（该参数在
    /// **内置 v555 表**中的槽号），保持既有行为；有参数表则**严格按名匹配**，
    /// 不再回落 —— 否则跨版本会读到相邻参数而静默出错。
    ///
    /// **回落不静默（T3）**：每次回落计入 [`Self::slot_fallback_uses`] 并对
    /// 每个 `(命令, 参数)` 首次打一条 stderr 告警（`YURIS_QUIET_SLOT_FALLBACK=1`
    /// 可关）—— 新的无参表版本进来时**必须可观测**，不能再无声读错槽。
    fn slot_is(&self, command_index: u8, slot: u8, param: &str, fallback: u8) -> bool {
        self.param_slot(command_index, param, fallback) == Some(slot)
    }

    /// 解析参数槽位：有参数表 → 按**参数名**；否则 → 回落 `fallback`（可观测）。
    ///
    /// 返回 `None` = 有参数表但该版本没有这个参数名（该参数在此版本不存在，
    /// 处理器须跳过，不得用别的槽位顶替）。
    fn param_slot(&self, command_index: u8, param: &str, fallback: u8) -> Option<u8> {
        if self.dispatch.profile().has_params() {
            self.dispatch.slot(command_index, param)
        } else {
            self.note_slot_fallback(command_index, param, fallback);
            Some(fallback)
        }
    }

    /// 记录一次「无参数表 → v555 槽号回落」(T3)。
    fn note_slot_fallback(&self, command_index: u8, param: &str, fallback: u8) {
        self.slot_fallback_uses.set(self.slot_fallback_uses.get() + 1);
        if std::env::var_os("YURIS_QUIET_SLOT_FALLBACK").is_some() {
            return;
        }
        let first = self
            .slot_fallback_warned
            .borrow_mut()
            .insert((command_index, param.to_string()));
        if first {
            let cname = self
                .dispatch
                .name_of(command_index as u16)
                .unwrap_or("<未命名>");
            yuris_log::warn!(
                "[vm] 槽位回落(v555 UNVERIFIED):{cname}.{param} → 槽 {fallback};                    分派 profile「{}」无参数表(v{}),按名解析不可用 —— 若这是新版本                     游戏,此处读到的参数可能是别的槽",
                self.dispatch.profile().source(),
                self.dispatch.profile().engine_version(),
            );
        }
    }

    /// 绑定跨脚本宿主(启用跨脚本 GO/GOSUB)。
    pub fn set_host(&mut self, host: Box<dyn ScriptHost>) {
        self.host = Some(host);
    }

    /// 注入光标逻辑坐标(@133/@138;播放器每帧调用,P9.2)。
    pub fn set_input_cursor(&mut self, x: i64, y: i64) {
        self.input_cursor = (x, y);
    }

    /// 注入游戏虚拟 FS 存在性索引(FILEINFO EXIST 查询用;P5.2)。
    pub fn set_file_probe(&mut self, probe: std::sync::Arc<crate::host::PacFileIndex>) {
        self.file_probe = Some(probe);
    }

    /// 注入音频状态查询通道（`SOUNDINFO` 用；宿主实现
    /// [`crate::audio_probe::SoundStateProbe`]）。
    pub fn set_audio_probe(
        &mut self,
        probe: std::sync::Arc<dyn crate::audio_probe::SoundStateProbe>,
    ) {
        self.audio_probe = Some(probe);
    }

    /// 音频状态查询（`None` = 未注入通道或该字段无真值）。
    pub(crate) fn audio_query(
        &self,
        layer_id: &str,
        field: crate::audio_probe::SoundField,
    ) -> Option<crate::audio_probe::SoundInfoValue> {
        self.audio_probe.as_ref()?.query(layer_id, field)
    }

    /// 标记脚本的声明组已消费(boot 全量消费后调用;防止 switch_script
    /// 的按需重消费用声明期边界**覆盖 YSVR 终态** —— 引擎描述符分配仅在
    /// boot 一次,成果 59)。
    pub fn mark_declarations_consumed(&mut self, script_id: u16) {
        self.declared_scripts.insert(script_id);
    }

    /// 注入标签表(name → (目标组号, 脚本号));典型来源为
    /// [`yuris_format::yslb::YslbTable`](yuris_format::yslb::YslbTable)。
    pub fn set_labels(&mut self, labels: HashMap<Vec<u8>, (u32, u16)>) {
        self.labels = labels;
    }

    /// 设置当前脚本号(跨脚本跳转判定;`yst%05d.ybn` 的 %05d)。
    pub fn set_script_id(&mut self, id: u16) {
        self.ctx.script_id = id;
    }

    /// strict 模式开关(默认 true:遇 Unsupported 挂起)。
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// 当前状态。
    pub fn state(&self) -> VmState {
        self.state
    }

    /// 程序计数器(组下标)。
    pub fn pc(&self) -> usize {
        self.pc
    }

    /// 已消费事件。
    pub fn events(&self) -> &[VmEvent] {
        &self.events
    }

    /// 与 [`Self::events`] 平行的**域标记**(0 = 主线,1..= 任务创建序;
    /// P10 域调度,成果 161)。长度恒等于 `events().len()`;消费侧按域合成
    /// 呈现/做诊断时用。**刻意不进 golden JSONL** —— golden 快照序列化的
    /// 只有 `VmEvent` 本身,本 vec 对其不可见(零回归)。
    pub fn event_domains(&self) -> &[u8] {
        &self.event_domains
    }

    /// 主线域是否在休息(FRAME 计数未尽或 TIME 期限未到;诊断/测试用,
    /// D1 回归哨兵:裸让出后必须为 true = 恰休一帧)。
    pub fn main_wait_pending(&self) -> bool {
        self.scheduler
            .as_ref()
            .and_then(|s| s.main_node())
            .map(|n| n.wait_frames > 0 || n.deadline_ms.is_some())
            .unwrap_or(false)
    }

    /// 事件入流的**唯一包装点**(P10):所有处理器经此推送,自动携带当前
    /// 域标记。域 = 发射时的 [`Self::cur_domain`](主线域步进期间恒 0;
    /// 任务域由 `step_tasks`/`pump_frame` 在切换执行位时同步切换)。
    fn push_event(&mut self, ev: VmEvent) {
        // 逐组执行事件是诊断面(引擎不产生):播放器关闭,避免 85 万组/帧
        // 时产生同等数量的事件对象。
        if !self.emit_group_events && matches!(ev, VmEvent::GroupExecuted { .. }) {
            return;
        }
        // Unsupported 汇总(不受 `emit_group_events` 影响 —— 排查主线)。
        if let VmEvent::Unsupported { pc, command, reason } = &ev {
            let name = self
                .dispatch
                .name_of(*command as u16)
                .map(|n| n.to_string())
                .unwrap_or_else(|| format!("idx{command}"));
            self.unsup_summary
                .entry(name)
                .and_modify(|h| h.count += 1)
                .or_insert_with(|| UnsupportedHit {
                    count: 1,
                    script_id: self.ctx.script_id,
                    pc: *pc,
                    reason: reason.clone(),
                });
        }
        self.event_domains.push(self.cur_domain);
        self.events.push(ev);
    }

    /// 统一文本通道(只读)。
    pub fn text_channel(&self) -> &crate::scenario::TextChannel {
        &self.text_channel
    }

    /// 取走本帧待呈现的全部文本(内联 + 明文剧本;按入队序)。
    pub fn drain_text_lines(&mut self) -> Vec<crate::scenario::TextLine> {
        self.text_channel.take()
    }

    /// **取走**全部待消费事件(连同平行域标),缓冲就地清空(零克隆)。
    pub fn take_events(&mut self) -> (Vec<VmEvent>, Vec<u8>) {
        let evs = std::mem::take(&mut self.events);
        let doms = std::mem::take(&mut self.event_domains);
        (evs, doms)
    }

    /// 开关逐组执行事件(缺省 true = golden/单测语义不变)。
    pub fn set_group_events(&mut self, on: bool) {
        self.emit_group_events = on;
    }

    /// 当前字号(`FONT` 的 SX/SY;`None` = 脚本尚未设置)。
    pub fn font_size(&self) -> Option<(i64, i64)> {
        self.font_px
    }

    /// 当前字体颜色(`COLOR`,`COLOR2`;0xRRGGBB)。
    pub fn font_colors(&self) -> Option<(i64, i64)> {
        self.font_color
    }

    /// 当前字体加粗位(`BOLD`)。
    pub fn font_bold(&self) -> Option<i64> {
        self.font_bold
    }

    /// 当前字体描画模式(`DRAWMODE`)。
    pub fn font_drawmode(&self) -> Option<i64> {
        self.font_drawmode
    }

    /// 启用/关闭逻辑时钟(播放器启用;单测保持墙钟)。
    pub fn set_logical_clock(&mut self, on: bool) {
        self.logical_clock = on;
        self.logical_frames = 0;
    }

    /// 逻辑时钟是否启用。
    pub fn logical_clock(&self) -> bool {
        self.logical_clock
    }

    /// 写入标题选择槽(`@1035`;UI 点击 → 脚本状态机入口)。
    pub fn set_title_choice(&mut self, slot: i64) {
        let r = yuris_value::VarRef {
            space: yuris_value::VarSpace::At,
            id: 1035,
        };
        self.store.set(&r, yuris_value::Value::Int(slot));
        yuris_log::info!("[vm] @1035(标题选择)← {slot}");
    }

    /// **打印 Unsupported 汇总**(不支持的命令被跳过后必须可查)。
    pub fn log_unsupported_summary(&self, tag: &str) {
        if self.unsup_summary.is_empty() {
            yuris_log::info!("[vm-unsup] {tag}: 无被跳过的命令");
            return;
        }
        let total: u64 = self.unsup_summary.values().map(|h| h.count).sum();
        yuris_log::warn!(
            "[vm-unsup] {tag}: {} 种命令被跳过,共 {total} 次",
            self.unsup_summary.len()
        );
        for (name, h) in &self.unsup_summary {
            yuris_log::warn!(
                "[vm-unsup]   {name} ×{}  首次 s{} pc{}  {}",
                h.count,
                h.script_id,
                h.pc,
                h.reason
            );
        }
    }

    /// 变量存储(只读)。
    pub fn store(&self) -> &VariableStore {
        &self.store
    }

    /// GOSUB 帧栈深度(诊断/测试用)。
    pub fn frame_depth(&self) -> usize {
        self.frames.len()
    }

    /// 当前帧 locals 可变访问(基帧恒在,成果 56;诊断/测试用)。
    ///
    /// 引擎 trace 样本中的 @53/@55 族值位于引擎**当前帧记录**(实参/返回值区),
    /// 测试回放时以此播种等价状态(全局 store 是 id≥1000 域,勿混)。
    pub fn frame_locals_mut(&mut self) -> &mut VariableStore {
        match self.frames.last_mut() {
            Some(f) => &mut f.locals,
            None => unreachable!("基帧恒在(load 预置;RETURN 保持 len≥1)"),
        }
    }

    /// 调用帧栈快照(诊断/测试用;返回各帧 return_pc 与脚本号)。
    pub fn frames_snapshot(&self) -> Vec<(usize, u16)> {
        self.frames
            .iter()
            .map(|f| (f.return_pc, f.script_id))
            .collect()
    }

    /// 当前帧局部可读变量数(诊断/测试用)。
    pub fn frame_local_count(&self) -> usize {
        self.frames.last().map(|f| f.locals.len()).unwrap_or(0)
    }

    /// 变量存储(可变;供外部预置全局变量)。
    pub fn store_mut(&mut self) -> &mut VariableStore {
        &mut self.store
    }

    /// 应用 YSVR 变量定义表(启动链 FUN_0046b63c 的「应用初值」步)。
    ///
    /// 消费时机(FUN_00451348 反编译,成果 59h):
    /// - boot(-1 调用)应用 **kind 1 与 kind 3**;
    /// - kind 2(按脚本)在该脚本**首次加载**时应用
    ///   ([`Self::apply_ysvr_for_script`],switch_script 钩子);
    /// - 抽样 60 个 kind2 变量,全部只在所属脚本被引用(探针全语料扫描);
    /// - 端到端证据:es.BT(script22) 读 $1895(kind2, script=22, STR[100]),
    ///   只应用 kind1 时报「undefined array variable」。
    /// - @5256(kind3, script=157, FLT[101,5])须在 boot 应用,否则 s177 的
    ///   2D 访问踩「declared 1」(实证;旧「kind3 不猜跳过」被反编译证伪)。
    pub fn apply_ysvr(&mut self, ysvr: &yuris_format::ysvr::YsvrTable) -> Result<usize> {
        let mut applied = 0usize;
        for e in ysvr.entries() {
            if e.kind == 2 {
                // 按脚本初值:挂起,待该脚本首次加载时应用
                self.ysvr_kind2.push(e.clone());
                continue;
            }
            if e.kind == 1 {
                // 全局初值快照(运行期 VARACT INIT 恢复组的镜像)
                self.ysvr_globals.push(e.clone());
            }
            self.apply_ysvr_entry(e)?;
            applied += 1;
        }
        Ok(applied)
    }

    /// 运行期 `VARACT[INIT/G_*]` 全局恢复(引擎 0x454e00→0x455b60;旁线仓库
    /// Round20 Confirmed,v494/v555 同族)。
    ///
    /// 语义 = 重放 YSVR `kind==1` 记录:mask 按 `(记录 ty, 记录 category)`
    /// 12 位命中才整槽重建;`G_INT/G_INT2/G_INT3/G_INT4`(组 1..4)、
    /// `G_FLT*`、`G_STR*` 槽在场**且求值非 0** → 对应位允许;没有 G_* 槽 →
    /// 全 0 掩码 → 什么都不恢复(引擎\"只给 INIT\"实测行为)。
    ///
    /// 输入 = `(槽名 → 槽号)` 解析器与 `(槽号 → 求值)` 取值器(命令序号逐
    /// 版本漂移,槽位必须按**游戏自身 YSCM 参数名**解析,不得写死序号)。
    fn varact_restore_globals(
        &mut self,
        value_of_slot: &dyn Fn(u8) -> Option<yuris_value::Value>,
        g_slots: &[(&'static str, u8, Option<u8>)],
    ) -> Result<usize> {
        let mut allow = [false; 12]; // (ty-1)*4 + (组-1);ty: 1=INT 2=FLT 3=STR
        for (name, ty, slot) in g_slots {
            let Some(slot) = slot else { continue };
            let Some(v) = value_of_slot(*slot) else {
                continue;
            };
            if v.as_int_opt().map(|n| n != 0).unwrap_or(true) {
                let ty = *ty;
                let group: u8 = name
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .fold(0u8, |a, c| {
                        a.saturating_mul(10)
                            .saturating_add(c.to_digit(10).unwrap() as u8)
                    });
                let group = group.clamp(1, 4);
                let bit = (ty as usize - 1) * 4 + (group as usize - 1);
                allow[bit] = true;
            }
        }
        let hit = self
            .ysvr_globals
            .iter()
            .filter(|e| {
                e.kind == 1
                    && e.ty >= 1
                    && e.ty <= 3
                    && e.category >= 1
                    && e.category <= 4
                    && allow[(e.ty as usize - 1) * 4 + (e.category as usize - 1)]
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut n = 0usize;
        for e in &hit {
            self.apply_ysvr_entry(e)?; // 重放初值(引擎 = 镜像整槽重建)
            n += 1;
        }
        if debug_frame_enabled() {
            yuris_log::trace_forced!("[varact] INIT 恢复掩码={allow:?} 重放 {n} 条");
        }
        Ok(n)
    }

    /// 应用某脚本名下的 kind2 条目(脚本首次加载时;幂等)。
    pub fn apply_ysvr_for_script(&mut self, script_id: u16) -> Result<usize> {
        if self.ysvr_kind2_applied.contains(&script_id) {
            return Ok(0);
        }
        self.ysvr_kind2_applied.insert(script_id);
        let entries: Vec<yuris_format::ysvr::YsvrEntry> = self
            .ysvr_kind2
            .iter()
            .filter(|e| e.script == script_id)
            .cloned()
            .collect();
        let mut applied = 0usize;
        for e in &entries {
            self.apply_ysvr_entry(e)?;
            applied += 1;
        }
        Ok(applied)
    }

    /// 应用单条 YSVR 条目(边界覆盖 + 初值)。
    fn apply_ysvr_entry(&mut self, e: &yuris_format::ysvr::YsvrEntry) -> Result<()> {
        use yuris_value::{ElemType, Value};
        if std::env::var("YURIS_WATCH_VAR")
            .ok()
            .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            == Some(e.var_id)
        {
            yuris_log::trace_forced!(
                "[watch] **YSVR** 应用 id={:#06x} kind={} ty={} bounds={:?} init={:?}",
                e.var_id, e.kind, e.ty, e.bounds, e.init
            );
        }
        let prefix: u8 = match e.ty {
            1 | 2 => 0x40, // INT/FLT -> @
            3 | 0 => 0x24, // STR -> $
            _other => {
                return Err(Error::Unimplemented("YSVR 条目类型 Unknown(不猜)"));
            }
        };
        let r = yuris_value::VarRef {
            space: yuris_value::VarSpace::from_prefix(prefix),
            id: e.var_id,
        };
        let v = match &e.init {
            yuris_format::ysvr::YsvrInit::Int(i) => Value::Int(*i),
            yuris_format::ysvr::YsvrInit::Float(f) => Value::Float(*f),
            // **勘误(2026-10-13,成果 119)**:旧实现 `Value::Str(b.clone())`
            // 把该字段当裸字符串 —— 错。它是**表达式指令流**(见
            // [`GroupVm::decode_ysvr_str_init`])。必改。
            yuris_format::ysvr::YsvrInit::Str(b) => {
                match Self::decode_ysvr_str_init(&mut self.store, b) {
                    Some(s) => Value::Str(s),
                    None => {
                        // 畸形初值:保留原字节(旧行为)但**留痕** ——
                        // 不静默把窗口字节当成"裸字符串"传下去。
                        yuris_log::warn!(
                            "[ysvr] 字符串初值无法按表达式解码 id={:#06x} {}B,按原字节保留",
                            e.var_id,
                            b.len()
                        );
                        Value::Str(b.clone())
                    }
                }
            }
            yuris_format::ysvr::YsvrInit::None => {
                // 仅声明无初值:数组按边界清零,标量按 type 默认
                if e.bounds.is_empty() {
                    // 形态互斥(引擎一份描述符):YSVR 后于声明应用 → 它的形态为准
                    self.store.clear_arrays_for_id(e.var_id);
                    if e.ty == 1 {
                        Value::Int(0)
                    } else if e.ty == 2 {
                        Value::Float(0.0)
                    } else if e.ty == 3 {
                        Value::Str(Vec::new())
                    } else {
                        return Ok(()); // ty==0 仅声明,无存储语义
                    }
                } else {
                    // 数组:declare_array + elem0(其余清零由 ArrayStorage::new)
                    let elem = if e.ty == 1 {
                        ElemType::Int
                    } else if e.ty == 2 {
                        ElemType::Float
                    } else {
                        ElemType::Str
                    };
                    self.store.declare_array(&r, elem, &e.bounds);
                    return Ok(());
                }
            }
        };
        // 形态互斥:YSVR 说标量就清掉同名数组,说数组就清掉同名标量。
        // (引擎一份描述符 —— 见 `VariableStore::clear_arrays_for_id` 的实测触发点)
        if e.bounds.is_empty() {
            self.store.clear_arrays_for_id(e.var_id);
            self.store.set(&r, v);
        } else {
            self.store.clear_scalars_for_id(e.var_id);
            let elem = if e.ty == 1 {
                ElemType::Int
            } else if e.ty == 2 {
                ElemType::Float
            } else {
                ElemType::Str
            };
            self.store.declare_array(&r, elem, &e.bounds);
            // **填满整个数组**(勘误 2026-10-17,成果 178):引擎 `ysv.ybn`
            // 的数组条目只记**一个**初值(`bounds=[20] init=-1`),脚本按任意
            // 下标读取时都期望拿到它。旧实现 `set_elem([0…0])` 只写元素 0,
            // 下标 ≥1 读到 0 ⇒ 脚本把「未指定」当显式 0 传给 SOUND,
            // `LOOP=0`(无限循环)导致语音/SE 永不停。
            self.store.fill_array(&r, v)?;
        }
        Ok(())
    }

    /// YSVR 字符串初值(`ty=3`)的**表达式解码**。
    ///
    /// **勘误(2026-10-13,成果 119)**:旧实现把该字段当裸字符串
    /// (`Value::Str(b.clone())`)。实机取证(本作 v494 `ysbin\ysv.ybn`)证伪:
    /// 该字段是**表达式指令流**(与 `.ybn` 窗口同一套编码:`4d`=PushStr +
    /// u16 长度 + 带定界符的字面量),引擎**求值**后才得到串。
    ///
    /// 证据链(全部可复现):
    /// - 全表形状(`cargo run -p yuris-tools --example ysvr_strinit -- <游戏>`):
    ///   `ty=3` 306 条 → 278 条无初值 / 28 条有初值;**28/28 首字节 = 0x4d**、
    ///   28/28 解码为**单条 PushStr**、0 条其他形态。
    /// - 字节对照:`id 0x040c` 初值 = `4d 10 00 22 "SCENARIO_START" 22`,
    ///   与脚本窗口(s20 g269 pstr 窗)里同一字面量的编码**逐字节一致**。
    /// - 语义对照(官方 SDK 0.495 示例 `ERIS/global.yst:63`):
    ///   `G_STR[$es.STARTLABEL="SCENARIO_START"]` ⇒ 期望值 `SCENARIO_START`。
    ///
    /// 旧行为的后果(实机实测):`$es.STARTLABEL` 变成 19 字节窗口字节 →
    /// `es.SCENARIO.FLOW`(s21 g8)的 `GO[#=$es.STARTLABEL]` 查不到标签 →
    /// 场景任务空转、一张图都不画 → **画面恒黑**;`id 0x040d`
    /// (`es.ScenarioScript`)同类污染 → 任务名显示为 `M "es.ScenarioScript"`。
    ///
    /// 返回 `None` = 无法按表达式解码(畸形),由调用方决定口径(现为保留原字节 + 留痕)。
    fn decode_ysvr_str_init(store: &mut VariableStore, b: &[u8]) -> Option<Vec<u8>> {
        if b.is_empty() {
            return Some(Vec::new());
        }
        let instrs = yuris_script::decode_window(b).ok()?;
        let mut ev = yuris_script::Evaluator::with_locals(store, None);
        let stack = ev.eval_instructions(&instrs).ok()?;
        match stack.into_iter().next_back() {
            // 字符串字面量经求值器剥定界符(eval.rs 0x4d 路径),
            // 故这里拿到的就是引擎语义的串(`"ABC"` → `ABC`)。
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    /// 已执行组数(配额审计)。
    pub fn executed_groups(&self) -> usize {
        self.executed_groups
    }

    /// 已执行窗口数(配额审计)。
    pub fn executed_windows(&self) -> usize {
        self.executed_windows
    }

    /// `RETURNCODE`(调用点返回标记)执行次数；语义取证中,计数供对拍。
    pub fn returncode_hits(&self) -> u64 {
        self.returncode_hits
    }

    /// 组数。
    pub fn group_count(&self) -> usize {
        self.ctx.groups.len()
    }

    /// 当前脚本号。
    pub fn script_id(&self) -> u16 {
        self.ctx.script_id
    }

    /// 已注册任务名(`TASKINFO EXIST` 的查询面)。
    pub fn task_exists(&self, id: &[u8]) -> bool {
        self.task_names.iter().any(|t| t == id)
    }

    /// **丢弃已消费的事件并返回消费方的新游标(恒 0)。**
    ///
    /// 事件缓冲 [`Self::events`] 是**只增不减**的 `Vec`,消费方通常持有
    /// 「已消费到 `events()[cursor]`」的游标。长跑时该缓冲会无限增长
    /// (实测:数十帧即 1.16e6 条 ≈ 数十 MB;开启任务调度后事件量再涨
    /// ~15 倍)—— 消费方每轮调用一次本方法即可封顶。
    ///
    /// 用法:
    /// ```ignore
    /// let start = self.cursor;
    /// for ev in &self.vm.events()[start..] { /* 处理 */ }
    /// self.cursor = self.vm.trim_consumed_events(self.vm.events().len());
    /// ```
    pub fn trim_consumed_events(&mut self, consumed: usize) -> usize {
        let n = consumed.min(self.events.len());
        if n > 0 {
            self.events.drain(..n);
            self.event_domains.drain(..n);
        }
        0
    }

    /// 任务数(诊断/测试;**不含主线节点**)。
    pub fn task_count(&self) -> usize {
        self.task_names.len()
    }

    /// 已结束任务数(诊断)。
    pub fn task_finished_count(&self) -> usize {
        let Some(s) = self.scheduler.as_ref() else { return 0 };
        self.task_names
            .iter()
            .filter(|id| s.find_by_id(id).map(|n| n.finished).unwrap_or(false))
            .count()
    }

    /// 任务表逐条摘要(诊断:名字 / 脚本+组 / 步进次数 / 待命帧 / 结束位)。
    pub fn task_summary(&self) -> Vec<String> {
        let Some(s) = self.scheduler.as_ref() else { return Vec::new() };
        self.task_names
            .iter()
            .filter_map(|id| s.find_by_id(id))
            .map(|n| crate::driver::task_summary_line(self, n))
            .collect()
    }

    /// 任务执行位(诊断/摘要用)。
    pub(crate) fn task_slots(&self) -> &HashMap<Vec<u8>, SavedExec> {
        &self.task_slots
    }

    /// 切换到目标脚本上下文(取目标脚本 ctx,替换当前 ctx,重设 pc 与 script_id)。
    ///
    /// 需要已绑定 `ScriptHost`;无 host 或加载失败 → `Err`。
    fn switch_script(&mut self, script_id: u16, target_pc: usize) -> Result<()> {
        let Some(host) = &mut self.host else {
            return Err(Error::format(format!(
                "跨脚本跳转到 script {script_id} 但未绑定 ScriptHost"
            )));
        };
        let new_ctx = host.load(script_id)?;
        self.ctx = new_ctx;
        self.pc = target_pc;
        // **分派表随脚本切换**(2026-10-18):序号是编译器版本的属性;同一封包里
        // 可能存在不同版本产出的脚本(实测 5/160 为 v479)。整包共用一张表会
        // 把 IF/LET 窗口当成 FLT/G_FLT 声明来消费 —— 静默的错误语义。
        self.refresh_dispatch();
        // 引擎加载脚本时消费其声明组(INT/FLT/STR 建立变量,Confirmed 形态)。
        // 每脚本只全扫一次(已消费集跟踪):GO/GOSUB 高频跨脚本,重复全扫
        // 实测成为端到端主热点(声明幂等,重复消费无语义收益)。
        // 注意:boot 已全量消费并标记,此处只为未标记脚本兜底(不覆盖 YSVR 终态)。
        if self.declared_scripts.insert(script_id) {
            let ctx = &self.ctx;
            // 声明族判定也走**本脚本**版本的表(`refresh_dispatch` 已在上面跑过)。
            consume_declarations_into(&mut self.store, ctx, &self.dispatch)?;
        }
        // YSVR kind2(按脚本初值):脚本**首次加载**时应用(成果 59h;
        // 幂等,已应用脚本直接跳过)。
        self.apply_ysvr_for_script(script_id)?;
        Ok(())
    }

    /// 消费当前脚本的**标量声明组**(FLT 0x19 / INT 0x32 / STR 0x5c)。
    ///
    /// 引擎在脚本加载期建立变量描述符(样本探针:全语料 4248 组全部为
    /// 「w0=标量引用 `48/56 [prefix][id]` + w1=名字 M-串」形态,零数组)。
    /// 默认值 Int(0)/Float(0.0)/Str("")(**Likely**,引擎零初始化);
    /// 数组声明走 YSVR(kind1/2 初值条目含维数边界),不在本组。
    ///
    /// 幂等:已定义标量不覆盖(初值应用须发生在本消费之后)。
    pub fn consume_script_declarations(&mut self) -> Result<usize> {
        // 按**本脚本**头版本选表:混版本封包里用错表会把非声明命令当成声明。
        let version = self.ctx.script.header().version;
        let d = self.dispatch_for_version(version);
        let ctx = &self.ctx;
        consume_declarations_into(&mut self.store, ctx, &d)
    }

    /// 从挂起点恢复。
    pub fn resume(&mut self, _resp: ResumeResponse) -> Result<()> {
        if self.state == VmState::Error {
            return Err(Error::format("Error 态需先处理事件;骨架版不支持自动恢复"));
        }
        Ok(())
    }

    /// GO:解码唯一窗口的 M-串标签 → 标签表 → (目标组号, 脚本号)。
    /// 跨脚本 → Unsupported(单脚本 VM 不猜多脚本重绑)。
    fn exec_go(&mut self, windows: &[yuris_format::ystb::CommandSlot]) -> Result<GoOutcome> {
        let Some(w) = windows.first() else {
            return Ok(GoOutcome::Halted(self.halted(
                self.pc,
                cmd::GO,
                "GO 无窗口",
            )));
        };
        // 引擎:len 高位非 0 = 载入期已解析的标签表下标(id|0x10000000)。
        // 该标记只存在于引擎内存(载入后回写);文件形态恒为原始 len。
        if w.len & 0xF000_0000 != 0 {
            return Ok(GoOutcome::Halted(self.halted(
                self.pc,
                cmd::GO,
                "GO len 域带载入期解析标记(0x10000000)——文件形态不应出现;不猜",
            )));
        }
        let Some(bytes) = self.ctx.script.window_bytes_pooled_copy(w) else {
            return Ok(GoOutcome::Halted(self.halted(
                self.pc,
                cmd::GO,
                "GO 窗口越过池尾",
            )));
        };
        // **勘误(2026-10-13,成果 120)**:旧实现用 `mstring(&bytes)` 直接把标签窗
        // 按「M-串字面量」解析 —— 只认 `GO[#="NAME"]` 形态,变量标签窗一律
        // 落到「GO 窗口不是 M-串标签」而不跳转。实机取证证伪:
        //   - 本作 `es.SCENARIO.FLOW`(s21 g8) 的 GO 标签窗是**变量**
        //     (`PushVar($1036)` = `$es.STARTLABEL`;转储见成果 119 §证据链);
        //   - 官方 SDK 示例源码 `ERIS/es_ScenarioFlow.yst:42` 即
        //     `GO[#=$es.STARTLABEL]` —— 变量标签是**规范用法**,不是边角;
        //   - 后果:GO 不跳转 ⇒ 标题脚本 `SCENARIO_START`(s142) 实测 **0 组**
        //     执行 ⇒ 冷启动画面出不来。
        // 现改用与 GOSUB 完全相同的**表达式求值 + 查表**路径
        // ([`Self::resolve_label_in`],引擎 0045124c murmur2 查表),
        // 字面量(`4d` M-串)与变量标签同型处理。
        let literal_name = mstring(&bytes).map(|b| b.to_vec());
        match self.resolve_label_in(windows)? {
            Some((target, script_id)) => {
                if script_id != self.ctx.script_id {
                    // 跨脚本:需要 host;无 host → 挂起(旧行为)。
                    if self.host.is_none() {
                        return Ok(GoOutcome::Halted(self.halted(
                            self.pc,
                            cmd::GO,
                            format!("跨脚本 GO 到 script {script_id} 但未绑定 ScriptHost"),
                        )));
                    }
                    return Ok(GoOutcome::CrossScript { script_id, target });
                }
                Ok(GoOutcome::Jump(target))
            }
            None => {
                // **标签未命中 ≠ 命令未实现**(2026-10-18 政策)。
                //
                // 引擎真值(v494 `0x4459ed`):查表函数返负 → `0x445ada` 报错
                // 并 `return -1`(让出);脚本此后不再推进。实测触发点是
                // `s142 g29` 的 `GO[$es.SEL.GO]`,而 `$es.SEL.GO` 此刻被
                // `g24` 赋成 `SCENARIO_LOGO` —— 该标签在本作 YSLB 里
                // **不存在**(全表 0 命中)。这是 v555 时代脚本的残留。
                //
                // 处置:`alert`(绕过等级阈值、可见) + 事件化 + **跳过该 GO**
                // 继续执行。不升级为致命 —— 根因是游戏数据里不存在的标签,
                // 不是内核缺功能;一张错误窗会盖住整条启动链。
                let label = match &literal_name {
                    Some(n) => String::from_utf8_lossy(n).to_string(),
                    None => self
                        .resolve_label_text(windows)
                        .unwrap_or_else(|| "<求值失败>".to_string()),
                };
                yuris_log::alert!(
                    "标签未命中(已跳过该跳转)\n\
                     命令: GO\n\
                     现场: script {} pc {}\n\
                     标签: {:?}\n\
                     引擎真值: 查表失败 → 报错并让出(v494 0x4459da)\n\
                     说明: 该标签不在本作 YSLB 中(s142 g29 的 $es.SEL.GO 被\n\
                     置为 SCENARIO_LOGO —— v555 时代脚本残留)。\n\
                     处置: 跳过该 GO 继续执行;YURIS_TRACE_LABELS=1 看查表轨迹",
                    self.ctx.script_id,
                    self.pc,
                    label
                );
                self.push_event(VmEvent::LabelMiss {
                    pc: self.pc,
                    command: "GO".into(),
                    label,
                    skipped: true,
                });
                Ok(GoOutcome::NoJump)
            }
        }
    }

    /// 求值标签窗得到标签文本（诊断用；不做查表）。
    fn resolve_label_text(&mut self, windows: &[yuris_format::ystb::CommandSlot]) -> Option<String> {
        for w in windows {
            if w.tag & 0xFF != 0 || w.len == 0 {
                continue;
            }
            if let Ok(Some(Value::Str(name))) = self.eval_window_condition(Some(w)) {
                return Some(String::from_utf8_lossy(&name).to_string());
            }
        }
        None
    }

    /// 在组窗口中解析标签并查表(引擎 GOSUB 004428c0)。
    ///
    /// 标签窗 = B0==0 的窗,**按表达式求值** → 字符串 → 查表:
    /// 载入期预解析缓存(`DAT_00661760/00661280`,`iVar10 != 0`)命中直接用,
    /// 否则求值结果 FUN_0045124c murmur2 查表 —— 字面量 M-串(0x4d)与
    /// 变量标签(如 s22 g42 `var($1909)` = "ES.FIRST.LOOP")同型。
    /// 未命中 → Ok(None)(GOSUB 处理器按引擎静默续跑建模)。
    fn resolve_label_in(
        &mut self,
        windows: &[yuris_format::ystb::CommandSlot],
    ) -> Result<Option<(usize, u16)>> {
        for w in windows {
            if w.tag & 0xFF != 0 {
                continue; // 引擎只解析 B0==0 的窗口
            }
            if w.len == 0 {
                continue;
            }
            let Some(v) = self.eval_window_condition(Some(w))? else {
                continue;
            };
            // 标签名保留**原始字节**:YSLB 键 = SJIS 字节(如
            // `es.日本語…`),经 UTF-8 lossy 会变 U+FFFD 而查表 miss
            // ([29208] s147 g788 实证;成果 59b)。
            let Value::Str(name) = v else {
                continue;
            };
            // 诊断(`YURIS_TRACE_LABELS=1`):跨脚本分派全靠名字查表,静默
            // miss 时从事件流看不出「谁没查到谁」。默认关闭,零开销。
            let trace = std::env::var_os("YURIS_TRACE_LABELS").is_some();
            if let Some(&(target_pc, script_id)) = self.labels.get(&name) {
                if trace {
                    yuris_log::trace_forced!(
                        "[label] s{} pc{} hit {:?} → s{script_id} pc{target_pc}",
                        self.ctx.script_id,
                        self.pc,
                        String::from_utf8_lossy(&name)
                    );
                }
                return Ok(Some((target_pc as usize, script_id)));
            }
            // 未命中 → 引擎行为:静默跳过,继续找下一个
            if trace {
                yuris_log::trace_forced!(
                    "[label] s{} pc{} MISS {:?}",
                    self.ctx.script_id,
                    self.pc,
                    String::from_utf8_lossy(&name)
                );
            }
        }
        Ok(None)
    }

    /// 求值条件窗口(表达式字节码)。窗口缺失 → `Ok(None)`。
    fn eval_window_condition(
        &mut self,
        window: Option<&yuris_format::ystb::CommandSlot>,
    ) -> Result<Option<Value>> {
        let Some(w) = window else {
            return Ok(None);
        };
        if w.len == 0 {
            return Ok(None);
        }
        // **解码缓存优先(2026-10-18 性能修复)**：
        //
        // 旧顺序是「先 `window_bytes_pooled_copy`(分配 Vec) → 再查缓存」，
        // 而缓存命中率接近 100%（窗口字节在脚本装载后不变）。于是每次求值
        // 都白分配一次 Vec 再丢弃 —— 稳态每帧约 70 万次求值 ⇒ 70 万次
        // 无用分配。现在先查缓存，**只有未命中时才取字节**。
        //
        // 越界校验仍必须做：缓存键 `(script_id, offset, len)` 不含池长，
        // 而损坏脚本可能给出池外窗口 —— 那条路径仍要报错（且不能因为
        // 「缓存里恰好有这个键」就放过）。
        if !self.ctx.script.window_fits_pool(w) {
            let msg = format!("窗口 {}+{} 越过 content+part4 池尾", w.offset, w.len);
            if self.strict {
                self.set_error_state();
            }
            return Err(Error::format(msg));
        }
        let instrs = match self.decode_cached_slot(w) {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("条件窗口解码失败: {e}");
                if self.strict {
                    self.set_error_state();
                }
                return Err(Error::format(msg));
            }
        };
        let locals = self.frames.last().map(|f| &f.locals);
        let ctx = self.eval_ctx();
        let mut ev = Evaluator::with_locals(&mut self.store, locals);
        ctx.apply(&mut ev);
        // **零分配求值(2026-10-18)**:只取末值,缓冲由 VM 持有并原地复用。
        let mut scratch = std::mem::take(&mut self.slot_scratch);
        let eval_out = ev.eval_last_value_in(&instrs, &mut scratch);
        scratch.clear();
        self.slot_scratch = scratch;
        match eval_out {
            Ok(v) => Ok(v),
            Err(e) => {
                // **勘误(2026-10-17,复现 evidence: <game-b> s66 pc5355)**:
                // 旧实现在 strict 下毒化共享状态。毒化被「引擎容忍继续」的
                // 调用方(子系统参数求值、TASK 参数等 —— 引擎真值:参数
                // 求值失败=按未知值继续)吞掉 ⇒ 状态残留 Error,下一拍整个
                // VM 被「VM 处于 Error 态」挡死、根因只在事件流的
                // `evaluated[err:…]` 里 —— 日志一行不剩。现改为**只冒
                // Err**:致命与否由调用方的控制流决定(suspend 族已各自
                // 置 Error;容错族吞错继续,现场由 [`Self::note_swallowed`]
                // 落一次性告警)。
                Err(Error::format(format!(
                    "条件窗口求值失败: {e} s{} pc={}",
                    self.ctx.script_id, self.pc
                )))
            }
        }
    }

    /// LET 复合赋值的"当前值"装载:
    /// - 数组 → `get_elem`(下标 = 左值 indices;无下标 → 全零下标首元素)
    /// - 标量 → `get`
    /// 任一路径失败 → `None`(调用方按 code==0/复合码分别处理)。
    fn store_current(&self, lval: &yuris_script::LValueRef) -> Option<Value> {
        // P5:帧局部优先(成果 50)—— 复合赋值的"当前值"须读帧局部
        if let Some(fr) = self.frames.last() {
            if fr.locals.has_array(&lval.var) {
                let indices: Vec<i64> = if lval.indices.is_empty() {
                    let dims = fr.locals.array_dims(&lval.var).unwrap_or_default();
                    vec![0; dims.len()]
                } else {
                    lval.indices.clone()
                };
                return fr.locals.get_elem(&lval.var, &indices).cloned().ok();
            }
            if lval.indices.is_empty() {
                if let Some(v) = fr.locals.get_opt(&lval.var) {
                    return Some(v.clone());
                }
            }
        }
        if self.store.has_array(&lval.var) {
            let indices: Vec<i64> = if lval.indices.is_empty() {
                let dims = self.store.array_dims(&lval.var).unwrap_or_default();
                vec![0; dims.len()]
            } else {
                lval.indices.clone()
            };
            return self.store.get_elem(&lval.var, &indices).cloned().ok();
        }
        self.store.get(&lval.var).cloned().ok()
    }

    /// 以给定帧局部实参**直接调用**一个标签(内部构造 GOSUB 帧)。
    ///
    /// 用途:scenario→VM 桥的地基 + 取证工具 —— 让引擎自己的桥宏
    /// (如 `ES.SCR.T1`)以样本参数运行,产出真实 CG 坐标(成果 87 §3)。
    /// - `ints`:(槽号, 值)→ 帧局部 INT `@53[槽]`
    /// - `strs`:(槽号, 值)→ 帧局部 STR `$55[槽]`
    ///
    /// 返回后帧的 `return_pc` = `usize::MAX`(不可达标记):调用方以
    /// **帧深回落**判定调用结束,勿在返回后继续 run(取证语义)。
    pub fn push_guest_call(
        &mut self,
        label: &str,
        ints: &[(u32, i64)],
        strs: &[(u32, &[u8])],
    ) -> Result<()> {
        let key = label.as_bytes().to_vec();
        let Some(&(target, target_script)) = self.labels.get(&key) else {
            return Err(Error::format(format!("标签不存在: {label}")));
        };
        let mut locals = VariableStore::new();
        let max_i = ints.iter().map(|(s, _)| *s).max().unwrap_or(0);
        let max_s = strs.iter().map(|(s, _)| *s).max().unwrap_or(0);
        if max_i > 0 {
            let r = yuris_value::VarRef {
                space: yuris_value::VarSpace::At,
                id: 53,
            };
            locals.declare_array(&r, yuris_value::ElemType::Int, &[max_i + 1]);
            for (slot, v) in ints {
                locals.set_elem(&r, &[*slot as i64], Value::Int(*v))?;
            }
        }
        if max_s > 0 {
            let r = yuris_value::VarRef {
                space: yuris_value::VarSpace::Dollar,
                id: 55,
            };
            locals.declare_array(&r, yuris_value::ElemType::Str, &[max_s + 1]);
            for (slot, v) in strs {
                locals.set_elem(&r, &[*slot as i64], Value::Str(v.to_vec()))?;
            }
        }
        let _from = self.pc;
        let from_script = self.ctx.script_id;
        self.frames.push(GosubFrame {
            return_pc: usize::MAX,
            script_id: from_script,
            locals,
            if_nest_depth: self.if_nest.len(),
            loop_depth: self.loops.len(),
        });
        if target_script != from_script {
            self.switch_script(target_script, target as usize)?;
        } else {
            self.pc = target as usize;
        }
        Ok(())
    }

    /// GOSUB 帧局部初始化(P5 定性,成果 50):实参按 B0 槽号写入帧局部数组
    /// (INT=@53 / FLT=@54 / STR=$55,槽=B0-类型基,见函数内注释)。
    /// 引擎:实参从参数槽表写入帧内 int/flt/str 区,读经 sysvar switch
    /// case 0x35/0x36/0x37(00447ebc)。
    fn seed_frame_locals(
        &mut self,
        locals: &mut VariableStore,
        windows: &[yuris_format::ystb::CommandSlot],
        gparam: u16,
    ) -> Result<()> {
        // P5 定性(成果 50 + 成果 52,引擎 CMDH_004428c0 @ 004428c0):
        // gparam u16 = part1 高半(part1[pc]>>16),解码本帧局部数组维数:
        //   int_count = (gparam & 0xff) >> 3
        //   flt_count = (gparam & 7) * 4 + (gparam >> 14)
        //   str_count = (gparam >> 9) & 0x1f
        // 槽号 1..count(1 基):有实参窗(B0 = 类型基 + 槽号)→ 写求值结果;
        // 无实参窗 → **清零**(INT 0 / FLT 0.0 / STR "")—— 引擎逐槽
        // `if (arg窗==0) { *str=0; flag=0 }` 分支;计数外槽不写不声明
        // (引擎写循环只到 count,读越界走 sysvar 边界检查 = 0)。
        // 类型区段:B0 0x01-0x0f → INT @53[slot];0x11-0x1f → FLT @54[slot-0x10];
        //           0x21-0x2f → STR $55[slot-0x20]。
        // 帧局部数组在 locals 内以 {At,53}/{At,54}/{Dollar,55} 键承载 ——
        // 与 LET 帧局部写路径(fr.locals.set_elem)及 aload 帧局部读路径
        // (eval.rs locals 优先)共用同一寻址。
        let dbg = debug_frame_enabled();
        // 先收集实参窗:(type, slot) → 窗口
        let mut args: [std::collections::HashMap<u32, &yuris_format::ystb::CommandSlot>; 3] =
            Default::default();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let b0 = (w.tag & 0xff) as u32;
            let (ty, slot) = match b0 {
                0x01..=0x0f => (0usize, b0),
                0x10..=0x1f => (1usize, b0 - 0x10),
                0x20..=0x2f => (2usize, b0 - 0x20),
                _ => continue, // 条件/标签窗(B0=0)或区段外
            };
            args[ty].insert(slot, w);
        }
        // —— 槽号上界(勘误,2026-10-13)——
        // 原实现按 v555 文档的「gparam = 维数位图」解码
        //   int=(u16&0xff)>>3, flt=(u16&7)*4+(u16>>14), str=(u16>>9)&0x1f
        // 对 GOSUB/GO 是**错的**:该公式属声明类命令(F_INT/G_INT 族)的数组维数
        // 位图,不适用于调用类。实测本语料 43001 个 GOSUB/GO 组,`gparam` **全部**
        // 等于「最大实参槽号」(= 实参窗个数 = 窗口数-1),位图公式在 40778 组上
        // 解出的槽数不足以容纳实际实参 —— 后果是帧局部**根本没被写入**:
        // 实测 `es._label.info.exist` 收不到标签名(读到空串)→ 场景存在性判定
        // 恒假 → `ES.FIRST.LOOP` 永不进入 → 画面全黑。
        //
        // 槽号是**全局**编号(跨类型共享),实测 `es.S.GO` 调用 = str[1]+int[2]+int[3]
        // 且 gparam=3。故:按实参窗定上界,再按该上界为三个类型各声明数组并逐槽
        // 清零(引擎「无实参窗 → 清零」同义)。
        let mut max_slot = 0u32;
        for ty in 0..3 {
            if let Some(&m) = args[ty].keys().max() {
                max_slot = max_slot.max(m);
            }
        }
        // gparam 兜底:实测 == 最大槽号;仅在**无实参窗**时启用,且只在它像一个
        // 槽号(≤ 0x2f)时 —— 防止把别的版本的位图形态当计数用而爆内存。
        if max_slot == 0 {
            let g = gparam as u32;
            if g <= 0x2f {
                max_slot = g;
            }
        }
        let int_count = max_slot;
        let flt_count = max_slot;
        let str_count = max_slot;
        let specs = [
            (
                0usize,
                int_count,
                yuris_value::VarRef {
                    space: yuris_value::VarSpace::At,
                    id: 53,
                },
                yuris_value::ElemType::Int,
            ),
            (
                1usize,
                flt_count,
                yuris_value::VarRef {
                    space: yuris_value::VarSpace::At,
                    id: 54,
                },
                yuris_value::ElemType::Float,
            ),
            (
                2usize,
                str_count,
                yuris_value::VarRef {
                    space: yuris_value::VarSpace::Dollar,
                    id: 55,
                },
                yuris_value::ElemType::Str,
            ),
        ];
        for (ty, count, r, et) in specs {
            if count == 0 {
                continue;
            }
            locals.declare_array(&r, et, &[count + 1]);
            for slot in 1..=count {
                let cv = match args[ty].get(&slot) {
                    Some(w) => {
                        if dbg {
                            if let Some(b) = self.ctx.script.window_bytes_pooled_copy(w) {
                                let mut hx = String::new();
                                for x in b.iter().take(36) {
                                    hx.push_str(&format!("{x:02x} "));
                                }
                                let ins = yuris_script::decode_window(&b)
                                    .map(|v| {
                                        v.iter()
                                            .map(|i| format!("{:?}", i.kind))
                                            .collect::<Vec<_>>()
                                            .join("|")
                                    })
                                    .unwrap_or_else(|e| format!("<decode err {e}>"));
                                yuris_log::trace_forced!(
                                    "[f]  ARG s{} pc{} tag={:#x} off={} len={} [{}] → {}",
                                    self.ctx.script_id, self.pc, w.tag, w.offset, w.len, hx, ins
                                );
                            }
                        }
                        // **实参缺席**:编译器用「未填右操作数的二元运算符」表示
                        // 「本实参不存在」(形态与语料统计见
                        // `yuris_script::is_absent_arg_idiom`)。此路不给该槽播种,
                        // 帧局部保持声明时的零值 —— 与手册 `@_PFLG(n)`
                        // 「跳转源是否存在 PINT/PFLT/PSTR」的语义一致。
                        if let Some(bytes) = self.ctx.script.window_bytes_pooled_copy(w) {
                            if let Ok(instrs) = yuris_script::decode_window(&bytes) {
                                if yuris_script::is_absent_arg_idiom(&instrs) {
                                    if dbg {
                                        yuris_log::trace_forced!(
                                            "[f] seed-absent s{} pc{} {}{}[{}]",
                                            self.ctx.script_id,
                                            self.pc,
                                            r.space.prefix(),
                                            r.id,
                                            slot
                                        );
                                    }
                                    continue;
                                }
                            }
                        }
                        let Some(v) = self.eval_window_condition(Some(w))? else {
                            if dbg {
                                yuris_log::trace_forced!(
                                    "[f] seed-skip s{} pc{} {}{}[{}](求值失败/空)",
                                    self.ctx.script_id,
                                    self.pc,
                                    r.space.prefix(),
                                    r.id,
                                    slot
                                );
                            }
                            continue;
                        };
                        Some(match (et, v) {
                            (yuris_value::ElemType::Int, Value::Int(i)) => Value::Int(i),
                            (yuris_value::ElemType::Int, Value::Float(f)) => {
                                Value::Int(f.round() as i64)
                            }
                            (yuris_value::ElemType::Float, Value::Int(i)) => Value::Float(i as f64),
                            (yuris_value::ElemType::Float, Value::Float(f)) => Value::Float(f),
                            (yuris_value::ElemType::Str, Value::Str(mut s)) => {
                                // 引擎实证:$55[1] = 8 字符(ES.FIRST),而 pushstr
                                // 字面量含包裹引号(10B)→ STR 帧局部写入时剥引号
                                if s.len() >= 2
                                    && s.first() == Some(&b'"')
                                    && s.last() == Some(&b'"')
                                {
                                    s = s[1..s.len() - 1].to_vec();
                                }
                                Value::Str(s)
                            }
                            // 类型不匹配:引擎按 desc+1 校验报错;这里保守透传
                            (yuris_value::ElemType::Int, other)
                            | (yuris_value::ElemType::Float, other)
                            | (yuris_value::ElemType::Str, other) => other,
                        })
                    }
                    None => Some(match et {
                        // 引擎:无实参窗 → 槽清零(0 / 0.0 / "")
                        yuris_value::ElemType::Int => Value::Int(0),
                        yuris_value::ElemType::Float => Value::Float(0.0),
                        yuris_value::ElemType::Str => Value::Str(Vec::new()),
                    }),
                };
                if let Some(cv) = cv {
                    if dbg {
                        let vs = match &cv {
                            Value::Int(i) => format!("int:{}", i),
                            Value::Float(f) => format!("flt:{}", f),
                            Value::Str(s) => {
                                let mut hx = String::new();
                                for b in s.iter().take(28) {
                                    hx.push_str(&format!("{b:02x} "));
                                }
                                format!("str:{}B [{hx}]", s.len())
                            }
                        };
                        yuris_log::trace_forced!(
                            "[f] s{} pc{} space={:?} {}{}[{}]={}",
                            self.ctx.script_id,
                            self.pc,
                            r.space,
                            r.space.prefix(),
                            r.id,
                            slot,
                            vs
                        );
                    }
                    let _ = locals.set_elem(&r, &[slot as i64], cv);
                }
            }
        }
        // 静默吞掉 set_elem 失败会丢实参;至少留痕(帧局部槽越界 = 上界算错)。
        Ok(())
    }

    /// LOOPBREAK/LOOPCONTINUE 的 LV 层数窗口:求值 → Int → usize。
    /// 无窗口/空窗 → 默认 1(引擎 DAT_006624a0==0 路径);非 INT → 报错。
    fn loop_level_from(
        &mut self,
        window: Option<&yuris_format::ystb::CommandSlot>,
    ) -> Result<Option<usize>> {
        let Some(w) = window else {
            return Ok(Some(1));
        };
        if w.len == 0 {
            return Ok(Some(1));
        }
        match self.eval_window_condition(Some(w))? {
            Some(Value::Int(n)) if n >= 1 => Ok(Some(n as usize)),
            Some(v) => {
                let reason = format!("LV 层数非正整数: {v:?}");
                self.set_error_state();
                Err(Error::format(reason))
            }
            None => Ok(Some(1)),
        }
    }

    /// LET 左值窗口解析(引用栈 + 下标表)。
    /// 按**槽位**求左值:窗口字节在此内部取(复用缓冲,稳态零分配)。
    fn eval_window_lvalue_slot(
        &mut self,
        w: &yuris_format::ystb::CommandSlot,
    ) -> Result<yuris_script::LValueRef> {
        // **缓存命中直接求值**(成果 184):免去取窗拷贝,且复用已解码指令。
        // 左值窗口同样是 `LET` 的热路径(占全部组 40.4% 的一半窗口)。
        if let Some(hit) = self
            .insn_cache
            .get(&(self.ctx.script_id, w.offset, w.len))
            .cloned()
        {
            let locals = self.frames.last().map(|f| &f.locals);
            let ctx = self.eval_ctx();
            let mut ev = Evaluator::with_locals(&mut self.store, locals);
            ctx.apply(&mut ev);
            return match ev.eval_lvalue_window_instrs(&hit) {
                Ok(l) => Ok(l),
                Err(e) => {
                    self.set_error_state();
                    Err(e)
                }
            };
        }
        let mut buf = std::mem::take(&mut self.bytes_buf);
        buf.clear();
        let ok = match self.ctx.script.window_bytes_pooled(w) {
            Some(b) => {
                buf.extend_from_slice(b);
                true
            }
            None => match self.ctx.script.window_bytes_pooled_copy(w) {
                Some(v) => {
                    buf.extend_from_slice(&v);
                    true
                }
                None => false,
            },
        };
        if !ok {
            self.bytes_buf = buf;
            return Err(Error::format(format!(
                "LET 左值窗口 {}+{} 越过 content+part4 池尾",
                w.offset, w.len
            )));
        }
        let r = self.eval_window_lvalue(&buf);
        self.bytes_buf = buf;
        r
    }

    /// 窗口是否为裸左值引用形态(诊断/引用槽分流用)。
    fn window_is_lvalue(&mut self, w: &yuris_format::ystb::CommandSlot) -> bool {
        let Some(bytes) = self.ctx.script.window_bytes_pooled_copy(w) else {
            return false;
        };
        let locals = self.frames.last().map(|f| &f.locals);
        let ctx = self.eval_ctx();
        let mut ev = Evaluator::with_locals(&mut self.store, locals);
        ctx.apply(&mut ev);
        ev.window_is_lvalue(&bytes)
    }

    fn eval_window_lvalue(&mut self, window: &[u8]) -> Result<yuris_script::LValueRef> {
        let instrs = match yuris_script::decode_window(window) {
            Ok(v) => v,
            Err(e) => {
                self.set_error_state();
                return Err(Error::format(format!("LET 左值解码失败: {e}")));
            }
        };
        let locals = self.frames.last().map(|f| &f.locals);
        let ctx = self.eval_ctx();
        let mut ev = Evaluator::with_locals(&mut self.store, locals);
        ctx.apply(&mut ev);
        match ev.eval_lvalue_window_instrs(&instrs) {
            Ok(l) => Ok(l),
            Err(e) => {
                self.set_error_state();
                Err(e)
            }
        }
    }

    /// **窗口 → 已解码指令**(缓存优先,命中时**不取字节、不分配**;成果 184)。
    ///
    /// 热路径依据:`LET` 占全部组的 40.4%(345k/854k),每组两个窗口,稳态每帧
    /// 约 70 万次求值;窗口字节在脚本装载后**完全不变**。缓存按**脚本**隔离
    /// (键带 `script_id`),换脚本无需清空也不串味。
    ///
    /// 旧变体 `decode_cached(offset,len,bytes)` 要求调用方先取 `&[u8]`,在跨
    /// content/part4 时那是一次 `Vec` 分配,已被本函数取代(2026-10-18 删除)。
    /// 热路径每帧调用约 70 万次,而缓存命中近乎 100%,故这里先查缓存,
    /// 只有**未命中**才取窗口字节。
    ///
    /// 调用方须先自行校验 `window_fits_pool`（本函数不做越界判断，
    /// 未命中时会因取字节失败而返回错误）。
    fn decode_cached_slot(
        &mut self,
        w: &yuris_format::ystb::CommandSlot,
    ) -> std::result::Result<std::sync::Arc<[yuris_script::insn::Instruction]>, yuris_core::Error>
    {
        let key = (self.ctx.script_id, w.offset, w.len);
        if let Some(hit) = self.insn_cache.get(&key) {
            return Ok(std::sync::Arc::clone(hit));
        }
        let Some(bytes) = self.ctx.script.window_bytes_pooled_copy(w) else {
            return Err(Error::format(format!(
                "窗口 {}+{} 越过 content+part4 池尾",
                w.offset, w.len
            )));
        };
        let v = yuris_script::decode_window(&bytes)?;
        let arc: std::sync::Arc<[yuris_script::insn::Instruction]> = v.into();
        self.insn_cache.insert(key, std::sync::Arc::clone(&arc));
        Ok(arc)
    }

    /// strict 挂起的 `Result` 形态:记录 Unsupported 事件 + 返回 Err。
    fn halt_err(&mut self, pc: usize, command: u8, reason: impl Into<String>) -> Error {
        Error::format(self.halted(pc, command, reason))
    }

    /// strict 挂起:记录 Unsupported 事件并返回原因(状态已置 Error)。
    ///
    /// **勘误(2026-10-17)**:旧实现**不发任何日志**(LOAD/YSSD 族走的是
    /// 本函数而非 `halt_or_record`,错因只写进事件流;实机看日志一行根因
    /// 都没有)。现与 `halt_or_record` 统一走 [`Self::note_unsupported`]。
    fn halted(&mut self, pc: usize, command: u8, reason: impl Into<String>) -> String {
        let reason = self.note_unsupported(pc, command, reason);
        self.set_error_state();
        reason
    }

    /// LOAD/YSSD 装载(引擎 FUN_00444648 文件分支 + FUN_0044564d 写回)。
    ///
    /// `fname` = 脚本槽 0 求值串(无扩展名 → 补 `.sd`,引擎 FUN_004409dc
    /// 分支);`dno` = 块号(1 基);`target` = 槽 3 延迟引用(块头 var_id
    /// 与之恒一致,语料 26 组实证)。装载路径 = 松散文件(save/ VFS 根)。
    fn load_yssd(&mut self, fname: &[u8], dno: i64, target: &yuris_value::VarRef) -> Result<()> {
        if dno < 1 {
            return Err(self.halt_err(
                self.pc,
                cmd::LOAD,
                format!("LOAD DNO={dno} 越界(YSSD 偏移表 1 基)"),
            ));
        }
        let mut name = String::from_utf8_lossy(fname).into_owned();
        if !name.contains('.') {
            name.push_str(".sd");
        }
        let Some(probe) = &self.file_probe else {
            return Err(self.halt_err(
                self.pc,
                cmd::LOAD,
                "LOAD 需要文件源(PacFileIndex 未注入;引擎 VFS 语义)",
            ));
        };
        let Some(bytes) = probe.read_loose(&name) else {
            return Err(self.halt_err(
                self.pc,
                cmd::LOAD,
                format!("LOAD 文件不存在: {name}(引擎 0x18ed4 错误框同族)"),
            ));
        };
        let yssd = yuris_format::yssd::YssdFile::from_bytes(&bytes)
            .map_err(|e| Error::format(format!("LOAD {name}: {e}")))?;
        let Some(block) = yssd.block(dno as u32) else {
            return Err(self.halt_err(
                self.pc,
                cmd::LOAD,
                format!("LOAD {name} 无块 {dno}(偏移表空项;引擎 0x18ede)"),
            ));
        };
        if block.btype == 1 {
            return Err(self.halt_err(
                self.pc,
                cmd::LOAD,
                "LOAD 块类型 1(0x690 配置)须配 ID 模式(引擎 0x1a612;不猜)",
            ));
        }
        if block.btype != 0 {
            return Err(self.halt_err(
                self.pc,
                cmd::LOAD,
                format!("LOAD 块类型 {} 未实现(多子块族;语料未出现)", block.btype),
            ));
        }
        let raw = yuris_format::yssd::snp_uncompress(&block.compressed)
            .map_err(|e| Error::format(format!("LOAD {name} 块{dno}: {e}")))?;
        let payload = yuris_format::yssd::YssdPayload::from_bytes(&raw)
            .map_err(|e| Error::format(format!("LOAD {name} 块{dno}: {e}")))?;

        // 类型一致(引擎 0x18ee8)+ 严格维数匹配(引擎 0x1a63a/0x1a630)。
        if self.store.has_array(target) {
            let elem = self.store.array_elem_type(target).unwrap();
            let ok = matches!(
                (elem, payload.ty),
                (yuris_value::ElemType::Int, 1)
                    | (yuris_value::ElemType::Float, 2)
                    | (yuris_value::ElemType::Str, 3)
            );
            if !ok {
                return Err(self.halt_err(
                    self.pc,
                    cmd::LOAD,
                    format!(
                        "LOAD 载荷类型 {} 与 {} 声明不符(引擎 0x18ee8)",
                        payload.ty,
                        target.display()
                    ),
                ));
            }
            let bounds = self.store.array_dims(target).unwrap();
            if block.strict == 1 && bounds != payload.dims {
                return Err(self.halt_err(
                    self.pc,
                    cmd::LOAD,
                    format!(
                        "LOAD 载荷维数 {:?} 与 {} 声明 {bounds:?} 不符(引擎 0x1a63a)",
                        payload.dims,
                        target.display()
                    ),
                ));
            }
            if block.strict == 0 && !payload.dims.is_empty() {
                return Err(self.halt_err(
                    self.pc,
                    cmd::LOAD,
                    format!("LOAD 非 strict 载荷须标量(引擎 0x1a630)"),
                ));
            }
        } else if let Some(cur_ty) = self.store.get_opt(target).map(|v| match v {
            Value::Int(_) => 1u8,
            Value::Float(_) => 2,
            Value::Str(_) => 3,
        }) {
            if cur_ty != payload.ty {
                return Err(self.halt_err(
                    self.pc,
                    cmd::LOAD,
                    format!(
                        "LOAD 载荷类型 {} 与标量 {} 类型不符(引擎 0x18ee8)",
                        payload.ty,
                        target.display()
                    ),
                ));
            }
            if !payload.dims.is_empty() {
                return Err(self.halt_err(
                    self.pc,
                    cmd::LOAD,
                    format!(
                        "LOAD 标量目标 {} 收到数组载荷(引擎 0x1a630)",
                        target.display()
                    ),
                ));
            }
        } else {
            return Err(self.halt_err(
                self.pc,
                cmd::LOAD,
                format!("LOAD 目标 {} 未声明(引擎 0x1a5ea 同族)", target.display()),
            ));
        }

        // 写回(FUN_0044564d:INT/FLT = 8B/元素 memcpy;STR = 逐 {len,bytes})。
        let elem_count: usize = payload.dims.iter().map(|&d| d as usize).product();
        let values: Vec<Value> = match payload.ty {
            1 => payload
                .data
                .chunks_exact(8)
                .map(|c| Value::Int(i64::from_le_bytes(c.try_into().unwrap())))
                .collect(),
            2 => payload
                .data
                .chunks_exact(8)
                .map(|c| Value::Float(f64::from_le_bytes(c.try_into().unwrap())))
                .collect(),
            _ => {
                let mut out = Vec::with_capacity(elem_count.max(1));
                let mut p = 0usize;
                for _ in 0..elem_count.max(1) {
                    if p + 4 > payload.data.len() {
                        return Err(self.halt_err(
                            self.pc,
                            cmd::LOAD,
                            format!("LOAD {} 载荷字符串记录截断", target.display()),
                        ));
                    }
                    let len =
                        u32::from_le_bytes(payload.data[p..p + 4].try_into().unwrap()) as usize;
                    p += 4;
                    if p + len > payload.data.len() {
                        return Err(self.halt_err(
                            self.pc,
                            cmd::LOAD,
                            format!("LOAD {} 载荷字符串记录截断", target.display()),
                        ));
                    }
                    out.push(Value::Str(payload.data[p..p + len].to_vec()));
                    p += len;
                }
                out
            }
        };
        if self.store.has_array(target) {
            self.store
                .load_array_data(target, values)
                .map_err(|e| Error::format(format!("LOAD 写回: {e}")))?;
        } else {
            // 标量(引擎 memcpy 单元素 8B / STR 首记录)
            match values.into_iter().next() {
                Some(v) => self.store.set(target, v),
                None => {
                    return Err(self.halt_err(
                        self.pc,
                        cmd::LOAD,
                        format!("LOAD 标量载荷为空({})", target.display()),
                    ))
                }
            }
        }
        Ok(())
    }

    /// 变量读取(P5,成果 50):**帧局部优先** —— 当前 GOSUB 帧 locals 含
    /// 同键数组(如 @53/@54/$55 帧局部族)时读帧,否则读全局
    /// (引擎 0042158e:id<1000 走帧/系统存储,id≥1000 走 desc+0x30)。
    /// 引擎毫秒计时(`@1[1]` 系统变量):自 VM 建立以来的单调毫秒数。
    ///
    /// 引擎侧对应 timeGetTime 派生计时(`docs/engine/command-layer.md` §9c
    /// case 0x01),脚本只用其**时间差**,故单调性即全部可观测语义。
    ///
    /// **勘误(2026-10-17,成果 184)**:曾把这里改成「逻辑帧数 × 1000/fps」,
    /// 实测**回归**:逻辑时间只随上屏帧数增长,而渲染只有 ~45fps ⇒
    /// `WAIT TIME` 一秒变 0.75 秒、按钮淡入淡出整体变慢(实机「卡卡的」),
    /// 且计时从 0 起使淡变相位与原版不同(中央多出一个游标残影)。
    /// 引擎真值 = `timeGetTime`,即**墙钟**;还原。
    fn timer_ms(&self) -> i64 {
        self.epoch.elapsed().as_millis() as i64
    }

    /// 求值上下文快照(`@1[1]` 计时 / `@48` 循环计数 / `@133`·`@138` 光标 /
    /// `@115`·`@116` 屏幕尺寸)。
    ///
    /// [`yuris_script::Evaluator::with_locals`] 会**可变借走 `self.store`**,
    /// 之后无法再读 `self` 的其它字段 —— 故四处 Evaluator 构造点统一先取
    /// 本快照、构造后再 [`EvalCtx::apply`](T20:屏幕尺寸走这条注入路径,
    /// 与 `WINDOWINFO SX/SY` 应答共用 `screen_w/screen_h` 同一真值)。
    fn eval_ctx(&self) -> EvalCtx {
        EvalCtx {
            loop_counter: self.loops.last().map(|l| l.counter as i64).unwrap_or(0),
            timer_ms: self.timer_ms(),
            cursor: self.input_cursor,
            screen: (self.screen_w, self.screen_h),
            input: self.input_vars.clone(),
        }
    }

    fn read_var_value(&self, r: &yuris_value::VarRef, indices: &[i64]) -> Option<Value> {
        // 输入系统变量（`@_MOUSE_X`/`@_INP`/`@_KEY_*`/`@_CODE_*`）：由输入层
        // 每帧快照提供。引擎这些 id 无存储（`FUN_00427350` 读即计算）。
        // 同帧脚本写入的 overlay 优先（LET 之后立刻读要看到新值）。
        if r.space == yuris_value::VarSpace::At {
            if let Some(overlay) = self
                .input_overlay
                .get(&(r.id, indices.first().copied().unwrap_or(-1)))
            {
                return Some(overlay.clone());
            }
            if let Some(input) = &self.input {
                if let Some(v) = input.read(r.id, indices) {
                    return Some(Value::Int(v));
                }
            }
        }
        // @1 系统变量(引擎 sysvar case 0x01):timeGetTime 派生**毫秒计时**。
        // 语料只用下标 1(1504 处时间差);其余子槽语义未证实 → 0,不猜。
        if r.space == yuris_value::VarSpace::At && r.id == 1 {
            return Some(Value::Int(if indices.first() == Some(&1) {
                self.timer_ms()
            } else {
                0
            }));
        }
        // @48 系统变量(P5 定性,引擎 sysvar case 0x30/00447ebc):内层 LOOP
        // 迭代计数,读即计算、无存储(引擎 obj+0x244 栈顶记录 +0x10);
        // LOOP 置 1、LOOPEND +1、LOOPBREAK 清零;无活动循环 = 0。
        if r.space == yuris_value::VarSpace::At && r.id == 48 {
            return Some(Value::Int(
                self.loops.last().map(|l| l.counter as i64).unwrap_or(0),
            ));
        }
        // @114(引擎 sysvar case 0x72/00447ebc):显示色深 bpp,原生全局
        // DAT_00872374(显示初始化写入;s40 色深检查用)。VM 无显示栈,
        // 按 oracle 环境 = 32bpp(成果 59c)。
        if r.space == yuris_value::VarSpace::At && r.id == 114 {
            return Some(Value::Int(32));
        }
        // @115/@116(引擎 sysvar case 0x73/0x74)= `_SCREEN_SIZE_X/Y`:
        // **当前分辨率**(手册「システム変数」表)。真值 = 游戏自身 yscfg,
        // 由启动链 `set_screen_size` 注入(成果 123;旧实现写死 1920×1080)。
        if r.space == yuris_value::VarSpace::At && r.id == 115 {
            return Some(Value::Int(self.screen_w));
        }
        if r.space == yuris_value::VarSpace::At && r.id == 116 {
            return Some(Value::Int(self.screen_h));
        }
        // @11 = `_DEBUGMODE`(手册:「デバッグモードか否か(1=デバッグモード/
        // 0=リリースモード)」)。**勘误(2026-10-13,成果 123)**:旧实现不提供该
        // 系统变量 → 读落到普通全局(实测被游戏读成 1)⇒ `ES.DEBUGMODE.BTDEF`
        // 的 `IF @11 == 1` 成立 ⇒ **调试按钮组(20,25)与调试窗一起上屏**
        // (实机「按钮没有/一团蓝块 + 网格文字」)。本内核不是调试器 ⇒ 恒 0。
        if r.space == yuris_value::VarSpace::At && r.id == 11 {
            return Some(Value::Int(0));
        }
        let out: Option<Value> = (|| {
            if let Some(fr) = self.frames.last() {
                if fr.locals.has_array(r) {
                    return match fr.locals.get_elem(r, indices) {
                        Ok(v) => Some(v.clone()),
                        Err(e) => {
                            // 帧局部系统族越界读 = 类型默认值(引擎 sysvar
                            // case 0x35 边界外 → 0;成果 52);其余数组严格
                            let _ = e;
                            if yuris_script::eval::is_frame_sysvar(r) {
                                Some(yuris_script::eval::frame_sysvar_default(
                                    fr.locals.array_elem_type(r),
                                ))
                            } else {
                                None
                            }
                        }
                    };
                }
                // 有帧但无该键:系统族 = 默认值,**绝无全局回退**(引擎
                // id<1000 读永远帧内定界;成果 52)
                if yuris_script::eval::is_frame_sysvar(r) {
                    return Some(yuris_script::eval::frame_sysvar_default(
                        yuris_script::eval::frame_sysvar_elem(r),
                    ));
                }
                if indices.is_empty() {
                    if let Some(v) = fr.locals.get_opt(r) {
                        return Some(v.clone());
                    }
                }
            }
            if self.store.has_array(r) {
                // 引擎全局数组读 FUN_00459490/00459418(成果 57):越界
                // → 类型默认值,不报错。
                return match self.store.get_elem(r, indices) {
                    Ok(v) => Some(v.clone()),
                    Err(e) => match self.store.array_elem_type(r) {
                        Some(et) => Some(yuris_script::eval::type_default(Some(et))),
                        None => {
                            let _ = e;
                            None
                        }
                    },
                };
            }
            if indices.is_empty() {
                return self.store.get(r).cloned().ok();
            }
            match self.store.get_elem(r, indices) {
                Ok(v) => Some(v.clone()),
                Err(e) => match self.store.array_elem_type(r) {
                    Some(et) => Some(yuris_script::eval::type_default(Some(et))),
                    None => {
                        let _ = e;
                        None
                    }
                },
            }
        })();
        if debug_frame_enabled() && r.id == 55 {
            let fr_has = self.frames.last().map(|f| f.locals.has_array(r));
            let vs = match &out {
                Some(Value::Str(s)) => format!("str:{}B", s.len()),
                Some(Value::Int(i)) => format!("int:{}", i),
                Some(Value::Float(f)) => format!("flt:{}", f),
                None => "none".to_string(),
            };
            yuris_log::trace_forced!(
                "[f] read $55{:?} frames={} frame_has={:?} -> {}",
                indices,
                self.frames.len(),
                fr_has,
                vs
            );
        }
        out
    }

    /// 变量写入(帧局部优先;同 [`Self::read_var_value`])。
    fn write_var_value(
        &mut self,
        r: &yuris_value::VarRef,
        indices: &[i64],
        v: Value,
    ) -> Result<()> {
        if let Some(fr) = self.frames.last_mut() {
            if fr.locals.has_array(r) {
                return fr.locals.set_elem(r, indices, v);
            }
            if indices.is_empty() && fr.locals.get_opt(r).is_some() {
                fr.locals.set(r, v);
                return Ok(());
            }
        }
        if self.store.has_array(r) {
            return self.store.set_elem(r, indices, v);
        }
        if indices.is_empty() {
            self.store.set(r, v);
            return Ok(());
        }
        self.store.set_elem(r, indices, v)
    }

    /// 记录 Unsupported 事件;strict → 返回 `Some(原因)`(调用方挂起),
    /// 否则 `None`(调用方继续)。状态仅在 strict 下置 Error。
    fn halt_or_record(
        &mut self,
        pc: usize,
        command: u8,
        reason: impl Into<String>,
    ) -> Option<String> {
        let reason = self.note_unsupported(pc, command, reason);
        if self.strict {
            self.set_error_state();
            Some(reason)
        } else {
            None
        }
    }

    /// 未实现/未命中命令的**统一诊断汇聚点**:落 stderr 告警 +
    /// `VmEvent::Unsupported` 事件,并把根因快照进 `err_cause`(供
    /// `run_main` 的「VM 处于 Error 态」挡回消息携带 —— 2026-10-17 之前
    /// 该消息不带任何原因,实机排障只能全量翻事件流)。
    ///
    /// **`command` 参数只是处理器声明的取值**(习惯性 `cmd::*` v555 留存
    /// 常量);事件写入的恒为分派期真实下标(T2 修复,同 [`Self::halted`])。
    fn note_unsupported(&mut self, pc: usize, command: u8, reason: impl Into<String>) -> String {
        let reason = reason.into();
        let actual = self.cur_command.unwrap_or(command);
        // **未实现/未命中命令的统一诊断(2026-10-15,用户要求)**。
        //
        // 这是全部 `VmEvent::Unsupported` 的汇聚点 —— 任何「命令没实现 /
        // 标签没找到 / 语义未逆向」都会经过这里。旧实现只在事件流里记一笔,
        // 实机跑的时候屏幕上什么都不说,排障只能靠事后翻 trace。
        //
        // **2026-10-17 全量收紧(用户要求)**:这里改成**醒目横幅 + 去重**,
        // 并在 strict(默认)下同时登记**致命错误** —— 播放器主循环据此弹出
        // 画布中央错误窗。不再允许「未知只留一行 warn、游戏照跑」。
        // `YURIS_QUIET_UNSUP=1` 仅用于大批量探针(仍计数、仍可能致命)。
        let cname = self
            .dispatch
            .name_of(actual as u16)
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("<未命名 cmd{actual}>"));
        let relic = if actual != command {
            format!(" [处理器常量 0x{command:02x} = v555 留存,非本版]")
        } else {
            String::new()
        };
        let sid = self.ctx.script_id;
        let first = self.unsup_alerted.borrow_mut().insert((sid, pc, actual));
        if self.err_cause.is_none() {
            self.err_cause = Some(format!("{cname}(idx{actual}) s{sid} pc={pc}: {reason}{relic}"));
        }
        if first {
            if std::env::var_os("YURIS_QUIET_UNSUP").is_none() {
                yuris_log::alert!(
                    "未实现/未命中命令\n\
                     命令: {cname} (idx {actual}, 0x{actual:02x}){relic}\n\
                     现场: script {sid} pc {pc}\n\
                     原因: {reason}\n\
                     处置: {}",
                    if self.strict {
                        "strict —— 挂起并上屏致命错误窗(修好后重跑)"
                    } else {
                        "lenient —— 记录后继续(仅供取证;此命令语义仍是 Unknown)"
                    }
                );
            }
            if self.strict && self.fatal_reporting {
                yuris_log::report_fatal(
                    module_path!(),
                    "运行时缺失 / UNIMPLEMENTED",
                    format!("{cname}(idx{actual}) s{sid} pc={pc}: {reason}{relic}"),
                );
            }
        }
        self.push_event(VmEvent::Unsupported {
            pc,
            command: actual,
            reason: reason.clone(),
        });
        reason
    }

    /// 打开/关闭 **strict 未实现 → 致命错误窗** 升级。
    ///
    /// 库与测试默认关闭(避免全局致命状态串味);`yuris-cli` 与播放器
    /// 启动时打开 —— 那边才有画布可以盖错误窗。
    pub fn set_fatal_reporting(&mut self, on: bool) {
        self.fatal_reporting = on;
    }

    /// 最近一次错误的根因(诊断/播放器升级致命用)。
    pub fn last_error(&self) -> Option<&str> {
        self.err_cause.as_deref()
    }

    /// **未建模参数槽**的醒目告警(每 `(命令, 槽)` 一次)。
    ///
    /// 这些槽当前只被求值记录、不参与语义 —— 用户要求「缺什么必须看得见」。
    /// 不升级为致命:部分实现(如 CG 的 55 个效果槽)仍是可用状态,
    /// 但每一个未建模槽都会在 `VmEvent` 的 `evaluated` 里留痕。
    pub fn note_unknown_slot(&self, command: u8, slot: u8, context: &str) {
        let key = (command, slot);
        if !self.unknown_slot_warned.borrow_mut().insert(key) {
            return;
        }
        let cname = self
            .dispatch
            .name_of(command as u16)
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("<未命名 cmd{command}>"));
        let pname = self
            .dispatch
            .param_name_of(command, slot)
            .unwrap_or_else(|| "无参数表/未命名".to_string());
        yuris_log::alert!(
            "未建模参数槽\n\
             命令: {cname} (idx {command})  槽 {slot} = {pname}\n\
             现场: script {} pc {}\n\
             上下文: {context}\n\
             处置: 值已求值并记入事件,但**不参与任何语义**(Unknown)",
            self.ctx.script_id,
            self.pc
        );
    }

    /// **整个子系统/命令未实装**的醒目告警(按命令名去重)。
    ///
    /// 用于「分类到了、但处理器只记录不执行」的命令(EMOTE / SOUNDINFO /
    /// DIALOG / FILEACT / ERROR …)—— 这类路径以前**完全静默**,
    /// 用户无从知道运行时缺了哪块。
    pub fn note_missing_feature(&self, command: u8, detail: &str) {
        let cname = self
            .dispatch
            .name_of(command as u16)
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("<未命名 cmd{command}>"));
        if !self
            .missing_feature_warned
            .borrow_mut()
            .insert(cname.clone())
        {
            return;
        }
        yuris_log::alert!(
            "命令已分派但子系统未实装\n\
             命令: {cname} (idx {command})\n\
             现场: script {} pc {}\n\
             缺失: {detail}\n\
             处置: {}",
            self.ctx.script_id,
            self.pc,
            if self.strict {
                "strict —— 本命令语义 Unknown,挂起并上屏致命错误窗"
            } else {
                "lenient —— 仅记录参数后继续(结果不可信)"
            }
        );
    }

    /// 未实装子系统在 strict 下的统一**中断**出口。
    ///
    /// 返回 `Some(原因)` = 调用方应返回 `Action::Suspend(VmSuspend::Error)`。
    /// 语义与用户要求一致:**不再允许「未实装但特判继续」** ——
    /// strict(运行时默认)直接挂起并上屏致命错误窗;lenient 仅用于取证。
    pub(crate) fn missing_feature_halt(&mut self, command: u8, detail: &str) -> Option<String> {
        self.note_missing_feature(command, detail);
        if self.strict {
            Some(self.halted(self.pc, command, detail.to_string()))
        } else {
            None
        }
    }
}

impl GroupVm {
    /// 读 SET 目标当前值(VARINFO 查询对象)。P5:帧局部优先(成果 50)。
    ///
    /// **未定义变量的 `Int(0)` 是兜底值,不是真值**:引擎对未定义读取的
    /// 默认值语义 Unknown,本内核按类型默认值继续(既有口径)。但每次
    /// 落到兜底都必须可见 —— 否则「查询了一个没声明的变量」会静默得到 0,
    /// 脚本分支结果不可信。
    fn read_var_target(&mut self, t: &VarTarget) -> Result<Value> {
        let (r, indices) = match t {
            VarTarget::Scalar(r) => (r.clone(), Vec::new()),
            VarTarget::Indexed(r, idx) => (r.clone(), self.eval_target_indices(idx)?),
        };
        if let Some(v) = self.read_var_value(&r, &indices) {
            return Ok(v);
        }
        yuris_log::alert_once(
            module_path!(),
            format!("var-target-undefined:{}:{indices:?}", r.display()),
            format!(
                "查询目标变量未定义,按类型默认值(Int 0)继续\n\
                 变量: {}\n\
                 下标: {indices:?}\n\
                 现场: script {} pc {}\n\
                 处置: 这是**兜底值**;引擎对未定义读取的真值语义 Unknown(铁律 2)",
                r.display(),
                self.ctx.script_id,
                self.pc
            ),
        );
        Ok(Value::Int(0))
    }

    /// 求值引用槽的下标指令(引擎 kind 2 延迟求值:写/读时求值)。
    /// 常量(PushInt)序列走快路径;其余经 Evaluator 活求值,
    /// 栈序 = 维度序(引擎:引用点与栈顶之间的纯值项 = 下标表)。
    fn eval_target_indices(&mut self, idx: &[yuris_script::Instruction]) -> Result<Vec<i64>> {
        if idx.is_empty() {
            return Ok(Vec::new());
        }
        if idx
            .iter()
            .all(|i| matches!(i.kind, yuris_script::Insn::PushInt(_)))
        {
            return Ok(idx
                .iter()
                .filter_map(|i| match i.kind {
                    yuris_script::Insn::PushInt(n) => Some(n),
                    _ => None,
                })
                .collect());
        }
        let locals = self.frames.last().map(|f| &f.locals);
        let ctx = self.eval_ctx();
        let mut ev = Evaluator::with_locals(&mut self.store, locals);
        ctx.apply(&mut ev);
        let stack = ev
            .eval_instructions(idx)
            .map_err(|e| Error::format(format!("引用槽下标表达式求值失败: {e}")))?;
        stack
            .into_iter()
            .map(|v| {
                v.as_int_opt()
                    .ok_or_else(|| Error::format("引用槽下标表达式结果非整数(不猜)"))
            })
            .collect()
    }

    /// 把查询结果写入 LET 引用目标。
    /// 目标的元素类型(引擎描述符 byte+1 等价:数组按声明类型,标量按现值)。
    /// 都不可知 → `None`(调用方按标量 INT 处理,SAVE 语境与引擎 STR 分支
    /// 「无存储写入」不同处如实标注)。
    fn var_target_elem(&self, t: &VarTarget) -> Option<yuris_value::ElemType> {
        match t {
            VarTarget::Scalar(r) => match self.store.get_opt(r) {
                Some(Value::Int(_)) => Some(yuris_value::ElemType::Int),
                Some(Value::Float(_)) => Some(yuris_value::ElemType::Float),
                Some(Value::Str(_)) => Some(yuris_value::ElemType::Str),
                None => None,
            },
            VarTarget::Indexed(r, _) => self.store.array_elem_type(r),
        }
    }

    fn write_var_target(&mut self, t: &VarTarget, v: Value) -> Result<()> {
        match t {
            VarTarget::Scalar(r) => self.write_var_value(r, &[], v),
            VarTarget::Indexed(r, idx) => {
                let indices = self.eval_target_indices(idx)?;
                self.write_var_value(r, &indices, v)
            }
        }
    }

    /// VARACT DIMSIZE:1 维数组 resize。
    fn resize_array_target(&mut self, t: &VarTarget, new_len: usize) -> Result<()> {
        match t {
            // 裸引用(0x76 无 aload,s37 g50 实证)= **整个数组**引用:引擎
            // DIMSIZE 分支直接按 id 取 desc 检查维数并 resize,与下标无关。
            VarTarget::Scalar(r) => {
                if !self.store.has_array(r) {
                    return Err(Error::format(format!(
                        "VARACT DIMSIZE 目标必须是数组(引擎 desc+2==1 检查) s{} pc={}",
                        self.ctx.script_id, self.pc
                    )));
                }
                let dims = self.store.array_dims(r).unwrap_or_default();
                if dims.len() != 1 {
                    return Err(Error::format(format!(
                        "VARACT DIMSIZE 要求 1 维数组(引擎 desc+2==1),得到 {} 维",
                        dims.len()
                    )));
                }
                self.store.resize_array(r, &[new_len as u32])
            }
            VarTarget::Indexed(r, idx) => {
                // 引擎 DIMSIZE 恒作用于整数组(裸引用);带下标形态按
                // 延迟求值解析(结果维数不参与 resize 定位,desc 按 id 取)。
                let _indices = self.eval_target_indices(idx)?;
                self.store.resize_array(r, &[new_len as u32])
            }
        }
    }

    /// VARACT 数值槽取值(非负 int;缺失 → 0,引擎槽标志未置位 = 0)。
    fn varact_slot_usize(&self, ops: &[(u8, u8, Value)], slot: u8) -> Result<usize> {
        match ops.iter().find(|(s, _, _)| *s == slot).map(|(_, _, v)| v) {
            None => Ok(0),
            Some(Value::Int(n)) if *n >= 0 => Ok(*n as usize),
            Some(other) => Err(Error::format(format!(
                "VARACT 槽 {slot} 需非负 int,得到 {other:?}"
            ))),
        }
    }

    /// VARINFO 查询执行(引擎 CMDH_004550a0 if-else 链)。
    /// 返回 `Some(结果值)`(写 LET 目标)或 `None`(纯显示/未启用)。
    fn exec_varinfo_query(
        &mut self,
        query: Option<u8>,
        set_target: Option<&VarTarget>,
        slot_vals: &[(u8, Value)],
        evaluated: &mut Vec<(u8, String)>,
    ) -> Result<Option<Value>> {
        let Some(target) = set_target else {
            return Err(Error::format("VARINFO 无 SET 引用槽(引擎必填)"));
        };
        match query {
            None => {
                // 引擎 fallback:SET 变量 type==3 → 串**字符数**(LENGTH 同源,
                // 0x4551dc 步进计数终证);否则报错 0x1a6bc
                let v = self.read_var_target(target)?;
                match v {
                    Value::Str(bytes) => {
                        let n = sjis_char_len(&bytes);
                        evaluated.push((13u8, format!("LENGTH(fallback)={n}")));
                        Ok(Some(Value::Int(n as i64)))
                    }
                    _ => Err(Error::format(
                        "VARINFO fallback: SET 变量非 STR(引擎 0x1a6bc)",
                    )),
                }
            }
            Some(2) => {
                // TYPE:1=INT 2=FLT 3=STR(按当前值/数组元素类型)
                let code = match self.read_var_target(target)? {
                    Value::Int(_) => 1i64,
                    Value::Float(_) => 2,
                    Value::Str(_) => 3,
                };
                evaluated.push((2u8, format!("TYPE={code}")));
                Ok(Some(Value::Int(code)))
            }
            Some(3) => {
                // STRTYPE:判定 SET 引用串**首字符**的 SJIS 宽度类型。
                //   1 = 半角(单字节)、2 = 全角(双字节前导)、0 = 空串。
                // 结果写 LET 引用的下标元素(引擎处理体 CMDH_004550a0 的
                // STRTYPE 分支;证据见 docs/engine/varact_varinfo.md §1、
                // PROGRESS 成果 40「判定结果(1=半角/2=全角)写入 LET」)。
                //
                // 语料唯一调用点 s70 g239(文本测宽宏):
                //   `VARINFO STRTYPE=1 / SET=$55[1] / LET=@4305`,
                // 紧接 `LET @4305 = @4305 + 10` ⇒ 返回值是**逐字符步进量**
                // 的宽度档(半角 → 11、全角 → 12),与 LENGTH 的 SJIS
                // 宽度表(`0x59b0c0`)同源。
                //
                // STRTYPE 槽自身是**开关**(`push 1`),不携带串;串只来自 SET。
                let bytes = match self.read_var_target(target)? {
                    Value::Str(b) => b,
                    _ => {
                        return Err(Error::format(
                            "VARINFO STRTYPE: SET 变量非 STR(引擎 0x1a6bc)",
                        ))
                    }
                };
                let code: i64 = match bytes.first() {
                    None => 0,
                    // CP932 双字节前导区:0x81..=0x9F 与 0xE0..=0xFC。
                    Some(&f) if (0x81..=0x9f).contains(&f) || (0xe0..=0xfc).contains(&f) => 2,
                    Some(_) => 1,
                };
                evaluated.push((3u8, format!("STRTYPE={code}")));
                Ok(Some(Value::Int(code)))
            }
            Some(4) => {
                // DIMNUM:维数。裸引用(Scalar)= 整数组(s37 g50/g51 实证,
                // 引擎 DIMSIZE/DIMNUM 直接按 id 取 desc)→ 按 store 数组解析;
                // 真标量 → 0。
                let n = match target {
                    VarTarget::Scalar(r) => self
                        .store
                        .array_dims(r)
                        .map(|d| d.len() as i64)
                        .unwrap_or(0),
                    VarTarget::Indexed(r, _) => self
                        .store
                        .array_dims(r)
                        .map(|d| d.len() as i64)
                        .unwrap_or(0),
                };
                evaluated.push((4u8, format!("DIMNUM={n}")));
                Ok(Some(Value::Int(n)))
            }
            Some(slot @ 5..=12) => {
                // DIMSIZE..DIMSIZE8:第 (slot-4) 维边界(引擎 a5..ac 各固定一维)。
                // Scalar 裸引用 = 整数组(同 DIMNUM,引擎按 id 取 desc)。
                let dim = (slot - 5) as usize;
                let n = match target {
                    VarTarget::Scalar(r) => self
                        .store
                        .array_dims(r)
                        .and_then(|d| d.get(dim).copied())
                        .map(i64::from)
                        .unwrap_or(0),
                    VarTarget::Indexed(r, _) => {
                        let dims = self.store.array_dims(r).unwrap_or_default();
                        dims.get(dim).copied().map(i64::from).unwrap_or(0)
                    }
                };
                evaluated.push((slot, format!("DIMSIZE[{}]={n}", dim + 1)));
                Ok(Some(Value::Int(n)))
            }
            Some(13) => {
                // LENGTH:SET 串的 SJIS **字符数**(汇编终证 0x4551dc-0x455208:
                // strlen 只作循环边界,入栈结果 = 宽度表步进的 EAX 计数器;
                // 旧「字节数」解读被证伪 —— 它曾使 s190 pc=36 的
                // POS=LEN−4+1 落到越界值)
                let v = self.read_var_target(target)?;
                match v {
                    Value::Str(bytes) => {
                        let n = sjis_char_len(&bytes);
                        evaluated.push((13u8, format!("LENGTH={n}")));
                        Ok(Some(Value::Int(n as i64)))
                    }
                    _ => Err(Error::format(
                        "VARINFO LENGTH: SET 变量非 STR(引擎 0x1a6bc)",
                    )),
                }
            }
            Some(14) => {
                // SEARCH(引擎 CMDH_004550a0 ae 支):在 SET 数组元素中从
                // NO(20) 起线性查找 key(INT=17 / FLT=18 / STR=19),返回首个
                // 匹配下标(0 基);未命中返回扫描边界(元素个数)。
                // 等级:Likely(分支汇编逐行,参数槽号取自 YSCM 参数名)。
                let key = slot_vals
                    .iter()
                    .find(|(s, _)| *s == 17 || *s == 18 || *s == 19)
                    .map(|(_, v)| v.clone());
                let start = slot_vals
                    .iter()
                    .find(|(s, _)| *s == 20)
                    .and_then(|(_, v)| match v {
                        Value::Int(n) => Some((*n).max(0) as usize),
                        _ => None,
                    })
                    .unwrap_or(0);
                let (r, count) = match target {
                    VarTarget::Scalar(r) | VarTarget::Indexed(r, _) => (
                        r,
                        self.store
                            .array_dims(r)
                            .and_then(|d| d.first().copied())
                            .map(|n| n as usize)
                            .unwrap_or(0),
                    ),
                };
                // 未命中 → **-1**(勘误,2026-10-13)。
                // 旧实现返回「扫描边界(元素个数)」,刻意为该版引擎语义,但被
                // 调用侧**直接证伪**:本语料 `es.BT.NO.GET`(s0 g2448-2456)
                // 拿到结果后立即做 `if (@[0x5bc] != -1) { @[0x5bb] = @[0x5bc] }`
                // —— 0x52 是**一元取负**(insn.rs `0x52 => Unary(Neg)`),故常量
                // 侧是 -1,即 -1 才是「未命中」哨兵。旧值 301(元素个数)在该
                // 判定下为真 → 被当成下标写 `@[0x4a4][301]` → 越界崩溃
                // (实测挂起:`idx[0]=301, bound=301`)。
                let found: i64 = match key {
                    Some(key) => {
                        let mut hit: i64 = -1;
                        for i in start..count {
                            let Ok(v) = self.store.get_elem(r, &[i as i64]) else {
                                break;
                            };
                            let m = match (&v, &key) {
                                (Value::Int(a), Value::Int(b)) => a == b,
                                (Value::Float(a), Value::Float(b)) => a == b,
                                (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
                                (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
                                (Value::Str(a), Value::Str(b)) => a == b,
                                _ => false,
                            };
                            if m {
                                hit = i as i64;
                                break;
                            }
                        }
                        hit
                    }
                    None => -1,
                };
                evaluated.push((14u8, format!("SEARCH={found}(n={count})")));
                Ok(Some(Value::Int(found)))
            }
            Some(slot) => Err(Error::format(format!(
                "VARINFO 查询槽 {slot}(STRFIRST/SJISCODE)未实现(不猜)"
            ))),
        }
    }
}

#[cfg(test)]
mod cp932_tests {
    use super::{sjis_char_len, varact_ascii_case, varact_char_to_byte};
    use yuris_value::Value;

    // ---- sjis_char_len:VARINFO/VARACT 的 LENGTH/POS 都按**字符**计数 ----
    //
    // 语义来源:引擎宽度步进表(汇编终证:strlen 只作循环边界,入栈结果 =
    // 每字符 +1 的计数器)。双字节首区步进 2,其余步进 1。

    #[test]
    fn ascii_is_one_byte_per_char() {
        assert_eq!(sjis_char_len(b"Hello"), 5);
        assert_eq!(sjis_char_len(b""), 0);
    }

    /// 半角片假名(0xA1–0xDF)是**单字节**——最容易被误当双字节的一族。
    #[test]
    fn halfwidth_katakana_is_single_byte() {
        // 0xB1 = 'ｱ', 0xB2 = 'ｲ', 0xDF = 'ﾟ'
        assert_eq!(sjis_char_len(&[0xB1, 0xB2, 0xDF]), 3);
    }

    #[test]
    fn double_byte_kanji_is_one_char() {
        // 0x93 0xFA = '日'
        assert_eq!(sjis_char_len(&[0x93, 0xFA]), 1);
    }

    /// 混排:ASCII + 半角片假名 + 双字节 —— 逐段都要对。
    #[test]
    fn mixed_ascii_halfwidth_and_doublebyte() {
        // '$' + 'ｱ'(0xB1) + '日'(0x93FA) + 'b'
        let s = [0x24u8, 0xB1, 0x93, 0xFA, 0x62];
        assert_eq!(sjis_char_len(&s), 4);
    }

    /// 双字节首区边界值。
    ///
    /// **勘误(T5 取证,2026-10-16)**:旧实现只认 `0x81..=0x9F` 与
    /// `0xE0..=0xEF`,把 `0xF0-0xFC`(CP932 用户区/IBM 扩展)当单字节 ——
    /// 当时标注 "引擎宽度表是否也步进 2 未取到证据"(U-CP932)。现已取证:
    /// v494 引擎表 `0x5b40a0` dump = `0x81-0x9F` 与 **`0xE0-0xFC`** 为 1
    /// (双字节前导),`0x80/0xA0/0xFD-0xFF` 为 0;三处使用点交叉证实
    /// (`FUN_0045543c`/`FUN_0045f0dc`/`FUN_00444f10`)。
    #[test]
    fn lead_byte_boundaries() {
        // 步进 2 区间:0x81..=0x9F、0xE0..=0xFC(取证后)
        assert_eq!(sjis_char_len(&[0x81, 0x40]), 1);
        assert_eq!(sjis_char_len(&[0x9F, 0x40]), 1);
        assert_eq!(sjis_char_len(&[0xE0, 0x40]), 1);
        assert_eq!(sjis_char_len(&[0xEF, 0x40]), 1);
        assert_eq!(sjis_char_len(&[0xF0, 0x40]), 1, "0xF0 取证为前导(T5)");
        assert_eq!(sjis_char_len(&[0xFC, 0x40]), 1, "0xFC 取证为前导(T5)");
        // 区间外:0x80 / 0xA0 / 0xFD / 0xFF 仍按**单字节**步进(表为 0)
        assert_eq!(sjis_char_len(&[0x80, 0x40]), 2);
        assert_eq!(sjis_char_len(&[0xA0, 0x40]), 2);
        assert_eq!(sjis_char_len(&[0xFD, 0x40]), 2);
        assert_eq!(sjis_char_len(&[0xFF, 0x40]), 2);
    }

    /// 截断的双字节(串尾只剩余首字节)—— 仍计 1 字符,不 panic、不越界。
    #[test]
    fn truncated_lead_byte_at_tail_is_safe() {
        assert_eq!(sjis_char_len(&[0x93]), 1);
        assert_eq!(sjis_char_len(b"A\x93"), 2);
    }

    // ---- varact_char_to_byte:字符序数 → 字节偏移 ----

    /// 语义:返回第 `ordinal` 个字符的**字节起始偏移**;
    /// `ordinal == 字符数` 时返回**串尾偏移**(排他上界,调用方切片用);
    /// 仅 `ordinal > 字符数` 才是 `None`。
    #[test]
    fn char_to_byte_ascii_and_bounds() {
        let s = Value::Str(b"ABC".to_vec());
        assert_eq!(varact_char_to_byte(&s, 0), Some(0));
        assert_eq!(varact_char_to_byte(&s, 2), Some(2));
        assert_eq!(
            varact_char_to_byte(&s, 3),
            Some(3),
            "== 字符数 → 串尾(排他上界)"
        );
        assert_eq!(varact_char_to_byte(&s, 4), None, "> 字符数 → None");
        assert_eq!(varact_char_to_byte(&s, usize::MAX), None);
    }

    #[test]
    fn char_to_byte_mixed_widths() {
        // 'ｱ'(1B) + '日'(2B) + 'b'(1B) = 4 字节 / 3 字符
        let s = Value::Str(vec![0xB1, 0x93, 0xFA, 0x62]);
        assert_eq!(varact_char_to_byte(&s, 0), Some(0));
        assert_eq!(varact_char_to_byte(&s, 1), Some(1));
        assert_eq!(varact_char_to_byte(&s, 2), Some(3));
        assert_eq!(varact_char_to_byte(&s, 3), Some(4), "== 字符数 → 串尾");
        assert_eq!(varact_char_to_byte(&s, 4), None);
    }

    #[test]
    fn char_to_byte_non_string_is_none() {
        assert_eq!(varact_char_to_byte(&Value::Int(1), 0), None);
    }

    // ---- varact_ascii_case:UPPER/LOWER 的 SJIS 感知 ----

    #[test]
    fn ascii_case_conversion() {
        assert_eq!(varact_ascii_case(b"aBc", true), b"ABC".to_vec());
        assert_eq!(varact_ascii_case(b"aBc", false), b"abc".to_vec());
    }

    /// 陷阱:双字节**尾字节**可能落在 ASCII 字母区(SJIS 尾区 0x40–0x7E),
    /// 若按单字节扫描就会被错误改写。实现按首区跳 2,必须原样保留。
    #[test]
    fn trail_byte_in_ascii_letter_range_is_untouched() {
        // 0x93 0x61:首 0x93 在前导区,尾 0x61 = 'a'
        let s = vec![0x93u8, 0x61];
        assert_eq!(varact_ascii_case(&s, true), s, "尾字节 'a' 不得被大写化");
        assert_eq!(varact_ascii_case(&s, false), s);
    }

    /// 半角片假名(0xA1–0xDF)不得被大小写转换。
    #[test]
    fn halfwidth_katakana_untouched_by_case_ops() {
        let s = vec![0xB1u8, 0xDF];
        assert_eq!(varact_ascii_case(&s, true), s);
        assert_eq!(varact_ascii_case(&s, false), s);
    }

    /// 单字节区间外侧(0x80 / 0xA0 / 0xFD)按实现语义:非字母 → 原样。
    ///
    /// **勘误(T5,2026-10-16)**:旧用例含 `0xF0` 并按"单字节"断言 ——
    /// 0xF0 现取证为**双字节前导**(见 `lead_byte_boundaries`),此处改用
    /// 表确认为 0 的 0xFD 作样例。
    #[test]
    fn non_ascii_single_bytes_untouched() {
        let s = vec![0x80u8, 0xA0, 0xFD];
        assert_eq!(varact_ascii_case(&s, true), s);
    }

    // U-CP932(**已解除**,T5 取证 2026-10-16):
    //   0xF0–0xFC 前导区问题已定性 —— v494 引擎表 0x5b40a0 dump 显示
    //   0x81-0x9F 与 0xE0-0xFC 为 1(双字节前导),0xFD-0xFF 为 0;
    //   三处使用点(FUN_0045543c / FUN_0045f0dc / FUN_00444f10)交叉证实。
    //   实现已按此修正(见 util.rs `is_sjis_lead`)。
}

#[cfg(test)]
mod truthy_tests {
    use super::value_truthy;
    use yuris_value::Value;

    /// 三类元素类型的真值语义(为 Float 补的回归;原先 Float 直接报未实现)。
    #[test]
    fn truthiness_across_element_types() {
        assert!(value_truthy(&Value::Int(1)).unwrap());
        assert!(!value_truthy(&Value::Int(0)).unwrap());
        assert!(value_truthy(&Value::Str(b"x".to_vec())).unwrap());
        assert!(!value_truthy(&Value::Str(Vec::new())).unwrap());
        assert!(value_truthy(&Value::Float(0.5)).unwrap());
        assert!(value_truthy(&Value::Float(-0.5)).unwrap());
        assert!(!value_truthy(&Value::Float(0.0)).unwrap());
    }
}
