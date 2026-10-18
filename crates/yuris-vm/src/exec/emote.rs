//! E-mote 命令族（`EMOTE` / `EMOTEINFO`）。
//!
//! ## 取证：为什么本内核把它们实现为「按运行时存在与否分派的空操作」
//!
//! v494 引擎处理体 `EMOTE` = `FUN_00440468`（`0x440468..0x440bfa`，
//! `docs/engine/v494-handler-table.md` idx 12）。反汇编的第一个门是：
//!
//! ```text
//! 0x440489  MOVSX EAX, byte ptr [0x00802a28]   ; 「E-mote 运行时已装载」
//! 0x440490  TEST  EAX, EAX
//! 0x440492  JZ    0x00440b40                   ; 未装载 → 直接 return 0（空操作）
//! ```
//!
//! `0x802a28` 的唯一写入点是 `0x466635`（`MOV byte ptr [0x802a28], 1`），
//! 而它只在该处之前 `LoadLibraryA("emotedriver.dll")` → `LoadLibraryA("YSEmote.dll")`
//! 都成功、且逐个 `GetProcAddress` 取到 0xF1..0x100 号导出之后才执行
//! （`0x465f70..0x466635`）。任一步失败即 `JE 0x46663c` 跳过置位。
//!
//! ⇒ **装载失败时 `EMOTE` 在引擎里是彻底的空操作**（连参数都不求值：
//! 门的判断在 `FUN_00429f65` 参数序言之后、任何槽读取之前）。
//!
//! `EMOTEINFO`（idx 13）在 v494 处理体表里**没有独立项** —— 它落在共享
//! stub `FUN_0044658c`（`xor eax,eax; ret`）上，即**编译期/空实现**。
//!
//! ## 本内核的建模
//!
//! - 运行时可用性是一个**可注入状态** [`GroupVm::set_emote_runtime`]，
//!   缺省 `false`：本内核不装载 `YSEmote.dll`，也不实现 E-mote 渲染面，
//!   因此默认行为 = 引擎在「无 DLL」时的行为 = 空操作。
//! - `true` 时 `EMOTE` 显式挂起（strict）——E-mote 渲染面确实未实现，
//!   不能假装有效果。
//! - 空操作分支仍发 [`VmEvent::Emote`] 事件，保证对拍可见（引擎侧无事件，
//!   但本内核的事件流用于诊断；不影响脚本状态）。

use yuris_core::Result;
use yuris_format::ystb::{CommandGroup, CommandSlot};

use crate::event::{VmEvent, VmSuspend};
use crate::exec::Action;
use crate::util::value_summary;
use crate::GroupVm;

impl GroupVm {
    /// `EMOTE[ID=… LABEL=… PLAY=… …]`（E-mote 角色动画控制）。
    ///
    /// 见模块文档：运行时未装载时引擎整条命令空操作。
    pub(crate) fn cmd_emote(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        let cmd = g.command_type;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        // 引擎的门的判断发生在参数序言之后、任何槽读取之前 —— 但**参数仍被
        // 序言解析**(0x429f65 已按 YSCM 参数表切好窗口)。故这里求值只为
        // 诊断可见性，与引擎的「不求值」差异仅在**无副作用**的求值上。
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if self.window_is_lvalue(w) {
                evaluated.push((slot, "<ref>".into()));
                continue;
            }
            match self.eval_window_condition(Some(w)) {
                Ok(Some(v)) => evaluated.push((slot, value_summary(&v))),
                Ok(None) => {}
                Err(e) => self.note_swallowed(self.pc, cmd, "EMOTE 参数", &e),
            }
        }

        if self.emote_runtime {
            // 运行时在位 = 本内核确实缺 E-mote 渲染面 → 显式挂起，不假装。
            self.push_event(VmEvent::Emote {
                pc: self.pc,
                runtime: true,
                evaluated,
            });
            let reason = format!(
                "EMOTE:检测到 E-mote 运行时在位,但本内核未实现 E-mote 动画/蒙皮\n\
                 现场: script {} pc {}\n\
                 处置: strict —— 挂起(strict 下上屏错误窗)",
                self.ctx.script_id, self.pc
            );
            if let Some(r) = self.halt_or_record(self.pc, cmd, reason) {
                return Ok(Action::Suspend(VmSuspend::Error(r)));
            }
        } else {
            // 引擎在无 YSEmote.dll 时的同款行为:空操作(不碰任何状态)。
            self.push_event(VmEvent::Emote {
                pc: self.pc,
                runtime: false,
                evaluated,
            });
        }
        self.pc += 1;
        Ok(Action::Keep)
    }

    /// `EMOTEINFO[...]`：v494 处理体表里落在共享 stub（空实现）。
    ///
    /// 引擎 `FUN_0044658c` = `xor eax,eax; ret` —— 连 `LET` 写回都不做。
    /// 本内核按空操作实现，事件化记录参数以便对拍。
    pub(crate) fn cmd_emoteinfo(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        let _ = g;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if self.window_is_lvalue(w) {
                evaluated.push((slot, "<ref>".into()));
                continue;
            }
            match self.eval_window_condition(Some(w)) {
                Ok(Some(v)) => evaluated.push((slot, value_summary(&v))),
                Ok(None) => {}
                Err(e) => self.note_swallowed(self.pc, g.command_type, "EMOTEINFO 参数", &e),
            }
        }
        self.push_event(VmEvent::EmoteInfo {
            pc: self.pc,
            evaluated,
        });
        self.pc += 1;
        Ok(Action::Keep)
    }

    /// 设置 E-mote 运行时可用性（缺省 `false` = 与无 `YSEmote.dll` 的引擎一致）。
    pub fn set_emote_runtime(&mut self, available: bool) {
        self.emote_runtime = available;
    }

    /// E-mote 运行时可用性。
    pub fn emote_runtime(&self) -> bool {
        self.emote_runtime
    }
}
