//! VM 的执行状态、挂起/恢复协议与**事件流**(Trace / Golden Test 的数据源)。
//!
//! 从 `lib.rs` 拆出(2026-10-15 重构,纯搬运不改行为)。事件流是内核与
//! 播放器 / 取证工具之间的**唯一观测面** —— `VmEvent` 一变,全部下游
//! (bridge / player-core / 探针 / golden 快照)都会看到。

use yuris_value::Value;

/// 效果槽的求值结果(类型保真;`CGACT` 的图形管线参数族)。
#[derive(Debug, Clone, PartialEq)]
pub enum EffectValue {
    /// 整数值(坐标/尺寸/开关/强度)。
    Int(i64),
    /// 浮点值(旋转角/透明度比例)。
    Float(f64),
    /// 字符串值(图层 ID 引用,如 `COPY` 的源/目标)。
    Str(String),
}

/// VM 状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    /// 尚未启动。
    Idle,
    /// 可继续执行(配额内)。
    Running,
    /// 脚本执行完毕。
    Finished,
    /// 遇到未证实/未实现语义,已挂起。
    Error,
}

/// 一次 `run` 返回的挂起信息。**绝不阻塞**。
#[derive(Debug, Clone, PartialEq)]
pub enum VmSuspend {
    /// 配额耗尽或本批执行完毕,可继续。
    None,
    /// 脚本执行完毕。
    Complete,
    /// 遇到未实现语义/错误。携带描述。
    Error(String),
    /// WAIT:引擎参数 FRAME(帧计数,obj+0x18)/TIME(ms,obj+0x1c=timeGetTime()+ms)。
    /// 调用方完成等待后 `resume` 继续。
    Wait {
        /// FRAME 参数(帧数);None = 未指定。
        counter: Option<u64>,
        /// TIME 参数(毫秒);None = 未指定。
        time_ms: Option<u64>,
    },
}

/// 恢复执行的外部应答。
#[derive(Debug, Clone, PartialEq)]
pub enum ResumeResponse {
    /// 无条件继续。
    Continue,
}

/// 跳转类别(事件流/Golden Test 用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpKind {
    /// GO。
    Go,
    /// IF 条件为假。
    IfFalse,
    /// ELSE(跳过 else 块)。
    Else,
    /// RETURN 弹帧。
    Return,
    /// LOOPBREAK(跳出循环)。
    LoopBreak,
    /// LOOPEND/LOOPCONTINUE(回到循环体顶部)。
    LoopContinue,
}

/// GO 的执行结局。
pub(crate) enum GoOutcome {
    /// 跳到目标组(当前脚本内)。
    Jump(usize),
    /// 跨脚本跳转(切换上下文后跳到目标脚本的组号)。
    CrossScript {
        /// 目标脚本号。
        script_id: u16,
        /// 目标组号。
        target: usize,
    },
    /// 不跳(非 strict 下未命中等),顺序继续。
    NoJump,
    /// strict 挂起,携带原因。
    Halted(String),
}

/// VM 事件流(Trace / Golden Test 的数据源)。
#[derive(Debug, Clone, PartialEq)]
pub enum VmEvent {
    /// 一个组执行完毕。
    GroupExecuted {
        /// 组下标。
        pc: usize,
        /// YSCM 命令下标。
        command: u8,
        /// 求值结果(仅 IF 的条件等单值场景;多数命令无单一结果)。
        condition: Option<Value>,
    },
    /// 发生跳转。
    Jump {
        /// 起始组。
        from: usize,
        /// 目标组。
        to: usize,
        /// 类别。
        kind: JumpKind,
    },
    /// GOSUB 压帧。
    Call {
        /// GOSUB 组下标。
        from: usize,
        /// 目标组。
        to: usize,
    },
    /// TEXT:显示文本(FILE 参数为 SJIS 字节;LET/CLEAR 布尔位)。
    Text {
        /// 参数槽下标。
        pc: usize,
        /// FILE 参数(SJIS 原文)。
        file: Option<Vec<u8>>,
        /// LET 参数(=1 允许按 [r]/[c] 划段)。
        let_flag: bool,
        /// CLEAR 参数(=1 先清空)。
        clear_flag: bool,
    },
    /// CG:立绘/背景操作(ID,+ X/Y/Z 位置等关键参数)。
    Cg {
        /// 组下标。
        pc: usize,
        /// 发出命令的脚本号(yst%05d;素材请求源定位用,成果 75)。
        script_id: u16,
        /// ID(名称,若为字符串)。
        id: Option<String>,
        /// X/Y/Z 位置(若提供)。
        ///
        /// **勘误(2026-10-13,成果 122)**:旧实现要求 **X 与 Y 同时到场**才产出
        /// `position`,于是「只给 Z」的合法写法(手册示例
        /// `CG[ID=BG Z=3 FILE="ABC.bmp"]`)把 Z **丢掉** —— 实测本作所有层都变
        /// `z=0`,叠放次序与引擎不一致。现改为**逐槽独立**的 `x`/`y`/`z`/`a`/`mode`,
        /// `position` 保留为「三轴齐备」的便捷投影(探针/诊断用)。
        position: Option<(i64, i64, i64)>,
        /// X 槽(仅当本次命令给了 X 时 `Some`;引擎:未给槽保持原值)。
        x: Option<i64>,
        /// Y 槽(同上)。
        y: Option<i64>,
        /// **Z 槽 = 层优先级**(手册;越大越靠前)。仅当本次给了 Z 时 `Some`。
        z: Option<i64>,
        /// **A 槽 = 不透明度 0〜256**(手册;256 = 全不透明)。仅当给了 A 时 `Some`。
        a: Option<i64>,
        /// **MODE 槽 = 描画模式 0〜18**(手册:0 通常/1 加算/2 减算/3 乘算/…)。
        /// 仅当给了 MODE 时 `Some`。
        mode: Option<i64>,
        /// 其他已提供参数计数(按 B0 下标)。
        param_count: usize,
        /// FILE 槽(46;带图像装载的 CG 命令非空;渲染驱动按此加载资源;
        /// 无 FILE = 位置/状态更新,沿用手头资源)。
        file: Option<Vec<u8>>,
        /// **E = 显隐**(本作槽 2;官方手册语义)。
        /// `Some(0)` = 关(不显示)、`Some(非零)` = 开、`None` = 本次未指定
        /// (引擎:未给的槽保持原值)。播放器据此判显隐(成果 121)。
        e: Option<i64>,
        /// **T 槽 = 过渡进行度 0〜256**(手册 `cg/cg.html`;本作槽 37)。
        /// 仅当本次给了 T 时 `Some`。
        t: Option<i64>,
        /// **TID 槽 = 过渡图案图层 ID**(手册;本作槽 38)。
        /// 图层按该图层的图案逐像素显现;仅当给了 TID 时 `Some`。
        tid: Option<Vec<u8>>,
        /// **TA 槽 = 过渡精度 0〜256**(手册:边缘软化宽度;本作槽 40)。
        ta: Option<i64>,
        /// **MID 槽 = 外部遮罩图层 ID**(手册:`cg.html` 遮罩示例;本作槽 35)。
        mid: Option<Vec<u8>>,
        /// **SX/SY 槽 = 图层组尺寸**(手册:「レイヤセットの幅(Ｘ)/高さ(Ｙ)を
        /// 指定 ※レイヤセットに対してのみ有効」;本作槽 9/10)。
        ///
        /// 这是脚本**声明「此 ID 是图层组」的唯一方式** —— 组没有 FILE,
        /// 唯一标识就是这对尺寸槽(`CG[ID="ABC/" Z=10 SX=400 SY=300]`,
        /// 手册示例[8])。渲染侧据此把该层当组处理:子层先合成进离屏目标,
        /// 组的 `A`/`MODE`/`E` 作用于合成结果(REA
        /// `docs/engine/render/02-layer-model.md` §2)。
        sx: Option<i64>,
        /// SY 槽(同上)。
        sy: Option<i64>,
        /// **`TEX` 槽 = テクスチャ化**(官方 `es_Sprite.yst` 高频:
        /// `CG[ID=… TEX=@_DXMODE]` = 「把该层转成可被合成引用的纹理」)。
        /// 精灵链的 `$LIDBUF` 一族全靠它;此前**未带出**,下游无从区分
        /// 「缓冲层」与「直接上屏的显示层」(成果 140)。
        tex: Option<i64>,
        /// **`DXBUF` 槽 = 離屏バッファ**(手册参数族;语义 Likely)。
        dxbuf: Option<i64>,
        /// **`DXDRAW` 槽 = 直接描画**(手册参数族;语义 Likely)。
        dxdraw: Option<i64>,
    },
    /// SOUND:音频操作（完整 14 参数，语义见官方手册 `sound/sound.html`）。
    ///
    /// **成果 162**：此前只带出 `ID/FILE/PLAY` 三个槽，其余参数在 VM 层
    /// 被丢弃，导致音频后端无法实现淡变/循环/音量/声像/音高。现全部保留，
    /// 槽位按**参数名**解析（版本漂移安全）。
    Sound {
        /// 组下标。
        pc: usize,
        /// `ID`：声层键。
        id: Option<String>,
        /// `E`：声层有效/静音（0/1）。
        enabled: Option<i64>,
        /// `FILE`：逻辑文件名（**CP932 原始字节**，可省略扩展名）。
        ///
        /// 与 `Cg.file` 同一约定：语料实测声音名含日文（如
        /// `voice/琴音…`），任何 `from_utf8_lossy` 都会毁掉名字。
        file: Option<Vec<u8>>,
        /// `PLAY`：1 播放 / 0 停止。
        play: Option<i64>,
        /// `PAUSE`：1 暂停 / 0 恢复。
        pause: Option<i64>,
        /// `FADE`：淡变时长（毫秒）。
        fade_ms: Option<i64>,
        /// `START`：首次播放起点（毫秒，可负）。
        start_ms: Option<i64>,
        /// `RESTART`：循环起点（毫秒，可负）。
        restart_ms: Option<i64>,
        /// `END`：终点/循环点（毫秒）。
        end_ms: Option<i64>,
        /// `LOOP`：0 无限 / N 总次数。
        loop_count: Option<i64>,
        /// `VOLUME`：0～256。
        volume: Option<i64>,
        /// `PAN`：0～512。
        pan: Option<i64>,
        /// `PITCH`：0～512。
        pitch: Option<i64>,
        /// `SPEED`：v494 处理器不读（Unknown）。
        speed: Option<i64>,
        /// 已求值参数计数。
        param_count: usize,
    },
    /// `SAVEEND[]`：清空 `SAVE` 预留保存的数据(手册 `file/saveend.html`)。
    SaveEnd {
        /// 组下标。
        pc: usize,
        /// 已求值参数摘要(该命令无参数,恒空)。
        evaluated: Vec<(u8, String)>,
    },
    /// `SOUNDEND[ID]`：删除声音图层(手册 `sound/soundend.html`)。
    SoundEnd {
        /// 组下标。
        pc: usize,
        /// 被删除的图层 ID(手册:唯一参数)。
        id: Option<String>,
        /// 已求值参数摘要。
        evaluated: Vec<(u8, String)>,
    },
    /// `ERROR[NO=… COM_CG=… …]`：**错误输出级别配置**（不是错误触发）。
    ///
    /// 引擎处理体（v494 `0x4411f8`）只把参数值写进进程级错误号/节流位，
    /// 不改脚本流；`NO` 是错误号，其余 `COM_*` 按命令名设级别。
    ErrorConfig {
        /// 组下标。
        pc: usize,
        /// `NO` 参数（错误号；未给 = `None`）。
        no: Option<i64>,
        /// 已求值的全部参数摘要（槽号 → 值）。
        evaluated: Vec<(u8, String)>,
    },
    /// `EMOTE[...]`：E-mote 角色动画控制。
    ///
    /// `runtime=false` = E-mote 运行时未装载，引擎侧**整条命令空操作**
    /// （见 `exec::emote` 模块文档的取证）；本内核同款。
    Emote {
        /// 组下标。
        pc: usize,
        /// E-mote 运行时是否在位（决定这是空操作还是显式缺失）。
        runtime: bool,
        /// 已求值的全部参数摘要（槽号 → 值）。
        evaluated: Vec<(u8, String)>,
    },
    /// `EMOTEINFO[...]`：引擎侧共享 stub（空实现，连 `LET` 都不写）。
    EmoteInfo {
        /// 组下标。
        pc: usize,
        /// 已求值的全部参数摘要（槽号 → 值）。
        evaluated: Vec<(u8, String)>,
    },
    /// **标签未命中**（`GO`/`GOSUB` 的标签窗求值成功但查表失败）。
    ///
    /// 与 [`Self::Unsupported`] 分开：命令**已实现**，只是这条脚本引用了
    /// 本游戏不存在的标签（实测来自 v555 时代的脚本残留）。引擎侧同样查不到
    /// （`0x4459ed` 返回负 → `0x445ada` 报错并 `return -1` 让出），
    /// 因此不是「本内核未实现」，不该占用致命错误窗。
    LabelMiss {
        /// 组下标。
        pc: usize,
        /// 命令名（`GO` / `GOSUB`）。
        command: String,
        /// 求值得到的标签名（原始字节 → lossy 显示）。
        label: String,
        /// 是否本内核在该处跳过并继续（当前恒 `true`）。
        skipped: bool,
    },
    /// `TASKINFO[ID=… LET=… EXIST=1 …]`：任务状态查询。
    TaskInfo {
        /// 组下标。
        pc: usize,
        /// `ID` 参数（任务名；`None` = 当前任务）。
        task: Option<String>,
        /// 已作答字段（字段名 → 值摘要）。
        answered: Vec<(String, String)>,
        /// 被请求但本内核未实现的字段名。
        missing: Vec<String>,
        /// 已求值的全部参数摘要。
        evaluated: Vec<(u8, String)>,
    },
    /// `SYSTEMINFO[SCRIPTFILE=… SCRIPTNUM=… TEXTNUM=… EMOTE=… NO=n LET=@v]`：
    /// 引擎自述信息查询（真值来自 `yst_list.ybn`，见 `exec::sysinfo`）。
    SystemInfo {
        /// 组下标。
        pc: usize,
        /// `NO` 参数（1 基脚本序号；未给 = `None`）。
        no: Option<i64>,
        /// 实际作答的字段（字段名 → 值摘要；按引擎求值次序）。
        answered: Vec<(String, String)>,
        /// 已求值的全部参数摘要。
        evaluated: Vec<(u8, String)>,
    },
    /// `SYSTEM[SCREENSAVER=… BACKGROUNDEXEC=… SX=… SY=… TIMECOUNT=… …]`：
    /// 系统设置（屏保抑制 / 后台执行 / 屏幕尺寸 / 计时器）。
    ///
    /// 引擎处理体 v494 `0x4500d0` 逐槽取证，见 `exec::system` 模块文档。
    SystemConfig {
        /// 组下标。
        pc: usize,
        /// 已求值的全部参数摘要（槽号 → 值）。
        evaluated: Vec<(u8, String)>,
    },
    /// `FILEACT[FILE=… FILE2=… FOLDER=… CREATE=… COPY=… DELETE=… …]`：
    /// **文件操作意图**（手册 `file/fileact.html`）。
    ///
    /// VM **不碰平台文件系统**（`RuntimeApi` 是唯一边界）—— 这里只把命令
    /// 参数解析成结构化意图，由播放器按自身根目录与安全策略执行。
    FileAct {
        /// 组下标。
        pc: usize,
        /// `FILE`：目标文件名。
        file: Option<String>,
        /// `FILE2`：复制目的地。
        file2: Option<String>,
        /// `FOLDER`：空目录删除/创建的目标。
        folder: Option<String>,
        /// `CREATE`：非 0 = 建立目录。
        create: Option<i64>,
        /// `COPY`：非 0 = 复制。
        copy: Option<i64>,
        /// `DELETE`：非 0 = 删除（存档数据或空目录）。
        delete: Option<i64>,
        /// `EXEC`：非 0 = 执行文件。
        exec: Option<i64>,
        /// `EXEC2`：执行方式（引擎侧第二分支）。
        exec2: Option<i64>,
        /// `PARAM`：传给被执行程序的参数。
        param: Option<String>,
        /// `SET`：PNG 压缩级别 / JPG 品质。
        set: Option<i64>,
        /// `ID`：导出为图片时的图层 ID。
        id: Option<String>,
        /// `BMP`。
        bmp: Option<i64>,
        /// `PNG`。
        png: Option<i64>,
        /// `JPG`。
        jpg: Option<i64>,
        /// `CGCASH`：登记进 CG 缓存表。
        cg_cache: Option<i64>,
        /// `SOUNDCASH`：登记进音源缓存表。
        sound_cache: Option<i64>,
    },
    /// **媒体模式**（`MOVIE`/`FLASH` 播放期间屏蔽其他图层；T23b,2026-10-16）。
    ///
    /// 引擎侧是**显示层状态**（非脚本时序）：手册 `movie/movie.html` 注[3]
    /// 「停止しても、MOVIEEND 命令でムービー再生を終了しない限り他のレイヤは
    /// 一切表示されません」+ `movieend.html`「終了することにより、他のレイヤが
    /// 見えるようになります」。
    ///
    /// 发事件方（VM）只做**置/清**：`MOVIE[PLAY=1]`/`FLASH[… PLAY=1]` → `on=true`，
    /// `MOVIEEND`/`FLASHEND` → `on=false`；消费方（场景）据此屏蔽其他图层。
    /// 本内核无解码器 ⇒ 媒体画面本身不绘制（显式 Unknown，不近似）。
    MediaMode {
        /// 组下标。
        pc: usize,
        /// `true` = 进入媒体模式（屏蔽其他层）；`false` = 退出。
        on: bool,
        /// 触发命令名（`MOVIE`/`FLASH`/`MOVIEEND`/`FLASHEND`；诊断用）。
        command: String,
    },
    /// VARACT/VARINFO 只读查询结果(字符串长度/维度/类型查询等)。
    /// 只读槽求值记录;**写回类槽(赋值/裁剪/大小写/PUSH/POP)走 Unsupported**——
    /// 未逆向写回目标前不猜(铁律)。
    VarQuery {
        /// 组下标。
        pc: usize,
        /// 命令(0x66 VARACT / 0x67 VARINFO)。
        command: u8,
        /// 已求值槽: (参数下标B0, 值摘要)。
        evaluated: Vec<(u8, String)>,
    },
    /// 声明类命令(INT/FLT/STR/G_*/S_*/F_* 等):加载期数据,引擎运行器
    /// 用默认 stub,不执行。记录后(trace)继续,stict 下按未实现挂起。
    Declaration {
        /// 组下标。
        pc: usize,
        /// 命令类型(YSCM 下标)。
        command: u8,
        /// 参数窗口数。
        windows: u8,
    },
    /// CGACT:CGM 图层操作(合成/变换等;71 参,运行期图形管线,只记录已求值参数摘要)。
    CgAct {
        /// 组下标。
        pc: usize,
        /// ID(B0=0,字符串,若给定)。
        id: Option<String>,
        /// 已求值参数摘要:(B0 槽, 值摘要)。
        evaluated: Vec<(u8, String)>,
        /// **效果槽(成果 133)**:除 ID/X/Y 外,CGACT 的 70 个图形管线参数。
        ///
        /// 引擎侧 `CMDH_0042607c`(12466B)是**运行期图形管线合成**命令:
        /// 官方手册关键字段 = `COPY`/`COPY2`(把源层合成结果复制到目标,
        /// 手册「スクリーンショット機能」)、`TEXCOPY`/`TEXSSCOPY`(纹理拷贝,
        /// 片头 LOGO 的 `SPL*` 层内容就靠它从 `SPLBUF*` 拷过来)、
        /// `SIZE*`/`ROTATION*`/`BLUR*`/`SEPIA`/`MONO`/`MOSAIC`/`WHIRLPOOL` 等。
        ///
        /// **等级 Likely**:手册给了每个关键字的名字与语义,但"槽号 → 关键字"
        /// 的绑定来自游戏自身 YSCM 参数表(逐版本漂移,故按名解析),不是
        /// 逐条反编译核对的。此处**只如实带出已求值的槽**,
        /// 由渲染/播放层决定实现哪些;不猜未实现的效果。
        effects: Vec<(u8, String, EffectValue)>,
    },
    /// CGINFO:CG 图层属性查询(引擎 CMDH_0043b084)。
    /// 无图形后端 ⇒ CG 必然不存在 ⇒ 引擎「不存在」路径(结果缓冲清零,LET=0)。
    CgInfo {
        /// 组下标。
        pc: usize,
        /// ID(槽 0,CG 名)。
        id: Option<String>,
        /// 已求值参数摘要((槽, 值摘要);255 = 写回目标记录)。
        evaluated: Vec<(u8, String)>,
    },
    /// LOAD:YSSD 系统数据装载(FILE + DNO + 写回目标;引擎 CMDH_00444648)。
    /// 装载路径 = 松散文件 + SNP 解压 + `FUN_0044564d` 写回(见 `load_yssd`);
    /// 事件保留参数摘要供 trace 对拍。
    Load {
        /// 组下标。
        pc: usize,
        /// FILE 路径(SJIS 原文)。
        file: Option<Vec<u8>>,
        /// 已求值参数摘要。
        evaluated: Vec<(u8, String)>,
    },
    /// SAVE:存档数据位图标记(DNO=存档号,SET=目标引用;引擎 CMDH_00451838)。
    /// 语料 298 组形态 = [DNO, SET 引用](位图置位/清零)+ [FILE](仅记录);
    /// 引擎按描述符类型把 INT 置 1/0、FLT 置 1.0/0.0。本实现按目标类型写值,
    /// 不落盘(存档文件格式 Unknown)。
    Save {
        /// 组下标。
        pc: usize,
        /// FILE 参数(若有;只记录)。
        file: Option<Vec<u8>>,
        /// DNO 存档号(若给定)。
        dno: Option<i64>,
        /// SET 目标摘要(写位图的变量)。
        set_target: Option<String>,
        /// 已求值参数摘要。
        evaluated: Vec<(u8, String)>,
    },
    /// CGEND:CG 显示结束通知(ID 字符串;引擎 LAB_0043ad14)。
    /// 无图形后端 → 事件化记录,不触碰图形状态。
    CgEnd {
        /// 组下标。
        pc: usize,
        /// ID(字符串)。
        id: Option<String>,
        /// 已求值参数摘要。
        evaluated: Vec<(u8, String)>,
    },
    /// 跨脚本上下文切换。
    ScriptSwitch {
        /// 源脚本号(0 表示未知/未初始化 —— 仅诊断)。
        from_script: u16,
        /// 目标脚本号。
        to_script: u16,
    },
    /// 未实现/不支持的命令(语义未逆向或跨脚本等)。
    /// 事件保留现场;strict 模式随后挂起。
    Unsupported {
        /// 组下标。
        pc: usize,
        /// YSCM 命令下标。
        command: u8,
        /// 原因描述。
        reason: String,
    },
    /// 后端子系统配置/查询族(P2):FONTINFO/MATH/FILEINFO/FILEACT/WINDOWINFO/
    /// MOUSE/FONT/SYSTEM/ERROR/FPS/INPUT。引擎处理器做子系统状态操作
    /// (字体层/文件/输入模式/数学函数),无后端 → 事件化记录已求值参数。
    Subsystem {
        /// 组下标。
        pc: usize,
        /// YSCM 命令下标。
        command: u8,
        /// 已求值参数摘要 (B0 槽, 值)。
        evaluated: Vec<(u8, String)>,
    },
}

/// 事件 → JSON 行(P1 引擎真值对拍的统一 schema)。
///
/// 手工序列化(非 serde):`Value::Str` 为 SJIS 原始字节 → 十六进制避免编码歧义;
/// `Value::Float` 用 f64 位模式保证 NaN/精度稳定;`Unsupported` 的 reason 做
/// 非 ASCII 图形字符转义保证字节稳定。golden 快照与 `vm_trace` 导出共用本函数。
pub fn event_json(e: &VmEvent) -> String {
    /// JSON 字符串转义(引擎侧 CG 名等含字面 `"`,如 `"CGS"100`;
    /// 不转义会产出非法 JSON 行 —— P2 对拍实测发现)。
    fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }
    /// (槽, 值摘要) 对 → JSON 数组(显式构造;Debug 格式化对非 ASCII 产出
    /// `\u{...}` 非 JSON 转义,不可用)。
    fn ev_arr(evaluated: &[(u8, String)]) -> String {
        let items: Vec<String> = evaluated
            .iter()
            .map(|(b, v)| format!("\"{}:{}\"", b, json_escape(v)))
            .collect();
        format!("[{}]", items.join(","))
    }
    fn val_json(v: &yuris_value::Value) -> String {
        match v {
            yuris_value::Value::Int(i) => format!(r#"{{"int":{i}}}"#),
            yuris_value::Value::Float(f) => {
                // 用位模式保证 NaN/精度稳定
                format!(r#"{{"f64bits":{}}}"#, f.to_bits())
            }
            yuris_value::Value::Str(b) => {
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                format!(r#"{{"str_hex":"{hex}"}}"#)
            }
        }
    }
    match e {
        VmEvent::GroupExecuted { pc, command, condition } => {
            let cond = condition
                .as_ref()
                .map(val_json)
                .unwrap_or_else(|| "null".to_string());
            format!(
                r#"{{"ev":"group","pc":{pc},"cmd":{command},"cond":{cond}}}"#
            )
        }
        VmEvent::Jump { from, to, kind } => {
            let k = match kind {
                JumpKind::Go => "go",
                JumpKind::IfFalse => "if_false",
                JumpKind::Else => "else",
                JumpKind::Return => "return",
                JumpKind::LoopBreak => "loop_break",
                JumpKind::LoopContinue => "loop_continue",
            };
            format!(r#"{{"ev":"jump","from":{from},"to":{to},"kind":"{k}"}}"#)
        }
        VmEvent::Call { from, to } => {
            format!(r#"{{"ev":"call","from":{from},"to":{to}}}"#)
        }
        VmEvent::Text { pc, file, let_flag, clear_flag } => {
            let f = file.as_ref().map(|b| {
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                format!(r#"{{"hex":"{hex}"}}"#)
            }).unwrap_or_else(|| "null".to_string());
            format!(
                r#"{{"ev":"text","pc":{pc},"file":{f},"let":{let_flag},"clear":{clear_flag}}}"#
            )
        }
        VmEvent::Cg {
            pc, script_id, id, position, param_count, file, e, x, y, z, a, mode, t, tid, ta, mid,
            sx, sy, tex, dxbuf, dxdraw,
        } => {
            let idj = id.clone()
                .map(|i| "{\"s\":\"".to_string() + &json_escape(&i) + "\"}")
                .unwrap_or_else(|| "null".to_string());
            let posj = position.map(|(x, y, z)| format!(r#"{{"x":{x},"y":{y},"z":{z}}}"#)).unwrap_or_else(|| "null".to_string());
            let fj = file.clone().map(|f| {
                let hex: Vec<String> = f.iter().map(|b| format!("{b:02x}")).collect();
                format!(r#"{{"hex":"{}"}}"#, hex.join(""))
            }).unwrap_or_else(|| "null".to_string());
            let ej = e.map(|n| n.to_string()).unwrap_or_else(|| "null".to_string());
            // 逐槽字段(成果 122):只给 Z 的调用也能被消费方看到。
            let oj = |v: &Option<i64>| v.map(|n| n.to_string()).unwrap_or_else(|| "null".into());
            // 过渡/遮罩的图层名(成果 125):与 `id` 同形的 JSON 编码。
            let namej = |b: &Option<Vec<u8>>| {
                b.clone()
                    .map(|b| {
                        "{\"s\":\"".to_string()
                            + &json_escape(&String::from_utf8_lossy(&b))
                            + "\"}"
                    })
                    .unwrap_or_else(|| "null".to_string())
            };
            let tj = namej(tid);
            let mj = namej(mid);
            format!(
                r#"{{"ev":"cg","sid":{script_id},"pc":{pc},"id":{idj},"pos":{posj},"n":{param_count},"file":{fj},"e":{ej},"x":{},"y":{},"z":{},"a":{},"mode":{},"t":{},"tid":{tj},"ta":{},"mid":{mj},"sx":{},"sy":{},"tex":{},"dxbuf":{},"dxdraw":{}}}"#,
                oj(x),
                oj(y),
                oj(z),
                oj(a),
                oj(mode),
                oj(t),
                oj(ta),
                oj(sx),
                oj(sy),
                oj(tex),
                oj(dxbuf),
                oj(dxdraw)
            )
        }
        VmEvent::SaveEnd { pc, evaluated } => {
            format!(
                r#"{{"ev":"saveend","pc":{pc},"args":[{}]}}"#,
                ev_arr(evaluated)
            )
        }
        VmEvent::SoundEnd { pc, id, evaluated } => {
            format!(
                r#"{{"ev":"soundend","pc":{pc},"id":"{}","args":[{}]}}"#,
                id.as_deref().map(json_escape).unwrap_or_default(),
                ev_arr(evaluated)
            )
        }
        VmEvent::ErrorConfig { pc, no, evaluated } => {
            format!(
                r#"{{"ev":"errorconfig","pc":{pc},"no":{},"args":[{}]}}"#,
                no.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                ev_arr(evaluated)
            )
        }
        VmEvent::Emote {
            pc,
            runtime,
            evaluated,
        } => {
            format!(
                r#"{{"ev":"emote","pc":{pc},"runtime":{},"args":[{}]}}"#,
                if *runtime { 1 } else { 0 },
                ev_arr(evaluated)
            )
        }
        VmEvent::EmoteInfo { pc, evaluated } => {
            format!(
                r#"{{"ev":"emoteinfo","pc":{pc},"args":[{}]}}"#,
                ev_arr(evaluated)
            )
        }
        VmEvent::LabelMiss {
            pc,
            command,
            label,
            skipped,
        } => format!(
            r#"{{"ev":"labelmiss","pc":{pc},"cmd":"{}","label":"{}","skipped":{}}}"#,
            json_escape(command),
            json_escape(label),
            if *skipped { 1 } else { 0 }
        ),
        VmEvent::TaskInfo {
            pc,
            task,
            answered,
            missing,
            evaluated,
        } => {
            let ans: Vec<String> = answered
                .iter()
                .map(|(k, v)| format!(r#""{}":{}"#, json_escape(k), json_escape(v)))
                .collect();
            let miss: Vec<String> = missing.iter().map(|m| format!(r#""{}""#, json_escape(m))).collect();
            format!(
                r#"{{"ev":"taskinfo","pc":{pc},"task":"{}","answered":{{{}}},"missing":[{}],"args":[{}]}}"#,
                task.as_deref().map(json_escape).unwrap_or_default(),
                ans.join(","),
                miss.join(","),
                ev_arr(evaluated)
            )
        }
        VmEvent::SystemInfo {
            pc,
            no,
            answered,
            evaluated,
        } => {
            let ans: Vec<String> = answered
                .iter()
                .map(|(k, v)| format!(r#""{}":{}"#, json_escape(k), json_escape(v)))
                .collect();
            format!(
                r#"{{"ev":"systeminfo","pc":{pc},"no":{},"answered":{{{}}},"args":[{}]}}"#,
                no.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                ans.join(","),
                ev_arr(evaluated)
            )
        }
        VmEvent::SystemConfig { pc, evaluated } => {
            format!(
                r#"{{"ev":"systemconfig","pc":{pc},"args":[{}]}}"#,
                ev_arr(evaluated)
            )
        }
        VmEvent::FileAct {
            pc,
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
        } => {
            fn s(v: &Option<String>) -> String {
                v.as_deref().map(json_escape).unwrap_or_default()
            }
            fn n(v: &Option<i64>) -> String {
                v.map(|x| x.to_string()).unwrap_or_else(|| "null".into())
            }
            format!(
                r#"{{"ev":"fileact","pc":{pc},"file":"{}","file2":"{}","folder":"{}","create":{},"copy":{},"delete":{},"exec":{},"exec2":{},"param":"{}","set":{},"id":"{}","bmp":{},"png":{},"jpg":{},"cgcache":{},"soundcache":{}}}"#,
                s(file),
                s(file2),
                s(folder),
                n(create),
                n(copy),
                n(delete),
                n(exec),
                n(exec2),
                s(param),
                n(set),
                s(id),
                n(bmp),
                n(png),
                n(jpg),
                n(cg_cache),
                n(sound_cache)
            )
        }
        VmEvent::MediaMode { pc, on, command } => {
            format!(
                r#"{{"ev":"mediamode","pc":{pc},"on":{},"cmd":"{}"}}"#,
                if *on { 1 } else { 0 },
                json_escape(command)
            )
        }
        VmEvent::Sound {
            pc,
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
            param_count,
        } => {
            let idj = id
                .clone()
                .map(|i| "{\"s\":\"".to_string() + &json_escape(&i) + "\"}")
                .unwrap_or_else(|| "null".to_string());
            let fj = file
                .as_ref()
                .map(|f| "{\"s\":\"".to_string() + &json_escape(&String::from_utf8_lossy(f)) + "\"}")
                .unwrap_or_else(|| "null".to_string());
            let num = |v: &Option<i64>| {
                v.map(|p| p.to_string())
                    .unwrap_or_else(|| "null".to_string())
            };
            format!(
                r#"{{"ev":"sound","pc":{pc},"id":{idj},"file":{fj},"play":{},"pause":{},"e":{},"fade":{},"start":{},"restart":{},"end":{},"loop":{},"vol":{},"pan":{},"pitch":{},"speed":{},"n":{param_count}}}"#,
                num(play),
                num(pause),
                num(enabled),
                num(fade_ms),
                num(start_ms),
                num(restart_ms),
                num(end_ms),
                num(loop_count),
                num(volume),
                num(pan),
                num(pitch),
                num(speed)
            )
        }
        VmEvent::ScriptSwitch { from_script, to_script } => {
            format!(r#"{{"ev":"switch","from":{from_script},"to":{to_script}}}"#)
        }
        VmEvent::Declaration { pc, command, windows } => {
            format!(r#"{{"ev":"decl","pc":{pc},"cmd":{command},"w":{windows}}}"#)
        }
        VmEvent::CgAct { pc, id, evaluated, effects } => {
            let idj = id.clone().map(|s| json_escape(&s)).unwrap_or_else(|| "null".to_string());
            let ev = ev_arr(evaluated);
            // 效果槽(trace 保真:名字 + 类型化值;片头 LOGO 的 COPY/TEXSSCOPY
            // 就从这里流出,供渲染/播放层实现)。
            let fx: Vec<String> = effects
                .iter()
                .map(|(slot, name, v)| {
                    let val = match v {
                        EffectValue::Int(n) => n.to_string(),
                        EffectValue::Float(f) => format!("{f}"),
                        EffectValue::Str(t) => format!("\"{}\"", json_escape(t)),
                    };
                    format!("{{\"slot\":{slot},\"name\":\"{}\",\"val\":{val}}}", json_escape(name))
                })
                .collect();
            format!(
                r#"{{"ev":"cgact","pc":{pc},"id":"{idj}","ev_":{ev},"fx":[{fx}]}}"#,
                fx = fx.join(",")
            )
        }
        VmEvent::CgInfo { pc, id, evaluated } => {
            let idj = id.clone().map(|s| json_escape(&s)).unwrap_or_else(|| "null".to_string());
            let ev: Vec<String> = evaluated.iter().map(|(b, v)| format!("{b}:{}", json_escape(v))).collect();
            let ev = ev_arr(&evaluated);
            format!(r#"{{"ev":"cginfo","pc":{pc},"id":"{idj}","ev_":{ev}}}"#)
        }
        VmEvent::Load { pc, file, evaluated } => {
            let fj = file.as_ref().map(|b| {
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                hex
            }).unwrap_or_else(|| "null".to_string());
            let ev = ev_arr(&evaluated);
            format!(r#"{{"ev":"load","pc":{pc},"file_hex":"{fj}","ev_":{ev}}}"#)
        }
        VmEvent::VarQuery { pc, command, evaluated } => {
            let ev: Vec<String> = evaluated.iter().map(|(b,v)| format!("{b}:{v}")).collect();
            let ev = ev_arr(&evaluated);
            format!(r#"{{"ev":"varquery","pc":{pc},"cmd":{command},"ev_":{ev}}}"#)
        }
        VmEvent::Save { pc, file, dno, set_target, evaluated } => {
            let fj = file.as_ref().map(|b| {
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                format!(r#"{{"hex":"{hex}"}}"#)
            }).unwrap_or_else(|| "null".to_string());
            let dj = dno.map(|d| d.to_string()).unwrap_or_else(|| "null".to_string());
            let sj = set_target.clone()
                .map(|s| format!(r#""{}""#, json_escape(&s)))
                .unwrap_or_else(|| "null".to_string());
            let ev = ev_arr(&evaluated);
            format!(
                r#"{{"ev":"save","pc":{pc},"file":{fj},"dno":{dj},"set":{sj},"ev_":{ev}}}"#
            )
        }
        VmEvent::CgEnd { pc, id, evaluated } => {
            let idj = id.clone().map(|s| json_escape(&s)).unwrap_or_else(|| "null".to_string());
            let ev = ev_arr(&evaluated);
            format!(r#"{{"ev":"cgend","pc":{pc},"id":"{idj}","ev_":{ev}}}"#)
        }
        VmEvent::Subsystem { pc, command, evaluated } => {
            let ev = ev_arr(&evaluated);
            format!(r#"{{"ev":"sub","pc":{pc},"cmd":{command},"ev_":{ev}}}"#)
        }
        VmEvent::Unsupported { pc, command, reason } => {
            // reason 含动态文本 → 截断转义,保持快照稳定
            let r: String = reason
                .chars()
                .map(|c| {
                    if c.is_ascii_graphic() || c == ' ' {
                        c.to_string()
                    } else {
                        format!("\\u{:04x}", c as u32)
                    }
                })
                .collect();
            format!(
                r#"{{"ev":"unsupported","pc":{pc},"cmd":{command},"reason":"{}"}}"#,
                json_escape(&r)
            )
        }
    }
}
