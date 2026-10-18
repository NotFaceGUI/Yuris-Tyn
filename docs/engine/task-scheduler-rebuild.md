# 任务调度器重建:差异审阅(`crates/yuris-vm/src/task.rs`)

> **状态:重建未接线。** `task.rs` 是按官方真值(成果 164,REA Confirmed)从零
> 重建的调度器,以独立模块形态与现行实现并存;`GroupVm::pump_frame`/
> `step_tasks_at`、`run()` golden 路径、player-core `tick` 均未改动。
> 本文档 = 审阅底稿:逐项对照「官方真值 / 现行实现 / 重建模块」,审阅通过后
> 按 §4 接线,不通过则整文件可弃,零回收成本。

## 1. 三方对照

| # | 维度 | 官方真值(Confirmed,除注明) | 现行 `GroupVm` | 重建 `task.rs` |
|---|---|---|---|---|
| 1 | 执行位模型 | 单一扁平任务链(`FUN_004046cc` 沿 next 取件),主线脚本是链中一员 | 主线 = `self` 执行位,任务 = `tasks: Vec<VmTask>`,两套路径 | 统一 `nodes: Vec<TaskNode>`,无主线特例 |
| 2 | 帧与上屏 | **逻辑帧与上屏解耦**:TASK 置 `DAT_00802c1c` ⇒ do-while 连跑 K 个逻辑帧,链间不渲染 | `tick = pump_frame + consume + 渲染`,1 逻辑帧 = 1 上屏 | `run_frame_chain` = 官方 do-while 同构;调用方链后渲染一次 |
| 3 | TASK 续帧 | `0x450ae9: MOV [0x802c1c],1`(处理器直写全局) | 无此原语 | `Effect::RequestExtraFrame` / `request_extra_frame()`,每条命令后同拍应用 |
| 4 | 每帧每任务一步 | 帧号戳(记录 +0x00..7,`0x404402`/`0x40440e`) | 结构性保证(`step_tasks_at` 每帧调一次) | `last_frame` 戳,逐字节对应官方 |
| 5 | 到期判定 | FRAME(+0x18)每访问递减;TIME(+0x1c)= 绝对 `timeGetTime()` 比较,到期清零 | 双条件等价(`wait_frames`/`resume_at: Instant`) | 同官方(+0x18/+0x1c 语义);**时钟口径 = u32 绝对 ms**(D-1) |
| 6 | 命令泵 | 跑到处理器返回非零,**无预算** | `run_main(512)` 循环 × `FRAME_GROUP_BUDGET=6000` | 无限泵;`YURIS_TASK_PUMP_CAP` 仅作失控护栏(非引擎常量,仿 `TASK_QUANTUM` 先例) |
| 7 | 步进入口链长快照 | 计数在入口读定(`0x40438d`),步进中新建任务留到下一逻辑帧 | `let n = self.tasks.len()` 同语义 | 同官方 |
| 8 | 启用位 | `record+0x15 & +5` 位测(`0x4043c9`);置/清命令待证 | `enabled: bool` | `enabled: bool`(接线前恒 true) |
| 9 | 任务树 | 顶层扁平链 + `+0x30` 子树容器(`0x404679` 递归**传任务对象为链根** ⇒ 容器自身不执行、递归步进 children;结构 Confirmed,置位命令待证) | 无对应概念 | `sync_subtree` + `children`,分支保留、不可达(置位命令未取证),单测锁定容器行为 |
| 10 | 错误路径 | 未取证 | 任务错误 → 事件 + finished | 同现行(Likely),`ChainReport.errors` 留痕 |
| 11 | 任务删除 | TASKEND `LAB_00452070`;对象删除时机未取证 | 运行期不删 | 软结束(`FinishTask`/`finish_by_id`),不物理删除(同现行) |

## 2. 与官方的已知偏差(重建内部,均低风险)

- **D-1 时钟口径**:官方逐任务调 `timeGetTime()`(winmm,ms);重建按链收一个
  `now_ms` 参数。同一逻辑帧内读数差微秒级;接线时由调用方传
  `timeGetTime` 等价物(u32 绝对毫秒,回绕语义与官方一致,不会永久挂起)。
- **D-2 入链位置**:官方 TASK 建对象的链入点未逐字节取证,按「链尾」重建
  (与现行 `tasks.push` 一致;若后续取证为链头,只改 `spawn` 一行)。
- **D-3 `FINISH` 结果**:官方「任务体走完」是处理器副作用(记录 +9),重建用
  `StepOutcome::Finish` 表达,由执行器映射 —— 语义等价,形态不同。

## 3. 接线计划(审阅通过后另行实施,本文件不含)

1. `GroupVm` 实装 `TaskExecutor`:`step_command` 包现有 exec 分派(参数求值、
   事件发射、pc 先自增再调用,`0x404502`);TASK 处理器改发
   `Effect::{RequestExtraFrame, SpawnTask}`。
2. `Player::tick`:`run_frame_chain` 替代单次 `pump_frame`,链后一次
   `consume_events` + 渲染(事件天然合批 = 官方「呈现链末状态」)。
3. 主线脚本 = 首个 `spawn`(链序第 0 位);`main_suspend`/TEXT 等待经
   `find_by_id` 查询。
4. **回归面**:`run()` 与 golden JSONL 不动;`p10_scheduler.rs` 5 例语义
   (TIME 期限/主线休息不冻结任务/裸让出一帧/域标/确定性)需在接线 PR 中
   以新调度重述;`FRAME_GROUP_BUDGET` 退役改告警。
5. 实机验收门(不变):boot f1–f620 配置页零曝光;logo→title 无纯黑段;
   3 次 SHA 一致;f600 六钮。

## 4. 未取证挂号(实施前须补 REA)

| 项 | 地址 | 问题 |
|---|---|---|
| A | TASK 处理器 `LAB_00450588` 内 `0x450ae9` 所属分支 | 置续帧是「仅创建」还是「重配置也置」? |
| B | `+0x30` 树标 | 哪条命令置位?递归语义 = 子树同步的推断是否成立? |
| C | 启用位 `+0x14/0x15/+5` | 哪条命令清位(TASK E=0?) |
| D | TASKEND 对象删除时机 | 链内即时 / 链末清扫? |
| E | worker 线程体 `0x425508` | 职责未知(成果 164 勘误后悬置) |

## 5. 本模块自测

`cargo test -p yuris-vm --lib`(`crates/yuris-vm/src/task.rs` `mod tests`,
12 例):一帧一步 / FRAME 先减后判(FRAME=1 当帧即醒、FRAME=2 休一帧)/
TIME 到期清零 / TASK 续帧 + 新任务次帧首步 / 续帧标志帧首清零 / 链序 =
创建序 / 无预算泵 / 护栏仅显式生效 / finished+enabled 跳过 / 容器不执行而
子链执行 / TASKEND 软结束 / 错误路径。

**承接真实脚本证明**(`cargo test -p yuris-vm --test p11_task_bridge`,5 例):
桥 = `GroupVm::run(1)`(公开 API,恰为「泵一组」)适配成 `TaskExecutor`,
`VmSuspend` → `StepOutcome` 映射,WAIT 副作用显式写回 `wait_frames`(=
官方 WAIT 处理器写任务 +0x18)。手写合成 YSTB 字节码脚本(`INT`/`WAIT`/`END`)
在调度器下逐链推进:WAIT 让出落在逻辑帧、FRAME=1/2 的苏醒帧序与官方递减
语义一致、变量/声明事件逐项可查、泵条数确定(=8);任务走完经
`Effect::FinishTask` 同帧杀同胞任务(与 TASK/TASKEND 同一条副作用通道)。
现行路径零改动、`GroupVm::run()` 零回归。

**淡变动画与时长**(同文件 3 例,展开的「A 步进 + WAIT」脚本形态):
- 步进节奏:WAIT FRAME=1 ⇒ 每逻辑帧恰一步,3 步淡变在第 1/2/3 链各走一步、
  第 4 链 END(时长 = K 逻辑帧);
- 节奏缩放:WAIT FRAME=2 ⇒ 两逻辑帧一步(时长随 WAIT 缩放);
- 官方压缩语义:TASK 开火的续帧逻辑帧**同样推进淡变**(WAIT 递减不分
  上屏帧/续帧)⇒ TASK 密集段 2 步淡变被压进一次上屏 —— 与官方 boot 转场
  行为同构,时长口径 = 逻辑帧(脚本真值),无 TASK 段 1 逻辑帧 = 1/60s。
