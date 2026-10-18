# 任务书:boot 相位修复(#1 配置页抢屏 / #2 logo 淡出黑屏)

> 本文件是**新会话的完整任务书**。开工前必读 `AGENTS.md`(铁律:禁 git 写操作 /
> 证据等级 / 勘误留痕)与 `PROGRESS.md` 尾部成果 152–159。
> 术语以 `CONTEXT.md` 为准(命令≠opcode;窗口≠参数槽;对拍≠diff)。

## 0. 三十秒概览

- 症状 #1:boot 首屏(约 f6–f10)直接显示**環境設定**配置预热页;官方引擎此刻是纯黑。
- 症状 #2:WARNING logo(白底"通り止め×人生")淡出后出现**长时间黑屏**才进 title;
  官方引擎 logo 淡出后直接交叉叠化进 title。
- 两案同根:boot 期 **CGS100 截屏盖幕的生命周期相位**与配置页点亮窗口错位
  (我们的幕布 f5 就 A=0、f6 CGEND;配置页 E=1 在 f3 且直到 f10 仍可见 ⇒ 幕布死后穿帮)。
- 已排除:渲染管线本身(帧 dump 证明幕布 opaque 时提交景是黑的)、CGACT 槽名
  (v494 YSCM 的 ID/ID2/COPY/COPY2/SIZE 族全对)、GO "SCENARIO_LOGO" 未命中
  (YSLB 真无此标签,可选挂点,静默继续,**不要碰**)。
- 已否决(into 缺省)的方案:`task_step=1 组/帧`(抢屏消失但 boot 摊成 4.5 分钟)。
  **该档仍保留为显式实验开关**:`YURIS_TASK_STEP=<n>` 切换"每步 n 组"档
  (缺省关 = run-to-yield);逐步流水 `YURIS_TASK_STEP_TRACE=1`(高等级门控)。
  引擎真值 = **工作线程泵**(见 §2.3),不是"每帧一命令";缺省档与引擎同构。

## 1. 第一步:验收上一线的 #3 修复(先做,半小时级)

工作树里 #3(标题按钮 z/alias/乱序)已由另一会话修复,**但 PROGRESS.md 可能没有
对应成果条目**。你要:

1. `cargo build --release -p yuris-cli`,跑:
   ```bash
   export TEMP="<side-repo>/workbench/tmp" TMP="$TEMP"
   G="<side-repo>/workbench/games/<game-b>"
   ./target/release/yuris-cli.exe run "$G" --lenient --shot 240 --shot 600 --shot-dir shots_handoff0/
   ```
   验收:f600 的 PNG 里 6 个标题按钮全部可见 @(585,380/445/510/575/640/705),
   重复 3 次 boot 的同帧 PNG 像素 SHA 一致(确定性)。
2. 若 PROGRESS.md 缺条目:按仓库风格补一条(含验证命令与实拍结果),这是别人的
   成果,注明"由并行会话实施,本会话验收"。
3. `git diff` 浏览 `crates/yuris-stage`、`crates/yuris-render`、
   `crates/yuris-player-core` 的未提交改动,把 #3 动过哪些机制写进你自己的
   工作笔记(后续避免冲突;**禁止任何 git 写操作**,只看不提交)。

## 2. 症状与证据链(全部实测,可直接引用,不必重跑)

### 2.1 事件级 trace(诊断工具已在本树)
`YURIS_TRACE_CG=<子串|1>`(实现于 `crates/yuris-player-core/src/lib.rs` 的
`stage_cg_trace`/`stage_cgact_trace`,输出带批序号 fN)。

boot 流关键序列(见 `<side-repo>/workbench/tmp/probe5.log` 全量):
```
f1:  CG CGS100 file="" E=1 Z=4551686018427387903 A=0        ← 幕布(63bitmax z)
f1:  CGACT ":/W0/" COPY2=1 ID2="CGS100"                     ← 整幕截屏
f1:  CGACT CGS100 RECTPAINT2=1 SET=255
f1:  CG CGS100 A=256                                        ← 盖幕(不透明)
f1:  CG BT.OFF.TMP file="cgsys/config/btn_tab_system_bt3" Z=1   ← 按钮模板层
f1:  CG BT.{OFF|OVER|ON|ONOV|NA|MASK}ES.CONFIG.BTN.TAB=* Z=2010 E=0
f3:  CG BT.OFFES.CONFIG.TIP.BG=1=1 E=Some(1) A=Some(256)    ← 配置页点亮!
f4:  (幕布重建/再盖一轮)
f5:  CG CGS100 A=0                                          ← 开幕
f6:  CGEND CGS100                                           ← 幕布死亡 ⇒ f6-f10 配置页穿帮
```
提交景 dump(`YURIS_LOG=1 YURIS_DUMP_AT=…`):f3 时幕布 opaque、配置页不可见
(盖住了);f10 配置页可见且幕布已不在景里。

### 2.2 对拍参照
- `<side-repo>/workbench/tmp/<game-b>_win_trace.log` = **本内核旧版
  (带 8ms 切块)的窗口化 run**。其中 "config 页 f137 E=1 ↔ f219 CGEND 间有
  82 帧曝光窗" —— 注意:这是旧内核的帧号,但**事件序列**与现版一致(同一字节码),
  可作为"这段 boot 的事件间应当隔多少帧"的相对锚:配置页 E=1 到它的 CGEND 之间
  隔着 ~82 帧的 WAIT 节拍,而现版内核把它压到了 f3→f6(3 帧)。
  **这就是 #1/#2 的直接病灶:这一段的 WAIT 帧节拍被压缩了。**
- 官方引擎参考画面:`<game-b>_v494.exe`(游戏目录内)可随时手动启动肉眼对拍;
  官方行为 = boot 全黑 → logo 淡入淡出 → title 六钮,全程无配置页、无长黑屏。

### 2.3 引擎调度真值(REA/Ghidra 已取证,<game-b>_v494.exe sha256=1ff3135d…)
- `FUN_004613e4` = WinMain 式一次性 init,内部只调一次 `FUN_00464fa4`。
- `FUN_00464fa4` = 队列**取件**:挑任务 → 写 `task+0x3c` → 从 `DAT_00802a04`
  命令表装填 6 个指针 → 回记录。它**不是**"每帧一命令"的步进器。
- `FUN_00463d04` = 按 `DAT_005b46f4`(CPU 数)生成工作线程,线程体 `LAB_00425508`,
  `SetThreadIdealProcessor` 逐核绑定 ⇒ **引擎的脚本分派跑在多个 worker 线程上,
  泵到让出为止;主线与任务并发推进**。
- 推论(已写入 PROGRESS 成果 159):任务体(BT 预热循环 ~1.6 万组、无 WAIT)在
  引擎里由 worker 泵**快速跑完**(毫秒级),呈现按帧采样;我们单线程 run-to-yield
  的**批边界**落在与引擎不同的位置 ⇒ 相位差。单独改任务量子(1 组/帧)方向被
  实测证伪;单独维持现状又让幕布/配置页相位错开 ⇒ 需要先精确定位**WAIT 节拍
  在哪一段被压缩**(见 §4)。

## 3. 修复方案(两阶段,先取证后动手)

### Phase A:WAIT 节拍对拍(必做,产出决定 Phase B 走哪条)
1. 给 VM 加 `YURIS_TRACE_WAIT=1` 门控 trace:每次 `VmSuspend::Wait` 打
   `[wait fN] s<sid> pc<pc> counter=<n> time_ms=<t>`(挂起点+计数)。改动点在
   `crates/yuris-vm/src/lib.rs` 的 `run`/`run_main` 与 `step_tasks` 的 Wait 分支。
2. 采集现版 boot 的 wait 序列;与 <game-b>_win_trace.log 的相对节拍对照
   (<game-b> 是旧内核,但其 wait 序列同样来自字节码,序列形状应一致)。
3. 找出**第一个发散点**:哪一段在旧内核隔 N 帧的事件,现版被压进同一帧?
   候选(按嫌疑排序):
   a. 任务体的 WAIT 计数:`step_tasks` 里 `Ok(VmSuspend::Wait{counter,..}) =>
      wait = counter.unwrap_or(1).max(1)` —— `unwrap_or(1)` 把 WAIT TIME/TIME 型
      让出也压成 1 帧;若 boot 预热窗的节拍来自任务体的 `WAIT TIME`,这里就是压缩点。
   b. 主线 `WAIT TIME`(`wait_until`)在 tick 里的处理:`Player::tick` 对
      `wait_until` 未到直接 return——确认这些帧**没有**顺带推进任务
      (对照引擎:worker 泵不因主线 WAIT 而停 ⇒ 我们在主线 WAIT 期间**应该**
      继续步进任务,现版 `run` 只在主线 Wait/Complete 时步进一次任务——但
      `drive_vm` 的预算循环 `run(512)` 在主线 Wait 后就 break,任务不再推进,
      直到下一帧 tick——这与引擎"worker 泵持续跑"不同,可能就是相位差的来源)。
   c. `FRAME_GROUP_BUDGET=300k` 把超长批切成多帧的切点位置。
4. 把发散点结论写成 PROGRESS 条目(含 trace 原始输出路径)。

### Phase B:按 Phase A 结论实施(二选一或组合)
- **B1(若发散点是 WAIT 语义)**:修 `unwrap_or(1)`/WAIT TIME 的任务侧计数,
  使任务体节拍与主线一致;验证抢屏窗消失。
- **B2(若发散点是批边界/并发)**:给 `GroupVm` 加**分派域**:主线域与任务域
  各自独立 run-to-yield,`VmEvent` 增加域标记(或分组事件通道);
  `PlayerCore::consume_events` 按域合成 Stage 批——呈现 = 每帧把"主线批 +
  各任务批"按引擎可见序合并(幕布/按钮事件以域内序进入 Stage)。
  规格补充进 `docs/engine/stage-core.md`(新增不变量须先写文档再实现)。
- **红线**:不改 `yuris-render` 的 z 排序语义(引擎真值 = z 升序、同 z 创建序,
  #3 已按此修复);不写任何按层名/按钮名硬编码的白名单;不改 GO/标签查表。

## 4. 验收硬门(全过才算完)

1. `--shot 10`(或 f1-f15 内任一帧)**不出现配置页**;boot 前段呈现 = 纯黑。
2. logo(WARNING 图)淡出后到 title 出现之间**无 >20 帧的纯黑段**
   (对照官方引擎的交叉叠化节奏;可用 `--shot 200..400` 逐帧扫)。
3. f600 title 六钮齐全、3 次 boot 帧像素 SHA 一致(不回归 #3)。
4. `cargo test --workspace --no-fail-fast`(TEMP 重定向后)无新增失败;
   已知环境坑:`yuris-vm/tests/p5_load` 6 个用例在本沙箱偶发 PermissionDenied
   (AGENTS §2 记载的 TEMP 家族),与本案无关,若复现请先确认 TEMP 导出生效,
   不要顺手"修"它。
5. `cargo test -p yuris-stage`、`-p yuris-vm` 全绿;新增调度语义必须有单测
   (仿 stage 的不变量测试风格)。
6. PROGRESS.md 追加成果条目:日期、根因、改动文件清单、验证命令、原始输出。
   若推翻了本文件/成果 159 的任何结论,按勘误留痕格式写明(旧结论/新结论/发现方式)。

## 5. 环境与命令速查

```bash
export TEMP="<side-repo>/workbench/tmp" TMP="$TEMP"   # 沙箱必做
G="<side-repo>/workbench/games/<game-b>"
cargo build --release -p yuris-cli
# 带 --shot 的 run 在**最后一个 shot 帧截完后自动退出**(--quit-after <帧> 可自定义上限,
# --quit-after 0 = 恢复常驻)。诊断批跑无需手动杀进程——后台任务不会再卡在
# 「正在获取任务输出」。
YURIS_TRACE_CG=1 ./target/release/yuris-cli.exe run "$G" --lenient --shot 300 --shot-dir shots/
YURIS_LOG=1 YURIS_DUMP_AT="1,3,10,240,300,600" ./target/release/yuris-cli.exe run "$G" --lenient --shot 600 --shot-dir shots/
```
- 游戏目录里 `<game-b>_v494.exe` = 官方引擎,可手动启动肉眼对拍(别在脚本里启动)。
- REA/Ghidra 可用(本会话已开过 <game-b>_v494.exe 会话;新会话需重新 open_binary)。
- 参考日志:`workbench/tmp/probe5.log`(现版全量 CG trace)、
  `probe8.log`(task_step=1 实验archive)、`<game-b>_win_trace.log`(旧内核切块 run)。

## 6. 禁止事项(重申)

- 任何 `git commit/push/branch/switch/reset/restore/clean` —— 工作区改动留给仓库主人审。
- 把 Hypothesis 写成 Confirmed;静默改旧结论。
- 为过验收在 player/stage 里加"boot 期间隐藏 CONFIG.*"之类的白名单补丁 —— 那是
  掩盖时序病根,列入禁区。
