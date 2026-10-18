//! 系统命令族（`SYSTEM` / `SYSTEMINFO`）—— 屏保、后台执行、屏幕尺寸、计时器。
//!
//! ## 取证来源
//!
//! v494 引擎处理体：`SYSTEM` = `0x4500d0`、`SYSTEMINFO` = `0x450478`
//! （`docs/engine/v494-handler-table.md` 的 idx 91/92）。
//! 参数表来自游戏自身 YSCM（逐版本漂移，故一律按**参数名**取槽）：
//!
//! ```text
//! SYSTEM      : SCREENSAVER BACKGROUNDEXEC SX SY TIMECOUNT TIMECOUNT2 TIMECOUNT3 TIMECOUNT4
//! SYSTEMINFO  : SCRIPTFILE SCRIPTNUM TEXTNUM EMOTE NO LET
//! ```
//!
//! ## 处理体逐槽语义（反汇编 `0x4500d0..0x450284`）
//!
//! | 槽 | 引擎动作 |
//! |---|---|
//! | `SCREENSAVER`(0) | 值低字节写进 `0x5b4748`（屏保抑制标志）；消费者 `0x45ad22` 在其非 0 时直接返回（跳过屏保/挂起判定） |
//! | `BACKGROUNDEXEC`(1) | 仅当任务对象 `+0x2a8 == 1`（非前台任务）时写 `0x5b4700` |
//! | `SX`/`SY`(2/3) | **两者都 > 0** 才应用：写引擎分辨率全局 `0x80297c/0x802980` 与任务 `+0xc/+0x10`；若窗口/全屏标志均未置位，同步 `0x802974/0x802978` 与 `0x802984/0x802988`（显示模式基准）；否则走 `0x41c750` 显示模式重建 + `0x4503f8` 后台缓冲区按新尺寸重建 |
//! | `TIMECOUNT`…`TIMECOUNT4`(4..7) | 四个独立**时间累加器**：置 1 时记录 `timeGetTime()` 起点；置 0 时把「当前 − 起点」累加进 `0x802934/802938/80293c/802940` 并清起点。起点寄存器为 `0x802508/802510/802518/802520` |
//!
//! 本内核的对应关系：
//! - `SX/SY` → [`GroupVm::set_screen_size`]（已有单一真值面）；
//! - `SCREENSAVER`/`BACKGROUNDEXEC` → VM 侧状态位（供播放器查询）；
//! - `TIMECOUNT*` → **累加毫秒计数**（引擎 `timeGetTime` 起点/差值模型，
//!   与 `chain_now_ms` 同源：VM 每帧写入的单调毫秒）。
//!
//! `SYSTEMINFO` 是**查询**命令（`LET` 目标 + 查询字段），引擎处理体在
//! `0x450478`；本内核按「无真值可答」显式挂起（strict），不写假值。

use yuris_core::Result;
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_value::Value;

use crate::event::VmEvent;
use crate::exec::Action;
use crate::util::value_summary;
use crate::GroupVm;

/// `SYSTEM` 的 `TIMECOUNT*` 累加器槽数。
const TIMECOUNT_SLOTS: usize = 4;

impl GroupVm {
    /// `SYSTEM[SCREENSAVER=… BACKGROUNDEXEC=… SX=… SY=… TIMECOUNT=… …]`。
    ///
    /// 见模块文档的逐槽表。**不存在**「未实装」分支：每个参数都有取证过的
    /// 语义，未给该参数 = 引擎保持原值。
    pub(crate) fn cmd_system(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        let cmd = g.command_type;
        let s_saver = self.param_slot(cmd, "SCREENSAVER", 0);
        let s_bg = self.param_slot(cmd, "BACKGROUNDEXEC", 1);
        let s_sx = self.param_slot(cmd, "SX", 2);
        let s_sy = self.param_slot(cmd, "SY", 3);
        let s_time: Vec<Option<u8>> = (0..TIMECOUNT_SLOTS)
            .map(|i| {
                let name = if i == 0 {
                    "TIMECOUNT".to_string()
                } else {
                    format!("TIMECOUNT{}", i + 1)
                };
                self.param_slot(cmd, &name, 4 + i as u8)
            })
            .collect();

        let mut evaluated: Vec<(u8, String)> = Vec::new();
        let mut new_sx: Option<i64> = None;
        let mut new_sy: Option<i64> = None;
        let mut time_sets: Vec<(usize, bool)> = Vec::new();

        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            let Some(v) = self.eval_window_condition(Some(w))? else {
                continue;
            };
            let as_int = |v: &Value| -> Option<i64> {
                match v {
                    Value::Int(n) => Some(*n),
                    Value::Float(f) => Some(*f as i64),
                    Value::Str(_) => None,
                }
            };
            if Some(slot) == s_saver {
                if let Some(n) = as_int(&v) {
                    self.screensaver_suppressed = n != 0;
                }
            } else if Some(slot) == s_bg {
                // 引擎只在「非前台任务」时应用（任务对象 +0x2a8 == 1）。
                // 本内核的域标记 `cur_domain != 0` 即任务侧步进（主线恒 0），
                // 见 `driver::step_command` 的换入模型。
                if let Some(n) = as_int(&v) {
                    if self.cur_domain != 0 {
                        self.background_exec = n != 0;
                    }
                }
            } else if Some(slot) == s_sx {
                new_sx = as_int(&v);
            } else if Some(slot) == s_sy {
                new_sy = as_int(&v);
            } else if let Some(i) = s_time.iter().position(|s| *s == Some(slot)) {
                if let Some(n) = as_int(&v) {
                    time_sets.push((i, n != 0));
                }
            } else if self.window_is_lvalue(w) {
                // 引用窗不是求值失败；但 SYSTEM 无引用参数 → 未建模槽。
                self.note_unknown_slot(cmd, slot, "SYSTEM 系统设置");
            } else if self.command_dispatch().param_name_of(cmd, slot).is_none() {
                self.note_unknown_slot(cmd, slot, "SYSTEM 系统设置");
            }
            evaluated.push((slot, value_summary(&v)));
        }

        // `SX`/`SY`：引擎要求**两者都 > 0** 才应用（`0x450151` 的两个 jle）。
        if let (Some(w), Some(h)) = (new_sx, new_sy) {
            if w > 0 && h > 0 {
                self.set_screen_size(w, h);
            }
        }
        // `TIMECOUNT*`：置 1 = 记起点，置 0 = 把差值累加进累计计数。
        for (i, on) in time_sets {
            if on {
                self.timecount_start[i] = Some(self.chain_now_ms);
            } else if let Some(start) = self.timecount_start[i].take() {
                let delta = self.chain_now_ms.wrapping_sub(start) as i64;
                self.timecount_total[i] = self.timecount_total[i].wrapping_add(delta);
            }
        }

        self.push_event(VmEvent::SystemConfig {
            pc: self.pc,
            evaluated,
        });
        self.pc += 1;
        Ok(Action::Keep)
    }

    /// 屏保抑制标志（`SYSTEM[SCREENSAVER=1]`）。
    pub fn screensaver_suppressed(&self) -> bool {
        self.screensaver_suppressed
    }

    /// 后台执行标志（`SYSTEM[BACKGROUNDEXEC=1]`，仅非主任务生效）。
    pub fn background_exec(&self) -> bool {
        self.background_exec
    }

    /// `TIMECOUNT*` 的累计毫秒（下标 0..4 对应 `TIMECOUNT`/`TIMECOUNT2..4`）。
    pub fn timecount_totals(&self) -> [i64; TIMECOUNT_SLOTS] {
        self.timecount_total
    }
}
