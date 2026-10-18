//! 命令执行器:把 `run_main` 的巨型 `match` 逐族拆成独立方法。
//!
//! ## 为什么要拆
//!
//! 旧实现把全部 ~30 个命令族的处理体塞在一个 2800 行的 `run_main` 里,
//! 其中夹着 95 个 `continue` 与 33 个 `return` —— 全部直接操作主循环控制流。
//! 后果:任何一个命令的修改都要在 2800 行里定位,且无法单独测试。
//!
//! ## 拆分方式:**动作枚举**,不是简单搬函数
//!
//! 直接把臂搬成 `fn exec_cg(&mut self) -> Result<()>` 是**做不到**的:
//! 臂里的 `continue` 表达的是"本组处理完,取下一组",`return` 表达的是
//! "挂起"。所以本模块定义 [`Action`],每个处理器返回它,由 `run_main`
//! 的循环统一执行 —— 控制流语义 1:1 保留,而处理器本身成为可读、可测的单元。
//!
//! ```text
//! // run_main 里的形态(示意):
//! let action = match kind {
//!     CmdKind::Wait => self.cmd_wait(g, windows)?,
//!     CmdKind::Cg   => self.cmd_cg(g, windows)?,
//!     // …
//! };
//! match action {
//!     Action::Next       => { self.pc += 1; }
//!     Action::Goto(pc)   => { self.pc = pc; }
//!     Action::Suspend(s) => return Ok(s),
//!     Action::Keep       => {}   // pc 已由处理器自己改好(如跨脚本切换)
//!     Action::Break      => break,
//! }
//! ```
//!
//! 命名与 REA `docs/engine/command-layer.md` 的 `CMDH_*` 处理器一一对应。


use crate::event::{VmSuspend, VmEvent};

/// 一个命令组执行完后的**控制流动作**。
///
/// 这是 `run_main` 旧 `match` 里 `continue` / `self.pc = x` / `return` 三种
/// 意图的结构化表达 —— 语义等价,但处理器不必再知道主循环长什么样。
#[derive(Debug)]
pub enum Action {
    /// 顺序推进到下一组(`self.pc += 1`;旧代码的 `self.pc += 1; continue;`)。
    Next,
    /// 跳转到指定组(旧代码的 `self.pc = target; continue;`)。
    Goto(usize),
    /// 挂起(旧代码的 `return Ok(VmSuspend::…)`)。
    Suspend(VmSuspend),
    /// 不动作:`pc` 已由处理器自行改好(跨脚本切换等),直接进入下一轮。
    Keep,
    /// 跳出主循环(旧代码里 `break` 的等价形态;目前无使用者,留作扩展)。
    Break,
}

impl Action {
    /// 顺序推进的简写。
    pub fn next() -> Self {
        Action::Next
    }
}

/// 处理器公共上下文:一次分派需要的全部信息。
///
/// 借用规则:`windows` 与 `g` 是 `run_main` 在进入分派前克隆/取值的快照,
/// 所以处理器可以拿 `&mut self`(VM 自身)—— 这正是旧代码在臂里做的事
/// (`self.push_event(...)` / `self.switch_script(...)` 等)。
pub struct Ctx<'a> {
    /// 本组的窗口切片(12B 记录;`run_main` 已克隆,避免借用冲突)。
    pub windows: &'a [yuris_format::ystb::CommandSlot],
    /// 本组的 YSCM 命令下标。
    pub command: u8,
    /// 本组所在组号(`run_main` 进入时的 `self.pc`)。
    pub pc: usize,
}

/// 记录「本组已执行」事件(绝大多数处理器都要发它;旧代码里重复了 ~30 次)。
/// 经 [`GroupVm::push_event`] 入流,携带当前域标记(主线/任务;P10 域调度)。
pub(crate) fn push_group_executed(vm: &mut GroupVm, pc: usize, command: u8) {
    vm.push_event(VmEvent::GroupExecuted {
        pc,
        command,
        condition: None,
    });
}

pub mod flow;
pub mod vars;
pub mod media;
pub mod io;
pub mod misc;
pub mod emote;
pub mod sysinfo;
pub mod taskinfo;
pub mod system;

use yuris_format::ystb::{CommandGroup, CommandSlot};

use crate::CmdKind;
use crate::GroupVm;

impl GroupVm {
    /// 命令分派:与版本无关的 [`CmdKind`] → 命令族处理器(`exec/` 下)。
    ///
    /// 这是 `run_main` 旧巨型 `match` 的唯一留存形态 —— 每个分支只做一件事:
    /// 把调用转给对应方法。控制流(推进 / 跳转 / 挂起)由处理器返回的
    /// [`Action`] 表达,`run_main` 统一执行。
    pub(crate) fn dispatch(
        &mut self,
        kind: CmdKind,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> yuris_core::Result<Action> {
        use CmdKind::*;
        match kind {
            Go => self.cmd_go(g, windows),
            Gosub => self.cmd_gosub(g, windows),
            ReturnCode => self.cmd_returncode(g, windows),
            ReturnBreak => self.cmd_returnbreak(g, windows),
            Return => self.cmd_return(g, windows),
            If => self.cmd_if(g, windows),
            IfBlend => self.cmd_ifblend(g, windows),
            Else => self.cmd_else(g, windows),
            IfEnd => self.cmd_ifend(g, windows),
            Loop => self.cmd_loop(g, windows),
            LoopBreak => self.cmd_loopbreak(g, windows),
            LoopContinue => self.cmd_loopcontinue(g, windows),
            LoopEnd => self.cmd_loopend(g, windows),
            Wait => self.cmd_wait(g, windows),
            End => self.cmd_end(g, windows),
            Let => self.cmd_let(g, windows),
            VarInfo => self.cmd_varinfo(g, windows),
            VarAct => self.cmd_varact(g, windows),
            LabelInfo => self.cmd_labelinfo(g, windows),
            Cg => self.cmd_cg(g, windows),
            CgInfo => self.cmd_cginfo(g, windows),
            CgAct => self.cmd_cgact(g, windows),
            CgEnd => self.cmd_cgend(g, windows),
            Text => self.cmd_text(g, windows),
            Word => self.cmd_word(g, windows),
            Sound => self.cmd_sound(g, windows),
            SoundEnd => self.cmd_soundend(g, windows),
            Load => self.cmd_load(g, windows),
            Save => self.cmd_save(g, windows),
            SaveEnd => self.cmd_saveend(g, windows),
            // `ERROR`:错误输出级别配置(引擎侧只写进程错误号/节流位,不改
            // 脚本流;见 `CmdKind::Error` 的取证说明)。
            Error => self.cmd_error(g, windows),
            // `FILEACT`:文件操作意图(VM 不碰平台 I/O;见 `CmdKind::FileAct`)。
            FileAct => self.cmd_fileact(g, windows),
            // `SYSTEM`:屏保/后台执行/屏幕尺寸/计时器(引擎处理体已逐槽取证)。
            System => self.cmd_system(g, windows),
            // `SYSTEMINFO`:查询真值来自 yst_list.ybn(引擎处理体已逐槽取证)。
            SystemInfo => self.cmd_systeminfo(g, windows),
            // `EMOTE`:E-mote 运行时不在位时引擎整条命令空操作(已取证)。
            Emote => self.cmd_emote(g, windows),
            EmoteInfo => self.cmd_emoteinfo(g, windows),
            // `TASKINFO`:任务状态查询(引擎处理体 0x452270 已逐槽取证)。
            TaskInfo => self.cmd_taskinfo(g, windows),
            // 媒体查询族：写回「无媒体对象」的自洽值（T23）;结束族：清媒体模式 + 空操作。
            MediaInfo => self.cmd_media_info(g, windows),
            MediaPlay | MediaPlayLayer | MediaPlayAudio => self.cmd_media_play(g, windows),
            MediaEnd => {
                // 「退出媒体再生模式」→ 清除场景层屏蔽位（T23b）。
                // 命名上四个 END 命令都归此族；只有 MOVIEEND/FLASHEND 会真的
                // 见到置位状态（YMVEND/MIDIEND 从未置位），故统一清一次是幂等的。
                let cname = self
                    .command_dispatch()
                    .name_of(g.command_type as u16)
                    .unwrap_or("<媒体END?>")
                    .to_string();
                if matches!(cname.as_str(), "MOVIEEND" | "FLASHEND") {
                    self.push_event(VmEvent::MediaMode {
                        pc: self.pc,
                        on: false,
                        command: cname,
                    });
                }
                crate::exec::push_group_executed(self, self.pc, g.command_type);
                Ok(Action::Next)
            }
            _ => self.cmd_fallback(kind, g, windows),
        }
    }
}
