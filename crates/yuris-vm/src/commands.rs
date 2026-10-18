//! VM 侧命令分类：把「游戏自己的 YSCM 序号」翻译成**与版本无关**的命令种类。
//!
//! ## 为什么不能 `match index`
//!
//! 旧实现直接在 `GroupVm::run` 里 `match g.command_type { cmd::GO => … }`，
//! 而 `cmd::*` 是**某一个引擎版本**的序号常量。实测（`docs/cross-version-commands.md`）
//! 两个版本共有 116 条命令里有 **104 条序号不同**，所以按序号分派必然跨版本错位。
//!
//! 正确链路：
//!
//! ```text
//! YSTB part1 byte0（= 该游戏 YSCM 序号）
//!   → CommandProfile.name_of(index)     （来自游戏自己的 ysc.json）
//!   → CmdKind::from_name(name)          （本模块，与版本无关）
//!   → 处理器
//! ```
//!
//! 分类只认**命令名**：名字是跨版本稳定的（同一语义命令在各版同名），
//! 序号不是。新增版本时，本模块不需要改动 —— 只要该版的 YSCM 提供了
//! 已知命令名，分派就自动正确。

use yuris_core::CommandProfile;

/// 与版本无关的命令种类。
///
/// 只列 VM **实际区分处理**的种类；其余一律 [`CmdKind::Other`]（走未实现路径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
    // ---- 已实装语义 ----
    /// `GO`：无条件跳转（标签 → 组号）。
    Go,
    /// `GOSUB`：压帧调用。
    Gosub,
    /// `RETURN`：弹帧返回。
    Return,
    /// `IF`：条件分支。
    If,
    /// `IFBLEND`：跳出嵌套 IF 块。
    IfBlend,
    /// `ELSE`。
    Else,
    /// `IFEND`。
    IfEnd,
    /// `LET`：赋值。
    Let,
    /// `LOOP`。
    Loop,
    /// `LOOPBREAK`。
    LoopBreak,
    /// `LOOPCONTINUE`。
    LoopContinue,
    /// `LOOPEND`。
    LoopEnd,
    /// `WAIT`。
    Wait,
    /// `RETURNCODE`：**调用点返回标记**，对脚本控制流无影响（Likely）。
    ///
    /// 全语料 28299 组，形态**恒为「恰好 1 个空窗口」**（`len==0`），窗口
    /// `tag` 是**逐脚本递增的序号** 1,2,3,…（实测 3559 种取值，max 0xE9B）。
    /// YSCM 参数数 = 0（本作 `ysc.ybn` 与官方 `YSCom.ycd` 双向一致）。
    ///
    /// 证据链：
    /// - 官方编译器定义 `YSCom.ycd` 里 `RETURNCODE` **无任何参数**；
    /// - 紧邻的 `RETURNBREAK` 在 v555 引擎中是 `xor eax,eax; ret`
    ///   （**空操作**；另一条线实测 `opcode_table.tsv` opcode 78 → `0x4634d8`）；
    /// - v555 引擎 opcode 79（= `RETURNCODE`）虽有真实处理器，但其副作用
    ///   全在引擎任务结构（`task+0xc`/`task+0x10`、事件队列 `0x801800`）上，
    ///   **不改脚本 pc、不动调用栈**（`0x44c390` 反汇编，见
    ///   `docs/engine/command-layer.md` §12）；
    /// - 结构证据：本作 `ES.CHARNAME.DEFINE`（s84，256 组）的 pc5 即
    ///   `RETURNCODE`；若它弹帧返回，其后 250 组（登记约 30 个角色名）
    ///   全部成为死代码，与脚本用途矛盾。
    ///
    /// ⇒ 本 VM 按「**推进 pc 的空操作**」实现。
    ///
    /// **U15 已解除(T4 取证,2026-10-16,成果 148)**:v494 侧处理器体已取证 ——
    /// 命令处理器表(基址 `0x7aae40`)映射出 `RETURNCODE`(idx 79)= **`0x44c20c`**;
    /// 反汇编确认副作用全在**任务对象**上(`task+0x0c`/`task+0x10` 调用点标记、
    /// 事件队列缓冲追加字节、`task+0x36` 状态位),末尾 `return -1`,
    /// **不改脚本 pc、不动调用栈** —— 与 v555 侧 `0x44c390` **双版本同判**。
    /// 调用点标记值由 [`crate::GroupVm::returncode_hits`] 计数(不落任务对象)。
    /// 表与逐条映射见 `docs/engine/v494-handler-table.md`。
    ReturnCode,
    /// `RETURNBREAK`：**编译期标记 / 空操作**（Confirmed）。
    ///
    /// v555 引擎实测处理器 = `0x4634d8`（`xor eax,eax; ret`），与
    /// `ALIAS`/`DEBUGLIST`/`PREP`/`S_FLT` 等编译期命令同址。
    ReturnBreak,
    /// `TEXT`：文本显示（FILE 参数指向文本文件）。
    Text,
    /// `WORD`：**内联文本**显示（窗口是裸 SJIS 字面量，不是字节码）。
    ///
    /// 本作用它承担全部台词 —— 全语料 28992 组,而 `TEXT` 仅 2 组;
    /// 因此「文本显示」这一职责在 v494 语料里由 `WORD` 承担。
    Word,
    /// `CG`：图像图层。
    Cg,
    /// `CGACT`：图层操作。
    CgAct,
    /// `CGINFO`：图层查询。
    CgInfo,
    /// `CGEND`：图层结束通知。
    CgEnd,
    /// `SOUND`。
    Sound,
    /// `END`：脚本结束。
    End,
    /// `SAVEEND`：清空 `SAVE` 预留保存的数据（手册 `file/saveend.html`）。
    SaveEnd,
    /// `SOUNDEND`：删除指定 ID 的声音图层（手册 `sound/soundend.html`）。
    ///
    /// **为什么必须是独立种类（2026-10-15,成果 133）**:片头链
    /// `s142(scenario_start) → es.SND("人生通行止め")` 内部立刻调它;
    /// 归入 `Other` 时 `strict` 模式直接挂起 ⇒ **整条片头演出(含厂商 LOGO)
    /// 全部不执行**,实机表现为片头纯黑。
    SoundEnd,
    /// `VARINFO`：变量属性查询。
    VarInfo,
    /// `VARACT`：字符串变量操作。
    VarAct,
    /// `LABELINFO`：标签存在性查询。
    LabelInfo,
    /// `LOAD`。
    Load,
    /// `SAVE`。
    Save,
    /// `TASK`：命名任务创建/重配置（**建立独立执行位并进入调度**）。
    Task,
    /// `TASKEND`：命名任务销毁（停调度、丢掉执行位，名字保留可被 `TASKINFO`
    /// 查到「已存在但未运行」的边界由引擎决定；此处按"停调度"实现）。
    TaskEnd,
    /// `TASKINFO`：任务状态查询。
    TaskInfo,

    // ---- 局部类型声明（带初值写入；`FLT` / `INT` / `STR`）----
    /// 局部浮点声明。
    DeclLocalFloat,
    /// 局部整型声明。
    DeclLocalInt,
    /// 局部字符串声明。
    DeclLocalStr,

    // ---- 其余声明族（加载期数据，运行期事件化；`F_*` / `G_*` / `S_*` / `VAR`）----
    /// 其他变量声明。
    DeclOther,

    // ---- 子系统族 ----
    /// `FILEINFO`：文件/路径查询（含 EXIST）。
    FileInfo,
    /// `WINDOW`：引擎侧为共享 no-op stub → 完全静默。
    Window,
    /// `WINDOWINFO`：窗口属性查询（含 SX/SY 写回）。
    WindowInfo,
    /// `FONTINFO`：字体属性查询（含 NUM/LANG 写回）。
    FontInfo,
    /// `ERROR`：**错误输出级别配置**（不是错误触发）。
    ///
    /// 参数表（YSCM，逐版本稳定）是 `NO / SET / COM_CG / COM_CGACT / …`：
    /// 每个 `COM_*` 对应一条命令，值为该命令的错误输出级别。SDK 自带
    /// `es_ERIS.yst` 第 33 行 `ERROR[COM_CG=0]` 就是启动时的这类配置。
    ///
    /// **引擎处理体（v494 `0x4411f8`，REA 取证）**：取最后一个窗口的 INT 值
    /// 写进进程级错误码字节 `0x802804`、把节流位 `0x802b44` 置 1，
    /// **不改脚本 pc、不动调用栈、不碰变量与场景**，随后 `ret 0`（继续）。
    /// 故本内核按「记录配置 + 推进 pc」实现，**不得**归入未实装子系统而挂起
    /// —— 那会在启动链最前面误判成致命错误（实测 s8 pc12 即此命令）。
    Error,
    /// `FILEACT`：文件操作（建立目录 / 复制 / 删除 / 执行 / 图层导出 / 缓存登记）。
    ///
    /// 手册 `file/fileact.html` 给全了参数语义；引擎处理体（v494
    /// `0x441234`）逐槽分支实测：`CREATE` → 递归建目录（`0x4426cc`）、
    /// `COPY` → `CopyFileA`、`DELETE` → `DeleteFileA` + `RemoveDirectoryA`、
    /// `EXEC` → `ShellExecuteExA`（`SEE_MASK_NOCLOSEPROCESS`）。
    ///
    /// VM 只把参数解析成 [`crate::VmEvent::FileAct`] 意图，**不碰平台文件
    /// 系统**（`RuntimeApi` 是唯一边界）。
    FileAct,
    /// `EMOTE`：E-mote 角色动画控制。
    ///
    /// 引擎处理体整体被「`YSEmote.dll` 已装载」标志（`0x802a28`）包住；
    /// 未装载时是**彻底的空操作**。见 `exec::emote` 模块文档。
    Emote,
    /// `EMOTEINFO`：E-mote 查询。v494 处理体表里落在共享 stub（空实现）。
    EmoteInfo,
    /// `SYSTEMINFO`：引擎自述信息查询（脚本名 / 脚本数 / 文本数 / E-mote 标志）。
    ///
    /// 真值来自 `yst_list.ybn`；处理体 v494 `0x450478` 逐槽取证。
    SystemInfo,
    /// `SYSTEM`：系统设置（屏保 / 后台执行 / 屏幕尺寸 / 计时器）。
    ///
    /// 引擎处理体 v494 `0x4500d0` 逐槽取证（见 `exec::system` 模块文档）。
    System,
    /// 其余子系统命令（`DIALOG` / `FONT` / `FPS` / `INPUT` / `MATH` /
    /// `MOUSE`）：参数求值后事件化记录。
    Subsystem,
    /// 媒体查询族（`MOVIEINFO` / `YMVINFO` / `MIDIINFO` / `FLASHINFO`）：
    /// 按手册字段语义把「无媒体对象」的唯一自洽值写进 `LET` 目标（T23）。
    MediaInfo,
    /// 媒体结束族（`MOVIEEND` / `YMVEND` / `MIDIEND` / `FLASHEND`）：
    /// 「退出媒体再生模式」——清除场景层媒体模式位、空操作推进（T23b）。
    MediaEnd,
    /// 模态播放族（`MOVIE` / `FLASH`）：置场景层**媒体模式位**（屏蔽其他图层），
    /// 记录事件后立即推进（无解码器 ⇒ 媒体画面本身不绘制，显式 Unknown）。
    MediaPlay,
    /// `YMV`：可寻址的**影片图层**（带 `ID`）——记录事件后立即推进，
    /// **不置**媒体模式位（它不是模态模式）。
    MediaPlayLayer,
    /// `MIDI`：音频播放（`FILE/PLAY`）——记录事件后立即推进。
    MediaPlayAudio,

    // ---- 未知 ----
    /// 本 VM 未区分处理（引擎默认 stub / 尚未逆向）。
    Other,
}

impl CmdKind {
    /// 由**命令名**分类（版本无关）。
    pub fn from_name(name: &str) -> Self {
        match name {
            "GO" => Self::Go,
            "GOSUB" => Self::Gosub,
            "RETURN" => Self::Return,
            "RETURNCODE" => Self::ReturnCode,
            "RETURNBREAK" => Self::ReturnBreak,
            "IF" => Self::If,
            "IFBLEND" => Self::IfBlend,
            "ELSE" => Self::Else,
            "IFEND" => Self::IfEnd,
            "LET" => Self::Let,
            "LOOP" => Self::Loop,
            "LOOPBREAK" => Self::LoopBreak,
            "LOOPCONTINUE" => Self::LoopContinue,
            "LOOPEND" => Self::LoopEnd,
            "WAIT" => Self::Wait,
            "TEXT" => Self::Text,
            "WORD" => Self::Word,
            "CG" => Self::Cg,
            "CGACT" => Self::CgAct,
            "CGINFO" => Self::CgInfo,
            "CGEND" => Self::CgEnd,
            "SOUND" => Self::Sound,
            // `SOUNDEND[ID]`：删除指定 ID 的声音图层（官方手册
            // `sound/soundend.html`：「サウンドレイヤを削除します」）。
            "SOUNDEND" => Self::SoundEnd,
            "END" => Self::End,
            "VARINFO" => Self::VarInfo,
            "VARACT" => Self::VarAct,
            "LABELINFO" => Self::LabelInfo,
            "LOAD" => Self::Load,
            "SAVE" => Self::Save,
            // `SAVEEND[]`：清空 `SAVE` 预留保存的数据（官方手册
            // `file/saveend.html`:「SAVE 命令で予約保存されているデータを
            // 初期化（クリア）します」）。片头/存档初始化链会调它。
            "SAVEEND" => Self::SaveEnd,
    "TASK" => Self::Task,
    "TASKEND" => Self::TaskEnd,
    "TASKINFO" => Self::TaskInfo,

            // 局部类型声明（运行期写初值）
            "FLT" => Self::DeclLocalFloat,
            "INT" => Self::DeclLocalInt,
            "STR" => Self::DeclLocalStr,

            // 其余声明族（含 VAR；`F_*` 帧局部 / `G_*` 全局 / `S_*` 脚本）
            "F_FLT" | "F_INT" | "F_STR" | "F_VAR" | "G_FLT" | "G_FLT2" | "G_FLT3"
            | "G_FLT4" | "G_INT" | "G_INT2" | "G_INT3" | "G_INT4" | "G_STR" | "G_STR2"
            | "G_STR3" | "G_STR4" | "G_VAR" | "S_FLT" | "S_INT" | "S_STR" | "S_VAR"
            | "VAR" => Self::DeclOther,

            "FILEINFO" => Self::FileInfo,
            "WINDOW" => Self::Window,
            "WINDOWINFO" => Self::WindowInfo,
            "FONTINFO" => Self::FontInfo,
            // `ERROR[NO=… COM_CG=… …]`:错误输出级别配置(见 `Error` 变体文档)。
            "ERROR" => Self::Error,
            // `FILEACT[...]`:文件操作意图(见 `FileAct` 变体文档)。
            "FILEACT" => Self::FileAct,
            // MATH3D(2 参:INT/FLT 双数组引用):eris 专属投影命令(引擎处理体
            // 0x44874c;官方 XYZCALC 源码注释「ERIS専用命令(高速計算用命令)」)。
            // 归 Subsystem = 走 misc.rs 的 cmd_math3d 写回分支(U36 主修复)。
            "EMOTE" => Self::Emote,
            "EMOTEINFO" => Self::EmoteInfo,
            "SYSTEMINFO" => Self::SystemInfo,
            "SYSTEM" => Self::System,
            "DIALOG" | "FONT" | "FPS" | "INPUT" | "MATH" | "MATH3D" | "MOUSE" => Self::Subsystem,

            // ---- 查询型子系统的显式处置 ----
            //
            // `SOUNDINFO`(24 参)：ID/EXIST/E/PLAY/PAUSE/VOLUME/PAN/PITCH/
            //   FILE/FILETYPE/START/RESTART/END/LOOP…（查询型）。
            // `SYSTEMINFO`(6 参)：SCRIPTFILE/SCRIPTNUM/TEXTNUM/EMOTE/NO/LET。
            //
            // 两者归 `Subsystem` = 求值参数 → 事件化 → 推进;若脚本真的发起
            // 写回则按「无真值可答」strict 挂起（不写假值）。
            // `EMOTE`/`EMOTEINFO` 有独立的引擎语义（空操作），见上方各自的
            // 变体文档;不得重新并回这里。
            "SOUNDINFO" => Self::Subsystem,

            // ---- 媒体族（T23,2026-10-16）----
            //
            // 官方手册（SDK 0.495 `マニュアル/YU-RIS/html/`）语义：
            //
            // - `MOVIEINFO`（8 参 PLAY/X/Y/SX/SY/VOLUME/CODEC/LET）与
            //   `FLASHINFO`（10 参 PLAY/VAR/X/Y/SX/SY/FRAME/ENDFRAME/TOTALFRAME/LET）
            //   = **查询型**：`<字段>=1 LET=@v` 把值写进 LET 目标；
            // - `YMVINFO`（7 参 ID/IDNO/LET/NUM/SX/SY/FPS）= YMV 影片查询；
            // - `MIDIINFO` = 无参数（SDK 表 0 参）。
            //
            // 本内核**没有视频/Flash/MIDI 子系统**（如音频层先行例）⇒ 查询
            // 一律走「无对象」路径：`PLAY=0`、几何/音量/FPS 等 = 0（与
            // `SOUNDINFO` 同一自洽口径，见 `exec/misc.rs` 的写回块）。
            "MOVIEINFO" | "YMVINFO" | "MIDIINFO" | "FLASHINFO" => Self::MediaInfo,
            // 媒体结束族：手册语义 =「退出媒体再生模式」（`MOVIEEND`:「ムービー
            // 再生モードから抜けます」）⇒ **清除**媒体模式位（见 `MediaPlay`）。
            "MOVIEEND" | "YMVEND" | "MIDIEND" | "FLASHEND" => Self::MediaEnd,
            // 播放族按**参数形态**分三类（参数表实证，SDK 0.495 与 v555 同）：
            //
            // - `MOVIE`   = `FILE/PLAY/X/Y/SX/SY/VOLUME` —— **全屏模态**
            //   （手册：「レイヤーとして操作することは出来ず、全てのレイヤより
            //   前面に表示されます」）；`PLAY=1` 进入媒体模式。
            // - `FLASH`   = `FILE/PLAY/X/Y/SX/SY/ENDFRAME` —— 同为**模态**
            //   （手册：「FLASHファイル再生モードになると、他のどのレイヤより
            //   最前面になります」）。
            // - `YMV`     = `ID/IDNO/FILE/FRAME` —— **带 ID ⇒ 可寻址的影片图层**，
            //   不是模态模式（语料 s66 里紧跟 4 条 `YMVINFO` 查几何后按帧推进）；
            //   故不置屏蔽位。
            // - `MIDI`    = `FILE/PLAY` —— **音频**（BGM 的 MIDI 通道），
            //   与画面层无关。
            //
            // **T23b 决策已定（2026-10-16，仓库主人采纳建议 (A)）**：无解码器 ⇒
            // 「记录 + 立即推进」，模态命令另发 [`crate::VmEvent::MediaMode`]
            // 让场景层复制手册明示的**屏蔽语义**；VM **不编造任何时长**（铁律 2）。
            // 证据：手册注[3]「停止しても、MOVIEEND で終了しない限り他のレイヤは
            // 一切表示されません」⇒ `MOVIE` 不阻塞脚本；语料 s66 的 `YMV`
            // 每帧设 `FRAME`（逐帧驱动）⇒ 阻塞语义下不成立。
            // 详见 `PROGRESS.md` 成果 149 §四 / 151。
            "MOVIE" | "FLASH" => Self::MediaPlay,
            "YMV" => Self::MediaPlayLayer,
            "MIDI" => Self::MediaPlayAudio,
            _ => Self::Other,
        }
    }

    /// 是否进入「子系统参数事件化」块。
    ///
    /// `FILEINFO` 有自己的前置分支（会先 `continue`），故不在本判定内 —— 与
    /// 旧实现的等价关系一致（旧码的 `matches!` 列表里 `FILEINFO` 槽不可达）。
    pub fn enters_subsystem_block(self) -> bool {
        matches!(self, Self::Subsystem | Self::WindowInfo | Self::FontInfo)
    }

    /// 是否属于声明族（含局部与 `F_*`/`G_*`/`S_*`/`VAR`）。
    pub fn is_declaration(self) -> bool {
        matches!(
            self,
            Self::DeclLocalFloat
                | Self::DeclLocalInt
                | Self::DeclLocalStr
                | Self::DeclOther
        )
    }

    /// 声明语句所建立的变量元素类型（`consume_declarations_into` 用）。
    ///
    /// `F_VAR`/`G_VAR`/`S_VAR`/`VAR` 不建标量元素 → `None`（与旧实现的
    /// 元素类型表一致：它们不在 Float/Int/Str 任一族里）。
    pub fn declared_elem_type(self) -> Option<DeclElem> {
        match self {
            Self::DeclLocalFloat => Some(DeclElem::Float),
            Self::DeclLocalInt => Some(DeclElem::Int),
            Self::DeclLocalStr => Some(DeclElem::Str),
            Self::DeclOther => None,
            _ => None,
        }
    }

    /// 全局/脚本声明（`G_*` / `S_*` / `F_*`）的元素类型。
    ///
    /// 这些**不**在运行期写初值（引擎走默认 stub），但**加载期**要建立
    /// 变量描述符，故单独给出元素类型表。
    pub fn global_decl_elem_type(name: &str) -> Option<DeclElem> {
        match name {
            "F_FLT" | "G_FLT" | "G_FLT2" | "G_FLT3" | "G_FLT4" | "S_FLT" => {
                Some(DeclElem::Float)
            }
            "F_INT" | "G_INT" | "G_INT2" | "G_INT3" | "G_INT4" | "S_INT" => {
                Some(DeclElem::Int)
            }
            "F_STR" | "G_STR" | "G_STR2" | "G_STR3" | "G_STR4" | "S_STR" => {
                Some(DeclElem::Str)
            }
            _ => None,
        }
    }

    /// 声明族**整体**（局部 + `F_*`/`G_*`/`S_*`）的变量元素类型。
    ///
    /// 加载期建变量描述符用；`*_VAR` 与 `VAR` 不建标量元素 → `None`。
    pub fn decl_elem_type(name: &str) -> Option<DeclElem> {
        match name {
            "FLT" => Some(DeclElem::Float),
            "INT" => Some(DeclElem::Int),
            "STR" => Some(DeclElem::Str),
            _ => Self::global_decl_elem_type(name),
        }
    }
}

/// 声明语句建立的变量元素类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclElem {
    /// 浮点。
    Float,
    /// 整型。
    Int,
    /// 字符串。
    Str,
}

/// 由 [`CommandProfile`] 预计算的分派表：`dispatch[index] = CmdKind`。
///
/// O(1) 查表；装载 profile 时构建一次。
///
/// **与引擎同构的实证**(T4/T6 取证,2026-10-16,成果 148):引擎的命令分派是
/// `CALL dword ptr [handler + idx*4]`(`FUN_0045177d`@`0x451b09`:表基址
/// `0x7aae40`,由初始化器 `FUN_0045cd30` 逐条填 119 项)—— 本结构即该模型的
/// 等价物:**下标 → 处理器**(本内核的"处理器"经 `CmdKind` 落到 `exec/` 方法)。
/// 逐条地址映射见 `docs/engine/v494-handler-table.md`。
#[derive(Debug, Clone)]
pub struct CommandDispatch {
    table: Vec<CmdKind>,
    profile: CommandProfile,
}

impl CommandDispatch {
    /// 由 profile 构建分派表。
    pub fn new(profile: CommandProfile) -> Self {
        let table = profile
            .names()
            .iter()
            .map(|n| CmdKind::from_name(n))
            .collect();
        Self { table, profile }
    }

    /// 按**引擎版本号**取内置分派表。
    ///
    /// `None` = 本内核不认识该版本（调用方须提供游戏自身的 YSCM，不得猜测）。
    pub fn for_engine_version(version: u32) -> Option<Self> {
        CommandProfile::for_engine_version(version).map(Self::new)
    }

    /// 官方 SDK 的默认分派表（最新已知 SDK）。
    ///
    /// 用于没有版本线索的兜底场景（工具/测试）；生产路径必须由启动链注入
    /// 游戏自身的 YSCM。
    pub fn sdk_default() -> Self {
        Self::new(CommandProfile::sdk_latest())
    }

    /// **未解析**分派表：空表 → 任何序号都得到 [`CmdKind::Other`]。
    ///
    /// 用途：版本未知且无 YSCM 时，让 VM **显式报未实现**（`strict` 下挂起），
    /// 而不是按一个错误的内置表静默执行错误语义。
    pub fn unresolved() -> Self {
        Self::new(CommandProfile::default())
    }

    /// 序号 → 命令种类。越界（脚本序号超出本 profile）→ [`CmdKind::Other`]。
    pub fn kind(&self, index: u16) -> CmdKind {
        self.table
            .get(index as usize)
            .copied()
            .unwrap_or(CmdKind::Other)
    }

    /// 序号 → 命令名。
    /// 取某命令某槽号的**参数名**(槽号即 YSCM 参数表下标)。
    ///
    /// 用途:`CGACT` 的 70 个效果槽按名带出(成果 133)—— 槽号逐版本漂移,
    /// 只有名字稳定。未知槽返回 `None`(调用方回退为 `slot{n}`,不猜语义)。
    pub fn param_name_of(&self, index: u8, slot: u8) -> Option<String> {
        self.profile
            .params_of(index as u16)
            .get(slot as usize)
            .cloned()
    }

    /// 命令序号 → 命令名(按当前分派表 = 游戏自身 `ysc.ybn` 口径)。
    pub fn name_of(&self, index: u16) -> Option<&str> {
        self.profile.name_of(index)
    }

    /// **参数名 → 槽位**（序号 → 参数名表 → 下标）。
    ///
    /// YSTB 参数窗口的 `tag & 0xff` 即此槽位。槽位号逐版本漂移（实测
    /// `CG.FILE`：v555 槽 46 / v494 槽 33 / v479 槽 29），故处理器必须按
    /// **参数名**取槽，不得写死数字。
    ///
    /// 返回 `None` = 该命令无参数表（旧式 [`CommandProfile::from_names`] 构建）
    /// 或表里无此参数名 —— 调用方须按「未确认」处理，不得回落某个版本的常量。
    pub fn slot(&self, command_index: u8, param: &str) -> Option<u8> {
        self.profile.slot_of(command_index as u16, param)
    }

    /// **参数名 → 槽位**（按命令名；诊断/测试用）。
    pub fn slot_named(&self, command: &str, param: &str) -> Option<u8> {
        self.profile.slot_of_named(command, param)
    }

    /// 底层 profile。
    pub fn profile(&self) -> &CommandProfile {
        &self.profile
    }

    /// 是否已解析（非空表且来源明确）。
    pub fn is_resolved(&self) -> bool {
        !self.table.is_empty()
    }

    /// 命令条数。
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_name_not_index() {
        assert_eq!(CmdKind::from_name("GO"), CmdKind::Go);
        assert_eq!(CmdKind::from_name("WAIT"), CmdKind::Wait);
        assert_eq!(CmdKind::from_name("VARACT"), CmdKind::VarAct);
        assert_eq!(CmdKind::from_name("INT"), CmdKind::DeclLocalInt);
        assert_eq!(CmdKind::from_name("G_INT2"), CmdKind::DeclOther);
        assert_eq!(CmdKind::from_name("WINDOW"), CmdKind::Window);
        assert_eq!(CmdKind::from_name("NO_SUCH"), CmdKind::Other);
    }

    /// `RETURN` 家族三分：`RETURN`(弹帧)、`RETURNCODE`(调用点标记,不弹帧)、
    /// `RETURNBREAK`(编译期空操作)。三者**不得混为一类** —— 混了会让
    /// `es.CHARNAME.DEFINE` 这类"登记型"脚本在第 5 组就提前返回（实测后果:
    /// 其后 250 组全成死代码，画面永远出不来）。
    #[test]
    fn return_family_is_split_three_ways() {
        assert_eq!(CmdKind::from_name("RETURN"), CmdKind::Return);
        assert_eq!(CmdKind::from_name("RETURNCODE"), CmdKind::ReturnCode);
        assert_eq!(CmdKind::from_name("RETURNBREAK"), CmdKind::ReturnBreak);
        assert_ne!(CmdKind::ReturnCode, CmdKind::Return);
        assert_ne!(CmdKind::ReturnBreak, CmdKind::Return);
    }

    /// 查询/动画族**不得**落到 `Other`（那会在严格模式下挂起）。
    ///
    /// 2026-10-18 变更：`EMOTE`/`EMOTEINFO` 已按引擎语义（E-mote 运行时
    /// 不在位 = 空操作）独立建模，`SYSTEMINFO` 也只是查询 —— 它们各有
    /// 专属 `CmdKind`，不再共用一个 `Subsystem` 兜底臂。本测试改为断言
    /// 「分类到了具体种类」，而不是「都归 Subsystem」。
    #[test]
    fn query_families_have_dedicated_kinds() {
        assert_eq!(CmdKind::from_name("EMOTE"), CmdKind::Emote);
        assert_eq!(CmdKind::from_name("EMOTEINFO"), CmdKind::EmoteInfo);
        assert_eq!(CmdKind::from_name("SYSTEMINFO"), CmdKind::SystemInfo);
        assert_eq!(CmdKind::from_name("SOUNDINFO"), CmdKind::Subsystem);
        assert!(CmdKind::from_name("SOUNDINFO").enters_subsystem_block());
        assert_eq!(CmdKind::from_name("SYSTEM"), CmdKind::System);
        assert_eq!(CmdKind::from_name("FILEACT"), CmdKind::FileAct);
        assert_eq!(CmdKind::from_name("ERROR"), CmdKind::Error);
        assert_eq!(CmdKind::from_name("TASKINFO"), CmdKind::TaskInfo);
        for n in [
            "EMOTE", "EMOTEINFO", "SYSTEMINFO", "SOUNDINFO", "SYSTEM", "FILEACT", "ERROR",
            "TASKINFO",
        ] {
            assert_ne!(CmdKind::from_name(n), CmdKind::Other, "{n} 不得落到 Other");
        }
    }

    /// 同一命令名在两版序号不同，但分派结果必须相同 —— 这是 P1 的核心验收。
    #[test]
    fn same_name_same_kind_across_versions() {
        let d555 = CommandDispatch::new(CommandProfile::v555());
        let d494 = CommandDispatch::new(CommandProfile::v494());

        for name in ["GO", "GOSUB", "IF", "LET", "RETURN", "WAIT", "VARACT", "CG", "END"] {
            let i5 = d555.profile().index_of(name).expect("555 应有该命令");
            let i4 = d494.profile().index_of(name).expect("494 应有该命令");
            assert_eq!(
                d555.kind(i5),
                d494.kind(i4),
                "{name}: 两版序号不同（{i5} vs {i4}）但种类必须相同"
            );
        }
        // 且序号确实不同（否则本测试无意义）
        assert_ne!(
            d555.profile().index_of("GO"),
            d494.profile().index_of("GO")
        );
    }

    /// 版本未知时不得猜测：未解析分派表对任何序号都给 Other。
    #[test]
    fn unresolved_dispatch_is_all_other() {
        let d = CommandDispatch::unresolved();
        assert!(!d.is_resolved());
        for i in 0..200u16 {
            assert_eq!(d.kind(i), CmdKind::Other);
        }
        assert!(CommandDispatch::for_engine_version(12345).is_none(), "未知版本应返回 None 而非猜测");
    }

    /// 官方 SDK 版本都能取到内置表。
    #[test]
    fn builtin_engine_versions_resolve() {
        for v in yuris_core::commands::BUILTIN_ENGINE_VERSIONS {
            let d = CommandDispatch::for_engine_version(v)
                .unwrap_or_else(|| panic!("版本 {v} 应有内置表"));
            assert!(d.is_resolved());
            assert_eq!(d.profile().engine_version(), v);
            assert_eq!(d.kind(d.profile().index_of("GO").unwrap()), CmdKind::Go);
        }
    }

    #[test]
    fn out_of_range_index_is_other() {
        let d = CommandDispatch::sdk_default();
        assert_eq!(d.kind(0xFFFF), CmdKind::Other);
    }

    /// 声明族元素类型表：与旧实现的 Float/Int/Str 集合逐条对齐。
    #[test]
    fn decl_elem_types() {
        assert_eq!(CmdKind::from_name("FLT").declared_elem_type(), Some(DeclElem::Float));
        assert_eq!(CmdKind::from_name("INT").declared_elem_type(), Some(DeclElem::Int));
        assert_eq!(CmdKind::from_name("STR").declared_elem_type(), Some(DeclElem::Str));
        assert_eq!(CmdKind::from_name("F_VAR").declared_elem_type(), None);
        assert_eq!(CmdKind::global_decl_elem_type("G_FLT4"), Some(DeclElem::Float));
        assert_eq!(CmdKind::global_decl_elem_type("S_STR"), Some(DeclElem::Str));
        assert_eq!(CmdKind::global_decl_elem_type("G_VAR"), None);
    }
}
