# 审查发现待办 02（2026-10-16 第二轮审查产出）

> 来源:同日第二轮只读审查,覆盖第一轮(`docs/core-alignment-plan.md`,下称「待办01」)未深入的层:
> `yuris-value`/`yuris-script`(变量模型与系统变量)、`exec/` 四模块的查询写回、`yuris-render`/`yuris-scene`
> 混合模式、存档/恢复链、`yuris-scenario`/剧本装载、`yuris-expr`/`yuris-resource`/`yuris-runtime`、
> CLI 输入面,以及**用 <game-b> 语料实测的未实装命令频度**(cmdcount 探针)。
> 本文只收录**新增发现**;待办01 的 19 项仍然有效,两者合并执行。
> 格式:审查发现 → 待办动作 → 验收。改动留工作区、禁 git 写操作,完成后 PROGRESS.md 记成果。

## 0. 本轮确认无恙的部分(**无任务,勿动**)

| 机制 | 结论 | 证据 |
|---|---|---|
| `murmur2` 标签哈希(seed 0 / 乘数 0x5BD1E995) | Confirmed,4153/4153 零失败 | `yuris-core/src/hash.rs` |
| YSCM tail 定性 | = 37 条 CRT 错误消息(旧「配置键」判断已勘误);结构 Unknown 原样保留,不猜 | `yscm.rs:51-81` |
| `yuris-runtime` L4 trait 边界 | RuntimeApi/四后端 trait + Null/Mock,方向正确(VM 不依赖具体后端) | 成果 48 |
| `yuris-resource` 挂载语义 | 松散文件优先 Confirmed;包间覆盖 Likely 已标注留对拍 | `yuris-resource/src/lib.rs` 头注 |
| 系统变量读路径语义 | 帧局部优先、越界=类型默认值、id<1000 无全局回退——与引擎一致 | 成果 52/57,`eval.rs:140-200` |
| 求值器未证实语义 | 显式报错(`UnresolvedOpcode`/未定义变量报错),不猜 | `eval.rs:1-8` |

---

## P0 —— 行为错误(同作即错,非跨作)

> **状态(2026-10-16)**:T20 已完成(成果 144);T21 已完成(成果 145,与待办01 T1 合并定稿);
> T22 已完成(成果 146)。p0_audit 测试文件:`crates/yuris-vm/tests/p0_audit.rs`。
> 下一批 = P1(先 T23 → T24/T4/T5/T6 → T25)。

### T20. `@115/@116` 屏幕宽高写死 oracle 值,且与 WINDOWINFO 应答路径**互相矛盾**
- **状态:已完成(成果 144)**
- **审查发现**:表达式窗口读 `@115/@116` 在求值器里硬编码 `Int(1920)`/`Int(1080)`
  (`yuris-script/src/eval.rs:159-166`,注释自证取自 **oracle 环境** = <game-a> 的 1920×1080);
  而 `WINDOWINFO NO=0 SX/SY` 应答走 `GroupVm::screen_w/screen_h`(`lib.rs:2095`,由 boot 从
  游戏自身 yscfg 注入,<game-b> = **1100×825**)。**同一个逻辑变量,两条读路径答案不同**;
  <game-b> 脚本里 `@1074 != @115` 这类比较(注释原例即 s41 g104)按表达式路径必然算错。
- **动作**:求值器比照 `loop_counter/cursor/timer_ms` 的注入模式增加 screen 字段,由 VM 每次
  求值前注入 `screen_w/h`;删除 eval.rs 的两个常量。
- **验收**:<game-b> 下 `@115==1100 && @116==825`;WINDOWINFO 与表达式两路径同值;双版本回归。

### T21. 文本解码是**跨层系统性**固化(VM 层写死 SJIS,剧本层写死 Big5)
- **状态:已完成(成果 145;与待办01 T1 合并定稿)**
- **审查发现**:待办01 T1 只记了剧本层 Big5(`scenario.rs:197`);本轮发现 VM 文本路径
  (`WORD`/`TEXT` 事件→显示)**写死 SHIFT_JIS**(`player-core/lib.rs:999`)。两层方向相反:
  旧繁中样本在 VM 层会乱码,<game-b>(CP932)在剧本层会乱码——各自潜伏只是因为台词
  恰好走另一条路径。
- **动作**:与 T1 合并为一个**编码策略任务**:判定一次、全层生效(数据侧判定或全局
  encoding 配置),`WORD`/`TEXT`/剧本/标题 caption 全部走同一策略。
- **验收**:两作台词显示均正确;`cp932_lookup` 类探针扩展到文本路径。

### T22. `MATH` 完全未实现(HASH/RND),脚本依赖其返回值时拿到原值
- **状态:已完成(成果 146)**
- **审查发现**:`MATH` 归 `Subsystem` 只做参数求值+事件记录(`exec/misc.rs:298`),LET 写回不发生。
  引擎语义已有双线证据(旁线仓库):`MATH HASH`=**标准 CRC-32**(多项式 0xEDB88320,
  检查值 `"123456789"→0xCBF43926`)、`RND`=**MT19937**。本仓库全库 0 命中这两个算法。
  <game-b> 语料实测 **MATH 21 组**——凡 `LET=@x` 的用法,脚本读到的都是变量原值。
- **动作**:按 AGENTS §4 证据实装 CRC-32 与 MT19937(证据自由互通;若搬 旁线仓库 代码,
  方向 旁线仓库→本仓库合规,过 GPL 门即可);接 MATH 处理器的 LET 写回。
- **验收**:`"123456789"→0xCBF43926` 向量测试;RND 序列与引擎/另一条线对拍;语料 21 组的
  写回值不再恒等原值。

---

## P1 —— 功能缺口(<game-b> 语料实测频度)

### T23. 影片/音频/效果命令族全落 `Other`:MOVIE 6+MOVIEEND 2+MOVIEINFO 4+YMV 3+YMVEND 2+YMVINFO 4、MIDI 33、FLASH 3、CLIPACT 2
- **状态:全部完成(成果 149 + 151,2026-10-16)**。
  INFO 四族 + END 四族(成果 149);**播放族按决策 (A) 实装**(成果 151):
  `MediaPlay`(MOVIE/FLASH,置场景媒体模式位)、`MediaPlayLayer`(YMV,影片图层)、
  `MediaPlayAudio`(MIDI,音频)—— 记录 + 立即推进,**不编造时长**;
  屏蔽语义由 `Scene::media_mode` / `drawable_layers` 复制(手册原文背书)。
  实测 strict 200 拍媒体零挂起;lenient 8000 拍媒体族 0 次 Unsupported。
  另:`CLIPACT`(2 组)属剪贴板族,不在此列(另行挂号)。
- **审查发现**:以上命令名都不在 `CmdKind::from_name` → 全部 `Other` → **默认 strict 直接挂起**;
  实践中被迫全程 `--lenient`(记录后推进),而 lenient 同时会掩盖真正的语义错误。
  开场影片(OP)大概率就是第一处 MOVIE。
- **动作**:分两步——(a) 查询族(MOVIEINFO/YMVINFO/…INFO)按手册/引擎实装应答(至少
  EXIST/PLAY 状态的忠实降级);(b) 播放族(MOVIE/YMV)做**决策**:无视频解码器时,
  「记录+立即推进」与「阻塞至引擎同款时长」哪个忠实,由仓库主人定;`MIDI`(33 组)与
  `FLASH` 族按手册实装最小语义或显式 Unsupported。
- **验收**:strict 模式能跑通 <game-b> 片头(不再被迫 lenient);决策记录进 PROGRESS。

### T24. 引擎系统变量(id<1000)case 表未全量取证,未实现项=未定义报错
- **审查发现**:已实装 @1[1](毫秒)/@48(LOOP 计数)/@114(=32bpp)/@115/@116(见 T20,错值)/
  @133/@138(光标);引擎派发点 `0x447ebc`/`0x447d4c` 的 id<1000 二次派发 case 全表从未 dump,
  其余 sysvar(@2? @3? 鼠标键/窗口态等)当前走普通查表 → 未定义报错。
- **动作**:REA dump `0x447ebc` 的 case 表(逐 case 语义);已证实的实装,未用到的挂号。
- **验收**:case 全表入 docs;每个 sysvar 三态(已实现/Unsupported/Unknown)记录。

### T25. F5/F9 快存**不是完整快照**(书签式存档)
- **审查发现**:存档 JSON 仅 `{file, label, globals[0..64]}`(`player-core/lib.rs:3035-3038`),
  `restore_from` = 恢复 64 个 INT 全局 + 隐藏层 + **跳回 file/label 重放**(:3056-3087)。
  没有:FLT/STR 全局、数组、GOSUB 帧/局部、IF/LOOP 栈、任务位、场景状态。
  原生 SAVE/LOAD 是全量快照(旁线仓库 线:12 个自备槽按引用读写 + 全状态)。
  当前行为下,「非声明点存档→读档」会回到标签重放,变量面不全。
- **动作**:中期任务——扩快照域至 VariableStore 全量 + 执行位 + Scene 序列化;
  或最低限度:在 UI/文档显式登记「书签式存档」限制,防误判为对齐。
- **验收**:存→读→续跑后变量面与存档时刻一致(以探针 diff 全局/帧);或限制显式登记。

### T26. BlendMode 精确实装 8/19(对待办01 T9 的精确化)
- **审查发现**:`yuris-scene::BlendMode` 声明全 19 档(0..18),`is_implemented()` = **8 档**
  (Normal/Add/Subtract/Multiply/Screen/Lighten/Darken/Exclusion,`yuris-scene/lib.rs:151-163`);
  未实装档位由调用侧拦截**跳过**(不做近似,合规,挂号 G11 着色器未取)。第一轮 T9 写
  「8 种精确化」即此;本轮补齐精确清单:未实装 = Divide/Overlay/ColorDodge/ColorBurn/
  LinearBurn/HardLight 及 15..18。
- **动作**:并入待办01 T9 执行(规格已在 `docs/engine/render/03-blend-modes.md` §2/§4)。
- **验收**:同 T9;三态清单逐档记录。

---

## P2 —— 结构/工程

### T27. `yuris-expr` 死 crate + 死依赖
- **状态:依赖边已断(成果 150,2026-10-16)**;crate 目录保留待主人审阅
  (未删文件,只在 `yuris-vm/Cargo.toml` 断依赖 + 留注释)。
- **审查发现**:`yuris-expr` 仅 13 行骨架;`yuris-vm/Cargo.toml:18` 声明依赖但 `src/` 零使用
  (`grep yuris_expr` 0 命中)。表达式实际由 `yuris-script` 承担。
- **动作**:删依赖与 crate(或留壳但在 workspace 注明 unused)——纯搬运级清理。
- **验收**:`cargo test --workspace` 全绿,依赖图无 yuris-expr。

### T28. YSVR `kind==3`(910 条)语义 Unknown 却已被应用
- **审查发现**:`ysvr.rs:11` 注释 kind3 = 「其他(精确语义 Unknown)」;`boot.rs` 应用
  kind1+kind3、kind2 暂存。铁律角度:kind3 的「应用」缺语义依据,当前行为是按 kind1 同法
  处理的**未标注假设**。
- **动作**:REA/数据取证 kind3 与 kind1 的处理差异;确认前在代码标注 UNVERIFIED 或收敛为
  仅应用 kind1(以取证为准)。
- **验收**:kind3 三态明确;初值面与实机对拍。

### T29. CLI 输入面 vs 引擎输入惯例
- **审查发现**:CLI 只处理 Esc/Enter/Space/F5/F9/左键(`main.rs:514-557`);引擎惯例的
  Ctrl 强制跳过、右键系统菜单、滚轮/历史回看、Auto 模式均无;`INPUT` 命令与
  `yuris-input`(空骨架)未实装;`$55` 键串注入已有但输入源只有上述六种。
- **动作**:列引擎输入真值清单(手册/trace),按序补;`yuris-input` 作为实现位。
- **验收**:输入三态清单;可跳过/可菜单的实机行为。

### T30. 注释层残留 v555 序号(低危,顺手清)
- **状态:已完成(成果 150 附带)**。`exec/misc.rs` 子系统族注释改为按命令名列举;
  TASK 注释去序号。零行为变化(仅注释)。
- **审查发现**:`exec/misc.rs:296-299` 等处注释仍以 `0x1b FONTINFO/0x3c MATH…` v555 序号
  标注命令(分派本身已按名,行为无误);与 `cmd` 模块「留存不参与分派」的说明并存,
  易误导后来者。
- **动作**:注释统一改为「命令名(CmdKind)」表述,序号只留在 `cmd` 模块与版本文档。
- **验收**:grep 抽查;零行为变化。

### T31. 剧本层对 <game-b> 实际**休眠**(对待办01 T17 的补充事实)
- **审查发现**:CLI 写死的剧本入口是 `scenario\scenario_start.txt`,而 <game-b> 的剧本
  位于 `data\script\userscript\scenario_start.txt`(成果 133)——CLI 的 start 路径对本作
  **不命中**,剧本播放器实际没参与 <game-b> 流程(台词走 `WORD`)。T1/T17/T21 的修复
  前需先决定:剧本子系统对本作是否启用、入口真值从哪来。
- **动作**:并入 T17 一并决策与实现。
- **验收**:同 T17,另加「<game-b> 剧本链可达」验证。

---

## 执行顺序建议

T20(两路径矛盾,一行级修复收益最大)→ T22/T21(编码与 MATH,都有现成证据)→
T23(解除 strict 被迫 lenient)→ T24/T25 → 其余 P2。
待办01 的 P0(T1/T2/T3)与本轮 T20/T21/T22 同属「行为正确性」,建议同一批做,
编码策略(T1+T21)一次定稿。

## 验收总则(同待办01 §验收)

- 可复现验证命令 + 原始结果;PROGRESS.md 成果 N;勘误留痕。
- `cargo test --workspace --no-fail-fast` 全绿(基线 243 passed / 0 failed / 1 ignored)。
- 行为改动附双版本(555/494)同语义测试;引擎语义项 REA 取证记 evidence ID。
