# yuris-stage — 渲染编排核心(Scene / Stage Orchestration)

> 2026-10-16 立项(用户决策:「打补丁没有出路,以通用游戏引擎为核心做超集重写渲染编排层」)。
> 本文件是该核心的规格与不变量;实现 = `crates/yuris-stage`。
> 证据规范与 AGENTS.md 一致:每条语义标注来源(Confirmed / Likely / UNVERIFIED)。

## 0. 一句话

**Stage = 一台确定性画布编排机**:吃 `Command` 流、产出 `FrameView`;引擎(YU-RIS)的
CG/CGACT/TEXT/截屏机制实现为它的指令集超集。玩家/播放器只是把 VM 事件翻译成指令,
不再自持状态。

## 1. 为什么重写player-core 的 CG 消费段

旧实现是「事件→直接改场景」

- 一场切换的中间态会多帧泄漏。证据:窗口化 trace(<game-b>_win_trace.log)
  - f=137 config 页 E=1,到 f=219 才 E=0(82 帧曝光窗)
  - f=218 CGS100 A=0(揭幕窗)与中途重绘相互曝光
  - f=244 到 f=360 SPL401 产物纹理丢失(1×1)
  - BOOT 全程可见白点=(TEXT→SC_TEXT 层,boot 不该可见)
  原版(用户确认):无 config 抢屏、无闪、无白点。
  结论是编排层的分解错误而各归各因以下的层。不是渲染器的问题:每帧渲染当前状态
  是正确的;**错的是"当前状态"被切成多帧出现**。

## 2. 核心不变量(每一条可测试)

| 不变量 | 内容 | 证 |
|---|---|---|
| I1 属性槽保持 | patch 未给的槽 = 保持原值(引擎 REA `02-layer-model`:层=有属性槽的持久对象) | Confirmed |
| I2 patch 从不解码/不改纹理 | 只有 FILE 族操作或 Copy/Snapshot 会换图(属性槽理论完毕) | Confirmed |
| I3 新层缺省 E=OFF | 层对象初值不可见;只有显式 E=1 点亮(成果 154 实证) | Confirmed |
| I4 批原子 | 批内可见序列不会被呈现;呈现 = 批提交后(对应引擎 "task到WAIT→draw" 一帧) | Confirmed(架构推导,引擎帧循环含一次 draw) |
| I5 CGEND=销毁 | 层对象移除+销毁;复活必须走新 FILE 族创建(手册「CGレイヤ削除」) | Confirmed |
| I6 Snapshot 即时 | COPY2-from-aux 取**批起始景 + 批内已执行命令的投影**(引擎同步表语义;旧版取纯批前视图 → 黑幕自锁,成果 158) | Confirmed(2026-10-16 修正) |
| I7 组=离屏合成 | SX/SY 的组(无 FILE)子层先合成,组属性作用于合成结果 | Confirmed(REA) |
| I8 Solid + SX/SY = 尺寸了 | FILE="" + SX/SY = 拉伸到声明尺寸纯色块(es.CGTBLACK) | Confirmed |
| I9 Text 与窗口态绑定 | 文本层只在消息窗载体(`MESW*` / `*meswindow*`)`E` 槽点亮后才可呈现;boot 无窗 ⇒ 原版无白点(成果 158,启发式已退役) | Confirmed(语料载体物证) |
| i10 文件主存 | texture 所有权单源:file-backed 按 fnv(path),产物按 fnv(目标+代+窗, snapshot per-target per-shot) | Confirmed |

## 3. 对象模型

```
Stage                       — 唯一状态容器
 ├─ layers: slab<LayerId, Entity>
 ├─ order: z 顺序(f64 插值排序)
 ├─ windows: aux 子窗口表(:W0..W9)
 └─ batch_lock: 批=正在积累的 commands

Entity
 ├─ slots: SlotSet(E/X/Y/Z/A/MODE/T/TID/TA/MID/SX/SY/…)
 ├─ pixel: PixelOwner — ⚠️ change ONLY through image ops:
 │    File(path) | Solid(color) | Product(Rid) | Snapshot(Rid) | Alias(LayerId)
 └─ bbox/clip 等(视野投影)
```

指令集(与 VM 事件 1:1,由叠加翻译层在线映射):
`CreateLayer / PatchSlots / KillLayer / BlitCopy / BlitSize / SnapshotFrom / GroupDeclare`
`TextShow / TextClear / …`

批:VM 的 run 一次返回(= 人为让出点,边界与引擎 task 让出一致)为一个批;
呈现只消费已提交的 (=绘制前的最后状态)。批内出现 SnapshotFrom = 取样"上一已提交帧"
(引擎=命令执行瞬间的曾呈现画面;CMDH 证明 COPY2 链是以呈现为取样源的)。

## 3.5 已落地的语义扩展(随实施滚动)

- **CopyAlias 指令**(2026-10-16 收口,成果 157):CGACT[COPY/COPY2] 模板层 →
  目标层合成结果复制。实体像素所有权新增 `Pixel::Alias(LayerId)`;提交时写
  `scene.pack_layer.alias_of`,渲染取源层合成结果(老 `alias_wanted` 通道退役)。
- 启动确定性:Stage 的批原子呈现把「同类 CG 分批时序漂移」消灭 — 3 boot 帧
  SHA 一致实测。

## 4. 里程碑

- M1(已完成,成果 155-157):stage crate + 语义单测 + player-core 的 CG/CGACT 路径改走 Stage。
- M2(已完成,成果 158):**旧手工 CG 消费段整体退役** —— 全部 CG 族(含精灵族
  SPL/DXBUF/CP*)统一译成 Stage 指令;TEXT/消息窗状态机迁 Stage(I9 从启发式
  转正为窗载体 `E` 门控);截屏语义修正(批中投影,灭黑幕自锁);
  按钮注册表重放改走 Stage 批。player-core 只剩音频/输入桥接 + 内置标题/
  子画面(输入面)。
- M3(进行中):把 winit/wgpu 环节改造为「stage.frame_view / Scene」纯消费
  (渲染器已如此:只读 `Scene` 绘制,不再自持层状态);Android 壳同构。

## 5. 已知开放项

- CGS1(第二块截屏层)的用途、z 恒 None:待其实际 `GOSUB` 序列案例。
- 文本系统的 window-态真值(引擎 TEXT 子系统属性),M2 分支解决。

## 3.6 域调度下的批语义(P10,成果 161)

调度侧(`GroupVm::pump_frame`,2026-10-17)改为引擎真值的单线程协作式复刻后,
批的边界与合成规则收敛为三条不变量:

1. **帧 = 一次域泵的结果**。`Player::tick` 每帧调一次 `pump_frame(now)`,把主线域
   与全部任务域各泵到让出点(WAIT / 文本 / 结束 / 失控预算);一个 tick 内产生的
   全部事件构成该帧的合成输入。**批边界(单次 run 的返回)不再具有呈现语义**。
2. **帧内序 = 域泵序**。主线域事件先、任务域按创建序后;域标记随事件出流
   (`GroupVm::event_domains()`,0 = 主线,1..= 任务创建序)。消费侧(`consume_events`)
   按此顺序线性翻译即可复现引擎可见层;引擎内不乱序的机制是 BT.* 族各建在独立的
   模板-状态层命名空间(成果 159),不是全序约定。
3. **呈现仍是批原子快照**。帧内全部 Stage 指令应用完毕后的 `Scene` 状态 = 本帧
   绘制输入;SnapshotFrom(截屏)取"上一已提交帧"。确定性验收(3 次 boot 帧
   像素 SHA 一致)以 `pump_frame` 的单时钟读数(`now` 参数)与固定域序为基础;
   `WAIT TIME` 域的到期判定随墙钟走,boot 全段无 TIME 型 WAIT(成果 160)故
   boot 帧序与墙钟无关。

红线(重申):z 排序语义不变(引擎真值 = z 升序、同 z 创建序);不写层名/按钮名
白名单;域标记不进 golden JSONL。
