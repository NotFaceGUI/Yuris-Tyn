# 交接文档 —— 游戏内 UI 显示链路的 7 个缺口

> 生成日期:2026-10-17
> 上游背景:`PROGRESS.md` 成果 176 / 178 / **184**(本轮取证全链)
> 本文用途:把「游戏内 UI 不显示」拆成**可独立实现/取证**的 7 个缺口,
> 交给后续模型逐项推进。**每一项都标了证据等级与禁止事项,不要跳读。**

---

## 0. 三十秒速览

**现象**:标题画面完全正常(六钮、有字、可点)。点「最初からはじめる」之后:

- 背景与立绘**全部消失**(不是没画,是脚本自己 `es.CGSDELALL` 清掉后没再建)
- 屏幕只剩**两个蓝框**(它们是真实贴图 `tip_meswindow_back` / `tip_meswindow_txspace`)
  + 一句提示文本
- 系统栏 11 个按钮是**空框**(3204 条 CG 事件里 3153 条 `file=""`,没有一张图)
- 按钮**点不动**

**根因分层**(不要混为一谈):

| 层 | 缺口 | 谁的问题 |
|---|---|---|
| 流程层 | `GO` 标签未命中被静默吞掉 + 进场路线号 = 0 撞上不存在的标签 | VM 分支建模 |
| 图形层 | `es.BT.CG.SET` 的图只到 `BT.OFF.TMP`,没拷到按钮层 | 宏族状态契约 |
| 文字层 | 字形记录门 `@1888` 从未打开 + 正文缓冲 `$123` 恒空 | 宏族状态契约 |
| 布局层 | `MENUINFO`/`MENUEND` 整族未实现 | 引擎原生命令缺失 |

---

## 1. 环境(必须逐行执行,写错会一片假失败)

```bash
cd /d/Art/ai/projects/Yuris-Tyn

export TEMP="<side-repo>/workbench/tmp"
export TMP="$TEMP"          # ← 必须单独一行!
```

- **`TMP` 必须单独 export**。Windows 的 `GetTempPath2` 优先读 `TMP`;
  同一行写 `TMP="$TEMP"` 时 `$TEMP` 尚未生效,会导致 6 个 `p5_load`
  用例 PermissionDenied 假失败。
- **`python3` 是坏的**(Microsoft Store 存根,退出码 49)。用 `python`。
- 游戏目录:`<side-repo>/workbench/games/<game-b>`
- **封包真值 = `data.ypf`**。磁盘上的 `ysbin/*.ybn` 是**未加密的旧副本**
  (`yst00123.ybn` 1 125 871 B vs 包内 945 959 B,且不含 `SCENARIO_LOGO`),
  **不是本作真值,不得作为取证源**。
- 基线:`cargo test --workspace --no-fail-fast` = **435 passed / 0 failed**
- 每次改代码后**先 build 再实测**:

```bash
cargo build --release -p yuris-cli
md5sum target/release/yuris-cli.exe     # 记下来,便于判断跑的是不是新二进制
```

> **本仓库可能有并发会话在改同一份代码**(2026-10-17 夜里发生过:构建三次被
> 打断、有一次实测跑的是旧二进制)。动手前先 `git status` 看有没有别人未提交的
> 改动,并记录二进制 md5。**不要 revert 你不认识的改动。**

---

## 2. 架构速览(改代码前必读)

```
crates/yuris-format      格式层(YSTB 脚本 / YSLB 标签 / YSCM 命令表 / YPF 封包)
crates/yuris-script      表达式译码(窗口 → Instruction IR)
crates/yuris-vm          组级 VM(命令分派、任务调度、宏求值)
  src/driver.rs          step_group(单组执行唯一实现点) / run_frame_chain
  src/exec/flow.rs       IF/LOOP/GO/GOSUB/RETURN 族
  src/exec/media.rs      CG/CGACT/CGINFO/SOUND 族
  src/exec/vars.rs       LET/声明/VARINFO 族
  src/exec/misc.rs       其余子系统
  src/commands.rs        ★命令名 → CmdKind 的映射表(版本无关)
  src/input.rs           输入系统变量桥
crates/yuris-player-core 播放器核心(tick / CG 事件消费 / Stage 同步)
crates/yuris-stage       图层实体表(hit_test / z / 可见性)
crates/yuris-cli         桌面入口
```

**关键术语(见 `CONTEXT.md`,不要自造同义词)**:
- 命令 ≠ opcode;窗口 ≠ 参数槽;共享池 = content‖part4
- **窗口 tag 低字节 = 参数槽号**,逐版本漂移 ⇒ 必须从游戏自己的 `ysc.ybn` 按名解析
- `@N` = INT 变量;`$N` = STR 变量;`@53/@54` = INT/FLT **帧局部数组**(GOSUB 实参);`$55` = STR 帧局部数组

**宏族的本质**:`es.BT.*` / `es.FONT.*` / `es.TX.*` **不是命令**,是普通
`GOSUB "es.xxx"` 调用的脚本函数(定义在 s0 / s11 / s71)。VM 不需要"实现"它们;
VM 要做的是**让它们内部的状态变量按契约推进**。

---

## 3. 缺口清单

> 优先级 = **先让画面出来,再让文字出来,再让布局精确**。
> 每项都给了「已知 / 待取证 / 禁止」。

---

### G1(最高优先)进场路线号:`SCENARIO_LOGO` 标签不存在

**现象**:点 START 后 `GO "SCENARIO_LOGO"` 命中失败,正篇永不载入。

**已确认(运行期,可复现 2/2)**:

```
s58 pc32  hit "ES.TITLE.BTN.START.ON" → s58 pc62
s58 pc63  LET @1096 = 0                 (GOSUB 实参 @53[1] = 0)
s142 pc10 LET @1163[4001] = 0
s142 pc24 LET $1078 = "SCENARIO_LOGO"
s142 pc29 GO $1078                      → MISS
```

**为什么是 0**:反汇编 `s58 g32` 是
`GOSUB "ES.TITLE.BTN.START.ON"` + **`pushint(0)`** —— 实参字面量就是 0。

**数据侧硬事实**:
- YSLB 共 **2132** 条标签(murmur2 哈希逐条校验通过,解析完整)
- 含 `SCENARIO_START`(s142 pc0)/ `SCENARIO_MAIN`(s143 pc0)/ `SCENARIO_EPILOGUE`(s145 pc0)
- **不含 `SCENARIO_LOGO`**(全封包 CP932 字节扫描 + YSLB 全表枚举,双向确认为 0)
- `@1163` 在 YSVR 里是 `id=048b bounds=[8192] init=Int(0)`,`[4001]` 在界内

**s142 的三路分支**(`wdump`):
```
g26  IF @1163[4001] == 0  → $1080 = "SCENARIO_LOGO"      ← 本作不存在
g40  IF @1163[4001] == 1  → $1080 = "SCENARIO_MAIN"      ← 存在
g66  IF @1163[4001] == 2  → $1080 = "SCENARIO_EPILOGUE"  ← 存在
```

**待取证(必须做,不许猜)**:
1. **正版引擎在 `GO` 标签未命中时的行为是什么?**
   我们现在 `exec/flow.rs` 的 `GoOutcome::NoJump` 分支是 `self.pc += 1;`
   (注释写的是「非 strict:未命中事件已记,顺序继续」)。
   这个建模**只对 `GOSUB` 有取证**,`GO` 没有。
   如果引擎实际是「中止任务」或「回退到某个安全点」,那当前实现就是**在掩盖错误**。
   - 取证入口:`docs/engine/v494-handler-table.md` 里 `GO` 的处理器地址;
     或对拍正版运行时 s142 的 pc 轨迹。
2. **正版点击 START 时 `@53[1]` 是多少?**
   若正版也走 `s58 g32`(实参 0),说明正版**同样** miss —— 那意味着
   `SCENARIO_LOGO` 应该由别的东西提供(例如另一个封包 / 另一个 exe),
   或我们的 YSLB 装载漏了某个区段。
   - 可行取证:用 `yslb_close` / `yslb_around` 工具看 `s142 pc29` 附近的标签分布;
     或检查游戏目录是否有第二份 `.ypf`(当前只有 `data.ypf`)。
3. **`s58 g32` 与 `es.BT.GOSUB.EXEC` 的关系**:
   标题按钮是 `es.BT` 按钮对象。正版点击时,`.ON` 处理函数很可能由
   **`es.BT.GOSUB.EXEC`(s0 g2361–2446)** 调起,那一路传的是队列里算出的
   `@1464`/`@1465`(按钮 X/Y),**不是** `pushint(0)`。
   - 若成立 ⇒ 我们走的 `s58 g32` 是**错误调用点**,应当打通 BT 队列派发(G3)。
   - 这是 **Hypothesis**,必须先取证再动。

**禁止**:
- ❌ 不要为了让流程走通,手工把 `@1163[4001]` 写成 1。
- ❌ 不要给 `SCENARIO_LOGO` 造一个假标签。
- ❌ 不要在 `s142` 里加特判。
- ❌ 不要把 `GoOutcome::NoJump` 改成「静默跳到下一个 GOSUB 目标」之类的猜测行为。

---

### G2(高)按钮图层拿不到图:`BT.OFF.TMP` → 按钮层 的拷贝缺失

**现象**:系统栏 11 个按钮是空框。

**硬数字**(`YURIS_TRACE_CG=1`,点击后 700 帧):

| 指标 | 值 |
|---|---|
| `GAMEMAIN.BTN.*` CG 事件总数 | 3204 |
| 其中 `file=""`(纯属性刷新) | **3153** |
| 含 `main/button` 路径的 | **0** |

**对照(标题按钮是正常的)**:
```
CG s0 pc1711 BT.OFFES.TITLE.BTN.START=1=1 file="cgsys/title/btn_start2_off"
CG s0 pc1772 BT.ONES.TITLE.BTN.START=1=1  file="cgsys/title/btn_start2_on"
```

**系统栏的图去哪了**:只到了临时缓冲,唯一一次:
```
CG s0 pc1699 BT.OFF.TMP file="cgsys/main/button/btn_save_bt3"  Z=Some(1)
CG s0 pc1718 BT.OFFES.GAMEMAIN.BTN.SAVE=0=1 file="" E=Some(0)   ← 按钮层拿到空串
```

**证据等级**:`Confirmed`(运行期 CG 事件逐条比对)。
**缺口位置**:从 `BT.OFF.TMP` 到 `BT.OFFES.GAMEMAIN.BTN.<名>` 的那一步没发生。

**相关脚本锚点**:
- `es.BT.CG.SET` = s0 pc870(把 FILE 写进 `$1168[]` 等槽位)
- `es.BT.DRAW.NO` = s0 pc425(按槽位号画;组 425–559)
- `BT.OFF.TMP` 的写入点:`s0 pc1699`(CG 命令)
- `es.BT.END.SET` = s0 g1880+(对 `BT.OFF<名>1` 做 `CGEND`,暗示图层按名共享 TMP)
- `es.BT.END.NO.EXEC` = s0 g1939+

**待取证**:
1. `BT.OFF.TMP` 到底是不是「引擎侧的瞬时缓冲」?它有几层?与
   `BT.OFFES.GAMEMAIN.BTN.SAVE=0=1` 的**名→图层**映射规则是什么?
2. `es.BT.DRAW.NO` 里 `@1374`(状态码 1..5)对应的 5 个 CG/NA/ON/ONOV/MASK 层,
   各自由哪个 `CG`/`CGACT` 写入?(反汇编 s0 g425–559 逐条对)
3. 为什么标题按钮(走 `es.BT.CG.SET` 直写路径)能拿到图,而系统栏不行?
   两者的**唯一差别**是 `es.BT.GROUP.SET` 的组号(s118 里系统栏是 `GROUP.SET 1`,
   标题族是别的值)⇒ 怀疑 `es.BT.GROUP.CHECK`(s0 g194)的可见性判定拦住了。

**禁止**:
- ❌ 不要在 `Stage` / `player-core` 层加「某按钮没图就用占位图」的兜底。
- ❌ 不要给 `BT.OFFES.GAMEMAIN.BTN.*` 做名字前缀白名单。

**验证方式**:
```bash
YURIS_TRACE_CG=1 ./target/release/yuris-cli.exe run "$G" --lenient \
  --click 585,380 --click-frame 420 --quit-after 700 > /tmp/cg.log 2>&1
grep GAMEMAIN.BTN /tmp/cg.log | grep -c 'main/button'   # 目标: > 0
```

---

### G3(高)文字字形记录门 `@1888` 从未打开 ⇒ `@1937` 恒 0

**现象**:消息窗内无文字。

**证据链(静态,`wdump` s11)**:

```
s11 g278  es.FONT.RECORD.SET    : LET @1888 = 1                    ← 唯一置 1 处
s11 g329  IF @1888 == 1 && @1938 < 4096
s11 g330-341  LET @1939..@1949[@1938] = @1877..@1887               ← 存字形/坐标记录
s11 g341       LET $1950[@1938]       = $1977                       ← 存文本
s11 g342       LET @1938 += 1
s11 g297  es.FONT.RECORD.DECISION : LET @1937 = @1938               ← 记录数
s11 g299  es.BT.FONT.DRAW.REPLAY: LOOP @1937 次 → es.FONT.DRAW.SET  ← 重放
```

**运行期实证**(`YURIS_TRACE_LET=1888,1937,1938`,当前二进制 `a6c69a3f`):

```
（零行 —— 三个变量一次 LET 都没有）
```

⇒ `@1938` 不增长 ⇒ `@1937 = 0` ⇒ REPLAY 循环体**一次不跑**。

**谁该调 `es.FONT.RECORD.SET`**(全语料 `litgrep`):

| 宏 | 调用点 |
|---|---|
| `es.FONT.RECORD.SET` | s71 g567 / g584 / g590 / g606 / g660 |
| `es.FONT.RECORD.DECISION` | s71 g435 / g462 |
| `es.FONT.RECORD.RESET` | s71 g103 / g419 / g435 / g455 |

`YURIS_TRACE_GROUPS=71` 实测:**s71 只执行了 178 / 1882 个不同 pc**,
上述 5 处 `RECORD.SET` **全部落在未执行区间**。

**待取证**:
1. **谁在引擎里调 `es.FONT.RECORD.SET`?** 上游应是 s71 的 TX 主循环
   (`es.TX.Main.103/104/105` = pc379/395/414 附近),而该循环的**启动条件**
   仍未定位。
2. s71 的 178 个已执行 pc 里最大是 1862,呈**离散跳跃**(见 `YURIS_TRACE_GROUPS=71`),
   说明有若干 TASK 只各自跑了入口几组就 `WAIT`。要查清是哪个 `WAIT`/`TASK`
   让 TX 主循环停住。

**禁止**:
- ❌ 不要在播放器里「有文本就自己调用 `RECORD.SET`」——那是我们编时机。
- ❌ 不要把 `@1888` 设成 1 当初始化。

---

### G4(高)正文缓冲 `$123` 恒空 ⇒ TX 状态机卡在 0

**现象**:同上(消息窗无字),但与 G3 **正交**——修好 G3 也画不出字。

**证据链(静态,`wdump` s71)**:

```
s71 g99  LET $122 = ""              ← 初始化(写空)
s71 g100 LET $123 = ""              ← 初始化(写空)
s71 g272 LET $4371 = $122           ← 取「名字」缓冲
s71 g273 LET $4374 = $123           ← 取「正文」缓冲
s71 g274 es._strlen $4374
s71 g275 LET @4375 = 返回长度
s71 g276 IF @4376 <= @4375          ← 1 <= 0 → 假
s71 g277 LET @4464 = 1              ← 永远不置
s71 g279 RETURN @4464               ← 恒返回 0
```

**运行期实证**(`YURIS_TRACE_LET=4375,4376,4462`):

| 变量 | 写入次数 | 值 |
|---|---|---|
| `@4376`(`$122` 长度) | 1 | 1 |
| `@4375`(`$123` 长度) | **160** | **恒 0** |
| `@4462`(TX 状态) | 1 | 0(之后不变) |
| `@4464` | **0** | — |

`@4462` 停在 0 ⇒ s71 的 `IF @4462 == N` 状态机分支全部不成立 ⇒
`es.TX.Ready` / `TX.Main.103/104/105` / `es.TX.OUTPUT.ONE` **一次都不调**。

**`$122`/`$123` 的写入点普查**(全语料):
- 只有 s71 g99/g100(写空串)与 g182/g273(自赋值)
- **没有任何一处写入正文**

**本内核自己的文本通道是正常的**:
```
YURIS_TRACE_TEXT=1  →  [text] lines=74 window_open=false   (boot)
                        [text] lines=1  window_open=true    (进入后)
```

⇒ 文本被 `WORD`/`TEXT` + `RETURNCODE` 正确收进 `TextChannel`
(`crates/yuris-vm/src/scenario.rs::TextChannel`),但**没有送进 `$122`/`$123`**。

**待取证(这是本项的核心问题)**:
1. `$122`/`$123` 是**引擎系统串**吗?即引擎在执行 `TEXT`/`WORD` 时
   是否直接写这两个变量?
   - 若**是** ⇒ 需要在 `cmd_text` / `cmd_word` 里同步写 `$122`/`$123`
     (但要先确证写法:整行?逐段?带控制符?)
   - 若**否** ⇒ 必须找到游戏脚本里真正的写入者(可能通过某个我们
     尚未实现的引擎命令,如 `MENU`/`TX` 族)
2. `s71 g300` 的 `LOAD $1123` 分支(存档/回想读入文本)是另一条写入路径,
   但它依赖 `$1123` 非空 —— 也要查它由谁写。

**禁止**:
- ❌ 不要把 `TextChannel` 的行**直接**塞进 `$123` 当作实现
  (时机/格式未取证,会引入新的错误)。
- ❌ 不要在 `es.TX.*` 里加「如果为空就跳过」的兜底。

---

### G5(中)`MENUINFO` / `MENUEND` 整族未实现

**现象**:按钮与窗的坐标/尺寸算不出来(`@2905..@2913` 全 0)。

**用量**(全语料 `cmdcount`):

| 命令 | 次数 |
|---|---|
| `MENU` | 4 |
| `MENUACT` | 0 |
| `MENUINFO` | **246** |
| `MENUEND` | **71** |

**当前状态**:`crates/yuris-vm/src/commands.rs::CmdKind::from_name`
**没有任何 MENU 分支**(`grep MENUEND commands.rs` = 0 命中)⇒ 全部落到
`CmdKind::Other`(未实现路径)。

**运行期实证**(`YURIS_TRACE_LET=2905,2910`):**零次写入**。

**已解出的部分(`MENUEND` = `0x44a408`,REA 静态反汇编)**:

```asm
44a408  53                 push ebx
44a409  83 ec 10           sub esp, 0x10
44a40c  b8 41 00 00 00     mov eax, 0x41            ; 参数槽上界 = 65
44a411  e8 4f fb fd ff     call <取实参表>
44a416  0f be c0           movsx eax, al
44a419  85 c0              test eax, eax
44a41b  74 0a              je   +0x0a               ; 无实参 → 返回 -1
44a41d  b8 ff ff ff ff     mov eax, -1
44a422  83 c4 10           add esp, 0x10
44a425  5b                 pop ebx
44a426  c3                 ret
44a427  33 db              xor ebx, ebx
44a429  88 1d 40 d0 5d 00  mov byte [0x5dd040], bl  ; 清一个全局活动位
44a42f  0f b6 15 c0 21 68 00  movzx edx, byte [0x6821c0]  ; 槽0 = 目标引用
44a436  88 5c 24 0c        mov [esp+0xc], bl
44a43a  b8 40 d0 5d 00     mov eax, 0x5dd040
44a43f  8b 14 95 20 20 68 00  mov edx, [0x682020 + edx*4]
44a446  8b 0d e8 29 80 00  mov ecx, [0x8029e8]
44a44c  e8 57 9e fc ff     call <求值 float>        ; 槽1 = 值
44a451  8b c8              mov ecx, eax
44a453  0f be d9           movsx ebx, cl
44a455  85 db              test ebx, ebx
44a457  7c c3              jl   -0x3d
44a459  a1 d8 29 80 00     mov eax, [0x8029d8]
44a45e  ba 40 d0 5d 00     mov edx, 0x5dd040
44a463  e8 c4 a1 fc        call <写菜单槽>
```

**已确证**:`MENUEND` 不是「结束菜单」,而是**赋值命令** ——
形态是 `MENUEND <左值引用> = <float 值>`(s27 g240 实测
`MENUEND @2905[101] = 0.0`)。

**关键阻碍**:`0x682020` 这张表在静态镜像里**全 0**(未初始化区),
且 `xrefs 0x682020` 有 **70 处**引用、分布在几乎所有命令处理器
(`0x42a4e1` CG … `0x459963`)⇒ 它是**引擎共享的「参数槽号 → 参数窗口地址」表**,
**不是**菜单专用下标映射。所以 `mov edx,[0x682020 + 槽0*4]` 的含义是
「取槽 0 的实参窗口」,`MENUEND` 的**写入目标仍未知**。

**待取证(必须做)**:
1. `0x5dd040` 是什么?(菜单槽数组基址?多大?元素类型 float?)
2. `<写菜单槽>`(`0x44a463` 调用的那个函数)的语义 —— 它拿
   `(ecx=求值结果, edx=0x5dd040)` 做了什么?
3. `MENUINFO`(`0x44a52c`)的槽号→字段映射:哪个下标是 X / Y / SX / SY / Z / A?
   入口已知 `mov eax,0x42`(参数上界 66),但主体未解。
4. `MENU`(`0x449c44`)与 `MENUACT`(`0x44a268`)的语义。

**取证建议**:Ghidra headless 脚本在这台机器上不可用(OSGi / PyGhidra 都失败),
REA 对未识别函数返回 `not_in_procedure`。可行路径:
- 用 `mcp__rea__read_bytes` 逐段读机器码 + 手工解码(本文的 `MENUEND`
  就是这么解出来的);
- 或用 `mcp__rea__inspect_native_instruction` 逐地址取反汇编。

**禁止**:
- ❌ 不要在不知道槽位语义的情况下「按 @2905=X, @2906=Y…」这类顺序猜。
- ❌ 不要把 `MENUINFO` 映射成「读任意变量」。

---

### G6(中)`@1035` 的语义用错了 —— 成果 177 勘误

**旧结论(成果 177)**:「`@1035` 是标题选择槽,点击写入按钮序号」。

**实际(s58 g31 反汇编)**:

```
IF @1035 != -1  &&  @1035 <= @1[1] - @2909
```

`@2909` 在 s58 g28 被写成 `LET @2909 = @1[1]`(进入状态 4 的时间戳)⇒
`@1035` 与**时间差**比较,它是**点击时刻戳**,不是按钮序号。

**当前实现的问题**(`crates/yuris-player-core/src/lib.rs`):

```rust
fn title_button_slot(layer_name: &str) -> Option<i64> {
    if up.contains("BTN.START") { Some(1) }
    else if ... { Some(2) }      // ← 其余 5 个按钮全部映射成 2
}
// dispatch_ui_click 里:
self.core.vm.set_title_choice(slot);
```

这把 1/2 写进时间戳槽,只是让 `1 <= 已过毫秒` 恒真,把状态机**强推过** g31。
它是**猜测实现**,且掩盖了真正的按钮身份传参路径
(`ES.TITLE.BTN.*.ON` 的 GOSUB 实参 `@53[1]`)。

**待取证**:
1. 正版里 `@1035` 到底由谁写?写什么值?
   - 全语料只有 `s142 g5` 写它(置 -1)。⇒ **必须**是引擎或输入层写。
   - 可能是 `MOUSE`/`INPUT` 族或 `es.BT` 子系统回写。
2. 按钮身份应当如何传给 `.ON`?是 `es.BT.GOSUB.EXEC` 的队列参数,还是别的?

**禁止**:
- ❌ 在没搞清 `@1035` 之前**不要删** `set_title_choice`(会让现在的标题
  流程直接卡死),但要把它标记为 `// UNVERIFIED: 猜测实现`,并登记为待修。
- ❌ 不要给另外 5 个标题按钮编不同的序号。

---

### G7(低,但有风险)`GO` 未命中的静默继续

见 G1 的「待取证 1」。代码位置:

`crates/yuris-vm/src/exec/flow.rs:69`
```rust
GoOutcome::NoJump => {
    // 非 strict:未命中事件已记,顺序继续
    self.pc += 1;
    return Ok(Action::Keep);
}
```

**风险**:这条把一个致命错误(跳不存在的标签)变成「继续跑」,导致
「清屏后停在黑屏」这种难以定位的现象。**但改成什么必须先取证。**

**待取证**:`GO` 处理器(v494 handler table)在标签解析失败时的返回值/行为。

---

## 4. 建议的推进顺序

```
第 1 步  G1-取证:确认 GO miss 的引擎行为 + 确认正版走哪个调用点
         ⚠ 这一步不改代码,只取证。它会决定 G1 的实现方向。
第 2 步  G2:打通 BT.OFF.TMP → 按钮层(让系统栏出图)
第 3 步  G3:打开字形记录门(让 FONT.DRAW.REPLAY 有内容可画)
第 4 步  G4:接通正文缓冲(让 TX 状态机走起来)
第 5 步  G5:MENUINFO/MENUEND 槽位语义(布局精确化)
第 6 步  G6/G7:修 @1035 与 GO miss
```

**理由**:G2/G3/G4 修好后,即使流程还卡在标题,「进游戏后的 UI」至少能显示
正确内容,可以肉眼验收;G1 的方向依赖取证结果,不宜盲改;G5 只影响精度。

---

## 5. 验收标准

### 必须全绿

```bash
export TEMP="<side-repo>/workbench/tmp"; export TMP="$TEMP"
cargo test --workspace --no-fail-fast      # 435 passed / 0 failed(基线)
```

### 行为验收(逐项)

```bash
G="<side-repo>/workbench/games/<game-b>"

# A. 按钮层出图(G2)
YURIS_TRACE_CG=1 ./target/release/yuris-cli.exe run "$G" --lenient \
  --click 585,380 --click-frame 420 --quit-after 700 2>&1 | grep GAMEMAIN.BTN | grep -c 'main/button'
#    当前: 0        目标: > 0

# B. 字形记录门打开(G3)
YURIS_TRACE_LET=1888,1937 ./target/release/yuris-cli.exe run "$G" --lenient \
  --click 585,380 --click-frame 420 --quit-after 700 2>&1 | grep -c ' 1937\['
#    当前: 0        目标: > 0

# C. 正文长度非 0(G4)
YURIS_TRACE_LET=4375 ./target/release/yuris-cli.exe run "$G" --lenient \
  --click 585,380 --click-frame 420 --quit-after 700 2>&1 | grep ' 4375\[' | grep -vc 'int:0'
#    当前: 0        目标: > 0

# D. 文字落到 TX.GAIJI 图层(G3+G4)
YURIS_TRACE_CG=1 ./target/release/yuris-cli.exe run "$G" --lenient \
  --click 585,380 --click-frame 420 --quit-after 700 2>&1 | grep -c TX.GAIJI
#    当前: 0        目标: > 0

# E. 布局槽(G5)
YURIS_TRACE_LET=2905,2910 ./target/release/yuris-cli.exe run "$G" --lenient \
  --click 585,380 --click-frame 420 --quit-after 700 2>&1 | grep -cE ' 2905\[| 2910\['
#    当前: 0        目标: > 0
```

### 实机验收(用户操作)

点「最初からはじめる」后:
1. 背景与立绘出现(有带 `FILE` 的 CG 事件)
2. 消息窗内有文字
3. 系统栏按钮有文字
4. 至少「回退 / CONFIG」之一可点动

### 性能不回退

```bash
./target/release/yuris-cli.exe run "$G" --lenient --bench 8
# boot 首帧组数应仍为 854,124(若变化必须在 PROGRESS 解释)
```

### 确定性

同输入同输出,三跑 SHA 一致(用 `--bench` 或固定 `--click` 序列;
注意**只有 `--bench` 会屏蔽鼠标**,`--shot` 不会)。

---

## 6. 硬性约束(违反即作废)

1. **禁止任何 git 写操作**:`commit` / `push` / `tag` / `branch` / `switch` /
   `checkout <分支>` / `merge` / `rebase` / `stash` / `cherry-pick`;
   以及 `reset` / `restore` / `clean` / `rm --cached`。
   所有改动留在工作区。
2. **真值只来自原引擎/原数据**。无证据一律 `Error::Unimplemented`。
3. 证据四级:`Confirmed` / `Likely`(须加 `// UNVERIFIED` + 对照测试)/
   `Hypothesis`(禁入核心)/ `Unknown`(保持未实现)。
4. **不要在 stage / player 层加「某作品特判」或表现层白名单**遮语义问题。
5. `CONTEXT.md` 是术语权威。
6. **改行为前先查 `PROGRESS.md`**;勘误必须留痕(旧结论/更正/发现方式),
   不得静默改旧结论。
7. **每完成一项**,在 `PROGRESS.md` 追加「成果 N」:日期 / 结论 /
   取证链(命令 + 原始输出 + 地址/组号/变量 id)/ 改动文件清单 /
   验证命令与原始结果 / 残余。

---

## 7. 工具与诊断开关

```bash
# 反汇编某脚本的组区间(名字 + 逐窗口指令)
cargo run --release -q -p yuris-vm --example tmp_s0_disasm -- <游戏> <脚本号> <起组> <止组>

# 单组窗口全解(精确指令序列 + tag)
cargo run --release -q -p yuris-tools --example wdump -- <游戏> <脚本号> <组号> [组数]

# 某变量的全部读写点
cargo run --release -q -p yuris-tools --example whowrites -- <游戏> <var_id>

# 按字面量找组(只搜窗口)
cargo run --release -q -p yuris-tools --example litgrep -- <游戏> <子串>

# 整条脚本字节搜(含窗口之外)
cargo run --release -q -p yuris-tools --example script_grep -- <游戏> <子串>

# 命令用量统计
cargo run --release -q -p yuris-tools --example cmdcount -- <游戏> <命令名...>

# 命令参数名表(槽号 → 名)
cargo run --release -q -p yuris-tools --example params -- <游戏> <命令名...>

# 标签全表
cargo run --release -q -p yuris-tools --example labels -- <游戏>

# 全封包 CP932 字节串扫描
cargo run --release -q -p yuris-tools --example cp932_sweep -- <游戏> <hex>

# YSVR 全局变量初值清单
cargo run --release -q -p yuris-tools --example ysvr_dump -- <游戏>
```

**环境变量**:

| 变量 | 作用 |
|---|---|
| `YURIS_TRACE_GROUPS=<脚本号\|1>` | 逐组执行流水(量极大,必须按脚本号过滤) |
| `YURIS_TRACE_LET=<id,id>` | LET 写入流水(带 s/pc/左值/新值) |
| `YURIS_TRACE_LABELS=1` | 标签解析 hit/MISS 流水 |
| `YURIS_TRACE_CG=<子串\|1>` | CG 事件流水(含 FILE/E/Z/A/SX/SY/X/Y) |
| `YURIS_TRACE_SCENE=1` | 每帧可见层 dump(名字/z/alpha) |
| `YURIS_TRACE_TEXT=1` | 文本通道流水 |
| `YURIS_TRACE_WAIT=1` | WAIT 让出流水 |
| `YURIS_TEXT_PX=<px>` | 字号强制覆盖(诊断) |
| `YURIS_WALL_CLOCK=1` | 回退墙钟(默认逻辑时钟) |

**CLI**:

```bash
./target/release/yuris-cli.exe run "<游戏>" --lenient                    # 交互
./target/release/yuris-cli.exe run "<游戏>" --lenient --shot 600 --shot-dir <目录>
./target/release/yuris-cli.exe run "<游戏>" --lenient --bench 10         # 屏蔽鼠标
./target/release/yuris-cli.exe run "<游戏>" --lenient --click x,y --click-frame n
```

---

## 8. 已排除的假设(别再查)

| 假设 | 排除依据 |
|---|---|
| 渲染器坏了 | 蓝框是真实贴图;标题画面完全正常 |
| 字号 / 编码错误 | `FONT` 真值 29×30 已生效;s127 文本完整 |
| z 被压住 | 21 层可见,z 均 1000/1500 档 |
| 点击传不到 VM | `set_input_cursor` 每帧同步;`es.BT.DOWN` 执行 11 次 |
| `es.BT` 队列是空的 | `@1353` 每帧递增;`MOUSECHECK` 476 次;**成果 178 的「队列空」结论已作废** |
| `ONMOUSE` 未实现 | 全语料 **0 次**使用 |
| Unsupported 被跳过 | `log_unsupported_summary` 输出「无被跳过的命令」 |
| 磁盘 `ysbin/*.ybn` 是真值 | 它们是未加密旧副本,与包内不一致,且不含 `SCENARIO_LOGO` |
