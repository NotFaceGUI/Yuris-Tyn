//! 存档族(从 `run_main` 巨型 match 拆出;语义逐行照搬,未改行为)。
//!
//! 拆分背景见 [`crate::exec`] 模块文档。**控制流约定**:方法体里不再出现
//! 「目标为外层 while」的 `continue` / `return Ok(VmSuspend…)` —— 分别替换为
//! `return Ok(Action::Keep)` / `return Ok(Action::Suspend(…))`,由 `run_main`
//! 的循环统一执行(语义 1:1)。

use yuris_core::{Error, Result};
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_value::Value;

use crate::event::VmEvent;
use crate::exec::Action;
use crate::util::{
    extract_var_target, value_summary, VarTarget,
};
use crate::GroupVm;
impl GroupVm {


    /// `LOAD` 的 12 个**脚本自备存档槽**(`G_INT..G_STR4`)。
    ///
    /// 语义(官方手册 `file/load.html` + SDK 自带 `es_GSD.yst`):
    /// `SAVE[DNO=03 G_INT2=1]` 与 `SAVE[DNO=03 SET=@G()]` 等价 ——
    /// 该槽不是"数值"，而是**恢复组选择位**（组 1..4 / 类型 INT-FLT-STR）。
    /// 这里只记录"要求读取哪些组"，真正的 .sd 容器读写尚未实装。
    const SAVE_GROUP_SLOTS: [&'static str; 12] = [
        "G_INT", "G_INT2", "G_INT3", "G_INT4", "G_FLT", "G_FLT2", "G_FLT3", "G_FLT4",
        "G_STR", "G_STR2", "G_STR3", "G_STR4",
    ];

    /// 处理 `Load`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_load(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_00444648(文件分支,反编译 p5_sysvar/):
        // FILE(求值;无扩展名 → 补 ".sd";'/'→'\')、MEM(非零 = 内存源,
        // 语料未出现)、DNO(块号,1 基 → YSSD 偏移表[DNO-1])、
        // LET(写回目标,**延迟引用**;s35 g705 实证,成果 59f)、
        // ID(非零 = 0x690 特殊配置格式,语料未出现)。
        // 块类型 0 = 普通载荷;1 = 0x690 配置;2 = 多子块(未出现)。
        //
        // **勘误(2026-10-18)**:旧实现按**写死的 v555 槽序** match 槽号
        // (`0=FILE / 1=MEM / 2=DNO / 3=LET / 4=ID`)。实测本作 v494 的槽序
        // 是 `FILE(0) DNO(1) LET(2) ID(3) IDNO(4) G_INT(5)…` ——
        // 于是 LOAD 把 **DNO 当 MEM**、把 **LET 引用窗当 ID 值**求值，
        // 存档链读到的是错槽(SAVE 侧同病,实机日志的「槽 1 = DNO」
        // 就是它)。现一律按**游戏自身 YSCM 的参数名**解析;无参数表时才
        // 回落 v555 号(可观测,`note_slot_fallback`)。
        let cmd = g.command_type;
        let slot_of = |me: &Self, name: &str, fallback: u8| -> Option<u8> {
            me.param_slot(cmd, name, fallback)
        };
        let s_file = slot_of(self, "FILE", 0);
        let s_mem = slot_of(self, "MEM", 1);
        let s_dno = slot_of(self, "DNO", 2);
        let s_let = slot_of(self, "LET", 3);
        let s_id = slot_of(self, "ID", 4);
        let s_idno = slot_of(self, "IDNO", 5);
        let group_slots: Vec<(u8, &'static str)> = Self::SAVE_GROUP_SLOTS
            .iter()
            .filter_map(|n| slot_of(self, n, 255).map(|s| (s, *n)))
            .collect();

        let mut file = None;
        let mut load_id: Option<Vec<u8>> = None;
        let mut mem: Option<i64> = None;
        let mut dno: Option<i64> = None;
        let mut groups: Vec<&'static str> = Vec::new();
        let mut target: Option<yuris_script::LValueRef> = None;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if Some(slot) == s_let {
                // 写回目标 = 左值引用窗(0x76 varidx / 0x48 var 单条,
                // s35 g705 与 s120 g17 实证),不求值。
                let bytes = self
                    .ctx
                    .script
                    .window_bytes_pooled_copy(w)
                    .ok_or_else(|| Error::format("LOAD 引用窗越过池尾"))?;
                let lval = self.eval_window_lvalue(&bytes)?;
                evaluated.push((slot, format!("@{}", lval.var.display())));
                target = Some(lval);
                continue;
            }
            if let Some(v) = self.eval_window_condition(Some(w))? {
                if Some(slot) == s_file {
                    if let Value::Str(bytes) = &v {
                        file = Some(bytes.clone());
                    }
                } else if Some(slot) == s_mem {
                    mem = v.as_int_opt();
                } else if Some(slot) == s_dno {
                    dno = v.as_int_opt();
                } else if Some(slot) == s_id {
                    // ID = 层名(btype1 存档截图块的目标层;成果 196)。
                    if let Value::Str(b) = &v {
                        load_id = Some(b.clone());
                    }
                } else if Some(slot) == s_idno {
                    // IDNO = 图层文件序号(引擎拼 `save\<ID><IDNO>` 路径;
                    // 本作语料恒走 ID 路径,IDNO 未出现 —— 留痕)。
                    self.note_unknown_slot(cmd, slot, "LOAD IDNO(语料未出现)");
                } else if let Some((_, gname)) = group_slots.iter().find(|(s, _)| *s == slot) {
                    if matches!(&v, Value::Int(n) if *n != 0) {
                        groups.push(gname);
                    }
                } else {
                    self.note_unknown_slot(cmd, slot, "LOAD 参数槽(参数名未建模)");
                }
                evaluated.push((slot, value_summary(&v)));
            }
        }
        if !groups.is_empty() {
            // 12 个自备槽 = 恢复组选择位(见 SAVE_GROUP_SLOTS 文档)。
            // btype=2 装载按子块自带 var 写回,与 LET 无关;组选择位本身
            // 只是意图记录(与 SAVE 侧对称)。
            evaluated.push((254, format!("全局组读取 {groups:?}")));
        }
        if let (Some(fname), Some(dno_v)) = (file.clone(), dno) {
            if let Some(mem_v) = mem {
                if mem_v != 0 {
                    return Err(self.halt_err(
                        self.pc,
                        g.command_type,
                        "LOAD MEM(内存源)路径未实现(语料未出现;不猜)",
                    ));
                }
            }
            let target = match &target {
                Some(lval) => {
                    if !lval.indices.is_empty() {
                        return Err(self.halt_err(
                            self.pc,
                            g.command_type,
                            "LOAD 写回目标带下标(基点偏移语义未逆向;语料未出现)",
                        ));
                    }
                    Some(lval.var.clone())
                }
                None => None,
            };
            let summary = self.load_yssd(&fname, dno_v, target.as_ref(), load_id.as_deref())?;
            if std::env::var_os("YURIS_TRACE_LOAD").is_some() {
                yuris_log::trace_forced!(
                    "[load] s{} pc{} file={:?} dno={dno_v} target={:?} → {summary}",
                    self.ctx.script_id,
                    self.pc,
                    String::from_utf8_lossy(&fname),
                    target.as_ref().map(yuris_value::VarRef::display),
                );
            }
            evaluated.push((254, summary));
        }
        self.push_event(VmEvent::Load {
            pc: self.pc,
            file,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// `SAVE` 预留/刷盘(v494 处理器 `0x44c2d8` 直译;2026-10-18 实装)。
    ///
    /// 引擎语义(手册 `file/save.html` + 反汇编):
    /// - `SAVE[DNO=… SET=@v]` —— 把变量**当前值**序列化成 YSSD 块体,
    ///   存进预留表(引擎 `0x44d5a7`:btype/strict/var 头 + compress2;
    ///   整数组引用 = 全元素,手册示例[3] `SET=@HA()`);
    /// - `SAVE[DNO=… G_INT2=1]` —— 12 自备槽 = 恢复组选择位,等价
    ///   `SET=@G()`(SDK `es_GSD.yst` 双写实证)= **btype=2 多变量块**,
    ///   子块 = 该 (类型,恢复组) 的全部 kind1 全局(引擎 `0x44d740` 形态);
    /// - `SAVE[FILE=…]` —— 把预留表全部刷盘(容器 = 头 + 0x400 偏移表 +
    ///   逐块 `dno + 块体`,引擎 `0x44cba7`),然后**清空预留**(手册例[6]);
    /// - `SAVE[DNO=… ID=…]` —— 配置块(btype=1)预留,序列化的是引擎
    ///   per-script 配置库(引擎 `0x44d9d0`)—— 本内核配置库未建模,显式
    ///   留痕不写假块(铁律 2)。
    ///
    /// 槽位按游戏自身 YSCM 参数名解析(勘误 2026-10-18,成果 193);v494
    /// 实测槽序 `FILE(0) DNO(1) SET(2) ID(3) IDNO(4) G_INT(5)…`。
    pub(crate) fn cmd_save(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        let cmd = g.command_type;
        let slot_of = |me: &Self, name: &str, fallback: u8| -> Option<u8> {
            me.param_slot(cmd, name, fallback)
        };
        let s_file = slot_of(self, "FILE", 0);
        let s_dno = slot_of(self, "DNO", 1);
        let s_set = slot_of(self, "SET", 2);
        let s_id = slot_of(self, "ID", 3);
        let s_idno = slot_of(self, "IDNO", 4);
        let group_slots: Vec<(u8, &'static str)> = Self::SAVE_GROUP_SLOTS
            .iter()
            .filter_map(|n| slot_of(self, n, 255).map(|s| (s, *n)))
            .collect();

        let mut dno: Option<i64> = None;
        let mut file: Option<Vec<u8>> = None;
        let mut set_ref: Option<Vec<u8>> = None;
        let mut set_summary: Option<String> = None;
        let mut id_name: Option<String> = None;
        let mut groups: Vec<&'static str> = Vec::new();
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if Some(slot) == s_set {
                // SET 双形态:变量引用窗(首 op 0x48/0x56/0x76 = 保存目标,
                // 延迟取值)/ 纯值窗(组选择位的值)。勘误(成果 193):
                // SET 是「保存する変数」,绝不回写。
                let bytes = self
                    .ctx
                    .script
                    .window_bytes_pooled_copy(w)
                    .ok_or_else(|| Error::format("SAVE SET 引用窗越过池尾"))?;
                let is_ref = yuris_script::decode_window(&bytes)
                    .map(|ins| {
                        ins.first()
                            .map(|i| matches!(i.raw_op, 0x48 | 0x56 | 0x76))
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if is_ref {
                    let target = extract_var_target(&bytes)?;
                    // 无下标引用(0x76 裸引用)= 整数组保存(手册示例[3]
                    // `SAVE[… SET=@HA()]`;s37 g50 亦证)。带下标 = 单元素。
                    let (whole_array, var_ref) = match &target {
                        VarTarget::Scalar(r) => (self.store.has_array(r), *r),
                        VarTarget::Indexed(r, _) => (false, *r),
                    };
                    set_summary = Some(if whole_array {
                        format!("{}()", target.summary())
                    } else {
                        target.summary()
                    });
                    set_ref = Some(bytes);
                    if whole_array {
                        evaluated.push((252, format!("整数组保存 {}", var_ref.display())));
                    }
                } else if let Some(v) = self.eval_window_condition(Some(w))? {
                    evaluated.push((slot, value_summary(&v)));
                }
                continue;
            }
            let Some(v) = self.eval_window_condition(Some(w))? else {
                continue;
            };
            if Some(slot) == s_dno {
                if let Value::Int(n) = &v {
                    dno = Some(*n);
                }
            } else if Some(slot) == s_file {
                if let Value::Str(b) = &v {
                    file = Some(b.clone());
                }
            } else if Some(slot) == s_id {
                if let Value::Str(b) = &v {
                    id_name = Some(String::from_utf8_lossy(b).into_owned());
                }
            } else if Some(slot) == s_idno {
                // IDNO = 图层文件序号(SAVE[ID+IDNO] 的落盘文件名后缀);
                // 配置块写出未建模,值仅进 evaluated 记录。
            } else if let Some((_, gname)) = group_slots.iter().find(|(s, _)| *s == slot) {
                if matches!(&v, Value::Int(n) if *n != 0) {
                    groups.push(gname);
                }
            } else {
                // 参数名未建模(或该版本没有这个参数)⇒ 值被忽略,留痕。
                self.note_unknown_slot(cmd, slot, "SAVE 参数槽(参数名未建模)");
            }
            evaluated.push((slot, value_summary(&v)));
        }

        // ① 预留(引擎在 SAVE 命令处即时序列化 —— `0x44d5a7` 体)。
        if let Some(dno_v) = dno {
            if !(1..=1024).contains(&dno_v) {
                return Err(self.halt_err(
                    self.pc,
                    g.command_type,
                    format!("SAVE DNO={dno_v} 越界(手册 1~1024)"),
                ));
            }
            if let Some(bytes) = set_ref {
                let body = self.save_body_from_ref(&bytes)?;
                self.save_reserved.insert(dno_v as u32, body);
            } else if !groups.is_empty() {
                let body = self.save_body_from_groups(&groups)?;
                self.save_reserved.insert(dno_v as u32, body);
            }
            if id_name.is_some() {
                // 配置块(btype=1)写出需要 per-script 配置库(引擎 0x44d9d0
                // 序列化的是配置子系统的变量)—— 未建模,显式留痕。
                self.note_unknown_slot(
                    cmd,
                    s_id.unwrap_or(255),
                    "SAVE ID= 配置块写出(配置子系统未建模)",
                );
            }
        }

        // ② 刷盘(手册:FILE 关键字写出预留并自动清空;SAVEEND 同清)。
        if let Some(fname) = file.clone() {
            let mut b = yuris_format::yssd::YssdBuilder::default();
            for (d, body) in &self.save_reserved {
                b.set_block(*d, body.clone());
            }
            let blocks = b.block_count();
            let bytes = b.build();
            let mut name = String::from_utf8_lossy(&fname).into_owned();
            if !name.contains('.') {
                name.push_str(".sd");
            }
            let len = bytes.len();
            self.saved_files.insert(name.clone(), bytes);
            self.push_event(VmEvent::SaveFile {
                pc: self.pc,
                name: name.into_bytes(),
                len,
                blocks,
            });
            self.save_reserved.clear();
        }

        self.push_event(VmEvent::Save {
            pc: self.pc,
            file,
            dno,
            set_target: set_summary,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// SET 引用 → YSSD 块体(压缩;strict=1 —— 引擎 SAVE 序列化恒写 1,
    /// `0x44d5ac`/`0x44d745`)。标量/单元素 = dims 空载荷;整数组引用 =
    /// 全元素扁平载荷(行主序,与 [`yuris_value::VariableStore::array_data`]
    /// 同序,即引擎描述符 memcpy 序)。
    fn save_body_from_ref(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        let target = extract_var_target(bytes)?;
        match target {
            VarTarget::Scalar(r) => {
                if self.store.has_array(&r) {
                    let ty = elem_type_code(self.store.array_elem_type(&r).unwrap());
                    let dims = self.store.array_dims(&r).unwrap_or_default();
                    let values = self.store.array_data(&r).unwrap_or(&[]).to_vec();
                    let payload = payload_from_values(ty, dims, &values)?;
                    Ok(yuris_format::yssd::encode_block_body(0, 1, r.id, &payload, true))
                } else {
                    let (ty, v) = self.scalar_of(&r)?;
                    let payload = payload_from_values(ty, Vec::new(), std::slice::from_ref(&v))?;
                    Ok(yuris_format::yssd::encode_block_body(0, 1, r.id, &payload, true))
                }
            }
            VarTarget::Indexed(r, idx_expr) => {
                let ty = if self.store.has_array(&r) {
                    elem_type_code(self.store.array_elem_type(&r).unwrap())
                } else {
                    self.scalar_ty(&r)?
                };
                let indices = self.eval_target_indices(&idx_expr)?;
                let v = self.store.get_elem(&r, &indices)?.clone();
                let payload = payload_from_values(ty, Vec::new(), std::slice::from_ref(&v))?;
                Ok(yuris_format::yssd::encode_block_body(0, 1, r.id, &payload, true))
            }
        }
    }

    /// 恢复组选择位 → btype=2 多变量块体(引擎 `0x44d740` 形态:
    /// 外层 var 头 = 0,子块自带 var;每子块 = 该组一个全局的整块载荷)。
    fn save_body_from_groups(&mut self, groups: &[&str]) -> Result<Vec<u8>> {
        let vars = self.ysvr_group_vars(groups);
        if vars.is_empty() {
            return Err(self.halt_err(
                self.pc,
                self.cur_cmd(),
                format!("SAVE 恢复组 {groups:?} 无 kind1 全局(YSVR 未注入或组号错)"),
            ));
        }
        let mut subs: Vec<(&u16, Vec<u8>)> = Vec::with_capacity(vars.len());
        for (id, ty) in &vars {
            let r = yuris_value::VarRef {
                space: yuris_value::VarSpace::At,
                id: *id,
            };
            let payload = if self.store.has_array(&r) {
                let dims = self.store.array_dims(&r).unwrap_or_default();
                let values = self.store.array_data(&r).unwrap_or(&[]).to_vec();
                payload_from_values(*ty, dims, &values)?
            } else {
                // kind1 全局在引擎侧恒有描述符(零/初值);本 store 未触碰
                // 时按类型取零值(与 YSVR 初值应用的零值分支一致)。
                let v = zero_value(*ty);
                payload_from_values(*ty, Vec::new(), std::slice::from_ref(&v))?
            };
            subs.push((id, payload));
        }
        let refs: Vec<(&u16, &[u8])> = subs.iter().map(|(id, p)| (*id, p.as_slice())).collect();
        Ok(yuris_format::yssd::encode_multi_body(&refs))
    }

    /// 标量当前值与类型码(未初始化 → 类型零值;描述符语义,见上)。
    fn scalar_of(&self, r: &yuris_value::VarRef) -> Result<(u8, Value)> {
        match self.store.get_opt(r) {
            Some(v) => {
                let ty = match v {
                    Value::Int(_) => 1u8,
                    Value::Float(_) => 2,
                    Value::Str(_) => 3,
                };
                Ok((ty, v.clone()))
            }
            None => {
                let ty = self.scalar_ty(r)?;
                Ok((ty, zero_value(ty)))
            }
        }
    }

    /// 标量声明类型码(Yaddock:YSVR 无此 id 时报未声明,不猜)。
    fn scalar_ty(&self, r: &yuris_value::VarRef) -> Result<u8> {
        self.ysvr_globals
            .iter()
            .find(|e| e.var_id == r.id)
            .map(|e| e.ty)
            .filter(|t| *t != 0)
            .ok_or_else(|| {
                Error::format(format!("SAVE 目标 {} 未声明(YSVR/声明命令无记录)", r.display()))
            })
    }

    /// `SAVEEND`(官方手册 `file/saveend.html`):清空 `SAVE` 预留数据。
    ///
    /// 本内核的预留表 = [`GroupVm::save_reserved`];`SAVE[FILE=…]` 刷盘
    /// 时已自动清空(手册例[6]),SAVEEND = 无条件清空 + 事件化记录。
    pub(crate) fn cmd_saveend(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        let _ = g;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if let Some(v) = self.eval_window_condition(Some(w))? {
                evaluated.push((slot, value_summary(&v)));
            }
        }
        let cleared = self.save_reserved.len();
        self.save_reserved.clear();
        if cleared > 0 {
            evaluated.push((253, format!("清空预留 {cleared} 块")));
        }
        self.push_event(VmEvent::SaveEnd {
            pc: self.pc,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }


    /// `ERROR[NO=… COM_CG=… …]`:**错误输出级别配置**(不是错误触发)。
    ///
    /// 取证(v494 处理体 `0x4411f8`,REA 反汇编):
    /// ```text
    ///   mov eax, 0x0e ; call prologue   ; 取本命令参数窗口
    ///   test al,al  ; jne -> mov eax,-1 ; ret   ; 参数解码失败 → 返回 -1(让出)
    ///   [0x802b44] = 1  ;节流位
    ///   [0x802804] = al ;错误号(参数值低字节)
    ///   xor eax,eax ; ret                       ; 成功 → 0(继续)
    /// ```
    /// 全函数**没有任何**脚本 pc / 调用栈 / 变量 / 场景写操作。参数表
    /// (YSCM `NO / SET / COM_CG / COM_CGACT / …`)也说明它是按命令设置
    /// 错误输出级别的配置命令;SDK 自带 `es_ERIS.yst:33` 的
    /// `ERROR[COM_CG=0]` 正是启动配置。
    ///
    /// 本内核按「记录配置 + 推进 pc」实现:消费方(播放器)可据
    /// [`VmEvent::ErrorConfig`] 调自己的日志等级,但脚本流不受影响。
    /// **不写任何变量/场景** —— 引擎侧这两者也不写。
    pub(crate) fn cmd_error(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        let cmd = g.command_type;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        let mut no: Option<i64> = None;
        let no_slot = self.param_slot(cmd, "NO", 0);
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            match self.eval_window_condition(Some(w)) {
                Ok(Some(v)) => {
                    if Some(slot) == no_slot {
                        no = v.as_int_opt();
                    } else if Some(slot) != no_slot {
                        // 其余 `COM_<命令>` 槽(逐版本漂移,按名取):记录级别。
                        if let Some(name) = self.command_dispatch().param_name_of(cmd, slot) {
                            if let Some(level) = v.as_int_opt() {
                                self.error_levels.insert(name, level);
                            }
                        } else {
                            // 槽位无对应参数名 = 该版本没有这个参数(不猜语义)。
                            self.note_unknown_slot(cmd, slot, "ERROR 错误输出级别");
                        }
                    }
                    evaluated.push((slot, value_summary(&v)));
                }
                Ok(None) => {}
                Err(e) => self.note_swallowed(self.pc, cmd, "ERROR 参数", &e),
            }
        }
        if let Some(v) = no {
            self.error_code = Some(v);
        }
        self.push_event(VmEvent::ErrorConfig {
            pc: self.pc,
            no,
            evaluated,
        });
        self.pc += 1;
        Ok(Action::Keep)
    }

    /// `ERROR` 当前设置(诊断/查询;`COM_<命令>` 名 → 级别)。
    pub fn error_levels(&self) -> &std::collections::HashMap<String, i64> {
        &self.error_levels
    }

    /// `ERROR[NO=…]` 设置的错误号(引擎进程级错误码字节的等价物)。
    pub fn error_code(&self) -> Option<i64> {
        self.error_code
    }

    /// `FILEACT[FILE=… FILE2=… FOLDER=… CREATE=… COPY=… DELETE=… EXEC=… …]`。
    ///
    /// 手册 `file/fileact.html` 给全参数语义;引擎处理体(v494 `0x441234`)
    /// 逐槽分支实测:
    /// - `CREATE`(槽3)→ `0x4426cc` 按 `\` 分段逐级建目录(`CreateDirectoryA`);
    /// - `COPY`(槽4)→ `CopyFileA(FILE, FILE2, 0)`;
    /// - `DELETE`(槽5)→ `DeleteFileA(FILE)` + `RemoveDirectoryA(FOLDER)`;
    /// - `EXEC`(槽6)/`EXEC2` → `ShellExecuteExA`(`SEE_MASK_NOCLOSEPROCESS`),
    ///   `PARAM` 作命令行参数;
    /// - `SET`/`BMP`/`PNG`/`JPG`/`ID` → 图层导出(本内核无导出面,记意图);
    /// - `CGCASH`/`SOUNDCASH` → 缓存登记。
    ///
    /// **VM 不执行文件 I/O** —— `RuntimeApi` 是 Kernel 与平台的唯一边界。
    /// 这里只把参数解析成 [`VmEvent::FileAct`],由播放器按自身根目录执行。
    pub(crate) fn cmd_fileact(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        let cmd = g.command_type;
        let slot = |me: &Self, name: &str, fallback: u8| me.param_slot(cmd, name, fallback);
        let s_file = slot(self, "FILE", 0);
        let s_file2 = slot(self, "FILE2", 1);
        let s_folder = slot(self, "FOLDER", 2);
        let s_create = slot(self, "CREATE", 3);
        let s_copy = slot(self, "COPY", 4);
        let s_delete = slot(self, "DELETE", 5);
        let s_exec = slot(self, "EXEC", 6);
        let s_exec2 = slot(self, "EXEC2", 7);
        let s_param = slot(self, "PARAM", 8);
        let s_set = slot(self, "SET", 9);
        let s_id = slot(self, "ID", 10);
        let s_bmp = slot(self, "BMP", 12);
        let s_png = slot(self, "PNG", 13);
        let s_jpg = slot(self, "JPG", 14);
        let s_cg_cache = slot(self, "CGCASH", 15);
        let s_sound_cache = slot(self, "SOUNDCASH", 16);

        let mut file = None;
        let mut file2 = None;
        let mut folder = None;
        let mut param = None;
        let mut id = None;
        let mut create = None;
        let mut copy = None;
        let mut delete = None;
        let mut exec = None;
        let mut exec2 = None;
        let mut set = None;
        let mut bmp = None;
        let mut png = None;
        let mut jpg = None;
        let mut cg_cache = None;
        let mut sound_cache = None;

        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot_no = (w.tag & 0xff) as u8;
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
            let as_str = |v: &Value| -> Option<String> {
                matches!(v, Value::Str(_)).then(|| v.str_as_string())
            };
            if Some(slot_no) == s_file {
                file = as_str(&v);
            } else if Some(slot_no) == s_file2 {
                file2 = as_str(&v);
            } else if Some(slot_no) == s_folder {
                folder = as_str(&v);
            } else if Some(slot_no) == s_param {
                param = as_str(&v);
            } else if Some(slot_no) == s_id {
                id = as_str(&v);
            } else if Some(slot_no) == s_create {
                create = as_int(&v);
            } else if Some(slot_no) == s_copy {
                copy = as_int(&v);
            } else if Some(slot_no) == s_delete {
                delete = as_int(&v);
            } else if Some(slot_no) == s_exec {
                exec = as_int(&v);
            } else if Some(slot_no) == s_exec2 {
                exec2 = as_int(&v);
            } else if Some(slot_no) == s_set {
                set = as_int(&v);
            } else if Some(slot_no) == s_bmp {
                bmp = as_int(&v);
            } else if Some(slot_no) == s_png {
                png = as_int(&v);
            } else if Some(slot_no) == s_jpg {
                jpg = as_int(&v);
            } else if Some(slot_no) == s_cg_cache {
                cg_cache = as_int(&v);
            } else if Some(slot_no) == s_sound_cache {
                sound_cache = as_int(&v);
            } else if self.window_is_lvalue(w) {
                // 引用窗(无对应参数名)不是求值失败;不猜其语义。
                self.note_unknown_slot(cmd, slot_no, "FILEACT 文件操作");
            } else if self.command_dispatch().param_name_of(cmd, slot_no).is_none() {
                self.note_unknown_slot(cmd, slot_no, "FILEACT 文件操作");
            }
        }

        self.push_event(VmEvent::FileAct {
            pc: self.pc,
            file,
            file2,
            folder,
            create,
            copy,
            delete,
            exec,
            exec2,
            param,
            set,
            id,
            bmp,
            png,
            jpg,
            cg_cache,
            sound_cache,
        });
        self.pc += 1;
        Ok(Action::Keep)
    }
}

/// 数组元素类型 → YSSD 载荷类型码(1=INT 2=FLT 3=STR)。
fn elem_type_code(e: yuris_value::ElemType) -> u8 {
    match e {
        yuris_value::ElemType::Int => 1,
        yuris_value::ElemType::Float => 2,
        yuris_value::ElemType::Str => 3,
    }
}

/// 声明类型码 → 零值(kind1 全局的描述符零值形态)。
fn zero_value(ty: u8) -> Value {
    match ty {
        2 => Value::Float(0.0),
        3 => Value::Str(Vec::new()),
        _ => Value::Int(0),
    }
}

/// 变量值序列 → YSSD 载荷(`ty + ndims + dims + dlen + 数据`;引擎
/// SAVE 序列化 `0x44da71` 的产物形态,即 [`yuris_format::yssd::YssdPayload`])。
/// 类型收敛与 LOAD 写回对称(Int→FLT 取 f64;FLT→INT 四舍五入)。
fn payload_from_values(
    ty: u8,
    dims: Vec<u32>,
    values: &[Value],
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    for v in values {
        match (ty, v) {
            (1, Value::Int(i)) => data.extend_from_slice(&i.to_le_bytes()),
            (1, Value::Float(f)) => data.extend_from_slice(&(*f).round().to_le_bytes()),
            (2, Value::Int(i)) => data.extend_from_slice(&(*i as f64).to_le_bytes()),
            (2, Value::Float(f)) => data.extend_from_slice(&f.to_le_bytes()),
            (3, Value::Str(s)) => {
                data.extend_from_slice(&(s.len() as u32).to_le_bytes());
                data.extend_from_slice(s);
            }
            _ => {
                return Err(Error::format(format!(
                    "SAVE 载荷类型 {ty} 与值 {v:?} 不符"
                )))
            }
        }
    }
    Ok(yuris_format::yssd::YssdPayload { ty, dims, data }.to_bytes())
}
