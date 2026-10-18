//! 媒体族(CG/文本/声音)(从 `run_main` 巨型 match 拆出;语义逐行照搬,未改行为)。
//!
//! 拆分背景见 [`crate::exec`] 模块文档。**控制流约定**:方法体里不再出现
//! 「目标为外层 while」的 `continue` / `return Ok(VmSuspend…)` —— 分别替换为
//! `return Ok(Action::Keep)` / `return Ok(Action::Suspend(…))`,由 `run_main`
//! 的循环统一执行(语义 1:1)。

use yuris_core::{Error, Result};
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_value::Value;

use crate::event::{EffectValue, VmEvent, VmSuspend};
use crate::exec::Action;
use crate::util::{
    extract_var_target, value_summary, VarTarget,
};
use crate::GroupVm;

/// CG 注册表条目的命中尺寸（逻辑像素）。
///
/// 手册：`SX/SY` 是**レイヤセットの幅/高さ**（只对组有效）。因此：
/// 1. 脚本给了 `SX/SY` → 用它（组尺寸）；
/// 2. 否则用装载图像原始尺寸（`img_w/img_h`）；
/// 3. 都没有 → 0（不可命中）。
fn cg_extent(st: &crate::CgState) -> (i64, i64) {
    if st.size_explicit {
        (st.sx, st.sy)
    } else if st.img_w > 0 || st.img_h > 0 {
        (st.img_w, st.img_h)
    } else {
        (0, 0)
    }
}

impl GroupVm {


    /// 处理 `Text`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_text(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_00452888:FILE 参数为字符串(UTF-8/SJIS)→ 显示层;
        // LET/CLEAR 为布尔。事件化;不阻塞(点击等待在文本层,非本 VM)。
        let mut file = None;
        let mut let_flag = false;
        let mut clear_flag = false;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            if let Some(v) = self.eval_window_condition(Some(w))? {
                match v {
                    Value::Str(bytes) => file = Some(bytes),
                    Value::Int(n) => {
                        let slot = (w.tag & 0xff) as u8;
                        // 参数槽:YSCM TEXT 序 = FILE(0) LET(1) CLEAR(2)
                        match slot {
                            1 => let_flag = n != 0,
                            2 => clear_flag = n != 0,
                            unknown => {
                                self.note_unknown_slot(
                                    g.command_type,
                                    unknown,
                                    "TEXT 参数槽(只建模 LET=1 / CLEAR=2)",
                                );
                            }
                        }
                    }
                    other => {
                        // 非字符串/整数参数:记事件,不猜 —— 但也必须可见。
                        self.note_unknown_slot(
                            g.command_type,
                            (w.tag & 0xff) as u8,
                            "TEXT 参数类型异常(非 INT/STR)",
                        );
                        let _ = other;
                    }
                }
            }
        }
        if let Some(bytes) = &file {
            // 统一文本通道:播放器只消费 `TextLine`,不区分 v555 明文剧本 /
            // v49x 编译内联(见 `scenario::TextChannel`)。
            self.text_channel.push_inline(self.ctx.script_id, self.pc, bytes);
        }
        self.push_event(VmEvent::Text {
            pc: self.pc,
            file,
            let_flag,
            clear_flag,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `Word`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_word(&mut self, _g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // WORD(v494 索引 106;SDK 命令字典参数数 = 1,无名)。
        //
        // **窗口形态 = 裸 SJIS 字面量,不是字节码**(Confirmed):
        // `offset/len` 直接框住一串 Shift-JIS 字节,`CG` 那套
        // 指令解码器对这些字节一律失败。全语料 28992 组实测,
        // 载荷就是本作的台词原文(SJIS)。例如
        // `92 ca 8f ed` = 「通常」。
        //
        // **职责判定 = 它才是本作的文本显示命令**(Likely,有对照):
        // 全语料 `WORD` 28992 组 / `TEXT` 仅 2 组 —— 台词不可能
        // 走 `TEXT`;`TEXT` 的 FILE 参数指向文本文件,而本发行版
        // 根本没有明文 txt(纯 YSTB),二者互补。
        //
        // 运行期语义:发 `Text` 事件交渲染/播放层,**不阻塞**
        // (与 `TEXT` 处理器同一处理方式)。UNVERIFIED:引擎是否
        // 另有排版/换行副作用未取证 —— 本内核不做任何排版推断。
        let mut text: Option<Vec<u8>> = None;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            // 属性窗(tag 低位非 0)= 参数槽,本命令无参数名可依;
            // 只取 B0==0 的那个内容窗。
            if (w.tag & 0xff) != 0 {
                continue;
            }
            if text.is_none() {
                text = self.ctx.script.window_bytes_pooled_copy(w);
            }
        }
        if let Some(bytes) = &text {
            // v494 内联台词(裸 SJIS):**追加**进当前行缓冲,不直接成行。
            //
            // **勘误(2026-10-17,成果 170)**:旧实现把每个 `WORD` 当一整行
            // 发出去 —— 而引擎 `LAB_00458ae4` 是「Append 进任务文本缓冲
            // (`ctx+0x38`)」,行由 `RETURNCODE` 提交。语料实证(s149
            // g723-726:`WORD`+`WORD`+`WORD`+`RETURNCODE` = 一句话),
            // 全语料 28992 WORD / 28299 RETURNCODE ≈ 1.02 片段/行。
            // 旧行为导致一句话被拆成多行、按钮标签拼不起来。
            self.text_channel
                .push_word(self.ctx.script_id, self.pc, bytes);
        }
        self.push_event(VmEvent::Text {
            pc: self.pc,
            file: text,
            let_flag: false,
            clear_flag: false,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `Cg`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_cg(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎处理器 = 0x423864(表初始化 DAT_0078b024 实锤;
        // 勘误:0x43c984 是 cmd 0x0a DIALOG,此前误标为 CG)。
        // 求值全部参数,按 B0 槽填入;
        // 事件化记录 ID(槽 0)/X,Y,Z(槽 4/5/6),其余计数。
        // 槽位映射草案见 bridge 模块文档(成果 48)。
        // P7.1(成果 62):按名 upsert CG 状态注册表(引擎仅写
        // 已指定槽,未指定字段保持原值/新建默认 —— 反编译
        // 0x423864 逐槽 if(DAT_006624xx) 写,实证)。
        let mut id = None;
        let mut id_bytes: Option<Vec<u8>> = None;
        let (mut cx, mut cy, mut cz) = (None, None, None);
        let (mut csx, mut csy) = (None, None);
        // **E 槽(显隐,本作 ysc 槽 2)**：官方手册语义 = 该 CG 是否显示。
        // **勘误(2026-10-13,成果 121)**:旧实现**完全没读 E** ——
        // 后果是游戏用 E=0 关掉的层在播放器侧仍被建/仍可见:
        // 实测标题菜单的 `BT.NAES.*`(常态)/`BT.OFFES.*`(关)/
        // `BT.ONES.*`(开)三套状态图**同时上屏**(用户报告「CG 混乱」)。
        let mut ce: Option<i64> = None;
        // A(不透明度 0〜256)/ MODE(描画模式 0〜18):手册关键字,
        // 逐槽独立带出给渲染侧(成果 122)。
        let mut ca: Option<i64> = None;
        let mut ca_op: Option<u8> = None;
        let mut cmode: Option<i64> = None;
        // 过渡三件套 `T`/`TID`/`TA` + 遮罩 `MID`(手册 `cg/cg.html`;
        // 本作槽 37/38/40/35 —— 从 YSCM 参数表按名取,不写死)。
        //
        // **为什么必须有(成果 125,实机「动画没有」)**:过渡是
        // YU-RIS 唯一的**逐像素显现**机制,本作标题按钮的显现就是
        // `CG[ID="BT.OFF"+$v TID="BTNTR" TA=16 T=<变量>]`(语料实证
        // s0 g3019):变量在 LOOP 里递增 ⇒ 按钮按图案逐帧「擦」出来。
        // 旧实现把这四个槽**整个丢掉**,所以按钮/立绘的进场动画
        // 一律没有,只有最终的静态帧。
        let mut ct: Option<i64> = None;
        let mut ctid: Option<Vec<u8>> = None;
        let mut cta: Option<i64> = None;
        let mut cmid: Option<Vec<u8>> = None;
        let mut file: Option<Vec<u8>> = None;
        let mut count = 0usize;
        // **TEX / DXBUF / DXDRAW**(官方 es_Sprite.yst 高频):
        // `CG[ID=… TEX=@_DXMODE]` = 「テクスチャ化」(把该层转成可被合成
        // 引用的纹理);`DXBUF` = 离屏缓冲标志;`DXDRAW` = 直接绘制。
        // 三者此前**未带出** ⇒ 下游无从得知某层是缓冲还是显示层。
        let mut ctex: Option<i64> = None;
        let mut cdxbuf: Option<i64> = None;
        let mut cdxdraw: Option<i64> = None;
        // **LINT/LINT2/REMALLOC**(v494 `CG` 处理体实证,见 `CgState` 文档):
        // 三者都是**图层对象持久字段**,不是一次性效果。旧实现落到
        // 「未建模参数槽」大告警里 —— 值丢弃 ⇒ `CGINFO[LINT=1 LET=@v]`
        // 恒读到 0。现按名解析并落库;渲染侧的插值/缓冲重分配仍未实现,
        // 但查询往返是真值。
        let mut clint: Option<i64> = None;
        let mut clint2: Option<i64> = None;
        let mut cremalloc: Option<i64> = None;
        let cmd = g.command_type;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if let Some(v) = self.eval_window_condition(Some(w))? {
                count += 1;
                // 槽位按**参数名**解析(YSCM 参数表;槽号逐版本漂移)。
                // 实测 CG.FILE:v555 槽 46 / v494 槽 33 / v479 槽 29 ——
                // 早期写死的 46 在 v494 上永远读不到 FILE,纹理不装载,
                // 画面全黑。
                if self.slot_is(cmd, slot, "ID", 0) {
                    if let Value::Str(bytes) = &v {
                        id = Some(v.str_as_string());
                        id_bytes = Some(bytes.clone());
                    }
                } else if self.slot_is(cmd, slot, "E", 2) {
                    // E(显隐;本作槽 2)—— 官方手册语义。
                    if let Value::Int(n) = &v {
                        ce = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "A", 3) {
                    // A = 不透明度 0〜256(手册;256 = 全不透明)。
                    // **运算码(2026-10-18,成果 196 追记二)**:窗 tag B3 =
                    // 槽级运算码(0=赋值 1=+= 2=-=,引擎 0x42bd50 RE)。
                    // 语料锚点:s20 配置页 `LOOP 32× CG[SYSBLACK A=8]`
                    // tag=0x02010003(B3=2)= 每帧 −8 渐入;旧实现丢 B3
                    // ⇒ 恒绝对赋值,黑幕停在 α=8,「盖住变浅」。
                    if let Value::Int(n) = &v {
                        ca = Some(*n);
                        ca_op = Some((w.tag >> 24) as u8);
                    }
                } else if self.slot_is(cmd, slot, "MODE", 41) {
                    // MODE = 描画模式 0〜18(手册)。
                    if let Value::Int(n) = &v {
                        cmode = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "T", 37) {
                    // T = トランジション進行度 0〜256(手册)。
                    if let Value::Int(n) = &v {
                        ct = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "TID", 38) {
                    // TID = トランジションレイヤＩＤ(手册)。
                    if let Value::Str(bytes) = &v {
                        ctid = Some(bytes.clone());
                    }
                } else if self.slot_is(cmd, slot, "TA", 40) {
                    // TA = トランジション精度 0〜256(手册)。
                    if let Value::Int(n) = &v {
                        cta = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "MID", 35) {
                    // MID = マスクとして適用するレイヤＩＤ(手册)。
                    if let Value::Str(bytes) = &v {
                        cmid = Some(bytes.clone());
                    }
                } else if self.slot_is(cmd, slot, "FILE", 46) {
                    if let Value::Str(bytes) = &v {
                        file = Some(bytes.clone());
                    }
                } else if self.slot_is(cmd, slot, "X", 4) {
                    if let Value::Int(n) = &v {
                        cx = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "Y", 5) {
                    if let Value::Int(n) = &v {
                        cy = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "Z", 6) {
                    if let Value::Int(n) = &v {
                        cz = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "TEX", 12) {
                    if let Value::Int(n) = &v {
                        ctex = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "DXBUF", 13) {
                    if let Value::Int(n) = &v {
                        cdxbuf = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "DXDRAW", 14) {
                    if let Value::Int(n) = &v {
                        cdxdraw = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "SX", 9) {
                    if let Value::Int(n) = &v {
                        csx = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "SY", 10) {
                    if let Value::Int(n) = &v {
                        csy = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "LINT", 43) {
                    if let Value::Int(n) = &v {
                        clint = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "LINT2", 44) {
                    if let Value::Int(n) = &v {
                        clint2 = Some(*n);
                    }
                } else if self.slot_is(cmd, slot, "REMALLOC", 32) {
                    if let Value::Int(n) = &v {
                        cremalloc = Some(*n);
                    }
                } else if self.command_dispatch().profile().has_params() {
                    // **未建模的 CG 参数槽**:以前整块静默丢弃。CG 有 ~55 个
                    // 效果/渐变槽(见 `bridge.rs` 的表),未建模 = 画面效果
                    // 不会发生 —— 必须让用户看见缺了什么。
                    self.note_unknown_slot(cmd, slot, "CG 参数槽(效果/渐变未建模)");
                }
            }
        }
        let position = match (cx, cy) {
            (Some(x), Some(y)) => Some((x, y, cz.unwrap_or(0))),
            _ => None,
        };
        // X/Y/Z 逐槽独立(成果 122):只给 Z 也必须带出去(手册示例 [1])。
        // 注册表维护(成果 62,引擎 0x423864 + watch oracle 实证):
        // FILE 槽非空 → 创建(注册;已存在则按已指定槽 patch);
        // 无 FILE / FILE="" → 静默不创建(处理器 return 0 路径),
        // 已注册者仅 patch —— 后续 CGINFO 查未注册名走「不存在」
        // 路径写 0(BT.OVER/ON/ONOV/MASK vs BT.OFF/NA 对拍实证)。
        // P1 收尾:FILE 非空时经 VFS 探针解析图像头,填
        // img_w/img_h(CGINFO 槽13/14 真值应答源)。
        //
        // **字节直通(2026-10-16,成果 143)**:FILE 槽是**字节串**,旧实现先
        // `String::from_utf8_lossy` 再传 `&str` —— 对 CP932 名
        // (`cg/ev/サークル`)会把非 ASCII 全毁成 U+FFFD,VFS 再正确也救不回来
        // ⇒ `img_w/img_h` 恒 0 ⇒ `CGINFO SX/SY` 应答 0 ⇒ 官方脚本的
        // `CG[ID=$LID+… SX=@sx SY=@sy]`(显示层 = 图像大小的图层组)
        // 退化成 1×1 ⇒ **片头 LOGO 画不出来**。与成果 139 同一类缺陷。
        let image_dims = match (&file, &self.file_probe) {
            (Some(f), Some(probe)) if !f.is_empty() => probe.image_dims_raw(f),
            _ => None,
        };
        if let Some(bytes) = &id_bytes {
            let create = matches!(&file, Some(f) if !f.is_empty());
            let exists = self.cg_registry.contains_key(bytes);
            if create || exists {
                let st = self.cg_registry.entry(bytes.clone()).or_default();
                if let Some(x) = cx {
                    st.x = x;
                }
                if let Some(y) = cy {
                    st.y = y;
                }
                if let Some(z) = cz {
                    st.z = z;
                    // 只有脚本真给过 Z 才允许作为「重建继承值」下发
                    // (见 `CgState::z_explicit`)。
                    st.z_explicit = true;
                }
                if let Some(sx) = csx {
                    st.sx = sx;
                    st.size_explicit = true;
                }
                if let Some(sy) = csy {
                    st.sy = sy;
                    st.size_explicit = true;
                }
                // E(显隐)= 引擎的「表示/非表示」状态位(手册)。
                if let Some(e) = ce {
                    st.e = e;
                }
                // A(不透明度 0〜256;引擎图层 +0x80)。CGINFO[A=1] 读回。
                if let Some(a) = ca {
                    st.a = a;
                }
                // 过渡/遮罩/描画模式(成果 125;手册关键字)。
                if let Some(t) = ct {
                    st.t = Some(t);
                }
                if let Some(v) = &ctid {
                    st.tid = Some(v.clone());
                }
                if let Some(ta) = cta {
                    st.ta = Some(ta);
                }
                if let Some(v) = &cmid {
                    st.mid = Some(v.clone());
                }
                if let Some(m) = cmode {
                    st.mode = Some(m);
                }
                // LINT/LINT2/REMALLOC/TEX:图层对象持久字段(见 `CgState`)。
                // 「未给槽保持原值」与引擎逐槽 if 写同族。
                if let Some(v) = clint {
                    st.lint = Some(v);
                }
                if let Some(v) = clint2 {
                    st.lint2 = Some(v);
                }
                if let Some(v) = cremalloc {
                    st.remalloc = Some(v);
                }
                // 引擎 `CG TEX` 只在值 == 1 时置位(0x42b011 dec/or/je 判定)。
                if ctex == Some(1) {
                    st.tex = true;
                }
                if let Some((w, h)) = image_dims {
                    st.img_w = w;
                    st.img_h = h;
                }
                if let Some(f) = &file {
                    if !f.is_empty() {
                        st.file = Some(f.clone());
                    }
                }
            }
        }
        self.push_event(VmEvent::Cg {
            pc: self.pc,
            script_id: self.ctx.script_id,
            id,
            position,
            param_count: count,
            file: file.clone(),
            // E(显隐):Some(0) = 关,Some(非零) = 开,None = 本次未指定
            // (引擎:未给槽保持原值)。播放器据此判显隐(成果 121)。
            e: ce,
            // X/Y/Z 逐槽独立(成果 122):只给 Z 不再丢失(手册示例 [1])。
            x: cx,
            y: cy,
            z: cz,
            // A(不透明度 0〜256)/ MODE(描画模式 0〜18)(手册)。
            a: ca,
            mode: cmode,
            // 纹理化/离屏缓冲/直接绘制(官方 es_Sprite.yst 三件套;
            // 成果 140 之前的版本**完全不带出**)。
            tex: ctex,
            dxbuf: cdxbuf,
            dxdraw: cdxdraw,
            // 过渡三件套 + 遮罩(手册 `cg/cg.html`;成果 125)。
            t: ct,
            tid: ctid.clone(),
            ta: cta,
            mid: cmid.clone(),
            // 图层组尺寸(手册:「レイヤセットに対してのみ有効」;成果 134)。
            sx: csx,
            sy: csy,
            a_op: ca_op.unwrap_or(0),
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `Sound`：音频声层操作（官方手册 `sound/sound.html`）。
    ///
    /// **成果 162**：此前只带出 `ID/FILE/PLAY`，其余 11 个参数在 VM 层被丢弃
    /// ⇒ 播放器无法实现淡变/循环/音量/声像/音高。现按**参数名**解析全部
    /// 14 个槽（槽号逐版本漂移，禁止写死），事件携带完整参数。
    pub(crate) fn cmd_sound(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        let cmd = g.command_type;
        // 参数名 → 槽（fallback = v555 表序，仅在无参数表时使用）。
        let slot = |me: &Self, name: &str, fallback: u8| -> Option<u8> {
            me.param_slot(cmd, name, fallback)
        };
        let s_id = slot(self, "ID", 0);
        let s_e = slot(self, "E", 1);
        let s_file = slot(self, "FILE", 2);
        let s_play = slot(self, "PLAY", 3);
        let s_pause = slot(self, "PAUSE", 4);
        let s_fade = slot(self, "FADE", 5);
        let s_start = slot(self, "START", 6);
        let s_restart = slot(self, "RESTART", 7);
        let s_end = slot(self, "END", 8);
        let s_loop = slot(self, "LOOP", 9);
        let s_volume = slot(self, "VOLUME", 10);
        let s_pan = slot(self, "PAN", 11);
        let s_pitch = slot(self, "PITCH", 12);
        let s_speed = slot(self, "SPEED", 13);

        let mut id = None;
        let mut file: Option<Vec<u8>> = None;
        let mut enabled = None;
        let mut play = None;
        let mut pause = None;
        let mut fade_ms = None;
        let mut start_ms = None;
        let mut restart_ms = None;
        let mut end_ms = None;
        let mut loop_count = None;
        let mut volume = None;
        let mut pan = None;
        let mut pitch = None;
        let mut speed = None;
        let mut count = 0usize;

        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot_no = (w.tag & 0xff) as u8;
            let Some(v) = self.eval_window_condition(Some(w))? else {
                continue;
            };
            count += 1;
            // 数值槽：INT 直取，FLT 截断（YSCM 声明为整数的参数不会给 FLT）。
            let as_int = |v: &Value| -> Option<i64> {
                match v {
                    Value::Int(n) => Some(*n),
                    Value::Float(f) => Some(*f as i64),
                    Value::Str(_) => None,
                }
            };
            if Some(slot_no) == s_id {
                if let Value::Str(_bytes) = &v {
                    id = Some(v.str_as_string());
                }
            } else if Some(slot_no) == s_file {
                if let Value::Str(bytes) = &v {
                    // **原始 CP932 字节**（与 `Cg.file` 同约定）。
                    file = Some(bytes.clone());
                }
            } else if Some(slot_no) == s_e {
                enabled = as_int(&v);
            } else if Some(slot_no) == s_play {
                play = as_int(&v);
            } else if Some(slot_no) == s_pause {
                pause = as_int(&v);
            } else if Some(slot_no) == s_fade {
                fade_ms = as_int(&v);
            } else if Some(slot_no) == s_start {
                start_ms = as_int(&v);
            } else if Some(slot_no) == s_restart {
                restart_ms = as_int(&v);
            } else if Some(slot_no) == s_end {
                end_ms = as_int(&v);
            } else if Some(slot_no) == s_loop {
                loop_count = as_int(&v);
            } else if Some(slot_no) == s_volume {
                volume = as_int(&v);
            } else if Some(slot_no) == s_pan {
                pan = as_int(&v);
            } else if Some(slot_no) == s_pitch {
                pitch = as_int(&v);
            } else if Some(slot_no) == s_speed {
                speed = as_int(&v);
            } else if self.command_dispatch().profile().has_params() {
                // 未建模的 SOUND 参数:以前静默丢弃。
                self.note_unknown_slot(cmd, slot_no, "SOUND 参数槽(未建模)");
            }
        }

        self.push_event(VmEvent::Sound {
            pc: self.pc,
            id,
            enabled,
            file,
            play,
            pause,
            fade_ms,
            start_ms,
            restart_ms,
            end_ms,
            loop_count,
            volume,
            pan,
            pitch,
            speed,
            param_count: count,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `SOUNDEND`(官方手册 `sound/soundend.html`):删除指定声音图层。
    ///
    /// **成果 133**:片头链 `es.SND("人生通行止め")`(s60)内部就调它 ——
    /// 未实装时 strict 模式挂起,导致整条片头演出(含厂商 LOGO)不执行。
    /// 语义 = 按 ID 停掉音源;内核把它事件化(`VmEvent::SoundEnd`),
    /// 播放器/音频后端据此停音。参数槽序取自游戏自身 YSCM(不写死):
    /// `SOUNDEND[ID]` —— 唯一参数,类型字符串。
    pub(crate) fn cmd_soundend(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        let cmd = g.command_type;
        let mut id = None;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if let Some(v) = self.eval_window_condition(Some(w))? {
                if self.slot_is(cmd, slot, "ID", 0) {
                    if let Value::Str(bytes) = &v {
                        id = Some(v.str_as_string());
                        let _ = bytes;
                    }
                }
                evaluated.push((slot, value_summary(&v)));
            }
        }
        self.push_event(VmEvent::SoundEnd {
            pc: self.pc,
            id,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `CgInfo`(从 `run_main` 拆出;语义逐行照搬)。
    ///
    /// **勘误(2026-10-16,成果 140)**:本函数原先按 **v555 的槽号**硬编码
    /// 查询面与 LET 槽 —— `LET(33)` / `COLOR(24)`。实测本作 v494 的
    /// `CGINFO` 只有 33 个参数(0..32):`LET` 在**槽 30**、`COLOR` 在**槽 26**、
    /// `ONMOUSE2` 占 24。编译码里 `CGINFO … LET=` 的窗 tag 就是 `0x1e`(=30)
    /// —— 与硬编码 33 永不相等 ⇒ `pending_let` 恒 `None`、**应答从不写回**。
    ///
    /// 修法:槽位一律**按参数名**解析([`Self::slot_is`]),未识别的名字才
    /// 回落 v555 号(仅在无参数表时报文下用到)。这正是 AGENTS.md §3.1
    /// 「跨版本差异不得散落硬编码」的实例。
    pub(crate) fn cmd_cginfo(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_0043b084(Confirmed 结构):
        // ID(槽0)=CG 名;IDNO=名字后缀拼接;LET=结果写回目标;
        // 其余槽(EXIST/X/Y/SX/SY/ONMOUSE…)=查询项(非零启用)。
        // 全部查询汇入结果缓冲(函数头清零 DAT_005c0840=0)→ 接收器写 LET。
        // P7.1(成果 62):按 CG 状态注册表应答已建模查询
        //   (EXIST=1 / X / Y / SX / SY / COLOR);
        // **CG 未注册(名不存在)⇒ 引擎「不存在」路径 ⇒ 恒写 Int(0)**
        //   (iVar6==0 → 接收器以清零缓冲写 LET,Confirmed);
        // 未建模查询(ONMOUSE/TRIM/LINT 族)在 CG 存在时不写回
        //   (语义 Unknown,不猜;boot trace 未执行到)。
        let cmd = g.command_type;
        let mut id: Option<String> = None;
        let mut id_bytes: Option<Vec<u8>> = None;
        let mut let_target: Option<VarTarget> = None;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        let mut pending_let: Option<yuris_format::ystb::CommandSlot> = None;
        // 查询项 = (槽号, 参数名)。**按名应答**,数字只作无参数表时的回落。
        let mut query: Option<(u8, String)> = None;
        // ONMOUSE 族与 SET/SET2 要在**全部**窗口里找：手册示例
        // `CGINFO[ID=… ONMOUSE=1 SET=@_MOUSE_X SET2=@_MOUSE_Y LET=@v]`
        // 的第一个参数不一定就是查询项（SET/SET2 可能在前）。
        let mut onmouse_q: Option<String> = None;
        let mut set_val: Option<i64> = None;
        let mut set2_val: Option<i64> = None;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if self.slot_is(cmd, slot, "ID", 0) {
                if let Some(v) = self.eval_window_condition(Some(w))? {
                    if let Value::Str(bytes) = &v {
                        id = Some(v.str_as_string());
                        id_bytes = Some(bytes.clone());
                    }
                    evaluated.push((slot, value_summary(&v)));
                }
                continue;
            }
            // LET = 写回目标(槽号逐版本漂移:v555=33 / v494=30)
            if self.slot_is(cmd, slot, "LET", 33) {
                pending_let = Some(w.clone());
                continue;
            }
            if let Some(v) = self.eval_window_condition(Some(w))? {
                let pname = self
                    .command_dispatch()
                    .param_name_of(cmd, slot)
                    .unwrap_or_else(|| format!("slot{slot}"));
                if matches!(pname.as_str(), "ONMOUSE" | "ONMOUSE2" | "ONMOUSE3") {
                    onmouse_q = Some(pname.clone());
                } else if pname == "SET" {
                    if let Value::Int(n) = v {
                        set_val = Some(n);
                    }
                } else if pname == "SET2" {
                    if let Value::Int(n) = v {
                        set2_val = Some(n);
                    }
                } else if !self.command_dispatch().profile().has_params() {
                    // 无参数表时的 v555 号回落(21/22/23 = ONMOUSE 族,
                    // 34/35 = SET/SET2)。
                    match slot {
                        21 => onmouse_q = Some("ONMOUSE".into()),
                        22 => onmouse_q = Some("ONMOUSE2".into()),
                        23 => onmouse_q = Some("ONMOUSE3".into()),
                        34 => {
                            if let Value::Int(n) = v {
                                set_val = Some(n);
                            }
                        }
                        35 => {
                            if let Value::Int(n) = v {
                                set2_val = Some(n);
                            }
                        }
                        _ => {}
                    }
                }
                evaluated.push((slot, value_summary(&v)));
                if query.is_none() {
                    query = Some((slot, pname));
                }
            }
        }
        if let Some(w) = pending_let {
            let bytes = self
                .ctx
                .script
                .window_bytes_pooled_copy(&w)
                .ok_or_else(|| Error::format("CGINFO LET 引用窗越过池尾"))?;
            let_target = Some(extract_var_target(&bytes)?);
        }
        // 查询应答:注册命中 → 状态字段;未命中 → 0(引擎
        // 「不存在」路径);已建模查询之外的槽不写回(Unknown)。
        // SX/SY = 装载图像真实宽/高(引擎 watch oracle 实证:
        // occ1 dummy.png 1×1 → 1.0;occ2 txspace → 1350.0)。
        //
        // ONMOUSE / ONMOUSE2 / ONMOUSE3(手册 `cg/cginfo.html`):
        // 「レイヤの上にマウスが乗っているか」—— 命中返回该点的
        // RGB / α / α+RGB 通道值,**未命中返回 -1**。
        // 引擎实现(`FUN_0045aa44`)读**遮罩位图**的 24 位像素,值 > 0
        // 才算命中(这就是按钮可点区域 = 非透明像素的语义)。
        //
        // 本内核当前只有 CG 注册表(几何),没有纹理像素可读 ⇒
        // **矩形级命中**:命中返回 255(通道值不可知时的「不透明」哨兵;
        // 脚本侧语料恒按 `>= 阈值` 判定,阈值字段 `BT.MAP.A` 缺省 0),
        // 未命中返回 -1(手册明确值)。像素级遮罩待渲染侧提供读回,
        // 代码标注 UNVERIFIED。
        let mut answer: Option<i64> = None;
        if let (Some(name), Some(_qname)) = (&id_bytes, onmouse_q.as_deref()) {
            answer = Some(match self.cg_registry.get(name) {
                Some(st) => {
                    // 坐标:脚本给了 SET/SET2 就用它,否则用输入光标
                    // (`@_MOUSE_X/@_MOUSE_Y` 的同一真值面)。
                    let (mx, my) = (
                        set_val.unwrap_or(self.input_cursor.0),
                        set2_val.unwrap_or(self.input_cursor.1),
                    );
                    let (w, h) = cg_extent(st);
                    let over = w > 0
                        && h > 0
                        && mx >= st.x
                        && my >= st.y
                        && mx < st.x + w
                        && my < st.y + h;
                    if over { 255 } else { -1 }
                }
                // 图层不存在 → 未命中(-1;手册「乗っていない場合 -1」)。
                None => -1,
            });
        } else if let (Some(name), Some((_qslot, qname))) = (&id_bytes, query.as_ref()) {
            match self.cg_registry.get(name) {
                Some(st) => match qname.as_str() {
                    "EXIST" => answer = Some(1),
                    "E" => answer = Some(st.e),
                    "A" => answer = Some(st.a),
                    "X" => answer = Some(st.x),
                    "Y" => answer = Some(st.y),
                    "Z" => answer = Some(st.z),
                    "MODE" => answer = Some(st.mode.unwrap_or(0)),
                    "T" => answer = Some(st.t.unwrap_or(0)),
                    "TA" => answer = Some(st.ta.unwrap_or(0)),
                    "LINT" => answer = Some(st.lint.unwrap_or(0)),
                    "LINT2" => answer = Some(st.lint2.unwrap_or(0)),
                    // `TEX` = 图层 +0xa4 的字节(v494 `CGINFO` 0x43d8cf
                    // 读 `[edx+0xa4]`;`CG TEX=1` 置位)。0/1。
                    "TEX" => answer = Some(i64::from(st.tex)),
                    "SX" => answer = Some(if st.size_explicit { st.sx } else { st.img_w }),
                    "SY" => answer = Some(if st.size_explicit { st.sy } else { st.img_h }),
                    "COLOR" => answer = Some(st.color),
                    _ => {
                        // 无参数表时的 v555 号回落(EXIST/X/Y/SX/SY/COLOR)
                        let mapped = match *_qslot {
                            2 => Some(1i64),
                            5 => Some(st.x),
                            6 => Some(st.y),
                            13 => Some(st.img_w),
                            14 => Some(st.img_h),
                            24 => Some(st.color),
                            _ => None,
                        };
                        match mapped {
                            Some(v) => answer = Some(v),
                            None => {
                                // **未建模的 CGINFO 查询**:以前静默不写回。
                                self.note_unknown_slot(
                                    cmd,
                                    *_qslot,
                                    "CGINFO 查询槽(应答未建模,不写回)",
                                );
                            }
                        }
                    }
                },
                None => answer = Some(0),
            }
        }
        if let (Some(target), Some(v)) = (&let_target, answer) {
            self.write_var_target(target, Value::Int(v))?;
            let note = match (&id, query.as_ref()) {
                (Some(n), Some((q, _))) => {
                    format!("→ {} (cg {n:?} q{q} = {v})", target.summary())
                }
                _ => format!("→ {}", target.summary()),
            };
            evaluated.push((255u8, note));
        } else if let (Some(target), Some(name_bytes)) = (&let_target, &id_bytes) {
            // 未建模查询且 CG 存在:不写回(变量保持原值)。
            if self.cg_registry.contains_key(name_bytes) {
                // **不再静默**(2026-10-17 用户要求):查询没有应答。
                self.note_unknown_slot(
                    cmd,
                    query.as_ref().map(|(s, _)| *s).unwrap_or(0),
                    "CGINFO 查询应答未建模(不写回,调用方读到变量原值)",
                );
                evaluated.push((
                    254u8,
                    format!("→ {} (未建模查询,不写回)", target.summary()),
                ));
            } else {
                self.write_var_target(target, Value::Int(0))?;
                let name = id.as_deref().unwrap_or("?");
                evaluated.push((
                    255u8,
                    format!("→ {} (cg {name:?} 不存在路径=0)", target.summary()),
                ));
            }
        }
        self.push_event(VmEvent::CgInfo {
            pc: self.pc,
            id,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `CgAct`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_cgact(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_0042607c(12466B):**71 参的运行期图形管线合成**命令。
        //
        // 官方手册(命令一览 → CG 系 → CGACT)字段族:
        //   - `ID/ID2/ID3/ID4`(槽 0..3)= 源/目标图层引用
        //   - `X/Y/X2/Y2`(12..15)= 几何;`SX/SY/FSX/FSY`(16/17/22/23)= 尺寸
        //   - `SIZE..SIZE5`(24..28)= 缩放族;`ROTATION*`(29/30)= 旋转
        //   - `SAIDO/MEIDO/SEPIA/MERGE*`(31..35)= 色调;`TRANSITION*`(36/37)
        //   - `XOR/AND/TRIM/RESIZE/AKARUSA/MONO/VERTICAL/HORIZONTAL/MOSAIC`
        //     (38..46)= 位运算与滤镜;`BLUR..BLUR6`(47..52)= 模糊
        //   - `DIFFUSION/RASTER*/REVERSE/RECTPAINT*/WHIRLPOOL/BNO/TEXT`
        //     (53..63)= 扩散/光栅/反转/矩形涂/旋涡/横幅/文字
        //   - **`COPY/COPY2`(64/65)= 源层合成结果复制到目标**(手册
        //     「スクリーンショット機能」;`es.CGSSS` 截图宏即此)
        //   - **`TEXCOPY/TEXSSCOPY`(68/69)= 纹理拷贝** —— 片头 LOGO 的
        //     `SPL*` 层内容全靠它从 `SPLBUF*` 缓冲拷过来
        //   - `SWAP/SWAP2`(66/67)= 两层属性交换(REA `FUN_0042f57d` 已证)
        //
        // **本内核的义务(成果 133)**:把这些槽**如实带出**给下游
        // (之前只留 ID/X/Y,其余 67 个槽直接丢掉 ⇒ 所有演出效果"没有")。
        // 槽号按**游戏自身 YSCM 参数名**解析(逐版本漂移,不写死);
        // 未识别的槽仍记入 `evaluated` 摘要,不猜语义、不静默丢弃。
        let cmd = g.command_type;
        let mut id = None;
        let mut id_bytes: Option<Vec<u8>> = None;
        let mut act_x = None;
        let mut act_y = None;
        let mut effects: Vec<(u8, String, EffectValue)> = Vec::new();
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            let Some(v) = self.eval_window_condition(Some(w))? else {
                continue;
            };
            // 槽 0 = ID(引擎注册表键,单独处理并保持既有 patch 语义)
            if self.slot_is(cmd, slot, "ID", 0) {
                if let Value::Str(b) = &v {
                    id_bytes = Some(b.clone());
                    id = Some(v.str_as_string());
                }
                evaluated.push((slot, value_summary(&v)));
                continue;
            }
            // X/Y(槽 12/13 的既有 patch 行为;其余同名槽按名匹配)
            if self.slot_is(cmd, slot, "X", 12) {
                if let Value::Int(n) = v {
                    act_x = Some(n);
                }
            }
            if self.slot_is(cmd, slot, "Y", 13) {
                if let Value::Int(n) = v {
                    act_y = Some(n);
                }
            }
            // 效果槽:按参数名带出(X/Y 也一并带出,便于下游统一处理)
            let pname = self
                .command_dispatch()
                .param_name_of(cmd, slot)
                .unwrap_or_else(|| format!("slot{slot}"));

            let ev = match &v {
                Value::Int(n) => EffectValue::Int(*n),
                Value::Float(f) => EffectValue::Float(*f),
                Value::Str(b) => EffectValue::Str(String::from_utf8_lossy(b).into_owned()),
            };
            effects.push((slot, pname, ev));
            evaluated.push((slot, value_summary(&v)));
        }
        if let Some(b) = id_bytes {
            if let Some(st) = self.cg_registry.get_mut(&b) {
                if let Some(x) = act_x {
                    st.x = x;
                }
                if let Some(y) = act_y {
                    st.y = y;
                }
            }
        }
        self.push_event(VmEvent::CgAct {
            pc: self.pc,
            id,
            evaluated,
            effects,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 媒体播放族（T23b，2026-10-16；决策 =(A) 记录+立即推进 + 场景层屏蔽位）。
    ///
    /// 四个命令各有不同语义（参数表实证，见 `commands.rs` 分类注释）：
    ///
    /// | 命令 | 形态 | 本处理器动作 |
    /// |---|---|---|
    /// | `MOVIE` / `FLASH` | **全屏模态** | `PLAY=1` → 发 `MediaMode{on:true}`；`PLAY=0` 只停播放（**不退模式**，手册注[3]） |
    /// | `YMV` | 带 `ID` 的**影片图层** | 记录 + 推进（不置屏蔽位） |
    /// | `MIDI` | **音频**（`FILE/PLAY`） | 记录 + 推进 |
    ///
    /// **不编造时长**（铁律 2）：无解码器 ⇒ 媒体画面本身不绘制（显式 Unknown）。
    /// 屏蔽语义由场景层 `Scene::media_mode` 复制（手册明示）。
    ///
    /// 参数**全部求值并事件化记录**（诊断面）；`PLAY` 按参数名解析
    /// （槽位逐版本漂移）。
    pub(crate) fn cmd_media_play(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        let cmd = g.command_type;
        let cname = self
            .command_dispatch()
            .name_of(cmd as u16)
            .unwrap_or("<媒体?>")
            .to_string();
        let is_modal = matches!(cname.as_str(), "MOVIE" | "FLASH");
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        let mut play_flag: Option<bool> = None;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            match self.eval_window_condition(Some(w)) {
                Ok(Some(v)) => {
                    // `PLAY` 槽判定:按参数名(逐版本漂移)
                    if self.param_slot(cmd, "PLAY", u8::MAX) == Some(slot) {
                        play_flag = Some(matches!(&v, Value::Int(n) if *n != 0));
                    }
                    evaluated.push((slot, value_summary(&v)));
                }
                Ok(None) => {}
                Err(e) => evaluated.push((slot, format!("err:{e}"))),
            }
        }
        // 模态命令 + PLAY=1 → 置位媒体模式（手册:影片/Flash 播放期间屏蔽其他层）。
        // PLAY=0 是"停止播放"、**不退模式**（手册注[3] 原文即"停止しても…不显示"），
        // 故只在 play_flag == Some(true) 时置位。
        if is_modal && play_flag == Some(true) {
            self.push_event(VmEvent::MediaMode {
                pc: self.pc,
                on: true,
                command: cname.clone(),
            });
        }
        self.push_event(VmEvent::Subsystem {
            pc: self.pc,
            command: cmd,
            evaluated,
        });
        // **2026-10-17 政策变更**:媒体播放族**没有解码器** ⇒ 画面/声音
        // 都不会出现。旧实现「记录 + 立即推进」是特判续命;现在 strict
        // (运行时默认)挂起 + 致命错误窗,让「缺什么」直接上屏。
        // lenient 才保留「记录后继续」的取证路径。
        if let Some(reason) = self.missing_feature_halt(
            cmd,
            &format!("{cname} 播放:本内核无影片/Flash/MIDI 解码与绘制路径"),
        ) {
            return Ok(Action::Suspend(VmSuspend::Error(reason)));
        }
        self.pc += 1;
        Ok(Action::Keep)
    }

    /// 媒体查询族写回（T23；引擎处理器地址见 `docs/engine/v494-handler-table.md`
    /// —— `MOVIEINFO`@`0x44b898` / `YMVINFO`@`0x459934` / `FLASHINFO`@`0x443cb4`；
    /// `MIDIINFO` 在 v494 表里是共享 stub）。
    ///
    /// 官方手册（SDK 0.495 `マニュアル/YU-RIS/html/movie|flash/*.html`）
    /// 语义 = **查询型**：`<字段>=1 LET=@v` 把值写进 LET 目标；手册示例
    /// `MOVIEINFO[PLAY=1 LET=@A]`（再生中 1 / 停止 0）、
    /// `FLASHINFO[X=1 LET=@POSX]` 等。
    ///
    /// 本内核**没有**影片/Flash/MIDI 子系统 ⇒ 与 `SOUNDINFO`（成果 133）同一
    /// 口径：走引擎「对象不存在」路径 —— `PLAY`（及"是否在放"）答 0，
    /// 几何/帧/音量等一切"对象属性"亦答 0。这不是猜测：无对象时这些字段
    /// 的**唯一自洽值**就是 0，其余答案都需要一个不存在的对象。
    ///
    /// **为什么必须写回而不是挂起**：<game-b> 语料里这四个查询命令共
    /// **162 组**，严格模式下挂起会掐断片头/配置链（与 `SOUNDEND` 同一失效
    /// 模式，成果 133 已实证过一次）。
    ///
    /// 播放族（`MOVIE`/`YMV`/`MIDI`/`FLASH`）**不在此列** —— 见
    /// `commands.rs` 的分类注释与 T23b 决策挂号。
    pub(crate) fn cmd_media_info(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        let cmd = g.command_type;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        // LET 目标（槽名逐版本稳定,按名解析）
        let let_slot = self.param_slot(cmd, "LET", 1);
        let mut target: Option<VarTarget> = None;
        let mut has_query = false;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if Some(slot) == let_slot {
                // LET 是引用窗(不装载):经 extract_var_target 解析
                if let Ok(bytes) = self
                    .ctx
                    .script
                    .window_bytes_pooled_copy(w)
                    .ok_or_else(|| Error::format("媒体 INFO LET 引用窗越过池尾"))
                {
                    match extract_var_target(&bytes) {
                        Ok(t) => target = Some(t),
                        Err(e) => yuris_log::alert_once(
                            module_path!(),
                            format!(
                                "media-let-parse-fail:{}:{}",
                                self.ctx.script_id, self.pc
                            ),
                            format!(
                                "媒体 INFO 的 LET 引用窗无法解析,查询结果不写回\n\
                                 现场: script {} pc {}\n\
                                 错误: {e}",
                                self.ctx.script_id, self.pc
                            ),
                        ),
                    }
                }
                evaluated.push((slot, "let-ref".into()));
            } else {
                // 其余槽 = 查询字段(值应为 1)
                match self.eval_window_condition(Some(w)) {
                    Ok(Some(v)) => {
                        if matches!(&v, Value::Int(n) if *n != 0) {
                            has_query = true;
                        }
                        evaluated.push((slot, value_summary(&v)));
                    }
                    Ok(None) => {}
                    Err(e) => evaluated.push((slot, format!("err:{e}"))),
                }
            }
        }
        // **2026-10-17 政策变更(用户要求,取代 T23 的「写 0 继续」)**:
        // 「无媒体对象 ⇒ 全部字段答 0」是**编造值** —— 本内核根本没有
        // 影片/Flash/MIDI 子系统,任何查询都没有真值可答。旧实现特判
        // 「不挂起」以免掐断片头,这正是用户指出的「本该中断却被特判续命」。
        // 现在:strict(运行时默认)一律挂起 + 致命错误窗;lenient 才记录。
        if let Some(t) = &target {
            if has_query {
                evaluated.push((
                    254u8,
                    format!("未实现:媒体查询无对象可答 LET={}", t.summary()),
                ));
            }
        }
        self.push_event(VmEvent::Subsystem {
            pc: self.pc,
            command: cmd,
            evaluated,
        });
        if target.is_some() && has_query {
            let cname = self
                .command_dispatch()
                .name_of(cmd as u16)
                .unwrap_or("<媒体INFO?>")
                .to_string();
            if let Some(reason) = self.missing_feature_halt(
                cmd,
                &format!("{cname} 查询:本内核无影片/Flash/MIDI 子系统,拒绝写假 0"),
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            // lenient:保留旧口径(写 0)但已在上面留下「未实现」标记。
            if let Some(t) = &target {
                self.write_var_target(t, Value::Int(0))?;
            }
        }
        self.pc += 1;
        Ok(Action::Keep)
    }

    /// 处理 `CgEnd`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_cgend(&mut self, _g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 LAB_0043ad14(cmd 0x03;命令表 slot 11):ID(槽0,字符串)
        // = 结束显示的 CG 名。P7.1(成果 62):同步从 CG 状态
        // 注册表移除(显示结束 ⇒ 后续 CGINFO 查询走「不存在」路径)。
        let mut id = None;
        let mut id_bytes: Option<Vec<u8>> = None;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if let Some(v) = self.eval_window_condition(Some(w))? {
                if slot == 0 {
                    if let Value::Str(bytes) = &v {
                        id = Some(v.str_as_string());
                        id_bytes = Some(bytes.clone());
                    }
                }
                evaluated.push((slot, value_summary(&v)));
            }
        }
        if std::env::var_os("YURIS_TRACE_CGEND").is_some() {
            eprintln!(
                "[cgend-src] s{} pc{} groups={} id={:?}",
                self.ctx.script_id,
                self.pc,
                self.ctx.groups.len(),
                id.as_deref().unwrap_or("")
            );
        }
        if let Some(bytes) = &id_bytes {
            // **CGEND = 销毁显示,不是销毁对象(勘误 2026-10-17,Confirmed)**。
            //
            // 旧实现 `cg_registry.remove()` 把整个 CG 对象连同 **x/y/z/file**
            // 一起抹掉。后果:游戏随后的 `CG[ID=… FILE=…]` 重建**不带 Z 槽**
            // (引擎里 Z 是对象属性,重建命令只改它显式给出的槽)⇒ 新建对象
            // 的 z 取 Default(0)⇒ 层沉到 z=0。实测:boot 期每次
            // `BT.OFFES.CONFIG.TIP.BG=1=1` 走 CGEND→重建循环后 z 从 998 掉到 0,
            // 開屏 環境設定 页因此上屏。
            //
            // 引擎真值(`docs/engine/render/02-layer-model.md` §1):
            // 层对象 `+0xa4` = **0=无效(E=0,不进绘制)**,对象仍在链表里;
            // 销毁只发生在真正的 DEL 路径。所以此处应保留对象、只把 E 置 0。
            if let Some(st) = self.cg_registry.get_mut(bytes) {
                st.e = 0;
            }
        }
        self.push_event(VmEvent::CgEnd {
            pc: self.pc,
            id,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }
}
