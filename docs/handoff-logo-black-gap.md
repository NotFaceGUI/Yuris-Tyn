# 任务书:logo 淡出 → title 之间的黑屏缺口

> 本文件是**新会话的完整任务书**,自成一体:读完这一份就能开工。
> 开工前仍须读 `AGENTS.md`(铁律)与 `PROGRESS.md` 尾部的**成果 193 / 遗留登记**。
> 术语以 `CONTEXT.md` 为准(命令 ≠ opcode;窗口 ≠ 参数槽;对拍 ≠ diff)。

## 0. 三十秒速览

- **症状**:厂商 LOGO 淡出后,有约 **3 帧纯黑**(亮度 0.0),之后 title 背景才开始淡入。
  官方引擎在 <game-b> 上表现为「LOGO 淡出 → title 淡入」的**交叉叠化**,中间没有黑场。
- **已确证**:该黑屏**不是**成果 193(SAVE/LOAD 槽位、RECTPAINT、CG/FONT 属性槽)引入的。
  把工作区整体还原到提交 `3f00fe3`(HEAD)后重跑同一段,f164–f166 同样为亮度的 **0.0**。
- **性质**:boot 相位**幕布生命周期**与 title 建层时机错位,是**既有缺陷**。
- **本轮不要做的事**:不要为了"看着不黑"而去改渲染管线/调透明度/延后 `CGEND`
  —— 那些都是掩盖症状。必须先取证,再动 `yuris-stage` 的批语义。

## 1. 环境(必须逐行执行)

系统 TEMP 不可写会导致 8 条测试 `PermissionDenied`(与代码缺陷无关)。
**必须**把 TEMP/TMP 指到工作区内:

```bash
export TEMP="<side-repo>/workbench/tmp"    # 必须是可写目录(不要用系统 TEMP)
export TMP="$TEMP"
export G="<games>/<game-b>"          # 含 pac/ 或 data.ypf 的游戏根
cd <repo>

cargo build --release -p yuris-cli
cargo test --workspace --no-fail-fast | tail -3
# 基线应为 474 passed / 0 failed / 1 ignored
```

> `<work-root>` / `<games>` 等占位符的真值见 `AGENTS.md` §1.5(该文件不入库)。
> 新会话若读不到 `AGENTS.md`,请向仓库主人索取游戏根路径,不要把真名写进被跟踪的文件。

## 2. 现象(逐帧实测,可复现)

命令(帧量化截图,确定性可复现):

```bash
rm -rf "$TEMP/scan" && mkdir -p "$TEMP/scan"
ARGS=""; for n in $(seq 158 176); do ARGS="$ARGS --shot $n"; done
YURIS_LOG_LEVEL=error ./target/release/yuris-cli.exe run "$G" $ARGS --shot-dir "$TEMP/scan"

# 逐帧平均亮度(Python + Pillow)
python - <<'PY'
from PIL import Image
import os
d=os.environ["TEMP"]+"/scan"
for f in sorted(os.listdir(d)):
    im=Image.open(os.path.join(d,f)).convert("L")
    print(f, round(sum(im.getdata())/(im.width*im.height), 1))
PY
```

实测输出(逻辑画布 1100×825):

| 帧 | 亮度 | 画面 |
|---|---|---|
| f158–f163 | 170.3 → 171.7 | LOGO 全显 |
| **f164–f166** | **0.0** | **纯黑(3 帧)** |
| f167 | 100.7 | title 背景开始淡入 |
| f168–f176 | 106.4 → 148.3 | title 淡入 |
| f190+ | 180.0 | title 稳态(六钮齐全) |

> 注:title 稳态段 f260→f280 有 17697 个像素变化,即菜单动画在动。
> 动画若"静止",先确认是不是只截了同一帧;不要凭单张截图下结论。

## 3. 已排除的假设(不要再查)

1. **不是成果 193 引入的**:HEAD(`3f00fe3`)重跑同样黑。
   还原方式(只读 + 覆盖文件,**不要用任何 git 写操作**,见 §7):
   `git show HEAD:<path> > <path>`,跑完再从备份恢复。
2. **不是渲染管线本身**:幕布不透明时提交景是黑的(成果 162 已有结论);
   丢弃/跳过图层、混合模式表、letterbox 都不是本症状成因。
3. **不是 CGACT 槽名错位**:v494 的 `ID/ID2/COPY/COPY2/SIZE` 族已逐槽实证。
4. **不是 `GO "SCENARIO_LOGO"` 未命中**:YSLB 里确实没有该标签,静默继续是正确行为。

## 4. 机制(事件序实测,这是关键证据)

以 `YURIS_TRACE_CG=1` 抓到的同帧事件序(节选,f164→f167):

```text
f164  CGEND SPL401                         ← LOGO 实体被销毁
f164  [scene] visible=3                    ← 只剩 SKIPICON / CLICK / CTRLSKIP
                                             (三者全部 alpha≈0 或不可见内容)
      ⇒ 屏幕上没有任何内容层 ⇒ 黑

f166  CG  CGS100  E=1 A=0                  ← 重开全屏幕布(此刻 A=0)
      CGACT ":/W0/" COPY2 → CGS100         ← 截的是**此刻已经黑掉的屏幕**
      CGACT CGS100 RECTPAINT2 SET=255
      CG  CGS100  A=256                    ← 幕布点亮(内容是黑的)
      CG  CGS1   E=1 A=0
      CGACT ":/W0/" COPY2 → CGS1           ← 同样截到黑屏
      CG  CGS1   A=256
      CG  BT.OFFES.TITLE.TIP.BG FILE="cgsys/title/back1"   ← title 背景到这里才建
f167  可见:title 背景(淡入中)+ 六钮
```

**结论**:黑的那 3 帧里,屏幕上**没有任何内容层**——LOGO 已销毁、幕布 `CGS100`
处于 `A=0`、title 层还没建。而 f166 重新截屏时,被截的"当前画面"本身已经是黑的,
于是幕布点亮后仍是黑,直到 title 背景出现。

## 5. 两个候选根因(需先取证再定案)

### 候选 A:`SnapshotTo` 抓"当前实体投影",而非"已呈现的帧缓冲"

现状(`crates/yuris-stage/src/lib.rs`,`Command::SnapshotTo` 分支):

```rust
let mut view = scene.clone();
self.project(&mut view);      // 投影 = 本批已执行命令的实体状态
self.apply_snapshot(&view, *dst, assets);
```

引擎的「スクリーンショット」抓的是**已经呈现出去的帧**;若本内核改为
"最近一次**非空**呈现帧",f166 的 `CGS100`/`CGS1` 就会拿到 LOGO 的最后一帧,
黑场自然消失,并形成官方那样的交叉叠化。

- **支持证据**:黑场恰好覆盖"LOGO 已销毁 / title 未建"的窗口,而两端的截屏
  都发生在窗口之内。
- **要证伪/证实**:加一条 trace,在每次 `SnapshotTo` 打印
  (a) 被截视图里可见层数与名字、(b) 该视图是否为全黑(采样若干点即可)。
  若 f166 的视图确实全黑,而上一帧视图非黑 ⇒ 候选 A 成立。
- **风险(必须处理)**:与 I6 的既有修复冲突。I6 要求"同批 `CG[E=1 A=0]` 的
  patch 对本次截屏生效"(否则黑幕自锁);而候选 A 想用"上一呈现帧"。
  这两件事必须**分开**:
  - *同批属性 patch* → 必须可见(保留现状);
  - *同批被 `CGEND` 销毁的层* → 仍应留在**本次取样**里(成果 134 的延迟 Kill 语义,
    以及成果 193 的"同批 Kill→Layer 撤销延迟删除")。
  合理形状可能是:取样 = 上一呈现帧 + 本批 patch − 本批**最终态**已销毁且未重建的层。
  改动前先把这三条语义写成测试。

### 候选 B:幕布淡出时长被钳成 1 帧(成果 163 已记录)

`es.CGFADEEXEC` 收到时长 0 → `@0x0732 <= 0` → 置 **1** ⇒ 幕布约 **62 ms** 就退场。
若官方对"0"的解释是"使用缺省时长",则本该存在一段渐变重叠窗口,
黑场就不会出现。

- **要证实**:对照官方 trace 取 LOGO→title 之间 `CG CGS100` 的 `A/T` 序列
  (见 `scripts/engine_trace.py`,Windows + 32 位引擎,WOW64 上下文;
  用法见 `docs/windows-handoff.md`)。
- **注意**:这条与候选 A **不互斥**。若两者都成立,优先修 A(结构性问题),
  B 属于数值/时长语义,单独一条成果。

## 6. 探针清单(按顺序做,每步留原始输出)

1. **确认黑场边界**:§2 的逐帧亮度扫描(已有基线,重跑确认)。
2. **确认黑场里的可见层**:`YURIS_TRACE_SCENE=1 YURIS_LOG_LEVEL=trace`,
   抓 f164–f166 的 `[scene] visible=…` 行(已实测为 `visible=3` 且无内容层)。
3. **打点截屏内容**:在 `Command::SnapshotTo` 处加**高等级门控**的 trace
   (例如 `YURIS_TRACE_SNAPVIEW=1`,已有该开关),打印视图层清单 +
   若干采样点的 RGB。目标是回答"f166 截到的到底是不是全黑"。
4. **对照官方**:用 `scripts/engine_trace.py` 采同一段的官方事件流,
   比对 `CG CGS100` 的 `A/T` 与 `CGEND` 时刻。**这一步是定案的必要条件**。
5. **写测试**:把 §5-A 的三条批语义写成 stage 单测(现状 + 期望),先让测试红,
   再改实现。

## 7. 硬性约束(违反即作废)

- **禁止任何 git 写操作**:不 commit / push / branch / merge / rebase / stash /
  cherry-pick / reset / restore / clean / rm --cached。只读命令(`git show`/
  `git diff`/`git log`)可以。改动留在工作区,由仓库主人审阅后自行提交。
- **禁止为了让画面"看起来对"而改渲染管线**(`yuris-render`/`yuris-scene`)。
  本症状的证据链全部在 `yuris-stage`(批语义)与 VM(幕布生命周期)。
  历史上已经因为"在着色器里覆盖通道"导致过两次回归(菜单动画冻结、开屏全黑),
  详见 `PROGRESS.md` 成果 193 §四。
- **证据等级**:没有官方 trace 对照的结论最多标 `Likely`,实现须加 `// UNVERIFIED`
  与对照测试;`Hypothesis` 禁止进核心路径。
- **注释写占位符**:被跟踪的文件里不写作品真名与本机绝对路径(见 `AGENTS.md` §1.5)。
- **不要用 `YURIS_REGEN_GOLDEN=1` 糊 golden 失败**;Windows 检出问题见 `AGENTS.md` §3.7。

## 8. 验收标准

1. f158–f190 逐帧亮度**不出现 0.0**(允许短暂接近黑,但不得连续 2 帧以上为纯黑)。
2. LOGO 与 title 之间能看到**重叠**(同一帧里既有 LOGO 残影又有 title 背景),
   而不是"黑 → 硬切"。
3. `cargo test --workspace --no-fail-fast` ≥ 474 passed / 0 failed
   (新增测试后总数应增加)。
4. `cargo check --workspace --all-targets` 0 warning / 0 error。
5. 新增/修改的批语义有对应单测,且在 `PROGRESS.md` 追加成果条目
   (日期 + 结论 + 证据 + **验证命令与原始结果**)。
6. 不得回退成果 193 的任何修复(SAVE/LOAD 槽位、RECTPAINT、CG/FONT 属性槽、
   纹理失败原因、同批 Kill→Layer)。

## 9. 相关代码与文档索引

| 内容 | 位置 |
|---|---|
| `SnapshotTo` 取样与投影 | `crates/yuris-stage/src/lib.rs`(`Command::SnapshotTo` 分支) |
| 批内延迟 Kill 与 Kill→Layer 撤销 | 同文件 `Stage::apply` |
| 截屏实现(渲染 + 上传 + 字节返回) | `crates/yuris-player-core/src/lib.rs`(`Assets::snapshot` / `snapshot_ex`) |
| 幕布生命周期(淡出时长钳制) | `PROGRESS.md` 成果 163 |
| boot 相位旧任务书(症状与已排除项) | `docs/handoff-boot-scheduler.md` |
| 图层模型(z 序/组/像素所有权) | `docs/engine/render/`、`docs/engine/stage-core.md` |
| 本轮相关成果 | `PROGRESS.md` 成果 193 + 「遗留登记」条目 |

## 10. 已完成的前置工作(不要重复做)

- 本轮已修:SAVE/LOAD 槽位按名解析、SAVE 的 SET 语义勘误、CG 的
  LINT/LINT2/REMALLOC/TEX、CGINFO 按名应答、FONT NAME、RECTPAINT 族实装、
  纹理失败原因可读、同批 Kill→Layer。
- 本地提交:`be076aa`(仓库主人已建历史;**新会话不要再提交**)。

---

# 附:新会话启动提示词(整段复制)

```text
接手 <repo> 的一个具体缺陷:厂商 LOGO 淡出后到 title 之间出现约 3 帧纯黑,
官方引擎在这里是交叉叠化。

第一步(不要跳过):
1) 读 AGENTS.md(铁律与占位符规则)、CONTEXT.md(术语)、
   docs/handoff-logo-black-gap.md(本任务完整任务书)、
   PROGRESS.md 尾部的「成果 193」与「遗留登记:logo 淡出 → title 之间约 3 帧纯黑」。
2) 按任务书 §1 设置 TEMP/TMP 到工作区内(系统 TEMP 不可写会造成 8 条
   PermissionDenied 假失败),然后 cargo test --workspace --no-fail-fast,
   确认基线 474 passed / 0 failed / 1 ignored。

已确证的结论(不要重新推翻,也不要重新取证):
- 该黑屏在提交 3f00fe3(HEAD)上同样存在,不是成果 193 引入的。
- 逐帧亮度:f158–163 约 170→172;f164–166 为 0.0;f167 起 title 淡入。
- f164 `CGEND SPL401` 之后 scene visible=3 且无内容层;
  f166 才重开 CGS100/CGS1 并 COPY2 截屏,但截到的是已经黑掉的屏幕;
  title 背景 cgsys/title/back1 到 f166 稍后才建。

要你做的事(按任务书 §6 的探针顺序):
1) 在 SnapshotTo 处用已有的 YURIS_TRACE_SNAPVIEW 开关打印"被截视图"的层清单
   与若干采样点 RGB,回答:f166 截到的到底是不是全黑?上一帧视图是否非黑?
2) 用 scripts/engine_trace.py 采官方同一段的 CG CGS100 的 A/T 与 CGEND 时刻,
   与我们的对比。这一步是定案的必要条件;拿不到官方 trace 就如实标注 Likely。
3) 只有在证据支持后,才动 crates/yuris-stage 的批语义。改动必须同时满足三条:
   (a) 同批属性 patch 对本次截屏可见(I6,否则黑幕自锁);
   (b) 同批被 CGEND 销毁、且未在同批重建的层,应仍留在本次取样里(成果 134);
   (c) 同批 Kill→Layer 重建的层不能被批末删掉(成果 193)。
   先写单测覆盖这三条,再改实现。
4) 验收:任务书 §8(f158–f190 不再出现连续纯黑、能看到 LOGO 与 title 重叠、
   测试全绿、0 warning),并在 PROGRESS.md 追加成果条目(含验证命令与原始结果)。

硬性约束:
- 禁止任何 git 写操作(不 commit/push/branch/reset/restore/clean/stash)。
  改动留在工作区,提交由我自己做。
- 禁止为"看着不黑"去改 yuris-render / yuris-scene。历史上在着色器里改通道
  导致过两次回归(菜单动画冻结、开屏全黑),详见成果 193 §四。
- 不要回退成果 193 的任何修复。
- 注释只写占位符,不写作品真名与本机绝对路径。

先给我一份"取证结果 + 定案 + 改动计划",我确认后再动代码。
```
