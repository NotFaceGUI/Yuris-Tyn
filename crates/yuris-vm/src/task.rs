//! 官方任务调度器(成果 164/165 真值;**本版已接线**)。
//!
//! 重建对象 = <game-b>_v494.exe(sha256 `1ff3135d…`)的调度三层:
//! WinMain `FUN_00403bec` → 帧驱动 `FUN_004040e8` → 帧步进 `FUN_00404388`
//! (取证:PROGRESS 成果 164;REA ev_2c34f93f… / ev_43197c98… / ev_94ec19bd…)。
//!
//! 与旧「主线域 + 任务域」双轨实现的三条本质差异(动机,全部 Confirmed):
//! 1. **逻辑帧与上屏解耦**:TASK 命令置续帧标志(官方 `0x450ae9`),一次 vsync
//!    内连跑 K 个完整逻辑帧、链期间不渲染;旧实现 1 逻辑帧 = 1 上屏。
//! 2. **单一扁平任务链**:主线脚本只是链中一员、按链序步进(官方 `FUN_004046cc`
//!    沿 next 逐个取件);旧实现主线域恒先 + 任务按创建序。
//! 3. **命令泵无预算**:到期任务从当前 pc 泵到让出(官方无任何条数上限);
//!    旧实现 `FRAME_GROUP_BUDGET` 会在 boot 密集段人为切批(官方不存在批边界)。
//!
//! 接线位置:[`crate::driver`] 的 `GroupVm::run_frame_chain` /
//! `step_command`(见 [`TaskExecutor`])。`GroupVm::run()`(golden 直连路径)
//! 保持「一次调用 = 一个组预算」的旧契约,只经 [`TaskScheduler::step_tasks_only`]
//! 推进任务,不改变既有测试语义。
//!
//! 证据等级遵循 AGENTS §1:每个字段/分支注明官方地址;未取证处显式
//! `待证`,不猜语义。

/// 工程护栏环境变量(非引擎常量;官方命令泵无上限)。设置后单任务单逻辑帧
/// 的命令泵超过该条数即中断并计 [`ChainReport::pump_cap_hits`],用于失控
/// 对拍;缺省 `None` = 与官方同构的无限泵。
pub const TASK_PUMP_CAP_ENV: &str = "YURIS_TASK_PUMP_CAP";

/// 主线节点在链中的 ID(执行期不参与 TASKINFO 查询 —— 引擎的 TASKINFO
/// 按名字查任务表,主线脚本不是 TASK 产物;此名带 `@` 前缀,不与脚本
/// 可用的标识符字符集冲突)。
pub const MAIN_TASK_ID: &[u8] = b"@main";

/// 一条命令的执行结果(官方 = 命令处理器的返回字节,`FUN_00404388`
/// `0x4044ce: MOV [0x8027f8], AL`;非零 = 让出)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// 返回 0:继续泵下一条。
    Continue,
    /// 返回非零:让出,本任务本逻辑帧结束。
    Yield,
    /// 任务体走到 END/基帧 RETURN:任务记录 +9 置位(官方 `0x404418` 检查)。
    Finish,
}

/// 命令处理器在执行期间产生的调度副作用(官方由 TASK/TASKEND 处理器直接写
/// 全局:`0x450ae9` 置续帧标志、建任务对象入链)。执行器把副作用排队,调度器
/// 在每条命令后立即取走应用 —— 与官方「处理器直接写全局」同拍。
#[derive(Debug, Clone)]
pub enum Effect {
    /// TASK 命令 → `MOV dword ptr [0x802c1c], 1`:本逻辑帧结束后立即再跑一个
    /// 完整逻辑帧(不渲染)。
    RequestExtraFrame,
    /// TASK 命令建任务对象。入链尾 ⇒ 步进入口的链长快照之外 ⇒ 下一个逻辑帧
    /// 才被首次步进 —— 官方续帧机制恰好让这「下一帧」立刻发生。
    ///
    /// **upsert 语义**:同 ID 已存在 = 官方「任务重配置」(任务表按名查;
    /// `p5_taskinfo` 语料 `TASKINFO EXIST` + 随后 `TASK` 亦即此形态)。
    SpawnTask(TaskSeed),
    /// TASK 重配置但**没有新入口标签**(`#` 槽缺省):只改启用位。
    SetEnabled(Vec<u8>, bool),
    /// TASKEND(官方 `LAB_00452070`):按 ID 结束。软结束(标 `finished`);
    /// 对象从链中删除的时机未取证,两版实现(旧/本模块)都不做物理删除。
    /// 查找范围 = 当前链(顶层链即全部任务)。
    FinishTask(Vec<u8>),
}

/// 新任务的种子(TASK 命令的 ID/SCRIPTPOS 参数)。
///
/// `id` 为**原始字节**(CP932/SJIS 任务名):`String::from_utf8_lossy` 会把
/// 日文任务名变成替换字符,导致 TASK / TASKEND / TASKINFO 三者匹配不上。
#[derive(Debug, Clone)]
pub struct TaskSeed {
    /// 任务名(脚本 `ID` 槽原始字节)。
    pub id: Vec<u8>,
    /// 入口脚本号。
    pub script_id: u16,
    /// 入口组号。
    pub pc: usize,
    /// `E` 槽:0 = 不参与调度。
    pub enabled: bool,
    /// 主线节点(= 链中一员,不是 TASK 创建的)。
    pub is_main: bool,
}

/// 命令执行器抽象。接线由 `GroupVm` 实装:执行一组(分派 + 参数求值 +
/// 事件发射),pc 推进保持官方「先自增再调用」(`FUN_00404388 0x404502:
/// 读 pc → +1 写回 → 按旧 pc 取处理器`)。
pub trait TaskExecutor {
    /// 泵一条命令。`task.pc` 由执行器在返回前同步;返回处理器结果。
    ///
    /// 错误路径(脚本执行失败):官方行为未取证,按「让出 + 任务结束 +
    /// 记录」处理(Likely;接线后转 `ChainReport::errors`,与旧任务错误
    /// 路径一致)。
    fn step_command(&mut self, task: &mut TaskNode) -> Result<StepOutcome, String>;

    /// 取走本条命令产生的调度副作用(缺省无)。
    fn drain_effects(&mut self) -> Vec<Effect> {
        Vec::new()
    }
}

/// 任务对象(官方 task 树节点;字段偏移为官方任务记录)。
///
/// 树结构(Confirmed,`FUN_00404388 0x404679` 递归把**任务对象**当链根传入、
/// `FUN_0042c394` 沿 `+0x00` 子指针 / `+0x0c` 兄弟指针递归):顶层是扁平链,
/// `sync_subtree` 节点是**子树容器**(自身不执行,递归步进 [`Self::children`]);
/// 哪条命令把任务建成容器/子节点**待证**,当前无路径写 `true`。
#[derive(Debug, Clone)]
pub struct TaskNode {
    /// TASK ID 槽(脚本侧名字;**原始字节**,CP932 保真)。
    pub id: Vec<u8>,
    /// 主线节点(链序第 0 位;由 `GroupVm::load` 建立,不是 TASK 产物)。
    pub is_main: bool,
    pub script_id: u16,
    /// 脚本 pc(官方记录 +0x20)。
    pub pc: usize,
    /// FRAME 型等待计数(官方记录 +0x18;每逻辑帧访问时递减 1,`0x40442a`)。
    pub wait_frames: u32,
    /// TIME 型期限(官方记录 +0x1c;**绝对 `timeGetTime()` 毫秒值**,`0x404435`
    /// 直接比较)。u32 约 49.7 天回绕;回绕后 `now < deadline` 不再成立 =
    /// 提前到期,与官方一致(不会永久挂起)。
    pub deadline_ms: Option<u32>,
    /// 启用位(官方 `0x4043c9`:`record+0x15 & record+5` 位测,为 0 则跳过;
    /// TASK 的 `E` 槽经 [`Effect::SetEnabled`]/[`TaskSeed::enabled`] 写入)。
    pub enabled: bool,
    /// 子树容器标(官方 `+0x30`;为 1 时 `0x404679` 递归步进 [`Self::children`],
    /// 自身不泵命令。**置位命令未取证** → 当前不可达)。
    pub sync_subtree: bool,
    /// 容器子链(`sync_subtree` 时被递归步进;非容器节点恒空)。
    pub children: Vec<TaskNode>,
    /// 任务体已走完(官方记录 +9,`0x404418`)。
    pub finished: bool,
    /// 上次被步进的逻辑帧号(官方记录 +0x00..7 的 64 位帧号戳,
    /// `0x404402` 比对 / `0x40440e` 写入)——「每逻辑帧每任务最多一步」的载体。
    pub(crate) last_frame: u64,
    /// 诊断:累计被步进次数。
    pub steps: u64,
}

impl TaskNode {
    fn from_seed(seed: TaskSeed) -> Self {
        TaskNode {
            id: seed.id,
            is_main: seed.is_main,
            script_id: seed.script_id,
            pc: seed.pc,
            wait_frames: 0,
            deadline_ms: None,
            enabled: seed.enabled,
            sync_subtree: false,
            children: Vec::new(),
            finished: false,
            last_frame: 0,
            steps: 0,
        }
    }

    /// 重配置(同 ID TASK):换入口、清等待、恢复运行位;`steps` 保留。
    fn reconfigure(&mut self, seed: TaskSeed) {
        self.script_id = seed.script_id;
        self.pc = seed.pc;
        self.wait_frames = 0;
        self.deadline_ms = None;
        self.enabled = seed.enabled;
        self.finished = false;
    }
}

/// 一次逻辑帧链(`run_frame_chain`)的量化报告。
#[derive(Debug, Default, Clone)]
pub struct ChainReport {
    /// 本链跑掉的逻辑帧数(官方 do-while 迭代数,含 TASK 续帧)。
    pub logical_frames: u32,
    /// 任务步进人次。
    pub tasks_stepped: u64,
    /// 命令泵条数。
    pub commands_pumped: u64,
    /// 触发工程护栏的次数(缺省配置恒 0)。
    pub pump_cap_hits: u64,
    /// 执行器报错(官方错误路径未取证 → 让出 + 结束 + 记录)。
    pub errors: Vec<String>,
    /// 主线节点本链内走完(END / 脚本尾)。
    pub main_finished: bool,
    /// 本链执行组数(诊断:慢帧定位;由 `run_frame_chain` 填)。
    pub groups_pumped: u64,
}

/// 官方任务调度器:单一扁平任务链 + 逻辑帧泵 + TASK 续帧标志。
///
/// 时钟口径:调用方按链传入 `now_ms`(官方在帧步进内逐任务调 `timeGetTime()`,
/// 同一逻辑帧内的读数差异为微秒级 —— 偏差 D-1,见审阅文档)。WAIT 处理器把
/// `timeGetTime()+ms` 换算成绝对期限时使用同一口径(执行器持有链起点值)。
pub struct TaskScheduler {
    /// 顶层任务链(链序 = 创建序;官方入链点未逐字节取证,按「建对象即入链尾」
    /// 重建 —— 与旧 `tasks.push` 一致,见审阅文档 D-2)。
    nodes: Vec<TaskNode>,
    /// 逻辑帧计数(官方 `DAT_00802540/44`,64 位)。
    frame: u64,
    /// TASK 续帧标志(官方 `DAT_00802c1c`;全二进制仅 3 个引用:清零
    /// `0x40413d` / 循环条件 `0x40417b` / TASK 处理器置位 `0x450ae9`)。
    extra_frame: bool,
    /// 工程护栏(非引擎常量;`None` = 官方同构的无限泵)。
    pump_cap: Option<usize>,
}

impl TaskScheduler {
    /// 读 [`TASK_PUMP_CAP_ENV`] 构造(诊断/对拍用;缺省无护栏)。
    pub fn from_env() -> Self {
        let cap = std::env::var(TASK_PUMP_CAP_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok());
        Self::with_pump_cap(cap)
    }

    pub fn with_pump_cap(pump_cap: Option<usize>) -> Self {
        TaskScheduler {
            nodes: Vec::new(),
            frame: 0,
            extra_frame: false,
            pump_cap,
        }
    }

    /// TASK 命令建任务:入链尾,返回链下标。
    pub fn spawn(&mut self, seed: TaskSeed) -> usize {
        self.nodes.push(TaskNode::from_seed(seed));
        self.nodes.len() - 1
    }

    /// TASK 重配置(同 ID):更新入口/启用位;不存在则等价 [`Self::spawn`]。
    pub fn upsert(&mut self, seed: TaskSeed) -> usize {
        if let Some(i) = self.nodes.iter().position(|n| n.id == seed.id) {
            self.nodes[i].reconfigure(seed);
            return i;
        }
        self.spawn(seed)
    }

    /// 建容器子任务(`parent` 须为 `sync_subtree` 节点;置位命令未取证,
    /// 当前仅供单测验证递归结构)。
    pub fn spawn_child(&mut self, parent: usize, seed: TaskSeed) -> usize {
        let node = &mut self.nodes[parent];
        node.sync_subtree = true;
        node.children.push(TaskNode::from_seed(seed));
        node.children.len() - 1
    }

    /// 直接置续帧标志(官方 TASK 处理器 `0x450ae9` 的等价入口;接线后由
    /// 执行器经 [`Effect::RequestExtraFrame`] 走同一路径)。
    pub fn request_extra_frame(&mut self) {
        self.extra_frame = true;
    }

    /// TASKEND 软结束:按 ID 标 `finished`(仅顶层链;对象删除时机未取证,
    /// 不物理删除 —— 与旧实现「运行期不删任务」一致)。
    pub fn finish_by_id(&mut self, id: &[u8]) -> bool {
        match self.nodes.iter_mut().find(|n| n.id == id) {
            Some(n) => {
                n.finished = true;
                true
            }
            None => false,
        }
    }

    /// TASK 重配置的启用位(官方 `record+0x15 & +5`)。
    pub fn set_enabled(&mut self, id: &[u8], enabled: bool) -> bool {
        match self.nodes.iter_mut().find(|n| n.id == id) {
            Some(n) => {
                n.enabled = enabled;
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, index: usize) -> Option<&TaskNode> {
        self.nodes.get(index)
    }

    pub fn node_mut(&mut self, index: usize) -> Option<&mut TaskNode> {
        self.nodes.get_mut(index)
    }

    pub fn find_by_id(&self, id: &[u8]) -> Option<&TaskNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// 顶层链只读遍历(诊断:任务摘要 / TASKINFO)。
    pub fn nodes(&self) -> &[TaskNode] {
        &self.nodes
    }

    /// 主线节点(链中 `is_main` 的那一位;不存在 = 未接线)。
    pub fn main_node(&self) -> Option<&TaskNode> {
        self.nodes.iter().find(|n| n.is_main)
    }

    /// 当前逻辑帧号(诊断)。
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// 官方 `FUN_004040e8` 的 do-while:跑一个**逻辑帧链**。
    ///
    /// ```text
    /// loop {
    ///     清续帧标志(0x40413d);逻辑帧计数 +(0x404147)
    ///     step_frame(0x404388)                    // 一个逻辑帧
    ///     if 未再请求续帧(0x40417b) { break }
    /// }
    /// ```
    /// 调用方在链返回后消费事件并渲染一次 —— 呈现 = 链末状态,链中间帧
    /// 从不上屏(与官方同构;这是「逻辑帧与上屏解耦」的落点)。
    pub fn run_frame_chain(&mut self, exec: &mut dyn TaskExecutor, now_ms: u32) -> ChainReport {
        let mut report = ChainReport::default();
        loop {
            self.extra_frame = false;
            self.frame = self.frame.wrapping_add(1);
            self.step_frame(exec, now_ms, &mut report, false);
            report.logical_frames += 1;
            if !self.extra_frame {
                break;
            }
        }
        if let Some(m) = self.main_node() {
            report.main_finished |= m.finished;
        }
        report
    }

    /// **旧 `GroupVm::run()` 兼容路径**:推进一个逻辑帧的**任务节点**
    /// (跳过主线 —— 主线的组预算由 `run_main` 自己管)。
    ///
    /// 语义与 [`Self::run_frame_chain`] 的单帧步进完全相同(FRAME 递减 /
    /// TIME 期限 / 每任务一步),只是不消费续帧标志:旧 API 一次调用不是
    /// 「vsync 链」,没有上屏边界可言。
    pub fn step_tasks_only(
        &mut self,
        exec: &mut dyn TaskExecutor,
        now_ms: u32,
    ) -> ChainReport {
        let mut report = ChainReport::default();
        self.extra_frame = false;
        self.frame = self.frame.wrapping_add(1);
        self.step_frame(exec, now_ms, &mut report, true);
        report.logical_frames = 1;
        report
    }

    /// 官方 `FUN_00404388`:遍历任务链一遍,到期任务各泵到让出。
    ///
    /// 链长在入口快照(`0x40438d` 读计数后递减遍历)——步进中 [`Effect::
    /// SpawnTask`] 新建的节点留在快照之外,留给下一个逻辑帧;TASK 续帧让
    /// 「下一个逻辑帧」立刻发生,新任务因此当次 vsync 内就能跑第一步。
    fn step_frame(
        &mut self,
        exec: &mut dyn TaskExecutor,
        now_ms: u32,
        report: &mut ChainReport,
        skip_main: bool,
    ) {
        let mut walk = Walk {
            exec,
            now_ms,
            stamp: self.frame,
            cap: self.pump_cap,
            report,
            extra_frame: false,
        };
        let mut nodes = std::mem::take(&mut self.nodes);
        let end = nodes.len();
        walk.run_list(&mut nodes, 0, end, skip_main);
        self.nodes = nodes;
        self.extra_frame |= walk.extra_frame;
    }
}

/// 单个逻辑帧的遍历状态(帧号戳 / 时钟读数 / 护栏 / 续帧标志都是**帧局部**
/// 量,与调度器字段分离以免跨递归借词)。
struct Walk<'a> {
    exec: &'a mut dyn TaskExecutor,
    now_ms: u32,
    stamp: u64,
    cap: Option<usize>,
    report: &'a mut ChainReport,
    extra_frame: bool,
}

impl Walk<'_> {
    /// 遍历一条链的 `[begin, end)` 区段(官方按入口计数快照递减遍历)。
    fn run_list(
        &mut self,
        nodes: &mut Vec<TaskNode>,
        begin: usize,
        end: usize,
        skip_main: bool,
    ) {
        let mut i = begin;
        while i < end {
            // —— 只读分支:启用位 / 子树容器(避免跨递归持有借词)——
            let (enabled, is_container) = {
                let nd = &nodes[i];
                (nd.enabled, nd.sync_subtree)
            };
            // 旧 run() 兼容路径:主线不在本函数的职责内。
            if skip_main && nodes[i].is_main {
                i += 1;
                continue;
            }
            // 启用位(0x4043c9;先于盖章 —— 与官方一致)。
            if !enabled {
                i += 1;
                continue;
            }
            // 子树容器(0x404679:递归调用传任务对象为链根 ⇒ 递归的是它的
            // children,不是本链;容器自身不泵命令)。
            if is_container {
                let mut kids = std::mem::take(&mut nodes[i].children);
                let kid_end = kids.len();
                self.run_list(&mut kids, 0, kid_end, false);
                nodes[i].children = kids;
                i += 1;
                continue;
            }
            let node = &mut nodes[i];
            // 每逻辑帧每任务最多一步(0x404402 比对)。
            if node.last_frame == self.stamp {
                i += 1;
                continue;
            }
            node.last_frame = self.stamp; // 0x40440e
            if node.finished {
                i += 1;
                continue;
            }
            // FRAME 递减(0x40442a:每访问一次;FRAME=1 ⇒ 下一逻辑帧即醒)。
            if node.wait_frames > 0 {
                node.wait_frames -= 1;
            }
            // TIME 期限(0x404435:`now < deadline` ⇒ 休息;到期清零)。
            let time_resting = match node.deadline_ms {
                Some(d) if self.now_ms < d => true,
                Some(_) => {
                    node.deadline_ms = None;
                    false
                }
                None => false,
            };
            // 到期判定(0x404460:`FRAME>0 || TIME 未到` ⇒ 本逻辑帧跳过)。
            if node.wait_frames > 0 || time_resting {
                i += 1;
                continue;
            }
            // 命令泵(0x404468..0x4044de):跑到处理器返回非零为止,无预算。
            self.pump_node(nodes, i);
            self.report.tasks_stepped += 1;
            nodes[i].steps += 1;
            i += 1;
        }
    }

    fn pump_node(&mut self, nodes: &mut Vec<TaskNode>, index: usize) {
        let mut pumped = 0usize;
        loop {
            if let Some(cap) = self.cap {
                if pumped >= cap {
                    // 工程护栏(非引擎常量):中断本任务的泵,下逻辑帧继续。
                    self.report.pump_cap_hits += 1;
                    break;
                }
            }
            let out = self.exec.step_command(&mut nodes[index]);
            // 副作用与官方同拍:TASK 处理器直接写全局 ⇒ 每条命令后立即应用。
            for effect in self.exec.drain_effects() {
                match effect {
                    Effect::RequestExtraFrame => self.extra_frame = true,
                    // 入链尾;入口链长快照之外 ⇒ 下一逻辑帧首次步进。
                    Effect::SpawnTask(seed) => {
                        if let Some(n) = nodes.iter_mut().find(|n| n.id == seed.id) {
                            n.reconfigure(seed);
                        } else {
                            nodes.push(TaskNode::from_seed(seed));
                        }
                    }
                    Effect::SetEnabled(id, on) => {
                        if let Some(n) = nodes.iter_mut().find(|n| n.id == id) {
                            n.enabled = on;
                        }
                    }
                    Effect::FinishTask(id) => {
                        if let Some(n) = nodes.iter_mut().find(|n| n.id == id) {
                            n.finished = true;
                        }
                    }
                }
            }
            match out {
                Ok(StepOutcome::Continue) => pumped += 1,
                Ok(StepOutcome::Yield) => {
                    pumped += 1;
                    break;
                }
                Ok(StepOutcome::Finish) => {
                    pumped += 1;
                    nodes[index].finished = true;
                    break;
                }
                Err(e) => {
                    // 官方错误路径未取证 → 让出 + 结束 + 记录(Likely)。
                    self.report.errors.push(format!(
                        "task {:?} pc{}: {e}",
                        String::from_utf8_lossy(&nodes[index].id),
                        nodes[index].pc
                    ));
                    if nodes[index].is_main {
                        self.report.main_finished = true;
                    }
                    nodes[index].finished = true;
                    break;
                }
            }
        }
        self.report.commands_pumped += pumped as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// 脚本化 mock:每个任务按预设结果序列回放;空序列 = 让出。
    /// `effects_on_first` 模拟「TASK/TASKEND 命令在该任务第一条命令里开火」。
    struct MockExec {
        scripts: std::collections::HashMap<Vec<u8>, VecDeque<StepOutcome>>,
        effects_on_first: std::collections::HashMap<Vec<u8>, Vec<Effect>>,
        calls: Vec<Vec<u8>>,
    }

    impl MockExec {
        fn new() -> Self {
            MockExec {
                scripts: std::collections::HashMap::new(),
                effects_on_first: std::collections::HashMap::new(),
                calls: Vec::new(),
            }
        }
        fn script(mut self, id: &str, outs: Vec<StepOutcome>) -> Self {
            self.scripts.insert(id.as_bytes().to_vec(), outs.into());
            self
        }
        fn effects_on_first(mut self, id: &str, effects: Vec<Effect>) -> Self {
            self.effects_on_first.insert(id.as_bytes().to_vec(), effects);
            self
        }
        fn count(&self, id: &str) -> usize {
            self.calls.iter().filter(|c| c.as_slice() == id.as_bytes()).count()
        }
    }

    impl TaskExecutor for MockExec {
        fn step_command(&mut self, task: &mut TaskNode) -> Result<StepOutcome, String> {
            self.calls.push(task.id.clone());
            task.pc += 1;
            Ok(self
                .scripts
                .get_mut(&task.id)
                .and_then(|q| q.pop_front())
                .unwrap_or(StepOutcome::Yield))
        }
        fn drain_effects(&mut self) -> Vec<Effect> {
            let Some(id) = self.calls.last().cloned() else {
                return Vec::new();
            };
            if self.calls.iter().filter(|c| **c == id).count() == 1 {
                self.effects_on_first.remove(&id).unwrap_or_default()
            } else {
                Vec::new()
            }
        }
    }

    fn seed(id: &str) -> TaskSeed {
        TaskSeed {
            id: id.as_bytes().to_vec(),
            script_id: 0,
            pc: 0,
            enabled: true,
            is_main: false,
        }
    }

    /// 无 TASK 的帧:1 逻辑帧;3 条链不会放大步进(每逻辑帧每任务最多一步)。
    #[test]
    fn one_step_per_logical_frame_without_task() {
        let mut s = TaskScheduler::with_pump_cap(None);
        s.spawn(seed("a"));
        let mut exec = MockExec::new().script("a", vec![StepOutcome::Continue, StepOutcome::Yield]);
        for _ in 0..3 {
            s.run_frame_chain(&mut exec, 0);
        }
        // 链1:Continue+Yield = 2 条;链2/链3:空脚本默认 Yield = 各 1 条。
        assert_eq!(exec.count("a"), 4);
    }

    /// FRAME 递减语义(0x40442a:先递减再判到期 ⇒ `FRAME=1` 当帧即醒、
    /// `FRAME=2` 休一帧后醒;与成果 161 勘误注释一致)。
    #[test]
    fn frame_wait_decrements_then_wakes() {
        let mut s = TaskScheduler::with_pump_cap(None);
        let i = s.spawn(seed("a"));
        s.node_mut(i).unwrap().wait_frames = 2;
        let mut exec = MockExec::new().script("a", vec![StepOutcome::Yield]);
        s.run_frame_chain(&mut exec, 0);
        assert_eq!(exec.count("a"), 0, "FRAME=2 的第一个逻辑帧应休眠(2→1)");
        s.run_frame_chain(&mut exec, 0);
        assert_eq!(exec.count("a"), 1, "第二逻辑帧递到 0 即醒(1→0)");
        assert_eq!(s.find_by_id(b"a").unwrap().wait_frames, 0);
    }

    /// TIME 期限:`now < deadline` 休眠,到期清零并苏醒(0x404435)。
    #[test]
    fn time_deadline_gates_until_due() {
        let mut s = TaskScheduler::with_pump_cap(None);
        let i = s.spawn(seed("a"));
        s.node_mut(i).unwrap().deadline_ms = Some(100);
        let mut exec = MockExec::new().script("a", vec![StepOutcome::Yield]);
        s.run_frame_chain(&mut exec, 99);
        assert_eq!(exec.count("a"), 0);
        s.run_frame_chain(&mut exec, 100);
        assert_eq!(exec.count("a"), 1);
        assert!(
            s.find_by_id(b"a").unwrap().deadline_ms.is_none(),
            "到期应清零(0x40444f)"
        );
    }

    /// TASK 续帧:置位 ⇒ 链跑 2 个逻辑帧;TASK 派生的新任务在第二个逻辑帧
    /// 被首次步进(0x450ae9 + 0x40438d 快照语义)。
    #[test]
    fn task_command_extends_frame_chain() {
        let mut s = TaskScheduler::with_pump_cap(None);
        s.spawn(seed("a"));
        let mut exec = MockExec::new()
            .script("a", vec![StepOutcome::Yield])
            .effects_on_first(
                "a",
                vec![Effect::RequestExtraFrame, Effect::SpawnTask(seed("b"))],
            );
        let report = s.run_frame_chain(&mut exec, 0);
        assert_eq!(report.logical_frames, 2, "TASK 开火 ⇒ 立即续一帧");
        assert_eq!(exec.count("a"), 2);
        assert_eq!(exec.count("b"), 1, "新任务下一逻辑帧即获得第一步");
    }

    /// 续帧标志在逻辑帧开头清零(官方 0x40413d)——帧前置位会被清掉,
    /// 与官方同构;帧内置位走 `Effect::RequestExtraFrame`。
    #[test]
    fn extra_frame_flag_is_cleared_at_frame_start() {
        let mut s = TaskScheduler::with_pump_cap(None);
        let mut exec = MockExec::new();
        s.request_extra_frame();
        let report = s.run_frame_chain(&mut exec, 0);
        assert_eq!(report.logical_frames, 1, "帧首清零 ⇒ 不续帧");
    }

    /// 链序 = 创建序(官方 FUN_004046cc 沿链取件;无「主线域恒先」)。
    #[test]
    fn chain_order_is_creation_order() {
        let mut s = TaskScheduler::with_pump_cap(None);
        s.spawn(seed("a"));
        s.spawn(seed("b"));
        let mut exec = MockExec::new()
            .script("a", vec![StepOutcome::Yield])
            .script("b", vec![StepOutcome::Yield]);
        s.run_frame_chain(&mut exec, 0);
        let ia = exec.calls.iter().position(|c| c.as_slice() == b"a").unwrap();
        let ib = exec.calls.iter().position(|c| c.as_slice() == b"b").unwrap();
        assert!(ia < ib, "先入链者先步进");
    }

    /// 泵到让出无预算;护栏仅显式配置时生效(工程护栏,非引擎常量)。
    #[test]
    fn pump_runs_to_yield_without_budget() {
        let mut s = TaskScheduler::with_pump_cap(None);
        s.spawn(seed("a"));
        let mut script = vec![StepOutcome::Continue; 5000];
        script.push(StepOutcome::Yield);
        let mut exec = MockExec::new().script("a", script);
        let report = s.run_frame_chain(&mut exec, 0);
        assert_eq!(report.commands_pumped, 5001);
        assert_eq!(report.pump_cap_hits, 0);
    }

    #[test]
    fn pump_cap_is_engineering_guard_only() {
        let mut s = TaskScheduler::with_pump_cap(Some(10));
        s.spawn(seed("a"));
        let mut script = vec![StepOutcome::Continue; 100];
        script.push(StepOutcome::Yield);
        let mut exec = MockExec::new().script("a", script);
        let report = s.run_frame_chain(&mut exec, 0);
        assert_eq!(report.pump_cap_hits, 1);
        assert_eq!(report.commands_pumped, 10);
    }

    /// finished / enabled 位的跳过语义(0x404418 / 0x4043c9)。
    #[test]
    fn finished_and_disabled_tasks_are_skipped() {
        let mut s = TaskScheduler::with_pump_cap(None);
        let f = s.spawn(seed("fin"));
        s.node_mut(f).unwrap().finished = true;
        let d = s.spawn(seed("dis"));
        s.node_mut(d).unwrap().enabled = false;
        let mut exec = MockExec::new();
        s.run_frame_chain(&mut exec, 0);
        assert_eq!(exec.count("fin"), 0);
        assert_eq!(exec.count("dis"), 0);
    }

    /// 子树容器(+0x30):自身不泵命令,递归步进 children(0x404679);
    /// 容器/子任务结构 Confirmed,置位命令待证 —— 此处仅验证分支行为。
    #[test]
    fn subtree_container_is_not_executed_but_children_are() {
        let mut s = TaskScheduler::with_pump_cap(None);
        let a = s.spawn(seed("a"));
        s.spawn_child(a, seed("c"));
        let mut exec = MockExec::new().script("c", vec![StepOutcome::Yield]);
        let report = s.run_frame_chain(&mut exec, 0);
        assert_eq!(report.logical_frames, 1);
        assert_eq!(exec.count("a"), 0, "容器自身不执行");
        assert_eq!(exec.count("c"), 1, "子链被递归步进");
    }

    /// TASKEND 软结束(Effect::FinishTask):同帧即生效,后到任务不再步进。
    #[test]
    fn taskend_effect_finishes_by_id() {
        let mut s = TaskScheduler::with_pump_cap(None);
        s.spawn(seed("a"));
        s.spawn(seed("b"));
        let mut exec = MockExec::new()
            .script("a", vec![StepOutcome::Yield])
            .effects_on_first("a", vec![Effect::FinishTask(b"b".to_vec())]);
        s.run_frame_chain(&mut exec, 0);
        assert!(s.find_by_id(b"b").unwrap().finished);
        assert_eq!(exec.count("b"), 0);
    }

    /// TASK 重配置 upsert:同 ID 换入口/清等待/恢复运行,链内只有一条。
    #[test]
    fn task_upsert_reconfigures_in_place() {
        let mut s = TaskScheduler::with_pump_cap(None);
        let i = s.spawn(seed("a"));
        s.node_mut(i).unwrap().wait_frames = 5;
        s.node_mut(i).unwrap().finished = true;
        let mut again = seed("a");
        again.pc = 42;
        let j = s.upsert(again);
        assert_eq!(i, j, "同 ID 不新增节点");
        let n = s.find_by_id(b"a").unwrap();
        assert_eq!(n.pc, 42);
        assert_eq!(n.wait_frames, 0);
        assert!(!n.finished);
    }

    /// 执行器报错:让出 + 结束 + 记录(官方路径未取证 → Likely)。
    #[test]
    fn executor_error_finishes_task_and_records() {
        struct Boom;
        impl TaskExecutor for Boom {
            fn step_command(&mut self, _task: &mut TaskNode) -> Result<StepOutcome, String> {
                Err("boom".into())
            }
        }
        let mut s = TaskScheduler::with_pump_cap(None);
        s.spawn(seed("a"));
        let mut exec = Boom;
        let report = s.run_frame_chain(&mut exec, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(s.find_by_id(b"a").unwrap().finished);
    }
}
