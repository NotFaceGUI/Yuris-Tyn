# 启动提示词 —— UI 宏族模块(`es.BT` / `es.FONT.DRAW` / `es.TX`)

> 用法:把下面 `===== 提示词开始 =====` 到 `===== 提示词结束 =====` 之间的全文
> 原样粘贴给新会话的第一条消息。它自包含,不依赖任何先前对话。

===== 提示词开始 =====

# 任务:实现 YU-RIS 的 `es.BT` / `es.FONT.DRAW` / `es.TX` 三层脚本宏族,让游戏内 UI(按钮文字、消息窗内容、系统栏)真正显示并能被点击

## 0. 你面对的是什么

仓库:`<repo>`(YU-RIS 引擎的纯 Rust 兼容内核)。
当前分支只用于工作,不在主线。**所有改动留在工作区,禁止任何 git 写操作**。

这是一个**逆向 + 实现**任务,不是修 bug。上一阶段已经把一个显示链路的
表层问题全部修完(字号、z 档位、文本通道、调度器),现在剩下的是**引擎 UI
子系统级**的缺口。

### 已经确认坏掉的现象(用户实机)

1. 游戏标题画面正常(六钮齐全、有字)。
2. 点「最初からはじめる」进入后:
   - 背景、立绘**全部没有**;
   - 消息窗框、系统栏按钮(SAVE/LOAD/AUTO/SKIP/CONFIG…)的**文字是空的**;
   - 只剩一句提示文本和几个空按钮框;
   - 在这些空按钮上**点不动**(没有回退按钮、无法继续)。

## 1. 环境(必须按这个设,否则一片假失败)

```bash
cd /d/Art/ai/projects/Yuris-Tyn
export TEMP="<side-repo>/workbench/tmp"
export TMP="$TEMP"          # ← 必须单独一行;写成 TMP="$TEMP" 同行会拿到旧的 E:\Temp
```

- **`TMP` 必须单独 export**。Windows 的 `GetTempPath2` 优先读 `TMP`;
  同一行写 `TMP="$TEMP"` 时 `$TEMP` 尚未生效,会导致 6 个 `p5_load`
  用例乃至更多 PermissionDenied 假失败。
- **`python3` 是坏的**(Microsoft Store 存根,执行即退出码 49)。用 `python`。
- 样本游戏:`<side-repo>/workbench/games/<game-b>`
- 工具链:cargo/rustc 1.96.0,MSVC。
- 逆向工具(需要时):`<side-repo>/tools/{jdk-21,ghidra_12.1.4_PUBLIC}`;
  Ghidra headless 需显式设 `JAVA_HOME` / `GHIDRA_INSTALL_DIR` / `TEMP` / `TMP`,
  且 **Python 脚本在 headless 下不可用**(PyGhidra 未启动),Java 脚本又会
  因 OSGi bundle 失败 —— 目前该机器上 Ghidra 脚本路径**不可用**,
  只能用 REA MCP 或 `scripts/ghidra/` 之外的手段。REA 对未识别函数的
  地址会返回 `not_in_procedure`(例如 `WORD` 处理器 `0x458ae4`)。
  优先用**数据侧取证**(语料扫描、反汇编脚本)而不是反编译。

## 2. 必读(按顺序,不要跳)

1. `AGENTS.md` —— 四条铁律(GPL 单向、真值只来自原引擎/原数据、证据等级、
   `CONTEXT.md` 是术语权威)。
2. `PROGRESS.md` 的这几条(用 grep 定位):
   - 成果 164(官方帧循环真值)、165(任务调度器重建)
   - 成果 168(调度器接线 + 剧本通道并入 VM)
   - 成果 169 / 174(文字、z、UI 判定)
   - **成果 176 / 177 / 178**(本轮 UI 诊断的全部证据链,最关键)
3. `docs/engine/task-scheduler-rebuild.md`
4. `docs/engine/v494-handler-table.md`(命令序号表)
5. `crates/yuris-vm/src/task.rs`、`crates/yuris-vm/src/driver.rs`、
   `crates/yuris-vm/src/scenario.rs`

## 3. 当前代码状态(你接手时的基线)

`cargo test --workspace --no-fail-fast` = **435 passed / 0 failed**。
先跑一遍确认,再动代码。

已完成的架构改动(**不要回退**):

| 位置 | 现状 |
|---|---|
| `crates/yuris-vm/src/driver.rs` | `step_group`(单组执行唯一实现点)、`run`(旧契约/golden)、`run_frame_chain`(官方 vsync 链)、`impl TaskExecutor for GroupVm` |
| `crates/yuris-vm/src/task.rs` | 官方扁平任务链 + TASK 续帧 + `Effect::{SpawnTask,SetEnabled,FinishTask}`;任务名是**原始字节** |
| `crates/yuris-vm/src/scenario.rs` | 剧本运行器 + `TextChannel`(`push_word` 追加 / `commit_line` 提交 / `push_inline`) |
| `crates/yuris-player-core/src/lib.rs` | `tick` 走 `run_frame_chain`;逻辑时钟(`advance_frame_clock` / `anim_now`);`dispatch_ui_click`(新) |
| `crates/yuris-stage/src/lib.rs` | `hit_test(x,y,prefix,fallback_size)`;`TEXT_Z = 1500` |

已修的具体缺陷(供你理解语义,不要重犯):

- `WORD` 是**追加**进行缓冲、`RETURNCODE` 才提交(语料 28992/28299 ≈ 1.02 片段/行)。
- `FONT` 命令的 `SX/SY` **就是字号**(语料实测 12/12、14/15、29/30);
  已接入 `GroupVm::font_px` → `text_px_or_default` → `rasterize_text`。
- UI 层 z 兜底:`btn_` → 2010,其余 → 1000(实测档位带);判定输入必须用
  **FILE 路径**而不是层名,且 `BT.` 前缀也算 UI。
- 消息窗开窗:`E=None` **且带 FILE** = 新建/换图 = 显示(不是只有 `E=1` 才开)。
- 逻辑时钟:`now_ms = 上屏帧数 × 1000/fps`;粒度是**上屏帧**不是链内逻辑帧。
  `YURIS_WALL_CLOCK=1` 回退墙钟。
- 逐组执行事件 `VmEvent::GroupExecuted` 是诊断面:播放器 `set_group_events(false)`。
- `--shot` **不再屏蔽鼠标**(只有 `--bench` 屏蔽)。这条踩过坑,别改回去。

## 4. 问题定位(已经做完的部分,不要重复)

### 4.1 已排除(有实测依据,别再查这些)

| 假设 | 排除依据 |
|---|---|
| Unsupported 命令被跳过 | 新增 `GroupVm::log_unsupported_summary`,实机输出「无被跳过的命令」 |
| 字号 / 编码 | `FONT` 真值 29×30 已生效,`s127` 文本完整正确 |
| z 被压住 | `YURIS_TRACE_SCENE=1` 显示 21 层可见,z 均为 1000/1500 档 |
| 点击传不到 VM | `set_input_cursor` 每帧同步;`CGINFO` 语料 151 次 |
| `ONMOUSE` 未实现 | 全语料 **0 次**使用 `ONMOUSE` / `_MOUSE_L` —— 不是这条路径 |
| `--shot` 破坏输入 | 已修(`--shot` 不屏蔽鼠标) |

### 4.2 实际观测(用户点击进入后的真实日志)

```
MESW 与系统栏 21 层全部点亮:
  BT.OFFES.GAMEMAIN.TIP.MESW.{TX,A}
  BT.OFFES.GAMEMAIN.BTN.{SAVE,LOAD,QSAVE,QLOAD,AUTO,SKIP,LOG,CONFIG,PREVSEL,TITLE,ERASE}
  BT.NAES.GAMEMAIN.BTN.VOICE
但:没有任何一层是背景/立绘,且全域 0 个带 FILE 的 CG 事件。
```

### 4.3 决定性数字 —— 执行覆盖不足

| 脚本 | 总组数 | 实际执行的不同 pc |
|---|---|---|
| `s71`(游戏内 UI 编排) | **1882** | **173** |

`s71` 里系统栏/MESW 相关组 `g521 / g526 / g561 / g607 / g642 / g644`
**一次都没执行**。

### 4.4 状态机流转

`@1029`(场景状态机)实测:`0 → 100 → 2 → 3 → 4`,之后 `996 → 100`,
然后**停住**。从未到 1(标题态),从未进正篇。

`@1035`(标题按钮选择)全语料**只有一处写入**:`s142 g5` 置 **-1**。
`s58 g31` 用 `@1035 != -1` 判「已选」⇒ 无外部写入时永远不成立。
(已加 `GroupVm::set_title_choice` 供播放器写入,但真实点击是否走到它**未验证**。)

### 4.5 结论(你要做的事)

`es.BT` / `es.FONT.DRAW` / `es.TX` 三层宏族**没有在 VM 内跑通**:

1. **`es.BT.*`** —— 按钮对象 / 队列 / 态组切换
   (`es.BT.SET`、`es.BT.QUE.*`、`es.BT.ACTIVEGROUP`、`es.BT.NAME.SET`、
   `es.BT.PUSH`、`es.BT.ID.OFF.GET`)
2. **`es.FONT.DRAW.*`** —— 文字 → 图层
   (定义`es.BT.FONT.DRAW.REPLAY` 在 **s11 pc299–322**;
   `es.FONT.DRAW.SET` 在 s11 pc323)
3. **`es.TX.*` / `TX.GAIJI.BUF`** —— 文本缓冲 → CG
   (`s11 g347 / g350` 里 `CG[ID="TX.GAIJI.BUF" …]`)

它们靠这些变量串起来:
`@1937 @1938 @1939..@1950 @1975..@1978 @1110 @1113 @1277 @1279 @1286
@1291 @1295 @1296 @1345 @1413 @1416 @1417 @1421`

## 5. 从哪里下手(建议顺序,但以证据为准)

### 第 1 步:确认 BT 队列为什么一直空

- BT 分发器 = **`s0 g1092–1520`**(`es.BT.SET` 本体)。
  它在 `g1105` 调 `es.BT.QUE.EMPTYGET`,在 `g1133` 起循环处理 `@1277` 条作业。
- 观察点:`YURIS_TRACE_GROUPS=0` + `YURIS_TRACE_LET=<id>` 跟踪
  `@1277`(作业数,实测 1233)、`@1279`(按钮状态位)、`@1286`(按钮 ID 槽)。
- **关键问题**:`@1279[i] == 1` 是「按钮被按下」的判定(`s0 g1135`),
  这个 1 由谁写?如果全语料没有写入者,就说明**入队信号确实缺失**,
  需要由播放器侧提供(见第 3 步)。

### 第 2 步:确认 `es.FONT.DRAW.SET` 的绘制目标

- `s11 pc299–322` = `es.BT.FONT.DRAW.REPLAY`:遍历 `@1937` 个条目,
  逐条取 `@1939..@1950` + `$1975` 文本,调 `es.BT.ID.OFF.GET`
  再调 `es.FONT.DRAW.SET`,最后落 `TX.GAIJI.BUF`。
- 核对:`TX.GAIJI.BUF` 这个 CG 层有没有被创建?它的 FILE 是什么?
  在 Stage 里是否可见?(`YURIS_TRACE_CG=TX.GAIJI` + `YURIS_TRACE_SCENE=1`)

### 第 3 步:打通「鼠标 → BT 队列」

已知可用但尚未验证的接口:

- `Stage::hit_test(x, y, "BT.OFFES.TITLE.BTN.", (317,76))` —— 命中测试已实装。
- `PlayerCore::dispatch_ui_click(x, y)` —— 已实装,命中后调
  `GroupVm::set_title_choice(slot)`。
- **尚未验证**:真实鼠标点击时 `dispatch_ui_click` 有没有被调用
  (目前只在 `inject_click` 里调)。

建议:在 `Player::tick` 的真实点击分支里也调一次,并加日志确认命中。

### 第 4 步:补 `es.BT` 的对象模型

`es.BT` 的完整变量布局可从 `s0 g1092–1520` 反推。已知:

- `@1277` = 作业总数(1233)
- `@1277` 循环体 `g1109–1131` 每轮处理 8 条子项(`LOOP 8`)
- `@1286[i]` = 作业 ID / `@1278`/`@1279` = 状态位
- `$1183` = 名字表(`S_STR $1183`)
- `es.BT.PUSH.NO.SET` / `es.BT.END.NO.EXEC` = 队列序号管理

**注意**:`s0` 的 3054 组里有 3000+ 组是 `es.BT` 的宏展开,不要试图
逐条硬编码;要找的是**变量契约**(谁写、谁读、什么条件下推进)。

## 6. 工具与诊断开关(全部已实装)

```bash
# 反汇编指定脚本的组区间(名字 + 逐窗口指令)
cargo run --release -q -p yuris-vm --example tmp_s0_disasm -- <游戏目录> <script号> <起组> <止组>

# 按字节串找组号(定位含某宏名的组)
cargo run --release -q -p yuris-vm --example tmp_find_mesw -- <游戏目录> <script号> <字符串>

# 单组窗口全解(精确指令序列)
cargo run --release -q -p yuris-tools --example wdump -- <游戏目录> <script号> <组号> [组数]

# 某变量的全部读写点
cargo run --release -q -p yuris-tools --example whowrites -- <游戏目录> <var_id>

# 命令用量统计
cargo run --release -q -p yuris-tools --example cmdcount -- <游戏目录> <命令名...>

# 全封包 CP932 字节串扫描
cargo run --release -q -p yuris-tools --example cp932_sweep -- <游戏目录> <hex>

# 标签表查询
cargo run --release -q -p yuris-tools --example labels -- <游戏目录>

# 实机(可交互)
./target/release/yuris-cli.exe run "<游戏目录>" --lenient

# 实机 + 截图(注意:--shot 现在**不**屏蔽鼠标)
./target/release/yuris-cli.exe run "<游戏目录>" --lenient --shot 600 --shot-dir <目录>

# 性能批跑(这个会屏蔽鼠标)
./target/release/yuris-cli.exe run "<游戏目录>" --lenient --bench 10

# 无头注入点击(只走 inject_click,不写 VM 的 @1035)
./target/release/yuris-cli.exe run "<游戏目录>" --lenient --click 757,405 --click-frame 420
```

环境变量:

| 变量 | 作用 |
|---|---|
| `YURIS_TRACE_GROUPS=<1\|脚本号>` | 逐组执行流水(量极大,boot 单帧 85 万组) |
| `YURIS_TRACE_LET=<id,id>` | LET 写入流水(`@48` 循环计数等) |
| `YURIS_TRACE_WAIT=1` | WAIT 让出流水 |
| `YURIS_TRACE_CG=<子串\|1>` | CG 事件流水(含 FILE/E/Z/A/SX/SY) |
| `YURIS_TRACE_SCENE=1` | 每帧可见层 dump(名字/z/alpha) |
| `YURIS_TRACE_STAGE=1` | 层属性流水(z 槽消费链) |
| `YURIS_TRACE_TEXT=1` | 文本通道流水(来源 s/pc + 内容) |
| `YURIS_TRACE_FONT=1` | FONT 状态流水 |
| `YURIS_TRACE_CHAIN=1` | 单链 >5000 组时打一行(慢帧定位) |
| `YURIS_WATCH_VAR=<id>` | **仅 YSVR 应用**;运行期请用 `YURIS_TRACE_LET` |
| `YURIS_TEXT_PX=<px>` | 字号强制覆盖(诊断) |
| `YURIS_WALL_CLOCK=1` | 回退墙钟(默认逻辑时钟) |
| `YURIS_LIVE_CURSOR=1` | `--bench` 下也保留真实鼠标 |

## 7. 性能契约(**必须守住,不要"优化掉"**)

- 官方命令泵**无预算**(`FUN_00404388` 泵到让出)。`FRAME_GROUP_BUDGET`
  已 deprecated,**不要传回调度器**。
- 唯一允许的护栏 = `YURIS_TASK_PUMP_CAP`(显式配置才生效,缺省关)。
- boot 单个逻辑帧实测 **854,124 组**;解释器吞吐 ≈ 468k 组/s。
  已做的优化(别改回去):
  - 窗口切片用**调用级复用缓冲**(`windows_buf`);
  - 指令解码走 `decode_window_into` + `insn_buf`(稳态零分配);
  - `eval_window_condition` 零拷贝借用取窗;
  - 消费侧用 `take_events()`(mem::take)而不是 `to_vec()`;
  - 播放器 `set_group_events(false)`。
- **任何预算/限速类开关缺省必须关闭**(曾经把任务预算缺省设成 2ms,
  直接导致整机卡顿)。

## 8. 硬性约束(违反即作废)

1. **禁止任何 git 写操作**:`commit` / `push` / `tag` / `branch` / `switch` /
   `checkout <分支>` / `merge` / `rebase` / `stash` / `cherry-pick`;
   以及会丢工作区的 `reset` / `restore` / `clean` / `rm --cached`。
   所有改动留在工作区。
2. **真值只能来自原引擎/原数据**。没有证据的行为一律显式
   `Error::Unimplemented`,禁止"看起来应该是这样"。
3. 证据分四级:`Confirmed`(样本实测+可复现)/ `Likely`(间接证据,
   实现须加 `// UNVERIFIED` 并配对照测试)/ `Hypothesis`(禁止进核心路径)/
   `Unknown`(保持未实现)。
4. **不要在 stage / player 层加"某作品特判"或表现层白名单**来遮语义问题。
5. `CONTEXT.md` 是术语权威:命令 ≠ opcode;窗口 ≠ 参数槽;
   共享池 = content‖part4;对拍 ≠ diff/golden 比对。
6. 改行为前先查 `PROGRESS.md` 有没有相关成果条目;**勘误要留痕**
   (旧结论 / 更正内容 / 发现方式),不得静默改旧结论。

## 9. 交付物

每完成一项,在 `PROGRESS.md` 追加「**成果 N**」,必须包含:

- 日期
- 结论(一句话)
- **取证链**:具体命令、原始输出片段、地址 / 组号 / 变量 id
- 改动文件清单
- **验证方式**(可直接复制的命令 + 原始结果)
- 残余(按优先级)

格式参考成果 168 / 174 / 178。

## 10. 验收标准

必须全部满足才算完成:

1. `cargo test --workspace --no-fail-fast` **全绿**(接手时 435 passed / 0 failed)。
2. 实机点「最初からはじめる」后:
   - 背景与立绘出现(**有带 FILE 的 CG 事件**);
   - 消息窗内有文字;
   - 系统栏按钮有文字;
   - 点击这些按钮能被响应(至少「回退/CONFIG」之一可点动)。
3. 同输入同输出:`--shot 200` / `--shot 400` 三跑 SHA 一致
   (注意:只有 `--bench` 会屏蔽鼠标,`--shot` 不会 —— 截 SHA 时用
   `--bench` 或固定 `--click` 序列)。
4. 性能不回退:boot 首帧组数仍为 **854,124**(若变化,说明语义改了,
   必须在 PROGRESS 里解释)。

## 11. 已知会浪费时间的坑

1. `TMP` 必须单独 export(见 §1)。
2. `python3` 坏了,用 `python`。
3. Ghidra headless 的 Python/Java 脚本在这台机器上都跑不起来
   (PyGhidra 未启动 / OSGi bundle 失败);REA 对未识别函数返回
   `not_in_procedure`。优先数据侧取证。
4. `YURIS_TRACE_GROUPS` 全量打开一次 85 万行/帧,必须先按脚本号过滤。
5. `--shot` 会让程序在最后一帧截图后自动退出(`--quit-after 0` 可关)。
6. 不要试图用 `ONMOUSE` 做点击判定 —— 全语料 0 次使用。
7. 不要用 `@1035` 之外的猜测值驱动状态机 —— 先确认 `s58 g31` 的判据。

===== 提示词结束 =====
