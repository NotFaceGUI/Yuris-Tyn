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
use crate::{cmd, GroupVm};
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
                } else if Some(slot) == s_id || Some(slot) == s_idno {
                    // 图层载入(ID/IDNO)未实装 —— 明确留痕,不静默。
                    self.note_unknown_slot(cmd, slot, "LOAD 图层载入(ID/IDNO)未实装");
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
            // 存档容器(.sd)读写尚未实装 ⇒ 只记录意图,不写假值。
            evaluated.push((
                254,
                format!("全局组读取 {:?}(存档容器未实装)", groups),
            ));
        }
        if let (Some(fname), Some(dno_v), Some(lval)) = (file.clone(), dno, &target) {
            if let Some(mem_v) = mem {
                if mem_v != 0 {
                    return Err(self.halt_err(
                        self.pc,
                        cmd::LOAD,
                        "LOAD MEM(内存源)路径未实现(语料未出现;不猜)",
                    ));
                }
            }
            if !lval.indices.is_empty() {
                return Err(self.halt_err(
                    self.pc,
                    cmd::LOAD,
                    "LOAD 写回目标带下标(基点偏移语义未逆向;语料未出现)",
                ));
            }
            self.load_yssd(&fname, dno_v, &lval.var)?;
        }
        self.push_event(VmEvent::Load {
            pc: self.pc,
            file,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `Save`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_save(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_00451838(反编译逐分支,2026-09-04):
        // DNO = 存档号;SET = 位图/变量引用;FILE = 文件名(无 FILE 时
        // 只做「预留保存」,写出发生在带 FILE 的那一条)。
        // a0 分支:按 SET 引用的描述符类型写位图 —
        // INT(desc byte+1==1)→ desc+0x30 区 8B 槽 = SET 值 ?1:0;
        // FLT(byte+1==2)→ desc+0x34 区 8B = SET 值 ?1.0:0.0;
        // STR(==3)→ FUN_0045b9f8 显示层(无存储写入)。
        // 语料 298 组:[DNO,SET]×284(=a0 分支置位/清零)、[FILE]×10、
        // [SET]×2、[DNO,ID]×2。SET 引用是左值目标(提取后写值)。
        //
        // **槽位按参数名解析(勘误 2026-10-18)**:旧实现写死 v555 槽号
        // (`2=DNO / 3=SET / 0=FILE`),而本作 v494 是 `FILE(0) DNO(1)
        // SET(2) ID(3) IDNO(4) G_INT(5)…` ⇒ DNO 落到"未知槽"、G_INT2/
        // G_STR2 等组选择位一律告警丢弃。实机日志 `SAVE 槽 1 = DNO`
        // 与 `槽 6 = G_INT2` 即此。
        let cmd = g.command_type;
        let slot_of = |me: &Self, name: &str, fallback: u8| -> Option<u8> {
            me.param_slot(cmd, name, fallback)
        };
        let s_file = slot_of(self, "FILE", 0);
        let s_dno = slot_of(self, "DNO", 2);
        let s_set = slot_of(self, "SET", 3);
        let s_id = slot_of(self, "ID", 4);
        let s_idno = slot_of(self, "IDNO", 5);
        let group_slots: Vec<(u8, &'static str)> = Self::SAVE_GROUP_SLOTS
            .iter()
            .filter_map(|n| slot_of(self, n, 255).map(|s| (s, *n)))
            .collect();

        let mut dno: Option<i64> = None;
        let mut file: Option<Vec<u8>> = None;
        let mut set_ref: Option<Vec<u8>> = None;
        let mut set_flag = false;
        let mut groups: Vec<&'static str> = Vec::new();
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if Some(slot) == s_set {
                // SET 双形态:变量引用窗(首 op 0x48/0x56/0x76,
                // 延迟求值 = SET 目标)/ 纯值窗(SET 置位标志)。
                // 语料 298 组恒单 SET 窗;双窗形态仅合成测试。
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
                    set_ref = Some(bytes);
                } else if let Some(v) = self.eval_window_condition(Some(w))? {
                    if let Value::Int(n) = &v {
                        set_flag = *n != 0;
                    }
                    evaluated.push((slot, value_summary(&v)));
                }
            } else {
                // **勘误(2026-10-13,成果 120)**:引用形窗
                // (左值,op `0x56`/`0x76`)是**脚本自备存档槽**
                // (SAVE 按引用读 / LOAD 按引用写回),不是表达式。
                // 旧实现一律送 `eval_window_condition` ⇒ 命中
                // 「窗口以左值引用结尾」并**致命挂起**;实机
                // `s12 g664 SAVE[1, @1113]`(0x76 窗)即此形态,
                // 后果是标题流程走到存档初始化就整条停摆。
                // 本内核无存档存储(`yuris-save` 空骨架)
                // ⇒ 只记录引用、不写值、不猜。
                let packed = self.ctx.script.window_bytes_pooled_copy(w);
                let is_ref = packed
                    .as_ref()
                    .map(|b| {
                        yuris_script::decode_window(b)
                            .map(|ins| {
                                ins.iter()
                                    .any(|i| matches!(i.raw_op, 0x56 | 0x76))
                            })
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if is_ref {
                    let sum = packed
                        .as_ref()
                        .and_then(|b| extract_var_target(b).ok())
                        .map(|t| t.summary());
                    evaluated.push((
                        slot,
                        format!(
                            "引用槽(脚本自备存档槽){}",
                            sum.unwrap_or_else(|| "?".into())
                        ),
                    ));
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
                } else if Some(slot) == s_id || Some(slot) == s_idno {
                    self.note_unknown_slot(cmd, slot, "SAVE 图层保存(ID/IDNO)未实装");
                } else if let Some((_, gname)) = group_slots.iter().find(|(s, _)| *s == slot) {
                    if matches!(&v, Value::Int(n) if *n != 0) {
                        groups.push(gname);
                    }
                } else {
                    // 参数名未建模(或该版本没有这个参数)⇒ 值被忽略。
                    self.note_unknown_slot(cmd, slot, "SAVE 参数槽(参数名未建模)");
                }
                evaluated.push((slot, value_summary(&v)));
            }
        }
        if !groups.is_empty() {
            // 组选择位:SAVE 时的语义 = 把该 (类型,恢复组) 的全部全局变量
            // 写入存档(= `SET=@G()` 的等价写法;SDK `es_GSD.yst` 实证)。
            let n = self.ysvr_group_len(groups.as_slice());
            evaluated.push((
                253,
                format!("全局组保存 {:?}({n} 个变量;存档容器未实装)", groups),
            ));
        }
        let mut set_target = None;
        if let Some(bytes) = set_ref {
            // **勘误(2026-10-18):SET 是「要保存的变量」,不是写回目标。**
            //
            // 官方手册 `file/save.html`:`SET = 変数名`(「保存する変数」);
            // `LOAD` 才是 `LET = 変数名`(「読み出した値を格納する変数」)。
            // 旧实现把 SET 引用当左值**写入** `set_flag ? 1 : 0` ——
            // 那等于每存一次档就把被存的变量改成 0/1。v555 布局下该槽恰
            // 不落在被执行的 SET 分支里(Long→`_`→is_ref→仅记录),所以
            // 缺陷没显形;一旦按参数名解析到 v494 的 SET(槽 2),实机
            // `SAVE[1, @1113]` 立刻炸:「array dimension mismatch: got 0
            // indices, declared 2」—— @1113 是 INT[256,128] 数组,
            // 而 `@arr` 无下标形式正是手册示例[3]的**整数组保存**。
            //
            // 本内核没有存档容器 ⇒ 只记录「要保存谁」,绝不回写。
            let target = extract_var_target(&bytes)?;
            // 无下标引用(0x76 裸引用)= 整数组保存(手册示例[3]
            // `SAVE[… SET=@HA()]`;s37 g50 亦证)。带下标 = 单元素。
            let (whole_array, var_ref) = match &target {
                VarTarget::Scalar(r) => (self.store.has_array(r), *r),
                VarTarget::Indexed(r, _) => (false, *r),
            };
            set_target = Some(if whole_array {
                format!("{}()", target.summary())
            } else {
                target.summary()
            });
            if whole_array {
                evaluated.push((
                    252,
                    format!(
                        "整数组保存 {} (存档容器未实装)",
                        var_ref.display()
                    ),
                ));
            }
            // `set_flag` 只在非引用形态出现(= 组选择位的值);引用形态下
            // 它没有语义,不参与任何写入。
            let _ = set_flag;
        }
        self.push_event(VmEvent::Save {
            pc: self.pc,
            file,
            dno,
            set_target,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `SAVEEND`(官方手册 `file/saveend.html`):清空 `SAVE` 预留数据。
    ///
    /// **成果 133**:片头/存档初始化链(`s12 g663`)调用它 —— 未实装时
    /// strict 模式挂起、掐断后续流程(与 SOUNDEND 同一失效模式)。
    ///
    /// 语义:手册说明「把 `SAVE`(无 FILE)依次预留的数据清空;`SAVE[FILE=…]`
    /// 写盘时会自动清空」。本内核的 `SAVE` 是**逐条即时事件化**
    /// (不积累预留列表),故 SAVEEND 等价于"释放预留缓冲"——无缓冲可清,
    /// 行为等价于空操作;事件化记录以保持对拍可见。
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
