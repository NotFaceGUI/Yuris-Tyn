//! `SYSTEMINFO` —— 引擎自述信息查询（脚本名 / 脚本数 / 文本数 / E-mote 标志）。
//!
//! ## 取证（v494 处理体 `0x450478`，逐槽反汇编）
//!
//! 参数表（游戏自身 YSCM）：`SCRIPTFILE / SCRIPTNUM / TEXTNUM / EMOTE / NO / LET`。
//!
//! | 槽 | 引擎动作 |
//! |---|---|
//! | `SCRIPTFILE`(0) | 需要 `NO=n`（1 基）；`1<=n<=count` 时取名字表 `0x802b28[n-1]`（`yst_list.ybn` 的源文件名指针）写进 `LET` 目标；越界 → 写空串 |
//! | `SCRIPTNUM`(1) | 写**脚本总数** `0x802a20`（= `yst_list.ybn` 条目数） |
//! | `TEXTNUM`(2) | 需要 `NO=n`；取文本数表 `0x802b2c[n-1]` 写进 `LET`；越界 → 0 |
//! | `EMOTE`(3) | 写 E-mote 运行时标志 `0x802a28`（0/1） |
//!
//! 两张表由 `0x45d650` 从 `ysbin\yst_list.ybn`（YSTL）建立 —— 本内核用
//! [`yuris_format::ystl::YstlTable`] 直接建模（格式见该模块文档）。
//!
//! **`NO` 是 1 基**（引擎 `0x4504da..0x4504e6` 先 `jle` 拒绝 `<=0`、
//! 再 `cmp` 上限，随后 `[table + eax*4 - 4]`）—— 与 `yst%05d.ybn` 的
//! 0 基脚本号**不同**，不要混用。

use yuris_core::{Error, Result};
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_value::Value;

use crate::event::VmEvent;
use crate::exec::Action;
use crate::util::{extract_var_target, value_summary, VarTarget};
use crate::GroupVm;

impl GroupVm {
    /// `SYSTEMINFO[SCRIPTFILE=… SCRIPTNUM=1 TEXTNUM=… EMOTE=1 NO=n LET=@v]`。
    pub(crate) fn cmd_systeminfo(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        let cmd = g.command_type;
        let s_scriptfile = self.param_slot(cmd, "SCRIPTFILE", 0);
        let s_scriptnum = self.param_slot(cmd, "SCRIPTNUM", 1);
        let s_textnum = self.param_slot(cmd, "TEXTNUM", 2);
        let s_emote = self.param_slot(cmd, "EMOTE", 3);
        let s_no = self.param_slot(cmd, "NO", 4);
        let s_let = self.param_slot(cmd, "LET", 5);

        let mut want_scriptfile = false;
        let mut want_scriptnum = false;
        let mut want_textnum = false;
        let mut want_emote = false;
        let mut no: Option<i64> = None;
        let mut target: Option<VarTarget> = None;
        let mut evaluated: Vec<(u8, String)> = Vec::new();

        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if Some(slot) == s_let {
                let bytes = self
                    .ctx
                    .script
                    .window_bytes_pooled_copy(w)
                    .ok_or_else(|| Error::format("SYSTEMINFO LET 引用窗越过池尾"))?;
                match extract_var_target(&bytes) {
                    Ok(t) => target = Some(t),
                    Err(e) => {
                        yuris_log::alert_once(
                            module_path!(),
                            format!("sysinfo-let-parse-fail:{}:{}", self.ctx.script_id, self.pc),
                            format!(
                                "SYSTEMINFO 的 LET 引用窗无法解析,查询结果不写回\n\
                                 现场: script {} pc {}\n\
                                 错误: {e}",
                                self.ctx.script_id, self.pc
                            ),
                        );
                    }
                }
                evaluated.push((slot, "<ref>".into()));
                continue;
            }
            if Some(slot) == s_no {
                // `NO` 是**值窗**(不是引用):取整数值。
                if let Some(v) = self.eval_window_condition(Some(w))? {
                    no = v.as_int_opt();
                    evaluated.push((slot, value_summary(&v)));
                }
                continue;
            }
            // 查询字段:引擎按「槽存在且值非 0」判为「发起查询」。
            let truthy = matches!(
                self.eval_window_condition(Some(w)),
                Ok(Some(v)) if v.as_int_opt().map(|n| n != 0).unwrap_or(false)
            );
            if Some(slot) == s_scriptfile {
                want_scriptfile |= truthy;
            } else if Some(slot) == s_scriptnum {
                want_scriptnum |= truthy;
            } else if Some(slot) == s_textnum {
                want_textnum |= truthy;
            } else if Some(slot) == s_emote {
                want_emote |= truthy;
            } else if self.window_is_lvalue(w) {
                self.note_unknown_slot(cmd, slot, "SYSTEMINFO 查询");
            } else if self.command_dispatch().param_name_of(cmd, slot).is_none() {
                self.note_unknown_slot(cmd, slot, "SYSTEMINFO 查询");
            }
            if let Ok(Some(v)) = self.eval_window_condition(Some(w)) {
                evaluated.push((slot, value_summary(&v)));
            }
        }

        // ---- 查询求值(引擎同序:SCRIPTNUM → TEXTNUM → SCRIPTFILE → EMOTE)----
        let mut answers: Vec<(&'static str, Value)> = Vec::new();
        if want_scriptnum {
            let n = self.ystl.as_ref().map(|t| t.len()).unwrap_or(0);
            answers.push(("SCRIPTNUM", Value::Int(n as i64)));
        }
        if want_textnum {
            let v = match (self.ystl.as_ref(), no) {
                (Some(t), Some(n)) if n >= 1 && (n as usize) <= t.len() => {
                    t.get(n as u32 - 1).map(|e| e.text_count as i64).unwrap_or(0)
                }
                (Some(_), Some(_)) => 0, // 越界 → 引擎写 0
                _ => 0,
            };
            answers.push(("TEXTNUM", Value::Int(v)));
        }
        if want_scriptfile {
            let bytes = match (self.ystl.as_ref(), no) {
                (Some(t), Some(n)) if n >= 1 && (n as usize) <= t.len() => t
                    .get(n as u32 - 1)
                    .map(|e| e.name.clone())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            answers.push(("SCRIPTFILE", Value::Str(bytes)));
        }
        if want_emote {
            answers.push(("EMOTE", Value::Int(if self.emote_runtime { 1 } else { 0 })));
        }

        // 写回:引擎一次查询只写一个 LET 目标;多字段同时给时按上面的次序
        // 依次写(后写覆盖,与引擎同槽写回语义一致)。
        if let Some(t) = &target {
            for (_, v) in &answers {
                self.write_var_target(t, v.clone())?;
            }
        }

        self.push_event(VmEvent::SystemInfo {
            pc: self.pc,
            no,
            answered: answers
                .iter()
                .map(|(name, v)| (name.to_string(), value_summary(v)))
                .collect(),
            evaluated,
        });
        self.pc += 1;
        Ok(Action::Keep)
    }
}
