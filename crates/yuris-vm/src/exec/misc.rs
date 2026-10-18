//! 兜底与声明族(从 `run_main` 巨型 match 拆出;语义逐行照搬,未改行为)。
//!
//! 拆分背景见 [`crate::exec`] 模块文档。**控制流约定**:方法体里不再出现
//! 「目标为外层 while」的 `continue` / `return Ok(VmSuspend…)` —— 分别替换为
//! `return Ok(Action::Keep)` / `return Ok(Action::Suspend(…))`,由 `run_main`
//! 的循环统一执行(语义 1:1)。

use yuris_core::{Error, Result};
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_script::Evaluator;
use yuris_value::{Value, VarRef};

use crate::event::{VmEvent, VmSuspend};
use crate::exec::Action;
use crate::util::{
    extract_var_target, value_summary, VarTarget,
};
use crate::{
    CmdKind, GroupVm, DEFAULT_FONT_PX, MAX_TASKS,
};
impl GroupVm {
    /// `MATH` 运算写回(T22;引擎处理体 `0x4482c4`)。
    ///
    /// 参数槽(YSCM 表序,SDK 495 与 v555 逐条同序):
    /// `0=LET`(写回目标)/ `1=SET` / `2=SET2` / `3=SETSTR` /
    /// `4..=21` = 各运算开关(SIN…HASH)/ `22=CALC`(v555 专有,未取证)。
    ///
    /// **运算选择器 = 引擎 `test` 链的原序**(`MATH_CHAIN`):同时给出多个
    /// 运算开关时,取链序中**最先出现的那个** —— 与 `YSCom.ycd` 的参数顺序
    /// 不同,必须按链序实现(证据:另一条线对处理体的逐 `test` 反汇编)。
    ///
    /// 语义(两条独立线取证,见 [`crate::math`] 模块文档):
    /// - 一元族(SIN…LOG10 / SQRT / EXP / LOG / LOG10)读 `SET` 成 f64 → 写回;
    /// - `ATAN2`/`POW` 读 `SET`/`SET2`,写浮点;
    /// - `ABS` 按 `SET` 窗的 B2 类型位分流:整数窗 → `wrapping_abs` 写整数,
    ///   否则 `f64::abs` 写浮点;
    /// - `RND` 读 `SET`/`SET2` 为整数,写 `lo + draw % (hi − lo + 1)`;
    /// - `HASH` 对 `SETSTR` 串求**标准 CRC-32**,写整数。
    ///
    /// 未实装:精确语义未取证的运算(如 `CALC`)→ **不写回、不猜**,事件里
    /// 如实记录(调用方读到变量原值)。
    pub(crate) fn cmd_math(&mut self, windows: &[CommandSlot]) -> Result<()> {
        let cmd = self.cur_command.unwrap_or(0);
        // 槽 → 窗(只取非空窗;空窗 = 未给该参数)
        let slot_of = |param: &str, fallback: u8| -> u8 { self.param_slot(cmd, param, fallback).unwrap_or(255) };
        let let_slot = slot_of("LET", 0);
        let set_slot = slot_of("SET", 1);
        let set2_slot = slot_of("SET2", 2);
        let setstr_slot = slot_of("SETSTR", 3);
        let mut target: Option<VarTarget> = None;
        let mut present: Vec<(u8, &CommandSlot)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if slot == let_slot {
                if let Ok(bytes) = self
                    .ctx
                    .script
                    .window_bytes_pooled_copy(w)
                    .ok_or_else(|| Error::format("MATH LET 引用窗越过池尾"))
                {
                    match extract_var_target(&bytes) {
                        Ok(t) => target = Some(t),
                        Err(e) => {
                            // LET 引用窗解析失败 ⇒ 写回不会发生(静默丢结果)。
                            yuris_log::alert_once(
                                module_path!(),
                                format!("math-let-parse-fail:{}:{}", self.ctx.script_id, self.pc),
                                format!(
                                    "MATH 的 LET 引用窗无法解析,写回被跳过
                                     现场: script {} pc {}
                                     错误: {e}
                                     处置: 运算结果不落变量(调用方读到原值)",
                                    self.ctx.script_id, self.pc
                                ),
                            );
                        }
                    }
                }
            } else {
                present.push((slot, w));
            }
        }
        // 运算槽位:**按参数名解析**(槽号逐版本漂移;严禁写死数字)。
        // 引擎 test 链原序(0x4482c4;另一条线逐 `test` 反汇编):
        // 同时给多个运算开关时,取链序中**最先出现**的那个。
        const MATH_CHAIN: [&str; 18] = [
            "SIN", "COS", "TAN", "ASIN", "ACOS", "ATAN", "SINH", "COSH", "TANH",
            "ATAN2", "SQRT", "ABS", "EXP", "LOG", "LOG10", "POW", "RND", "HASH",
        ];
        let selector = MATH_CHAIN.iter().copied().find(|name| {
            self.param_slot(cmd, name, 255)
                .map(|s| present.iter().any(|(slot, _)| *slot == s))
                .unwrap_or(false)
        });
        let Some(selector) = selector else {
            // 一个运算都没给:引擎处理体末尾 `xor eax,eax; ret` —— 空操作
            return Ok(());
        };
        // 读参数值(求值失败 → 该参数按缺省处理,与引擎"未声明读取容忍"一致)。
        //
        // **2026-10-17**:失败不再无声 —— 求值错误会打去重横幅;但运算仍按
        // 引擎容忍语义继续(参数缺失 = 0)。缺参数是 MATH 的常见形态,
        // 不能因此升级为致命。
        let read = |me: &mut Self, slot: u8| -> Option<Value> {
            let w = present.iter().find(|(s, _)| *s == slot).map(|(_, w)| *w)?;
            match me.eval_window_condition(Some(w)) {
                Ok(v) => v,
                Err(e) => {
                    yuris_log::alert_once(
                        module_path!(),
                        format!("math-param-eval-fail:{slot}"),
                        format!(
                            "MATH 参数求值失败,按缺省 0 参与运算\n\
                             槽 = {slot}\n\
                             现场: script {} pc {}\n\
                             错误: {e}\n\
                             处置: 结果可信度下降;检查该参数表达式",
                            me.ctx.script_id, me.pc
                        ),
                    );
                    None
                }
            }
        };
        let as_f64 = |v: Option<Value>| -> f64 {
            match v {
                Some(Value::Int(i)) => i as f64,
                Some(Value::Float(f)) => f,
                _ => 0.0,
            }
        };
        let as_i64 = |v: Option<Value>| -> i64 {
            match v {
                Some(Value::Int(i)) => i,
                Some(Value::Float(f)) => f as i64,
                _ => 0,
            }
        };
        let set_val = read(self, set_slot);
        let set2_val = read(self, set2_slot);
        // 一元浮点族(运算名 ↔ 函数;按名分派,不按槽号)
        let unary: Option<fn(f64) -> f64> = match selector {
            "SIN" => Some(f64::sin),
            "COS" => Some(f64::cos),
            "TAN" => Some(f64::tan),
            "ASIN" => Some(f64::asin),
            "ACOS" => Some(f64::acos),
            "ATAN" => Some(f64::atan),
            "SINH" => Some(f64::sinh),
            "COSH" => Some(f64::cosh),
            "TANH" => Some(f64::tanh),
            "SQRT" => Some(f64::sqrt),
            "EXP" => Some(f64::exp),
            "LOG" => Some(f64::ln),
            "LOG10" => Some(f64::log10),
            _ => None,
        };
        let Some(target) = target else {
            // 无 LET 目标:运算无副作用可落(引擎会写 desc,本 VM 无目标 → 空操作)
            return Ok(());
        };
        let result: Value = match selector {
            "ATAN2" => Value::Float(as_f64(set_val).atan2(as_f64(set2_val))),
            "POW" => Value::Float(as_f64(set_val).powf(as_f64(set2_val))),
            "ABS" => {
                // ABS:SET 窗的 B2 类型位 == 1(@INT)→ 整数路径(引擎 0x448412)
                let is_int_window = present
                    .iter()
                    .find(|(s, _)| Some(*s) == self.param_slot(cmd, "SET", 1))
                    .map(|(_, w)| ((w.tag >> 16) & 0xFF) == 1)
                    .unwrap_or(false);
                if is_int_window {
                    Value::Int(as_i64(set_val).wrapping_abs())
                } else {
                    Value::Float(as_f64(set_val).abs())
                }
            }
            "RND" => Value::Int(self.math_rng.range(as_i64(set_val), as_i64(set2_val))),
            "HASH" => {
                let bytes = match read(self, setstr_slot) {
                    Some(Value::Str(b)) => b,
                    _ => Vec::new(),
                };
                Value::Int(i64::from(crate::math::crc32(&bytes)))
            }
            name => match unary {
                Some(f) => Value::Float(f(as_f64(set_val))),
                None => {
                    // 未取证运算开关(CALC 等):不写回、不猜。
                    // **2026-10-17 起不再静默**:调用方按 Err 升级为
                    // 「子系统缺失 ⇒ strict 挂起 + 致命错误窗」。
                    return Err(Error::format(format!(
                        "MATH 运算 {name} 语义未取证(CALC 等),不写回"
                    )));
                }
            },
        };
        self.write_var_target(&target, result)
    }

    /// `MATH3D` 3D→2D 投影(**引擎处理体 `0x44874c`**,4755B;U36 解除的主证据)。
    ///
    /// 形态(<game-b> s66 pc5355 反汇编 + 官方 `es.SP.XYZCALC` 源码双证):
    /// 两参数都是**裸变量引用**(`0x76` PushVarRef;见 [`crate::util::extract_var_target`
    /// 的"`0x76` 裸引用 = 整数组"实证),`INT=@i FLT=@f`:
    ///   - `INT` 整数组 `[0..6]` = 槽号 / 背景槽 / 相机槽 / 父槽 / 屏宽 / 屏高 / TIP;
    ///   - `FLT` 工作数组:脚本先填 `[13..27]`(精灵坐标/锚点/中心/父坐标/DEFZ),
    ///     处理器读 `[3..28]`(相机/背景块,未填按缺省 0 —— 引擎越界/缺省读同),
    ///     算完写回 **`[31..34]` = 屏X / 屏Y / 缩放X% / 缩放Y%**(百分比,100=原大);
    ///     脚本随后 `RETURN` 读回 `[31..34]` 与 5 个输入回传位。
    ///
    /// 元素映射 = 反编译数据区直读:**元素 N 于 data+8N**(`0x18`=N3 … `0xe0`=N28、
    /// 写 `0xf8..0x110`=N31..34;<game-b> XYZCALC 填 `[13..27]`、RETURN 读
    /// `[31..34]` 与之逐位咬合)。官方 ERIS 填 `@M3D.F(N)` 语义对照:
    /// N3..5=相机XYZ、N6..8=背景XYZ、N9..12=背景中心/裁剪、N13..15=精灵XYZ、
    /// N16..17=锚点B、N18..19=中心C、N20..21=M(步进÷100)、N22..26=父组、
    /// N27=DEFZ、N28=未填(未知,候补相机锚)。
    ///
    /// 数学(反编译直译;幂基常数 = 引擎 `0x58ee00/08/18` = 0.0/100.0/0.5):
    /// ```text
    /// zoom_bg = 2^((DEFZ−bgZ+camZ+CMZ)/100)
    /// X = WSX/2 + (bgX−bgCX−(camX+f28)·mx)·zoom_bg + bgCLX·mx
    /// Y = WSY/2 + (bgY−bgCY−(camY+CMY)·my)·zoom_bg + bgCLY·my
    /// 若 lno≠lbg:X2 = bgCX·zoom_bg+X;无父 → X = X2+(spX·mx−spCX)·zoom_sp;
    ///            有父 → zoom_p = 2^((DEFZ−pZ+CMZ+camZ)/100),
    ///            X = X2+((spX+pX)·mx−spCX−pCX)·zoom_p(同构 Y)
    /// 锚点混合:border X += spCX·(100−spBX)·zoom_sp/100(同构 Y)
    /// 输出:zx = spBX·zoom_sp、zy = spBY·zoom_sp
    /// ```
    /// TIP≠0(E-mote 族)= 非投影直通:X = spX−spCX·spBX/100,缩幅 = B 原值。
    ///
    /// `// UNVERIFIED`:两全局标量(FUN_004499e4 连读,反编译里寄存器传参未留 id)
    /// ——按"引擎相机全体量"建模为 0.0(<game-b> 片头路径相机全体量为 0);
    /// 接相机全体量时须在 PlayerCore 相机模型上补齐。x87 与 f64 的舍入方向未逐位对拍。
    pub(crate) fn cmd_math3d(&mut self, windows: &[CommandSlot]) -> Result<()> {
        let cmd = self.cur_command.unwrap_or(0);
        let int_slot = self.param_slot(cmd, "INT", 0).unwrap_or(255);
        let flt_slot = self.param_slot(cmd, "FLT", 1).unwrap_or(255);
        let mut i_ref: Option<VarRef> = None;
        let mut f_ref: Option<VarRef> = None;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            let which = if slot == int_slot {
                Some(0)
            } else if slot == flt_slot {
                Some(1)
            } else {
                None
            };
            let Some(which) = which else { continue };
            let Some(bytes) = self.ctx.script.window_bytes_pooled_copy(w) else {
                return Err(Error::format("MATH3D 引用窗越过池尾"));
            };
            match extract_var_target(&bytes) {
                Ok(VarTarget::Scalar(r)) => {
                    if which == 0 {
                        i_ref = Some(r);
                    } else {
                        f_ref = Some(r);
                    }
                }
                _ => {
                    // 引用窗非裸变量形态(语料外):不猜 —— 记录后原样返回
                    self.push_event(VmEvent::Subsystem {
                        pc: self.pc,
                        command: cmd,
                        evaluated: vec![(
                            254u8,
                            "MATH3D 引用窗非裸变量引用形态:不猜".to_string(),
                        )],
                    });
                    return Ok(());
                }
            }
        }
        let (Some(i_ref), Some(f_ref)) = (i_ref, f_ref) else {
            // INT/FLT 两參须齐备(引擎对缺参不动作)。
            return Ok(());
        };
        let dbg_on = std::env::var("YURIS_MATH3D_DEBUG").is_ok();
        let (out_x, out_y, out_zx, out_zy) = {
            let f_at = |i: i64| -> f64 {
                match self.store.get_elem(&f_ref, &[i]) {
                    Ok(Value::Int(v)) => *v as f64,
                    Ok(Value::Float(v)) => *v,
                    _ => 0.0,
                }
            };
            let i_at = |i: i64| -> i64 {
                match self.store.get_elem(&i_ref, &[i]) {
                    Ok(Value::Int(v)) => *v,
                    Ok(Value::Float(v)) => *v as i64,
                    _ => 0,
                }
            };
            let cam_x = f_at(3);
            let cam_y = f_at(4);
            let cam_z = f_at(5);
            let bg_x = f_at(6);
            let bg_y = f_at(7);
            let bg_z = f_at(8);
            let bg_cx = f_at(9);
            let bg_cy = f_at(10);
            let bg_clx = f_at(11);
            let bg_cly = f_at(12);
            let sp_x = f_at(13);
            let sp_y = f_at(14);
            let sp_z = f_at(15);
            let bx = f_at(16);
            let by = f_at(17);
            let cx = f_at(18);
            let cy = f_at(19);
            let mx = f_at(20) / 100.0;
            let my = f_at(21) / 100.0;
            let p_x = f_at(22);
            let p_y = f_at(23);
            let p_z = f_at(24);
            let p_cx = f_at(25);
            let p_cy = f_at(26);
            let def_z = f_at(27);
            let cmx2 = f_at(28);
            // **INT 元素下标 = 官方宏 I(N) = arr[N](1 基;元素 0 不用)**。
            // 证据:g5323–5329 yourselves 反汇编 —— XYZCALC 把 I(1..7) 写入
            // `@3521[1..7]`(元素 0 从不写);按 I(N)=arr[N-1] 读会得到
            // lno=0/lbg=4 的镜像错位(vardump→fill 数值吻合实证)。
            let lno = i_at(1);
            let lbg = i_at(2);
            let lpr = i_at(4);
            let wsx = i_at(5) as f64;
            let wsy = i_at(6) as f64;
            let tip = i_at(7);
            if dbg_on {
                let mut dbg = format!(
                    "[m3d@pc{}] i0..6=[{},{},{},{},{},{},{}]",
                    self.pc, lno, lbg, i_at(2), lpr, wsx, wsy, tip
                );
                for (k, v) in [
                    cam_x, cam_y, cam_z, bg_x, bg_y, bg_z, bg_cx, bg_cy, bg_clx, bg_cly,
                    sp_x, sp_y, sp_z, bx, by, cx, cy, f_at(20), f_at(21),
                    p_x, p_y, p_z, p_cx, p_cy, def_z, cmx2,
                ]
                .iter()
                .enumerate()
                {
                    dbg.push_str(&format!(" f{:02}={}", k + 3, v));
                }
                yuris_log::trace_forced!("{dbg}");
            }
            // 引擎侧相机全体量(UNVERIFIED;<game-b> 片头为 0)
            const CMY: f64 = 0.0;
            const CMZ: f64 = 0.0;
            let d_var1 = cam_z + CMZ;
            if tip == 0 {
                let zoom_bg = f64::exp2((def_z - bg_z + cam_z + CMZ) / 100.0);
                let mut zoom_sp = f64::exp2((def_z - sp_z + d_var1) / 100.0);
                let mut x = (bg_x - bg_cx - (cam_x + cmx2) * mx) * zoom_bg
                    + bg_clx * mx
                    + wsx * 0.5;
                let mut y = bg_cly * my
                    + (wsy * 0.5)
                    + ((bg_y - bg_cy) - (cam_y + CMY) * my) * zoom_bg;
                if lno != lbg {
                    let x2 = bg_cx * zoom_bg + x;
                    let y2 = y + bg_cy * zoom_bg;
                    if lpr == 0 {
                        x = x2 + (sp_x * mx - cx) * zoom_sp;
                        y = y2 + (sp_y * my - cy) * zoom_sp;
                    } else {
                        zoom_sp = f64::exp2((def_z - p_z + d_var1) / 100.0);
                        x = x2 + ((sp_x + p_x) * mx - cx - p_cx) * zoom_sp;
                        y = y2 + ((sp_y + p_y) * my - cy - p_cy) * zoom_sp;
                    }
                }
                // 锚点混合与缩放输出(tip==0 层级,不随 lno==lbg 分支收敛)
                x += cx * (100.0 - bx) * zoom_sp / 100.0;
                y += cy * (100.0 - by) * zoom_sp / 100.0;
                let zx = bx * zoom_sp;
                let zy = by * zoom_sp;
                (x, y, zx, zy)
            } else {
                // TIP≠0(E-mote 族)非投影直通;缩幅 = B 原值(引擎写回同区)
                (
                    sp_x - cx * bx / 100.0,
                    sp_y - cy * by / 100.0,
                    bx,
                    by,
                )
            }
        };
        for (idx, v) in [(31i64, out_x), (32, out_y), (33, out_zx), (34, out_zy)] {
            if let Err(e) = self.store.set_elem(&f_ref, &[idx], Value::Float(v)) {
                // 引擎对越界写回是硬错误;这里如实记录后终止命令(不猜)
                return Err(Error::format(format!("MATH3D 写回 [{idx}] 失败:{e}")));
            }
        }
        if dbg_on {
            yuris_log::trace_forced!("[m3d] -> x={out_x} y={out_y} zx={out_zx} zy={out_zy}");
        }
        Ok(())
    }

    /// 处理 `_`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_fallback(
        &mut self,
        kind: CmdKind,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        let other = g.command_type;
        // 运行期声明族 INT(0x32)/FLT(0x19)/STR(0x5c):引擎有真
        // 处理器(P1 对拍实锤 + 反编译 00443538_CMD_INT_runtime.c /
        // 004411fc_CMD_FLT_0x19.c —— 两者逐行对称;STR 同形见
        // script12 g10「STR $1748 = $55[1]」且引擎 pc 顺序推进):
        //   w0 = 变量引用(描述符类型/边界重置),
        //   后续窗 = 初值表达式求值后写入标量。
        // 实证:script190 g28「INT @6292 = @53[2]」、g30
        // 「INT @6293 = 0」、script12 g11「INT @1749 = 0」。
        if matches!(
            kind,
            CmdKind::DeclLocalFloat | CmdKind::DeclLocalInt | CmdKind::DeclLocalStr
        ) {
            let mut target: Option<yuris_value::VarRef> = None;
            let mut array_form = false;
            if let Some(w0) = windows.first() {
                if w0.tag & 0xFF == 0 {
                    if let Some(bytes) = self.ctx.script.window_bytes_pooled_copy(w0) {
                        if let Ok(instrs) = yuris_script::decode_window(&bytes) {
                            for ins in &instrs {
                                match &ins.kind {
                                    yuris_script::Insn::PushVar(r)
                                    | yuris_script::Insn::PushVarRef(r) => {
                                        if target.is_none() {
                                            target = Some(r.clone());
                                        }
                                    }
                                    yuris_script::Insn::PushVarIndexed(_) => {
                                        array_form = true;
                                    }
                                    yuris_script::Insn::ArrayLoad { .. } => {
                                        array_form = true;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            let Some(r) = target else {
                let reason = format!(
                    "运行期声明 0x{other:02x}:窗口未含变量引用(不猜); \
                     array_form={array_form}"
                );
                if let Some(reason) =
                    self.halt_or_record(self.pc, other, reason)
                {
                    return Ok(Action::Suspend(VmSuspend::Error(reason)));
                }
                self.pc += 1;
                return Ok(Action::Keep);
            };
            // —— 数组形态:窗口 0 = `56 base | <上界表达式> | 29` ——
            // **实机勘误(2026-10-13)**:旧实现只置了 `array_form` 标志,
            // 从不真的建数组,于是 `STR $[0x10c2][@[0x10c3]]`(s70 g124)
            // 声明完之后 `$[0x10c2]` 仍然不存在 —— 首次下标写
            // (s70 g146 `LET $[0x10c2][@[0x10c3]] = $[62][1]`)
            // 直接 `undefined array variable $4290` 打死 VM。
            // 上界 = 窗口 0 里**基引用之后、ArrayLoad 之前**那段表达式的值
            // (与加载期消费的「上界引用」形态同源,成果 41/99)。
            if array_form && !self.store.has_array(&r) {
                let mut bound: Option<i64> = None;
                if let Some(w0) = windows.first() {
                    if w0.tag & 0xFF == 0 {
                        if let Some(bytes) = self.ctx.script.window_bytes_pooled_copy(w0) {
                            if let Ok(instrs) = yuris_script::decode_window(&bytes) {
                                // 去掉首条(基引用)与末条(ArrayLoad)
                                if instrs.len() >= 2 {
                                    let mid = &instrs[1..instrs.len() - 1];
                                    let locals = self.frames.last().map(|f| &f.locals);
                                    // 上下文须在 ev 之前取(ev 借走 store)
                                    let ctx = self.eval_ctx();
                                    let mut ev = Evaluator::with_locals(&mut self.store, locals);
                                    ctx.apply(&mut ev);
                                    match ev.eval_instructions(mid) {
                                        Ok(stack) => match stack.last() {
                                            Some(yuris_value::Value::Int(n)) => bound = Some(*n),
                                            _ => {}
                                        },
                                        Err(e) => {
                                            if self.strict {
                                                self.set_error_state();
                                                return Ok(Action::Suspend(VmSuspend::Error(
                                                    e.to_string(),
                                                )));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // 引擎界语义:声明的下标表达式即元素**个数**(成果 41:401);
                // 求不出上界 → 不建(维持未定义,交给后续证据)
                if let Some(n) = bound {
                    if n > 0 {
                        let et = match kind {
                            CmdKind::DeclLocalFloat => yuris_value::ElemType::Float,
                            CmdKind::DeclLocalStr => yuris_value::ElemType::Str,
                            _ => yuris_value::ElemType::Int,
                        };
                        self.store.declare_array(&r, et, &[n as u32]);
                        self.push_event(VmEvent::Declaration {
                            pc: self.pc,
                            command: other,
                            windows: g.window_count,
                        });
                        self.pc += 1;
                        return Ok(Action::Keep);
                    }
                }
            }
            // 初值 = 第一个非空后续窗的求值结果;无 → 类型默认值
            // (引擎 INT→FUN_0046a140(i64) / FLT→FUN_0046a060(f64);
            //  STR 同族字符串写入)
            let mut init = match kind {
                CmdKind::DeclLocalFloat => yuris_value::Value::Float(0.0),
                CmdKind::DeclLocalStr => yuris_value::Value::Str(Vec::new()),
                _ => yuris_value::Value::Int(0),
            };
            for w in windows.iter().skip(1) {
                if w.len == 0 {
                    continue;
                }
                // 初值窗求值失败:按本条**已文档化**的「无初值 → 类型默认值」
                // 处理并留痕,而不是让整个 VM 停摆。
                // 理由:引擎此处写的是 `eval(初值窗)`,而初值窗缺省语义就是
                // 「没有初值」;求值失败(如未实现语义)时把整个进程打死,
                // 会让「跳过一条声明」升级成「画面永远出不来」。
                // 严格模式仍照旧报错 —— 默认跑批不受影响。
                match self.eval_window_condition(Some(w)) {
                    Ok(Some(v)) => init = v,
                    Ok(None) => {}
                    Err(e) => {
                        if self.strict {
                            self.set_error_state();
                            return Ok(Action::Suspend(VmSuspend::Error(e.to_string())));
                        }
                        let reason = format!("声明初值窗求值失败(非严格:取类型默认值): {e}");
                        self.push_event(VmEvent::Unsupported {
                            pc: self.pc,
                            command: other,
                            reason,
                        });
                    }
                }
                break;
            }
            // **形态互斥(2026-10-13 实机勘误)**:运行期 `INT`/`FLT`/`STR`
            // 声明的是**标量**(引擎:一份描述符),而加载期声明消费可能
            // 在同一个 id 上留下一份幻影数组 —— `load_auto` 会优先走数组
            // 分支,于是 `@[0x0be8]`(脚本刚声明为 INT=0)读出数组第 0 元素
            // (类型不符 → 默认值),后续 `% 10` 直接炸「非整数/混合语义」。
            // 实测:s60 g1092 `INT @[0x0be8]=0` / g1093 `INT @[0x0be9]=0`,
            // 而 VM 里 `@be8` 是 dims[1] 数组、`@be9` 是 Str 标量。
            // 声明窗口不是数组形态时,清掉同名数组,让标量成为唯一存储。
            if !array_form {
                self.store.clear_arrays_for_id(r.id);
            }
            self.store.set(&r, init);
            self.push_event(VmEvent::Declaration {
                pc: self.pc,
                command: other,
                windows: g.window_count,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // 其余声明类命令:引擎运行器用默认 stub(G_*/F_* 族)或
        // 已确证 no-op(S_* 0x52-0x54 → FUN_00423080),不执行;
        // 记录 Declaration 事件后继续(与引擎一致)。
        // 注:0x5c STR 处理器 0x451598 的 LAB_ 边界反编译形似
        // 「按名切脚本」,不可靠;实测引擎 pc 顺序推进,已按上
        // 声明族实现。0x65 VAR(0x453158)语义未定性(Unknown)。
        if kind.is_declaration() {
            self.push_event(VmEvent::Declaration {
                pc: self.pc,
                command: other,
                windows: g.window_count,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // FILEINFO(0x15,引擎 0043eb28):FOLDER(0)/FILE(1)/
        // CGFILE(2)/SOUNDFILE(3) = 路径源;EXIST(4)/SIZE(5)/
        // RELATION(6) = 查询项;LET(7) = 写回目标。
        // 本实现:EXIST+FILE+LET → 虚拟 FS 存在性
        // (`host::PacFileIndex`;实证 s207 es.R18Check:引擎查
        // `cg/thumb_cg/A_HAN_2002_a.png` 得 1)。无 probe → 不写
        // 回,事件记 err(诚实缺省)。其余查询项出现时再逆向。
        if kind == CmdKind::FileInfo {
            let mut path: Option<String> = None;
            let mut exist = false;
            let mut let_ref: Option<yuris_format::ystb::CommandSlot> = None;
            let mut evaluated: Vec<(u8, String)> = Vec::new();
            for w in windows {
                if w.len == 0 {
                    continue;
                }
                let slot = (w.tag & 0xff) as u8;
                match slot {
                    1 => {
                        if let Some(v) = self.eval_window_condition(Some(w))? {
                            path = Some(v.str_as_string());
                            evaluated.push((slot, value_summary(&v)));
                        }
                    }
                    4 => {
                        if let Some(v) = self.eval_window_condition(Some(w))? {
                            exist = matches!(&v, Value::Int(n) if *n != 0);
                            evaluated.push((slot, value_summary(&v)));
                        }
                    }
                    7 => let_ref = Some(w.clone()),
                    unknown => {
                        // FILEINFO 其它查询项(FOLDER/CGFILE/SOUNDFILE/SIZE/
                        // RELATION)未建模:值只记录,结果不写回。
                        self.note_unknown_slot(
                            g.command_type,
                            unknown,
                            "FILEINFO 查询项(只建模 EXIST+FILE+LET)",
                        );
                        if let Some(v) = self.eval_window_condition(Some(w))? {
                            evaluated.push((slot, value_summary(&v)));
                        }
                    }
                }
            }
            if exist {
                match (&let_ref, (&path, &self.file_probe)) {
                    (Some(w), (Some(p), Some(probe))) => {
                        let bytes = self
                            .ctx
                            .script
                            .window_bytes_pooled_copy(w)
                            .ok_or_else(|| Error::format("FILEINFO LET 引用窗越过池尾"))?;
                        let target = extract_var_target(&bytes)?;
                        let hit = probe.exists(p);
                        self.write_var_target(&target, Value::Int(hit as i64))?;
                        evaluated.push((
                            255u8,
                            format!("EXIST({p})={} → {}", hit as i64, target.summary()),
                        ));
                    }
                    (Some(_), (None, _)) => {
                        // EXIST 查询但缺 FILE 路径 = 调用侧脚本异常/我们漏读槽。
                        self.note_unknown_slot(other, 1, "FILEINFO EXIST 缺 FILE 路径窗");
                        evaluated.push((255u8, "err:EXIST 无 FILE 路径".into()));
                    }
                    (Some(_), (_, None)) => {
                        // 无 file_probe = 宿主没注入虚拟 FS:EXIST 无真值,
                        // 写回被跳过。这是**环境缺失**,不是脚本问题。
                        yuris_log::alert_once(
                            module_path!(),
                            "fileinfo-no-probe",
                            "FILEINFO EXIST 无法应答\n\
                             宿主未注入 file_probe(PacFileIndex)\n\
                             后果: LET 目标保持原值,存在性分支结果不可信\n\
                             处置: 启动链须 `vm.set_file_probe(index)`(CLI 已注入)"
                                .to_string(),
                        );
                        evaluated.push((255u8, "err:无 file_probe".into()));
                    }
                    // 无 LET 引用窗(纯查询不接收)或无 EXIST:引擎同为无副作用。
                    _ => {}
                }
            }
            self.push_event(VmEvent::Subsystem {
                pc: self.pc,
                command: other,
                evaluated,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // 后端子系统配置/查询族(P2,处理器反编译见
        // docs/reverse/decompiled/engine/0044*_CMD_*.c):
        // `FONTINFO` / `FILEINFO` / `FILEACT` / `WINDOWINFO` / `MOUSE` /
        // `FONT` / `SYSTEM` / `ERROR` / `FPS` / `INPUT` / `MATH` / `DIALOG` /
        // `WINDOW`(**按命令名;序号逐版本漂移,见 `commands.rs` 的 `CmdKind`**)。
        // 引擎侧为子系统状态操作(字体层表/文件/输入模式/数学函数
        // 派发),无后端 → 求值全部参数事件化记录,不建子系统状态。
        // DIALOG(引擎 0043c984):模态消息框;无 LET/LETSTR
        // 槽时对流程零影响 → 记录后继续(= 用户瞬时按默认键;
        // 交互阻塞语义归运行时层 P8/P9)。带 LET 的对话框出现时
        // 须先逆向返回值编码再实现(铁律:不猜)。
        // WINDOW(0x69):引擎处理器 = 共享 no-op stub(0045c4d4,
        // engine_trace_config 实测)→ 无求值、无副作用;引擎 trace
        // 侧 stub 命中被过滤为伪事件 → VM 同样**完全静默**
        // (发事件会破坏对拍域,成果 59d)。
        if kind == CmdKind::Window {
            // 引擎侧确认为共享 no-op stub(Confirmed),这里静默**是正确的**;
            // 但把「已知无操作」记入去重告警表会让「真缺失」更难辨认,
            // 故只落一条 debug 供深挖。
            if yuris_log::enabled(yuris_log::Level::Debug) {
                yuris_log::debug!(
                    "[vm] WINDOW(no-op stub,引擎同) s{} pc{}",
                    self.ctx.script_id,
                    self.pc
                );
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // TASK(引擎 0044fe40 反编译):命名任务创建/重配置。序号逐版本漂移,按名分派。
        // 槽 0=ID(名)、槽 1=E/槽 2=A/槽 3=Z(优先级)、槽 4-13=TINT、
        // 14-23=TFLT、24-33=TSTR、槽 34='#'(入口标签)、35/36=SCRIPTPOS/
        // TEXTPOS、37=RESTART。引擎:查/建任务对象 → 标签查表装载
        // 入口脚本 → 参数写入任务对象。VM:**注册名字**(TASKINFO
        // EXIST 的查询面)+ 求值记录;不 spawn —— 引擎 trace 实证
        // 任务入口经主线 GOSUB 标签链顺序调起(s24 g195 → s2 pc47
        // GOSUB → s266 → s9),boot 段无并行交错。
        if kind == CmdKind::Task {
            // TASK(引擎 004506f4):建任务对象 —— ID 取自槽 0,
            // 入口标签取自槽 34(`#`),`E`(槽 1)控制是否参与调度。
            // **勘误(成果 118)**:旧实现只注册名字、不调度,
            // 已证会阻断整条场景推进链(见 `tasks` 字段注释)。
            // 现在按「任务体 = 独立执行位」建立并交由 `step_tasks`
            // 并行步进。
            let mut evaluated: Vec<(u8, String)> = Vec::new();
            let mut name: Option<Vec<u8>> = None;
            let mut entry: Option<Vec<u8>> = None;
            let mut enabled = true;
            let cmd = g.command_type;
            for w in windows {
                if w.len == 0 {
                    continue;
                }
                let slot = (w.tag & 0xff) as u8;
                let named = |this: &Self, p: &str| this.slot_is(cmd, slot, p, 0xff);
                match self.eval_window_condition(Some(w)) {
                    Ok(Some(v)) => {
                        if self.slot_is(cmd, slot, "ID", 0) {
                            if let Value::Str(bytes) = &v {
                                name = Some(bytes.clone());
                            }
                        } else if self.slot_is(cmd, slot, "#", 34) {
                            if let Value::Str(bytes) = &v {
                                entry = Some(bytes.clone());
                            }
                        } else if self.slot_is(cmd, slot, "E", 1) {
                            if let Value::Int(n) = &v {
                                enabled = *n != 0;
                            }
                        } else {
                            let _ = named;
                        }
                        evaluated.push((slot, value_summary(&v)));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.note_swallowed(self.pc, cmd, "TASK 参数", &e);
                        evaluated.push((slot, format!("err:{e}")))
                    }
                }
            }
            if let Some(n) = name {
                // 入口标签 → (脚本, 组)。`#` 缺失(纯重配置调用):
                // 只改启用位,不重建执行位(官方 TASK 的重配置分支)。
                let target = entry.as_ref().and_then(|e| self.labels.get(e).copied());
                let existed = self.task_exists(&n);
                let mut note = if existed { "已存在" } else { "注册" };
                match target {
                    Some((pc, sid)) => match self.build_task_exec(sid, pc as usize) {
                        Ok(st) => {
                            if !existed && self.task_names.len() >= MAX_TASKS {
                                note = "拒建(任务表已达上限)";
                            } else {
                                if !existed {
                                    self.task_names.push(n.clone());
                                }
                                // 执行位**换入**表(upsert);调度节点由
                                // `Effect::SpawnTask` 走官方同拍通道。
                                self.task_slots.insert(n.clone(), st);
                                self.pending_effects.push(crate::task::Effect::SpawnTask(
                                    crate::task::TaskSeed {
                                        id: n.clone(),
                                        script_id: sid,
                                        pc: pc as usize,
                                        enabled,
                                        is_main: false,
                                    },
                                ));
                                // 官方 TASK 处理器 `0x450ae9`:置续帧标志。
                                self.pending_effects
                                    .push(crate::task::Effect::RequestExtraFrame);
                                note = if existed {
                                    "已存在(重配置为入口)"
                                } else {
                                    "建立并进入调度"
                                };
                            }
                        }
                        Err(e) => {
                            note = "入口装载失败";
                            evaluated.push((255u8, format!("err:{e}")));
                        }
                    },
                    None => {
                        // 无 `#`:官方语义 = 建对象/重配置属性。新名字只登记
                        // (等价「建了对象但没有入口脚本」),已存在则改启用位。
                        if !existed && self.task_names.len() < MAX_TASKS {
                            self.task_names.push(n.clone());
                        }
                        self.pending_effects
                            .push(crate::task::Effect::SetEnabled(n.clone(), enabled));
                    }
                }
                evaluated.push((
                    255u8,
                    format!("task {note} {}", String::from_utf8_lossy(&n)),
                ));
            }
            self.push_event(VmEvent::Subsystem {
                pc: self.pc,
                command: other,
                evaluated,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        if kind == CmdKind::TaskEnd {
            // TASKEND(v555 处理器 0x004521dc):按 ID 销毁任务。
            // 本内核:**停调度**(finished)并丢掉执行位,但**不从
            // 表里删** —— 表下标是 `step_tasks` 的轮换锚点,
            // 运行期增删会让在飞任务错位。`TASKINFO EXIST` 是否
            // 随之变 0 **未取证**(U19),保持"名字仍在"。
            let mut evaluated: Vec<(u8, String)> = Vec::new();
            let mut name: Option<Vec<u8>> = None;
            for w in windows {
                if w.len == 0 {
                    continue;
                }
                let slot = (w.tag & 0xff) as u8;
                if let Ok(Some(v)) = self.eval_window_condition(Some(w)) {
                    if self.slot_is(g.command_type, slot, "ID", 0) {
                        if let Value::Str(bytes) = &v {
                            name = Some(bytes.clone());
                        }
                    }
                    evaluated.push((slot, value_summary(&v)));
                }
            }
            if let Some(n) = name {
                // 软结束(finished)+ 释放执行位;名字保留(TASKINFO EXIST
                // 是否随之变 0 未取证 —— U19,保持既有口径)。
                let hit = self.task_slots.remove(&n).is_some();
                self.pending_effects
                    .push(crate::task::Effect::FinishTask(n.clone()));
                evaluated.push((
                    255u8,
                    format!(
                        "taskend {} {}",
                        String::from_utf8_lossy(&n),
                        if hit { "已停调度" } else { "未注册" }
                    ),
                ));
            }
            self.push_event(VmEvent::Subsystem {
                pc: self.pc,
                command: other,
                evaluated,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // `TASKINFO` 已抽成独立模块(`exec/taskinfo.rs`)并按引擎处理体
        // `0x452270` 逐槽取证;此处不再重复实现。
        if kind.enters_subsystem_block() {
            // 子系统查询输出写回(P5.2,引擎 FUN_0045baf8 族
            // 查询→LET 写回;反编译 00457d64/00441c08):
            // - WINDOWINFO(0x6b):SX(槽10)/SY(槽11)查询 → 窗口宽/高
            //   写入 LET(槽1)目标(s24 g133/134 → @1074/@1075);
            // - FONTINFO(0x1b):NUM(槽11,值=1)查询 → 注册字体数
            //   写入 LET(槽18)目标(s45 g26 → @2684,LOOP 计数)。
            // oracle 环境(引擎 trace 采集时刻)常量:全屏 1920×1080
            // (s41 g77 范围检查 ±32/±18 反推);713 字体(引擎
            // s45 循环圈数实测)。同 @114=32bpp 先例(成果 59c)。
            if matches!(kind, CmdKind::WindowInfo | CmdKind::FontInfo) {
                let let_slot = if kind == CmdKind::WindowInfo {
                    1u8
                } else {
                    18u8
                };
                let mut target: Option<VarTarget> = None;
                let mut query_a = false;
                let mut query_b = false;
                // FONTINFO 的**状态查询槽**(SX/SY/COLOR/…):语料实证
                // `s70 g291-299` 的「暂存字号 → 改字号 → 还原」惯用法 ——
                // `FONTINFO SX→@4319` / `FONTINFO SY→@4320` 取当前字号,
                // 调用 `FONT SX/SY` 改完后由调用方用这对值还原。
                // 旧实现只认 NUM/LANG 两槽,这两个查询**静默不写回** ⇒
                // 还原时把 SX/SY 写成 0 ⇒ 渲染侧 `text_px_or_default`
                // 把 0 当「未设置」回落 40px(实机「字号忽然变大」)。
                let mut font_state_query: Option<u8> = None;
                for w in windows {
                    if w.len == 0 {
                        continue;
                    }
                    let slot = (w.tag & 0xff) as u8;
                    if slot == let_slot {
                        if let Ok(bytes) = self
                            .ctx
                            .script
                            .window_bytes_pooled_copy(w)
                            .ok_or_else(|| Error::format("子系统 LET 引用窗越过池尾"))
                        {
                            match extract_var_target(&bytes) {
                                Ok(t) => target = Some(t),
                                Err(e) => {
                                    // 查询结果无处写回 ⇒ LET 目标保持原值。
                                    yuris_log::alert_once(
                                        module_path!(),
                                        format!(
                                            "subsystem-let-parse-fail:{}:{}",
                                            self.ctx.script_id, self.pc
                                        ),
                                        format!(
                                            "子系统 LET 引用窗无法解析,查询结果不写回
                                             现场: script {} pc {}
                                             错误: {e}
                                             处置: 调用方读到变量原值(分支可能走错)",
                                            self.ctx.script_id, self.pc
                                        ),
                                    );
                                }
                            }
                        }
                    } else if kind == CmdKind::WindowInfo && slot == 10 {
                        query_a = true;
                    } else if kind == CmdKind::WindowInfo && slot == 11 {
                        query_b = true;
                    } else if kind == CmdKind::FontInfo && slot == 11 {
                        // NUM 查询须值为 1(引擎 slot11==1 检查)
                        query_a =
                            matches!(self.eval_window_condition(Some(w)), Ok(Some(Value::Int(1))));
                    } else if kind == CmdKind::FontInfo && slot == 13 {
                        // LANG 查询(须值 1;s45 g28 FONTINFO(NO,LANG→LET)):
                        // 引擎 713 字体全部非零(pc29 IF 真 → LOOPCONT×713
                        // 实测)→ oracle 常量 1。
                        query_b =
                            matches!(self.eval_window_condition(Some(w)), Ok(Some(Value::Int(1))));
                    } else if kind == CmdKind::FontInfo && matches!(slot, 2..=10) {
                        // ANTIALIASING(2)/COLOR(3)/COLOR2(4)/GRADATION(5)/
                        // ROTATION(6)/DRAWMODE(7)/SX(8)/SY(9)/BOLD(10):
                        // 槽值非零 = 发起查询(语料一律 `push 1`)。
                        if matches!(
                            self.eval_window_condition(Some(w)),
                            Ok(Some(Value::Int(n))) if n != 0
                        ) {
                            font_state_query = Some(slot);
                        }
                    }
                }
                if let Some(t) = &target {
                    if kind == CmdKind::WindowInfo {
                        if query_a {
                            self.write_var_target(t, Value::Int(self.screen_w))?;
                        }
                        if query_b {
                            self.write_var_target(t, Value::Int(self.screen_h))?;
                        }
                    } else if query_a {
                        // 注册字体数 = oracle 常量(环境依赖:
                        // 2026-09-04 采集 = 713,2026-09-05 采集 = 783
                        // —— 字体注册数随运行环境变化,对拍基线
                        // 须与引擎 trace 同批采集;当前以 400k trace
                        // 为准 = 783)
                        self.write_var_target(t, Value::Int(783))?;
                    } else if query_b {
                        self.write_var_target(t, Value::Int(1))?;
                    } else if let Some(q) = font_state_query {
                        // 查询返回**当前生效值**。等级:Likely —— 依据 =
                        // 脚本自带的保存/还原惯用法(`s70 g290-299`)只有
                        // 在「查询值 = 当前 FONT 状态」时才是恒等还原。
                        // 未设置过时返回内核缺省字号(与渲染侧同一常量),
                        // 保证 `FONTINFO SX → FONT SX=` 往返自洽。
                        let v: i64 = match q {
                            2 => self.font_aa.unwrap_or(0),
                            3 => self.font_color.map(|c| c.0).unwrap_or(0),
                            4 => self.font_color.map(|c| c.1).unwrap_or(0),
                            5 => self.font_gradation.unwrap_or(0),
                            6 => self.font_rotation.unwrap_or(0),
                            7 => self.font_drawmode.unwrap_or(0),
                            8 => self.font_px.map(|p| p.0).unwrap_or(DEFAULT_FONT_PX),
                            9 => self.font_px.map(|p| p.1).unwrap_or(DEFAULT_FONT_PX),
                            10 => self.font_bold.unwrap_or(0),
                            _ => 0,
                        };
                        self.write_var_target(t, Value::Int(v))?;
                    }
                }
            }
            // ---- MATH 运算 + LET 写回(T22,引擎处理体 0x4482c4)----
            //
            // 修前缺陷:`MATH` 归 `Subsystem` 只做参数求值 + 事件记录,
            // **LET 写回不发生** —— 凡 `LET=@x` 的用法,脚本读到的都是变量
            // 原值(<game-b> 语料实测 21 组)。现按两条独立线取证实装
            // (HASH = 标准 CRC-32 / RND = MT19937),代价与语义见 `cmd_math`。
            let is_math = self
                .command_dispatch()
                .name_of(other as u16)
                .map(|n| n.eq_ignore_ascii_case("MATH"))
                .unwrap_or(false);
            if is_math {
                // **2026-10-17 政策变更**:旧实现把 MATH 写回失败吞成一条事件
                // 继续跑 —— 那会让「运算没执行」变成静默。现在统一走
                // `halt_or_record`:strict(运行时默认)挂起 + 致命错误窗。
                if let Err(e) = self.cmd_math(windows) {
                    if let Some(reason) = self.halt_or_record(
                        self.pc,
                        other,
                        format!("MATH 执行失败: {e}"),
                    ) {
                        return Ok(Action::Suspend(VmSuspend::Error(reason)));
                    }
                }
            }
            // ---- FPS 帧率设定(**手册 `fps/fps.html`;成果 162**)----
            //
            // `FPS[SET=n SKIP=1]`(本作 s0 g318 / s142 等,值恒 60)。
            // 修前缺陷:FPS 归 Subsystem = **只记录不消费** ⇒ 内核的帧间隔
            // 恒为渲染器的 present 节拍,脚本无法规定推进速率;而官方引擎
            // 按此值限帧(`FUN_0041c2f0` 族)。现把 SET 写进 `GroupVm::fps`,
            // 由 `Player::tick` 决定帧间隔(缺省 60)。
            //
            // 语义取证(手册):SET = 1 秒内的帧数;SKIP = 跳帧时的推进补偿
            // (本作恒 1 = 不补偿)。未给 SET 时保持原值。
            let is_fps = self
                .command_dispatch()
                .name_of(other as u16)
                .map(|n| n.eq_ignore_ascii_case("FPS"))
                .unwrap_or(false);
            if is_fps {
                for w in windows {
                    if w.len == 0 {
                        continue;
                    }
                    let slot = (w.tag & 0xff) as u8;
                    if self.slot_is(other, slot, "SET", 0) {
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            if n > 0 {
                                self.fps = Some(n as u32);
                                self.push_event(VmEvent::Subsystem {
                                    pc: self.pc,
                                    command: other,
                                    evaluated: vec![(0u8, format!("FPS SET={n}"))],
                                });
                            }
                        }
                    }
                }
            }
            // ---- FONT 字体属性(**成果 175;U27 解除**)----
            //
            // 词表 = 游戏自身 YSCM:`BNO NO NAME ANTIALIASING COLOR COLOR2
            // GRADATION ROTATION DRAWMODE SX SY BOLD`。
            //
            // **修前缺陷**:`FONT` 落在 `enters_subsystem_block()` 的通用路径 =
            // 只发事件、不建状态 ⇒ 文本渲染只能吃内核缺省字号(40px),
            // 实机「字太大 / 超出消息框」。语料实证 SX/SY 即字号
            // (12/12、14/15;也有走变量 `@4321/@4322` 的)。
            //
            // 本内核只有一条字体链(系统 CJK 字体):保留**最近一次**的字号/
            // 颜色/加粗/描画模式;`BNO`/`NO` 仅记录(多字体表未实装)。
            let is_font = self
                .command_dispatch()
                .name_of(other as u16)
                .map(|n| n.eq_ignore_ascii_case("FONT"))
                .unwrap_or(false);
            if is_font {
                let mut px: Option<(i64, i64)> = None;
                let mut color: Option<(i64, i64)> = None;
                let mut bold: Option<i64> = None;
                let mut drawmode: Option<i64> = None;
                let _sel: Option<(i64, i64)> = None;
                let mut bno: Option<i64> = None;
                let mut no: Option<i64> = None;
                let mut aa: Option<i64> = None;
                let mut gradation: Option<i64> = None;
                let mut rotation: Option<i64> = None;
                let mut evaled: Vec<(u8, String)> = Vec::new();
                for w in windows {
                    if w.len == 0 {
                        continue;
                    }
                    let slot = (w.tag & 0xff) as u8;
                    let mut recognized = false;
                    if self.slot_is(other, slot, "BNO", 0) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            bno = Some(n);
                        }
                    } else if self.slot_is(other, slot, "NO", 1) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            no = Some(n);
                        }
                    } else if self.slot_is(other, slot, "SX", 9) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            px = Some((n, px.map(|p| p.1).unwrap_or(n)));
                        }
                    } else if self.slot_is(other, slot, "SY", 10) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            px = Some((px.map(|p| p.0).unwrap_or(n), n));
                        }
                    } else if self.slot_is(other, slot, "COLOR", 4) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            color = Some((n, color.map(|c| c.1).unwrap_or(n)));
                        }
                    } else if self.slot_is(other, slot, "COLOR2", 5) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            color = Some((color.map(|c| c.0).unwrap_or(n), n));
                        }
                    } else if self.slot_is(other, slot, "BOLD", 11) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            bold = Some(n);
                        }
                    } else if self.slot_is(other, slot, "DRAWMODE", 8) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            drawmode = Some(n);
                        }
                    } else if self.slot_is(other, slot, "ANTIALIASING", 3) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            aa = Some(n);
                        }
                    } else if self.slot_is(other, slot, "GRADATION", 6) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            gradation = Some(n);
                        }
                    } else if self.slot_is(other, slot, "ROTATION", 7) {
                        recognized = true;
                        if let Ok(Some(Value::Int(n))) = self.eval_window_condition(Some(w)) {
                            rotation = Some(n);
                        }
                    }
                    // 未识别的 FONT 槽:**不再静默丢弃** —— 大告警一次,
                    // 值仍只进事件(语义 Unknown,不猜)。
                    if !recognized {
                        self.note_unknown_slot(other, slot, "FONT 属性槽");
                    }
                    if let Ok(Some(v)) = self.eval_window_condition(Some(w)) {
                        evaled.push((slot, value_summary(&v)));
                    }
                }
                // 引擎语义:未给的槽**保持原值**(与 CG 逐槽 if 写同族)。
                if let Some(p) = px {
                    self.font_px = Some(p);
                }
                if let Some(c) = color {
                    self.font_color = Some(c);
                }
                if let Some(b) = bold {
                    self.font_bold = Some(b);
                }
                if let Some(d) = drawmode {
                    self.font_drawmode = Some(d);
                }
                if let Some(v) = aa {
                    self.font_aa = Some(v);
                }
                if let Some(v) = gradation {
                    self.font_gradation = Some(v);
                }
                if let Some(v) = rotation {
                    self.font_rotation = Some(v);
                }
                if bno.is_some() || no.is_some() {
                    self.font_sel = Some((
                        bno.unwrap_or(self.font_sel.map(|s| s.0).unwrap_or(0)),
                        no.unwrap_or(self.font_sel.map(|s| s.1).unwrap_or(0)),
                    ));
                }
                if std::env::var_os("YURIS_TRACE_FONT").is_some() {
                    yuris_log::trace_forced!(
                        "[font] s{} pc{} px={:?} color={:?} bold={:?} mode={:?} sel={:?}",
                        self.ctx.script_id,
                        self.pc,
                        self.font_px,
                        self.font_color,
                        self.font_bold,
                        self.font_drawmode,
                        self.font_sel
                    );
                }
                self.push_event(VmEvent::Subsystem {
                    pc: self.pc,
                    command: other,
                    evaluated: evaled,
                });
            }
            // ---- MATH3D 3D→2D 投影(**U36 主修复**;引擎处理体 `0x44874c`)----
            //
            // 修前缺陷:未实现 ⇒ es.SP.XYZCALC 的输出数组 `[31..34]` 恒 0 ⇒
            // 每帧 es.CGACT_SIZE 判「描画範囲外」(LEFT=999999999)⇒
            // `CGACT[SIZE=1]` 从不执行 ⇒ 显示层永远没有内容
            // —— 片头 LOGO 黑屏的最终根因(成果 152 §四;本条注释即勘误归档)。
            let is_math3d = self
                .command_dispatch()
                .name_of(other as u16)
                .map(|n| n.eq_ignore_ascii_case("MATH3D"))
                .unwrap_or(false);
            if is_math3d {
                if let Err(e) = self.cmd_math3d(windows) {
                    self.push_event(VmEvent::Subsystem {
                        pc: self.pc,
                        command: other,
                        evaluated: vec![(254u8, format!("MATH3D 写回失败: {e}"))],
                    });
                }
            }
            // ---- SOUNDINFO 查询写回(成果 133)----
            //
            // 官方手册 `sound/soundinfo.html`:`SOUNDINFO[ID LET=@v <查询字段>=1]`
            // —— **查询型**,把结果写进 LET 指定的变量。
            //
            // **2026-10-18 重构**:旧实现把「没有音源对象」写成 LET=0 ——
            // 那是编造值。音频层(`yuris-audio`)其实早已实现完整的状态查询
            // (`AudioSystem::sound_info`),缺的只是 VM→音频层的**查询通道**。
            // 现在走 [`crate::audio_probe::SoundStateProbe`] 依赖注入:
            // 宿主(播放器)注入后,VM 逐字段问真值;未注入/无真值时**显式挂起**
            // (strict),不再写假 0。
            //
            // **按命令名精确匹配**(不能只判 `CmdKind::Subsystem`)——
            // 该种类还含 EMOTE/EMOTEINFO/SYSTEMINFO/MATH/FONT…(见
            // `commands.rs` 的 `from_name`),只判种类会让 MATH 之类的命令
            // 也走这条写回路径、把 0 写进它们的 LET 目标(实测:
            // `subsystem_math_real_sample` 因此回归)。
            // **2026-10-17 政策变更(用户要求)**:旧实现把「没有音源对象」
            // 写成 LET=0 并继续 —— 这是**编造值**,而且正是「本会中断却被
            // 特判续命」的典型。现在 strict(运行时默认)一律挂起 + 致命
            // 错误窗;lenient 才保留写 0 的取证口径。
            let is_soundinfo = self
                .command_dispatch()
                .name_of(other as u16)
                .map(|n| n.eq_ignore_ascii_case("SOUNDINFO"))
                .unwrap_or(false);
            let mut soundinfo_missing: Option<String> = None;
            if is_soundinfo {
                let mut target: Option<VarTarget> = None;
                let mut layer: Option<String> = None;
                // 被指定的查询字段（引擎按「槽存在 + 值非 0」判为发起查询）。
                let mut fields: Vec<crate::audio_probe::SoundField> = Vec::new();
                for w in windows {
                    if w.len == 0 {
                        continue;
                    }
                    let slot = (w.tag & 0xff) as u8;
                    if self.slot_is(other, slot, "LET", 1) {
                        if let Ok(bytes) = self
                            .ctx
                            .script
                            .window_bytes_pooled_copy(w)
                            .ok_or_else(|| Error::format("SOUNDINFO LET 引用窗越过池尾"))
                        {
                            match extract_var_target(&bytes) {
                                Ok(t) => target = Some(t),
                                Err(e) => yuris_log::alert_once(
                                    module_path!(),
                                    format!(
                                        "soundinfo-let-parse-fail:{}:{}",
                                        self.ctx.script_id, self.pc
                                    ),
                                    format!(
                                        "SOUNDINFO 的 LET 引用窗无法解析,查询结果不写回
                                         现场: script {} pc {}
                                         错误: {e}",
                                        self.ctx.script_id, self.pc
                                    ),
                                ),
                            }
                        }
                        continue;
                    }
                    if slot == 0 {
                        // `ID`：声层键（脚本给的字符串，原样使用）。
                        if let Ok(Some(Value::Str(b))) = self.eval_window_condition(Some(w)) {
                            layer = Some(String::from_utf8_lossy(&b).to_string());
                        }
                        continue;
                    }
                    let truthy = matches!(
                        self.eval_window_condition(Some(w)),
                        Ok(Some(v)) if v.as_int_opt().map(|n| n != 0).unwrap_or(false)
                    );
                    if !truthy {
                        continue;
                    }
                    let Some(pname) = self.command_dispatch().param_name_of(other, slot) else {
                        self.note_unknown_slot(other, slot, "SOUNDINFO 查询字段");
                        continue;
                    };
                    match crate::audio_probe::SoundField::from_param_name(&pname) {
                        Some(f) => fields.push(f),
                        None => self.note_unknown_slot(other, slot, "SOUNDINFO 查询字段"),
                    }
                }
                if let Some(t) = &target {
                    let id = layer.clone().unwrap_or_default();
                    for field in &fields {
                        match self.audio_query(&id, *field) {
                            Some(crate::audio_probe::SoundInfoValue::Int(n)) => {
                                self.write_var_target(t, Value::Int(n))?;
                            }
                            Some(crate::audio_probe::SoundInfoValue::Str(b)) => {
                                self.write_var_target(t, Value::Str(b))?;
                            }
                            Some(crate::audio_probe::SoundInfoValue::Unsupported) => {
                                soundinfo_missing = Some(format!(
                                    "SOUNDINFO {field:?} 查询无真值(编码未取证),拒绝写假 0;LET={}",
                                    t.summary()
                                ));
                            }
                            None => {
                                // 无查询通道(宿主未注入)或声层状态未知 —— 显式挂起。
                                soundinfo_missing = Some(format!(
                                    "SOUNDINFO {field:?} 查询:无音频状态通道或声层 {id:?} 无状态可答\n\
                                     处置: 拒绝写假值;若这是启动链必经点,宿主须注入 SoundStateProbe"
                                ));
                            }
                        }
                    }
                }
            }
            // `INPUT` / `MOUSE`（手册 `system/input.html`、`system/mouse.html`）：
            // 引擎侧是**输入子系统状态**，不是无副作用记录。参数按命令自身
            // 的 YSCM 参数名解析（`apply_input_command` 只读窗口不改状态流）。
            let input_cmd = self
                .command_dispatch()
                .name_of(other as u16)
                .map(|n| n.eq_ignore_ascii_case("INPUT") || n.eq_ignore_ascii_case("MOUSE"))
                .unwrap_or(false);
            let mut input_err: Option<String> = None;
            if input_cmd {
                let cname = self
                    .command_dispatch()
                    .name_of(other as u16)
                    .unwrap_or("")
                    .to_string();
                if let Err(e) = self.apply_input_command(&cname, other, windows) {
                    input_err = Some(e.to_string());
                }
            }
            let mut evaluated: Vec<(u8, String)> = Vec::new();
            if let Some(e) = input_err {
                evaluated.push((254u8, format!("err:{e}")));
            }
            for w in windows {
                if w.len == 0 {
                    continue;
                }
                let slot = (w.tag & 0xff) as u8;
                // 求值失败不传播:引擎对未声明/未初始化变量读取
                // 容忍(实测 desc 全零仍继续),事件如实记录。
                // **吞错留痕(2026-10-17)**:eval 失败不再毒化状态后,这里
                // 的 Err 若仍无声,排障只剩泛文挡回 —— note_swallowed
                // 每现场一次性落 warn。
                // 裸左值引用窗(`MATH3D` 的 INT/FLT、`VARINFO` 的
                // SET/LET、SOUNDINFO 的 LET)**不是求值失败** —— 它们按定义
                // 就是引用,由处理器本体用 `extract_var_target` 读取。
                // 诊断循环这里跳过,避免把正常用法记成假告警
                // (成果 173:s66 pc5355 的 MATH3D 曾报「以左值引用结尾」)。
                let is_lvalue = self.window_is_lvalue(w);
                if is_lvalue {
                    evaluated.push((slot, "<ref>".to_string()));
                    continue;
                }
                match self.eval_window_condition(Some(w)) {
                    Ok(Some(v)) => evaluated.push((slot, value_summary(&v))),
                    Ok(None) => {}
                    Err(e) => {
                        self.note_swallowed(self.pc, other, "子系统参数", &e);
                        evaluated.push((slot, format!("err:{e}")))
                    }
                }
            }
            self.push_event(VmEvent::Subsystem {
                pc: self.pc,
                command: other,
                evaluated,
            });
            // ---- **「分派到了但没实装」的显式中断(2026-10-17,用户要求)----
            //
            // 这一族的共同点:命令被 `CmdKind::from_name` 归到 `Subsystem`,
            // 于是**绕过**了 `cmd_fallback` 的「未实现 ⇒ strict 挂起」路径,
            // 只做参数求值 + 事件记录就继续 —— 即「本会中断却被特判续命」。
            //
            // 已实装(或部分实装且有真值可答)的白名单:
            //   WINDOWINFO / FONTINFO / MATH / FONT / FPS / MATH3D /
            //   INPUT / MOUSE(状态机) / SOUNDINFO(lenient 下仍写 0)。
            // 其余一律视为**子系统缺失**:EMOTE / EMOTEINFO / SYSTEMINFO /
            // DIALOG / ERROR / FILEACT / SYSTEM,以及未来新增但未列入的名字。
            let cname2 = self
                .command_dispatch()
                .name_of(other as u16)
                .unwrap_or("<未命名>")
                .to_string();
            let implemented = matches!(
                cname2.as_str(),
                "WINDOWINFO"
                    | "FONTINFO"
                    | "MATH"
                    | "FONT"
                    | "FPS"
                    | "MATH3D"
                    | "INPUT"
                    | "MOUSE"
                    | "SOUNDINFO"
            );
            let detail = if let Some(m) = soundinfo_missing {
                Some(m)
            } else if implemented {
                None
            } else {
                Some(format!("{cname2}:子系统未实装,参数仅记录不执行"))
            };
            if let Some(detail) = detail {
                if let Some(reason) = self.missing_feature_halt(other, &detail) {
                    return Ok(Action::Suspend(VmSuspend::Error(reason)));
                }
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // ---- **未实现命令的显式诊断(2026-10-15,用户要求)----
        //
        // 排障时最耗时的一步是「画面不动了,但不知道卡在哪条命令」。这里把
        // 命令**名**、YSCM 下标、槽位与已求值参数一并打进 stderr,
        // 让任何一次运行都能直接看出缺什么:
        //   [vm] 未实现命令 SAVEEND(0x55 idx85) s12 pc663 槽[] 参数[]
        // 除名外还带:
        //   - `idx` = YSCM 命令下标(跨版本核对用);
        //   - `槽[..]` = 本次实际出现的参数槽号(按 YSCM 参数表可查名);
        //   - `可选` = 该命令是否在 `CmdKind` 里被显式分类过(未分类 =
        //     连种类都没有,优先级更高)。
        let cname = self
            .command_dispatch()
            .name_of(other as u16)
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("<未命名 cmd{other}>"));
        let slots: Vec<String> = windows
            .iter()
            .filter(|w| w.len != 0)
            .map(|w| format!("{}", w.tag & 0xff))
            .collect();
        let mut args_dbg: Vec<String> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            match self.eval_window_condition(Some(w)) {
                Ok(Some(v)) => args_dbg.push(format!("{slot}={}", value_summary(&v))),
                Ok(None) => args_dbg.push(format!("{slot}=<空>")),
                Err(e) => args_dbg.push(format!("{slot}=<err:{e}>")),
            }
        }
        let reason = format!(
            "命令 0x{other:02x}({cname}) 的运行语义未逆向(处理器未实现;不猜)\n\
             槽[{}] 参数[{}]",
            slots.join(","),
            args_dbg.join(" ")
        );
        // 统一走 `halt_or_record`:大告警横幅 + 环形缓存 + strict 下登记致命
        // 错误窗(此前这里自建 warn+push_event,绕过了汇聚点的致命升级)。
        if let Some(reason) = self.halt_or_record(self.pc, other, reason) {
            return Ok(Action::Suspend(VmSuspend::Error(reason)));
        }
        self.pc += 1;
        return Ok(Action::Keep);
    }
}
