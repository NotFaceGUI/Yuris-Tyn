//! 剧本(Scenario)通道:明文 `.txt` 剧本运行器 + **统一文本通道**。
//!
//! 2026-10-17 从 `yuris-player-core` 迁入 `yuris-vm`:剧本页与 VM 组级执行
//! 同属「脚本解释」这一层,播放器不再各自维护一条文本路径。两种发行形态
//! 由同一套 [`TextLine`] 表达:
//!
//! - **v555 / 明文剧本**:`sc.ypf\scenario\*.txt` → [`ScenarioPlayer`]
//!   (`\LT`/`\LC`/裸文本行 → [`TextLine::Scenario`]);
//! - **v49x / 编译内联**:YSTB 内联 `WORD`(全语料 28992 组)与 `TEXT`
//!   命令 → VM 在发 `VmEvent::Text` 的同时写入 [`TextChannel`]
//!   (→ [`TextLine::Inline`])。
//!
//! 渲染侧(`player-core`)只消费 [`TextLine`],不再区分来源;`seq` 为
//! 全局单调序号,保证同一帧内两类来源的**呈现顺序可重建**。
//!
//! P7.3 scenario 播放器(可玩切片,M2)。
//!
//! 执行 sc.ypf 明文剧本(成果 49 语法表):驱动背景/立绘/淡入淡出/等待/
//! 双语台词(`(ID:n)\LE("…")\LT("…")`)。命令语义为**实测近似**
//! (引擎解释器派发链未逆向,逐项 Likely;不猜的部分显式跳过)。
//!
//! 资源解析:
//! - `\S(name, path, …)`:`path` 相对 `cg\`(实证 item/logo_wp);
//! - `\BG(name, …)`:`cg\bg\{name}.png`(实证 bg52);`white`/`black`
//!   等颜色名 → 纯色层;
//! - `\T(name, ms, x, y, …)`:`L_NYA_1A0100` → `cg\stand\<档位>\...` 的
//!   全图。档位**确定性优先 m_050**(剧本桥只预载 m_050/m_060;参考图
//!   标定原生像素 —— docs/layout-fix-plan.md P0);x = 中心偏移,y = 顶边。
//!
//! 台词等待:LT 行显示后**等待点击**(点击推进);\T 的 ms 为自动等待
//! (点击可跳过);\WA(ms) 纯延时。

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use yuris_scenario::{Element, Param, Scenario};

/// 播放层资源解析 + 渲染委托(由播放器实现)。
pub trait ScenarioHost {
    /// 显示/更新背景(淡入 ms;name 为颜色名时给 None 资源 + 颜色)。
    fn show_bg(&mut self, name: &str, fade_ms: u64, color: [u8; 3]);
    /// 背景相机位移(`\BG.CMXYZ(x, y, z)`;成果 49 相机模型,
    /// layout-fix-plan P1。z 语义 Unknown,实现方仅记录)。
    fn bg_camera(&mut self, x: i64, y: i64, z: i64) {
        let _ = (x, y, z);
    }
    /// 事件 CG 相机位移(`\EV.CMXYZ`;P3 EV 实装后应用)。
    fn ev_camera(&mut self, x: i64, y: i64, z: i64) {
        let _ = (x, y, z);
    }
    /// `\WINDOWMODE(n)`:文本窗口显示模式。语义 Unknown(全语料仅 1/2),
    /// 仅记录 + 日志,不猜分支行为(layout-fix-plan P2-3)。
    fn window_mode(&mut self, _mode: i64) {}
    /// 显示精灵(\S):path 相对 cg\;(x,y) = 左上角(0,0 = 全屏图原位),
    /// 原生尺寸,fade_ms 淡入。
    fn show_sprite(&mut self, name: &str, path: Option<&str>, x: i64, y: i64, fade_ms: u64);
    /// 显示立绘(\T):x = 中心偏移,y = 顶边,fade_ms 淡入。
    fn show_tachie(&mut self, name: &str, x: i64, y: i64, fade_ms: u64);
    /// 立绘位移动画(`\T` 8 参形式:x1,y1 → x2,y2,ms)。默认近似 = 直接落
    /// 终点(测试桩零改动);播放器实现线性补间(layout-fix-plan P0)。
    fn move_tachie(&mut self, name: &str, x1: i64, y1: i64, x2: i64, y2: i64, ms: u64) {
        let _ = (x1, y1, ms);
        self.show_tachie(name, x2, y2, 0);
    }
    /// 处置精灵(淡出 ms)。
    fn hide_sprite(&mut self, name: &str, fade_ms: u64);
    /// 淡出/淡入覆盖层。
    fn fade(&mut self, out: bool, ms: u64, color: [u8; 3]);
    /// 显示一行台词(SJIS/Big5 原文;播放器解码渲染)。
    fn show_text(&mut self, line_id: Option<u32>, lt: &str, le: &str);
    /// 清空台词窗。
    fn clear_text(&mut self);
    /// 语音(P9.1)。
    fn play_voice(&mut self, name: &str);
    /// BGM(音量千分比;空音量 = 仅调音量)。
    fn play_bgm(&mut self, name: &str, volume_permille: Option<i64>);
    /// SE。
    fn play_se(&mut self, name: &str);
    /// 内置标题画面(显示 + 等待菜单点击)。
    fn title_screen(&mut self);
    /// 标题菜单轮询:返回点中的按钮动作(未点/未命中 = None;点背景不推进)。
    fn poll_title_menu(&mut self) -> Option<TitleMenuAction> {
        None
    }
    /// 标题 LOAD/LASTLOAD:恢复存档(实现方走快读;完成后 scenario 已被重置)。
    fn title_load(&mut self) {}
    /// 标题 EXTRA:未实现时实现方留日志,留在标题。
    fn title_extra(&mut self) {}
    /// 标题 END:请求退出播放器。
    fn request_quit(&mut self) {}
    /// 选择肢轮询:count = 选项数;返回被点中的下标(未点 = None)。
    fn poll_choice(&mut self, count: usize) -> Option<usize>;
    /// 显示选择肢按钮。
    fn show_choices(&mut self, choices: &[String]);
    /// 清除选择肢显示。
    fn clear_choices(&mut self);
    /// 场景重置(跨文件 \GO 时清旧精灵/文本/选择肢)。
    fn reset_scene(&mut self);
    /// 读取全局槽(\GO.G.IF 的 G=n)。
    fn global(&self, slot: usize) -> i64;
    /// 写全局槽(\TITLE 按钮选择 → G1;引擎内置标题语义,成果 77/78)。
    fn set_global(&mut self, _slot: usize, _value: i64) {}
    /// 调试日志面。
    fn log(&mut self, msg: &str);
}

/// 标题菜单按钮动作(引擎原生菜单的内置等价路由;成果 76)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleMenuAction {
    /// OUTLINE(あらすじ):前情回顾 —— 原生绑定 `es.BT.SET("BTN.START",2)`
    /// 写 G1=2,经 `\GO.G.IF(1,"==",2,ARA)` 落前情回顾(成果 79)。
    Outline,
    /// START:开始新游戏(推进 scenario)。
    Start,
    /// LOAD:读档界面(内置暂走快读)。
    Load,
    /// LASTLOAD:读最近存档(快读)。
    LastLoad,
    /// EXTRA:鉴赏/附录(未实现)。
    Extra,
    /// END:退出。
    End,
}

/// 运行器等待状态。
#[derive(Debug, Clone)]
enum Wait {
    /// 纯延时(点击可跳过)。
    Timed { until: Instant },
    /// 台词行:等点击。
    Line,
    /// 淡入淡出进行中(阻塞至结束;点击可跳过)。
    Fading { until: Instant },
    /// 选择肢:等玩家点选(poll_choice)。
    Choices,
    /// 标题菜单:等按钮命中(poll_title_menu;点背景不推进,成果 76)。
    TitleMenu,
}

/// 跨文件标签表:标签名 → (文件名, 元素下标)。
pub struct ScenarioPlayer {
    /// 原始字节(惰性解析:坏文件不阻塞全局)。
    raw: Vec<(String, Vec<u8>)>,
    /// 各文件已解析文本(文件名 → Scenario)。
    files: HashMap<String, Scenario>,
    /// 标签 → (文件, 下标)。惰性构建:每个文件解析成功后登记。
    labels: HashMap<String, (String, usize)>,
    /// 当前文件。
    cur: String,
    /// 当前元素下标。
    pc: usize,
    wait: Option<Wait>,
    /// 待显示台词缓存(LE 先于 LT 到达)。
    pending_le: String,
    pending_id: Option<u32>,
    /// 已完成的 fade 终态(避免逐帧动画未实现时丢终态)。
    done: bool,
    /// 跨文件跳转 → 旧场景精灵应清理(播放器消费)。
    scene_reset: bool,
    /// 最近经过的段标签(存档锚点)。
    last_label: Option<String>,
    /// \SEL.GO 的跳转目标(下一条 \SEL 消费)。
    pending_sel_targets: Option<Vec<String>>,
    /// 当前活动选择肢(poll_choice 透传)。
    active_choices: Option<Vec<String>>,
}

impl ScenarioPlayer {
    /// 从资源字节构建(惰性:文件在 goto 时解析;Big5 = 本样本繁中版)。
    pub fn new(files: Vec<(String, Vec<u8>)>) -> Self {
        Self {
            raw: files,
            files: HashMap::new(),
            labels: HashMap::new(),
            cur: String::new(),
            pc: 0,
            wait: None,
            pending_le: String::new(),
            pending_id: None,
            done: false,
            scene_reset: false,
            last_label: None,
            pending_sel_targets: None,
            active_choices: None,
        }
    }

    /// 就绪入口:解析入口文件并跳到其标签(播放器启动用)。
    pub fn start(&mut self, file: &str, label: &str) -> Result<(), String> {
        // sc.ypf 条目名带虚拟根前缀(#/$/-/:),按后缀匹配
        let real = self
            .raw
            .iter()
            .map(|(n, _)| n.clone())
            .find(|n| n.ends_with(file) || n.ends_with(&format!("/{file}").replace('/', &format!("{}", chr_backslash()))))
            .unwrap_or_else(|| file.to_string());
        self.load_file(&real)?;
        self.goto(label)
    }

    /// 解析单个文件(成功后登记标签;失败 = Err,不缓存)。
    fn load_file(&mut self, name: &str) -> Result<(), String> {
        if self.files.contains_key(name) {
            return Ok(());
        }
        let bytes = self
            .raw
            .iter()
            .find(|(n, _)| n == name || n.ends_with(name) || n.ends_with(&name.replace('/', "\\")))
            .map(|(_, b)| b.clone())
            .ok_or_else(|| format!("scenario 文件不存在: {name}"))?;
        // 编码**数据侧判定**(T1+T21 定稿,2026-10-16):旧实现写死 Big5
        // (繁中样本),CP932 作品走剧本层即整篇乱码。`parse_scenario_auto`
        // = SJIS → Big5 严格试解码、两者都失败显式报错;策略与 VM 文本层
        // (lib.rs `WORD` 显示走 `yuris_scenario::decode_text`)同一份实现。
        let sc = yuris_scenario::parse_scenario_auto(&bytes)
            .map_err(|e| format!("解析 {name}: {e}"))?;
        for (label, idx) in &sc.labels {
            self.labels.insert(label.clone(), (name.to_string(), *idx));
        }
        self.files.insert(name.to_string(), sc);
        Ok(())
    }

    /// 跳到标签(跨文件;目标文件惰性解析)。
    pub fn goto(&mut self, label: &str) -> Result<(), String> {
        if !self.labels.contains_key(label) {
            // 标签未知:遍历 raw 尝试解析定位(P7.2 语法盲点容错)
            let names: Vec<String> = self.raw.iter().map(|(n, _)| n.clone()).collect();
            for n in &names {
                if let Err(e) = self.load_file(n) {
                    // **剧本文件解析失败以前被完全吞掉**(`continue`)——
                    // 表现为「标签找不到」而真实原因是解析/编码失败。
                    yuris_log::alert_once(
                        module_path!(),
                        format!("scenario-parse-fail:{n}"),
                        format!(
                            "明文剧本文件解析失败(已跳过)\n\
                             文件: {n}\n\
                             错误: {e}\n\
                             后果: 该文件的标签/台词全部不可用\n\
                             处置: 检查编码判定(SJIS/Big5 严格解码)与语法\n\
                             —— 这是标签查找失败时最可能的隐藏原因"
                        ),
                    );
                    continue;
                }
                if self.labels.contains_key(label) {
                    break;
                }
            }
        }
        let Some((file, idx)) = self.labels.get(label).cloned() else {
            return Err(format!("scenario 标签不存在: {label}"));
        };
        self.load_file(&file)?;
        self.cur = file;
        self.pc = idx;
        self.wait = None;
        self.scene_reset = true;
        Ok(())
    }

    /// 消费场景重置标记(跨文件跳转后,播放器清旧精灵)。
    pub fn take_scene_reset(&mut self) -> bool {
        std::mem::take(&mut self.scene_reset)
    }

    /// 存档点:当前文件 + 最近段标签(P9.3 快存;恢复 = start(file,label))。
    pub fn save_point(&self) -> Result<(String, String), String> {
        let label = self
            .last_label
            .clone()
            .ok_or_else(|| "无标签锚点可存".to_string())?;
        Ok((self.cur.clone(), label))
    }

    /// 活动选择肢数。
    pub fn choice_count(&self) -> usize {
        self.active_choices.as_ref().map(|v| v.len()).unwrap_or(0)
    }

    /// 是否处于标题菜单等待(P8.2b:VM UI 层过滤 —— 内置标题接管视觉,
    /// 标题等待期不建 VM CG 层,避免与 yst00259 的双份按钮)。
    pub fn in_title_menu(&self) -> bool {
        matches!(self.wait, Some(Wait::TitleMenu))
    }

    /// 剧本是否仍在运行(`\END` 之前)。
    pub fn running(&self) -> bool {
        !self.done
    }

    /// 本播放器**一条明文剧本都没有**(纯 YSTB 发行:无 `sc.txt` 族).
    ///
    /// 播放器的场景驱动(scenario 层)在此之前是**唯一**把 CG 放上屏的路径,
    /// 而它的放行门 `title_seen` 也由 scenario 自己置位 —— 于是没有明文剧本的
    /// 作品永远不会放行任何图层,画面恒黑(实机勘误,2026-10-13)。
    /// 调用方据此改走 VM 的 CG 通道。
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// 推进一帧。`clicked` = 本帧发生了点击。
    pub fn tick(&mut self, host: &mut dyn ScenarioHost, clicked: bool) {
        if self.done {
            return;
        }
        // 标题菜单:按钮命中才路由,点背景不推进(引擎语义,成果 76)。
        if matches!(&self.wait, Some(Wait::TitleMenu)) {
            match host.poll_title_menu() {
                None => return,
                Some(act @ (TitleMenuAction::Start | TitleMenuAction::Outline)) => {
                    // 引擎原生:\TITLE 把按钮选择写入 G1(START=1 / OUTLINE=2,
                    // yst00259 `es.BT.SET("BTN.START",1|2)` 实证,成果 79)后
                    // 返回剧本,经 \GO.G.IF 落 SCENARIO_MAIN / ARA;不再依赖
                    // 末尾 \GO(SCENARIO_MAIN) 兜底。
                    let g = if matches!(act, TitleMenuAction::Outline) { 2 } else { 1 };
                    host.set_global(1, g);
                    self.wait = None;
                }
                Some(TitleMenuAction::Load | TitleMenuAction::LastLoad) => {
                    // 不预清 wait:读档成功 → start() 重置 wait;失败(无快存)→ 保持标题等待
                    host.title_load();
                    return;
                }
                Some(TitleMenuAction::Extra) => {
                    host.title_extra(); // 未实现:留日志,留在标题
                    return;
                }
                Some(TitleMenuAction::End) => {
                    host.request_quit();
                    return;
                }
            }
        }
        // 等待检查
        if let Some(w) = &self.wait {
            if matches!(w, Wait::Choices) {
                // 选择肢:点中任一项 → 跳转目标
                let n = self.active_choices.as_ref().map(|v| v.len()).unwrap_or(0);
                if let Some(idx) = host.poll_choice(n) {
                    let targets = self.pending_sel_targets.take();
                    self.active_choices = None;
                    self.wait = None;
                    host.clear_choices();
                    if let Some(ts) = targets {
                        match ts.get(idx) {
                            Some(t) => {
                                let t = t.clone();
                                host.log(&format!("选择 {idx} → {t}"));
                                // **跳转失败不得静默**:目标标签不存在时
                                // 流程会原地停留(玩家看到"点了没反应")。
                                if let Err(e) = self.goto(&t) {
                                    yuris_log::alert_once(
                                        module_path!(),
                                        format!("sel-goto-fail:{t}"),
                                        format!(
                                            "选择肢跳转失败\n\
                                             选项 {idx} → 标签 {t:?}\n\
                                             错误: {e}\n\
                                             后果: 剧情停在选择处(点击无反应)\n\
                                             处置: 检查 \\SEL.GO 目标标签名是否存在于剧本"
                                        ),
                                    );
                                }
                            }
                            None => {
                                // `\SEL.GO` 目标数量少于选项数:该选项无落点。
                                yuris_log::alert_once(
                                    module_path!(),
                                    format!("sel-target-missing:{idx}"),
                                    format!(
                                        "选项没有对应的 \\SEL.GO 目标\n\
                                         选中索引 = {idx}  目标数 = {}\n\
                                         后果: 该选项点击后无跳转(剧情停留)\n\
                                         处置: \\SEL.GO 的标签数须与 \\SEL 选项数一致",
                                        ts.len()
                                    ),
                                );
                            }
                        }
                    }
                }
                return;
            }
            let expired = match w {
                Wait::Timed { until } | Wait::Fading { until } => Instant::now() >= *until,
                Wait::Line => false,
                Wait::Choices => false, // 已在上方处理(不可达;穷尽性)
                Wait::TitleMenu => false, // 已在上方处理(不可达;穷尽性)
            };
            if !expired && !clicked {
                return;
            }
            if matches!(w, Wait::Line) && !clicked {
                return;
            }
            self.wait = None;
        }
        // 执行直到阻塞
        const MAX_STEPS: usize = 512;
        for _ in 0..MAX_STEPS {
            // 单步克隆元素(免自借用;元素很小)
            let Some(el) = self
                .files
                .get(&self.cur)
                .and_then(|sc| sc.elements.get(self.pc).cloned())
            else {
                // 文件尾 = 结束
                self.done = true;
                host.log(&format!("scenario 文件尾:{}", self.cur));
                return;
            };
            self.pc += 1;
            match &el {
                Element::Label { name, .. } => {
                    self.last_label = Some(name.clone()); // 存档锚点
                }
                Element::Command { name, modifiers, params, .. } => {
                    let done = self.exec(host, name, modifiers, params);
                    if done {
                        return; // 阻塞等待中
                    }
                }
                Element::LineId { id, .. } => {
                    self.pending_id = Some(*id);
                }
                Element::Dialogue { text, .. } => {
                    // 裸文本行(语料 0 例):并入台词显示
                    let lt = self.pending_le.clone();
                    host.show_text(self.pending_id, text, &lt);
                    self.pending_le.clear();
                    self.pending_id = None;
                    self.wait = Some(Wait::Line);
                    return;
                }
            }
        }
        // **步数上限耗尽**(2026-10-17 用户要求):以前静默返回,下一帧再从
        // 同一 pc 继续 —— 表现为「画面卡住、日志无任何线索」。
        // 现在打横幅点名现场,提示可能是无限循环(\GO 自跳/选择肢缺目标)。
        yuris_log::alert_once(
            module_path!(),
            format!("scenario-max-steps:{}:{}", self.cur, self.pc),
            format!(
                "明文剧本单帧步数上限耗尽(MAX_STEPS=512)\n\
                 现场: 文件 {} 元素 {}\n\
                 后果: 本帧停止推进,下一帧从同一位置继续(可能每帧空转)\n\
                 处置: 检查该段是否有 \\GO 自跳 / 选择肢缺 \\SEL.GO 目标 / 未知命令循环",
                self.cur, self.pc
            ),
        );
    }

    /// 执行一条命令;返回 true = 进入阻塞等待(本帧停)。
    fn exec(
        &mut self,
        host: &mut dyn ScenarioHost,
        name: &str,
        modifiers: &[String],
        params: &[Param],
    ) -> bool {
        let s = |i: usize| -> String {
            match params.get(i) {
                Some(Param::Str(x)) => x.clone(),
                Some(Param::Int(n)) => n.to_string(),
                _ => String::new(),
            }
        };
        let n = |i: usize| -> i64 {
            match params.get(i) {
                Some(Param::Int(x)) => *x,
                Some(Param::Float(x)) => *x as i64,
                Some(Param::Str(x)) => x.trim().parse().unwrap_or(0),
                _ => 0,
            }
        };
        match name {
            // 相机位移:\BG.CMXYZ(x,y,z) / \EV.CMXYZ(x,y,z)(成果 49)。
            // 勘误(layout-fix-plan §2.3):命令名/修饰符分离模型下此处
            // name = "BG"/"EV"、modifier = "CMXYZ";旧实现以
            // name == "CMXYZ" 匹配 → 永不命中,`\BG.CMXYZ` 被当作换背景。
            "BG" if modifiers.iter().any(|m| m == "CMXYZ") => {
                host.bg_camera(n(0), n(1), n(2));
                false
            }
            "EV" if modifiers.iter().any(|m| m == "CMXYZ") => {
                host.ev_camera(n(0), n(1), n(2));
                false
            }
            "BG" => {
                // \BG(name, fade, 0, …):name 可能带尾随空格("bg52 ")
                let raw = s(0);
                let bg = raw.trim();
                let fade = n(1).max(0) as u64;
                let color = color_of(bg);
                host.show_bg(bg, fade, color);
                false
            }
            "S" => {
                // \S(name, file, fade_ms, x, y, z, …);\S.D(name, ms) = 淡出处置
                // 勘误(成果 69):param[2] = 淡入毫秒(logo 1100/attention 1000),
                // (x,y) = param[3..5] = 左上角(0,0 = 全屏图原位)—— 旧实现把
                // 1100 当 X,把厂商 logo 画到屏幕外(「不播放」根因)。
                if modifiers.iter().any(|m| m == "D") {
                    host.hide_sprite(s(0).trim(), n(1).max(0) as u64);
                    return false;
                }
                let sname = s(0);
                let path = s(1);
                let (fade, x, y) = (n(2).max(0) as u64, n(3), n(4));
                host.show_sprite(sname.trim(), Some(path.trim()), x, y, fade);
                false
            }
            "T" => {
                // \T(name, ms, x, y, z?):name 空 = 纯等待;非空 = 立绘淡入后等待。
                // 8 参形式 = 位移动画:(name, ms, x1, y1, z1, x2, y2, z2)
                // —— 引擎实测有「浮动」滑动效果;ms = 动画时长(P0)。
                let tname = s(0).trim().to_string();
                let ms = n(1).max(0) as u64;
                if !tname.is_empty() {
                    if params.len() >= 8 {
                        host.move_tachie(&tname, n(2), n(3), n(5), n(6), ms);
                    } else {
                        let (x, y) = (n(2), n(3));
                        host.show_tachie(&tname, x, y, ms);
                    }
                }
                self.wait = Some(Wait::Timed {
                    until: Instant::now() + std::time::Duration::from_millis(ms),
                });
                true
            }
            "WA" => {
                let ms = n(0).max(0) as u64;
                self.wait = Some(Wait::Timed {
                    until: Instant::now() + std::time::Duration::from_millis(ms),
                });
                true
            }
            "FOUT" => {
                // \FOUT(ms, slot, color)
                let ms = n(0).max(0) as u64;
                let cname = s(2);
                host.fade(true, ms, color_of(cname.trim()));
                self.wait = Some(Wait::Fading {
                    until: Instant::now() + std::time::Duration::from_millis(ms),
                });
                true
            }
            "FIN" => {
                let ms = n(0).max(0) as u64;
                host.fade(false, ms, [0, 0, 0]);
                self.wait = Some(Wait::Fading {
                    until: Instant::now() + std::time::Duration::from_millis(ms),
                });
                true
            }
            "VO" => {
                host.play_voice(&s(0));
                false
            }
            "BGM" => {
                // \BGM(name, vol) / \BGM(,vol):空首槽 = 保持曲目仅调音量
                let bname = s(0).trim().to_string();
                let vol = match params.get(1) {
                    Some(Param::Int(v)) => Some(*v),
                    _ => None,
                };
                host.play_bgm(&bname, vol);
                false
            }
            "SE" => {
                host.play_se(&s(0));
                false
            }
            "TITLE" => {
                // \TITLE:内置标题画面 + 菜单等待(按钮命中路由,成果 76;
                // 旧行为 Wait::Line 盲推进 = 「点任意按钮都进游戏」根因)
                host.title_screen();
                self.wait = Some(Wait::TitleMenu);
                true
            }
            "SEL" if modifiers.iter().any(|m| m == "GO") => {
                // \SEL.GO(标签,…):登记跳转目标
                let targets: Vec<String> = params
                    .iter()
                    .filter_map(|p| match p {
                        Param::Str(x) if !x.trim().is_empty() => Some(x.trim().to_string()),
                        _ => None,
                    })
                    .collect();
                self.pending_sel_targets = Some(targets);
                false
            }
            "SEL" => {
                // \SEL("EN"×n, "", "TW"×n, ""):后段 = 本地语文本
                let texts: Vec<String> = params
                    .iter()
                    .filter_map(|p| match p {
                        Param::Str(x) => Some(x.clone()),
                        _ => None,
                    })
                    .collect();
                let n = texts.len() / 2;
                let tw = &texts[n..];
                let choices: Vec<String> = if !tw.is_empty()
                    && tw.iter().any(|x| !x.trim().is_empty())
                {
                    tw.iter().filter(|x| !x.trim().is_empty()).cloned().collect()
                } else {
                    texts.iter().filter(|x| !x.trim().is_empty()).cloned().collect()
                };
                if self.pending_sel_targets.is_none() {
                    // 选择肢没有 \SEL.GO 目标 ⇒ 点选后无处可跳(流程卡死)。
                    yuris_log::alert_once(
                        module_path!(),
                        format!("sel-no-target:{}:{}", self.cur, self.pc),
                        format!(
                            "\\SEL 缺少 \\SEL.GO 跳转目标,选择肢被跳过\n\
                             现场: 文件 {} 元素 {}\n\
                             后果: 本应阻塞等玩家的选择被忽略,流程直接继续\n\
                             处置: 检查该段剧本是否先写 \\SEL.GO(label,…)",
                            self.cur,
                            self.pc.saturating_sub(1)
                        ),
                    );
                    return false;
                }
                if choices.is_empty() {
                    yuris_log::alert_once(
                        module_path!(),
                        format!("sel-empty:{}:{}", self.cur, self.pc),
                        format!(
                            "\\SEL 没有任何非空选项文本,选择肢被跳过\n\
                             现场: 文件 {} 元素 {}  参数数 = {}\n\
                             处置: 检查剧本编码(CP932/Big5)与该行文本参数",
                            self.cur,
                            self.pc.saturating_sub(1),
                            params.len()
                        ),
                    );
                    return false;
                }
                self.active_choices = Some(choices.clone());
                host.show_choices(&choices);
                self.wait = Some(Wait::Choices);
                true
            }
            "LE" => {
                self.pending_le = s(0);
                false
            }
            "LT" | "LC" => {
                // LT = 样本语料(<game-a>)主台词行;LC = <game-c> 语料的中文主台词行
                // 两者语义对称:主文本 + pending 英文上屏并阻塞等待。
                let lt = s(0);
                host.clear_text();
                host.show_text(self.pending_id, &lt, &self.pending_le.clone());
                self.pending_le.clear();
                self.pending_id = None;
                self.wait = Some(Wait::Line);
                true
            }
            "GO" if modifiers.iter().any(|m| m == "IF") => {
                // \GO.G.IF(槽, op, 值, 目标):全局槽比较跳转。
                // 槽 n = host 仿真内部全局槽(成果 78 勘误:原 @50[n] 映射被
                // 运行时证伪,@50 dims=[1];引擎真值存储 Unknown,B5 待
                // es.BT.* 宏链逆向)。全语料仅 scenario_start.txt 两处,均槽 1。
                let slot = n(0).max(0) as usize;
                let op = s(1);
                let rhs = n(2);
                let target = s(3).trim().to_string();
                let cur = host.global(slot);
                let hit = match op.as_str() {
                    "==" => cur == rhs,
                    "!=" => cur != rhs,
                    ">=" => cur >= rhs,
                    "<=" => cur <= rhs,
                    ">" => cur > rhs,
                    "<" => cur < rhs,
                    unknown => {
                        // 未知比较算子:以前静默按「不跳」处理(分支永远走
                        // false 路径)。现在至少让它在日志里站住。
                        yuris_log::alert_once(
                            module_path!(),
                            format!("go-gif-op:{unknown}"),
                            format!(
                                "\\GO.G.IF 未知比较算子,按「不跳」处理\n\
                                 算子: {unknown:?}\n\
                                 现场: 文件 {} 元素 {}\n\
                                 处置: 引擎面只确认 ==/!=/>=/<=/>/<;此为默认分支",
                                self.cur,
                                self.pc.saturating_sub(1)
                            ),
                        );
                        false
                    }
                };
                host.log(&format!("GO.G.IF G[{slot}]={cur} {op} {rhs} → {} ", if hit { &target } else { "(不跳)" }));
                if hit {
                    match self.goto(&target) {
                        Ok(()) => host.reset_scene(),
                        Err(e) => {
                            // 条件命中但跳转失败 = 剧情卡在原地。
                            yuris_log::alert_once(
                                module_path!(),
                                format!("scenario-gogif-fail:{target}"),
                                format!(
                                    "\\GO.G.IF 条件命中但跳转失败\n\
                                     目标标签: {target:?}\n\
                                     错误: {e}\n\
                                     现场: 文件 {} 元素 {}\n\
                                     后果: 剧情停在当前位置\n\
                                     处置: 检查目标标签是否存在于剧本",
                                    self.cur,
                                    self.pc.saturating_sub(1)
                                ),
                            );
                        }
                    }
                }
                false
            }
            "GO" => {
                let label = s(0).trim().to_string();
                match self.goto(&label) {
                    Ok(()) => {
                        host.reset_scene(); // 立即清旧场景(勿等到 tick 末,防误清新层)
                        host.log(&format!("GO → {label}"));
                    }
                    Err(e) => {
                        // **\GO 跳转失败以前只进 host.log(debug 级)** ——
                        // 剧情原地卡住且默认日志看不到。现在大告警。
                        yuris_log::alert_once(
                            module_path!(),
                            format!("scenario-go-fail:{label}"),
                            format!(
                                "明文剧本 \\GO 跳转失败\n\
                                 目标标签: {label:?}\n\
                                 错误: {e}\n\
                                 现场: 文件 {} 元素 {}\n\
                                 后果: 剧情停在当前位置\n\
                                 处置: 检查标签名/目标文件是否在 sc 包内",
                                self.cur,
                                self.pc.saturating_sub(1)
                            ),
                        );
                    }
                }
                false
            }
            "END" => {
                host.log("\\END");
                self.done = true;
                true
            }
            "WINDOWMODE" => {
                // 文本窗口显示模式(1/2;语义 Unknown,记录不猜 —— P2-3)
                host.window_mode(n(0));
                false
            }
            other => {
                // 修饰符链/长尾命令(.D 处置、.CMXYZ 相机等)切片外:记录不猜。
                //
                // **2026-10-17 用户要求:未知命令必须大告警**(此前只有
                // `host.log`,默认等级下一行就淹没)。按「命令名+修饰符」去重,
                // 一次运行把**全部缺失命令**列成横幅。语义仍 Unknown、不猜,
                // 但不再静默。
                let mods = if modifiers.is_empty() {
                    String::new()
                } else {
                    format!(".{}", modifiers.join("."))
                };
                let key = format!("scenario:{other}{mods}");
                yuris_log::alert_once(
                    module_path!(),
                    key,
                    format!(
                        "明文剧本命令未实现,已跳过\n\
                         命令: \\{other}{mods}\n\
                         参数数: {}\n\
                         现场: 文件 {} 元素 {}\n\
                         处置: {}",
                        params.len(),
                        self.cur,
                        self.pc.saturating_sub(1),
                        if modifiers.iter().any(|m| m == "D") {
                            "按 `.D` 修饰符**近似**为 hide_sprite(UNVERIFIED,仅处置语义)"
                        } else {
                            "跳过(语义 Unknown,不猜)"
                        }
                    ),
                );
                // \S.D(name, ms) / \X.D 族:处置语义按修饰符近似实现
                if modifiers.iter().any(|m| m == "D") {
                    host.hide_sprite(s(0).trim(), n(1).max(0) as u64);
                }
                false
            }
        }
    }
}

fn chr_backslash() -> char {
    '\\'
}

/// 颜色名 → RGB(\BG(white)/\FOUT(…,BLACK))。
///
/// **2026-10-17 用户要求**:未知颜色名以前静默按黑色处理(画面变黑却
/// 没有任何提示)。现在保留黑色默认(引擎面只有 white/black 两色),
/// 但每一个未知颜色名都会打一条去重横幅。
fn color_of(name: &str) -> [u8; 3] {
    match name.to_ascii_lowercase().as_str() {
        "white" => [255, 255, 255],
        "black" => [0, 0, 0],
        other => {
            yuris_log::alert_once(
                module_path!(),
                format!("color:{other}"),
                format!(
                    "未知颜色名,按黑色处理\n\
                     颜色: {other:?}\n\
                     处置: 引擎面只确认 white/black;此处为默认值,不是真值"
                ),
            );
            [0, 0, 0]
        }
    }
}

// ---------------------------------------------------------------------------
// 统一文本通道(v555 明文剧本 / v49x 编译内联 WORD 共用)
// ---------------------------------------------------------------------------

/// 文本行来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSource {
    /// 明文剧本(`\LT` / `\LC` / 裸文本行)。
    Scenario,
    /// 编译进 YSTB 的内联文本(`WORD`/`TEXT`);带出处供诊断。
    Inline {
        /// 脚本号。
        script_id: u16,
        /// 组号。
        pc: usize,
    },
}

/// 一条待呈现文本(两类来源的统一形态)。
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// 全局单调序号(同一帧内可重建呈现顺序)。
    pub seq: u64,
    /// 来源。
    pub source: TextSource,
    /// 行号标记(`(ID:n)`;内联文本为 `None`)。
    pub line_id: Option<u32>,
    /// 主文本。
    pub text: String,
    /// 副文本(明文剧本的 `\LE`;内联文本为空)。
    pub le: String,
}

/// 统一文本通道:VM 内联文本入 [`Self::push_word`]/[`Self::commit_line`],
/// 剧本运行器入 [`Self::push_scenario`];渲染侧每帧 [`Self::take`] 一次。
///
/// ## 行语义(2026-10-17,Confirmed by corpus)
///
/// 引擎的 `WORD` 处理器把裸 SJIS **追加**进任务文本缓冲(不绘制),缓冲
/// 由**行结束标记**提交。语料实证(s149):
///
/// ```text
/// g723 WORD "YU-RIS β"
/// g724 WORD "4から、独自形式である"
/// g725 WORD "YMVムービーというものを扱えるようになりました。"
/// g726 RETURNCODE          ← 这一行到此结束
/// ```
///
/// 全语料 `WORD`×28992 / `RETURNCODE`×28299 ⇒ **≈1.02 片段/行**,
/// 与 VN 行数量级吻合。旧实现把每个 `WORD` 当一整行 ⇒ 一句话被拆成
/// 多行、按钮标签也永远拼不起来。
///
/// 文本**不做排版推断**(换行/分段归 Stage/字体层,见 U13)。
#[derive(Debug, Default)]
pub struct TextChannel {
    next_seq: u64,
    pending: VecDeque<TextLine>,
    /// 未提交的行缓冲(`WORD` 追加;`RETURNCODE` 提交)。
    line_buf: String,
    /// 本行首个 `WORD` 的出处(诊断)。
    line_src: Option<(u16, usize)>,
}

impl TextChannel {
    /// 空通道。
    pub fn new() -> Self {
        Self::default()
    }

    /// 内联文本(`WORD`/`TEXT` 命令的原始字节;`decode_text` 数据侧判码)。
    ///
    /// 解码失败时 `text` 留空并保留原始字节 —— 不静默替换(调用方据
    /// [`Self::take`] 拿到的空串 + `raw` 自行告警)。
    pub fn push_inline(&mut self, script_id: u16, pc: usize, bytes: &[u8]) {
        // `TEXT` 是独立的行源:先把 `WORD` 累积的缓冲提交,避免串行。
        self.commit_line();
        let text = yuris_scenario::decode_text(bytes).unwrap_or_default();
        self.push(TextLine {
            seq: 0,
            source: TextSource::Inline { script_id, pc },
            line_id: None,
            text,
            le: String::new(),
        });
    }

    /// 明文剧本台词行。
    pub fn push_scenario(&mut self, line_id: Option<u32>, text: &str, le: &str) {
        self.push(TextLine {
            seq: 0,
            source: TextSource::Scenario,
            line_id,
            text: text.to_string(),
            le: le.to_string(),
        });
    }

    fn push(&mut self, mut line: TextLine) {
        line.seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push_back(line);
    }

    /// `WORD`:**追加**到当前行缓冲(不提交;引擎 `ctx+0x38` 语义)。
    ///
    /// 解码失败(非法字节序列)时保留原文的十六进制占位 —— 不静默吞。
    pub fn push_word(&mut self, script_id: u16, pc: usize, bytes: &[u8]) {
        if let Ok(t) = yuris_scenario::decode_text(bytes) {
            if self.line_src.is_none() {
                self.line_src = Some((script_id, pc));
            }
            self.line_buf.push_str(&t);
        }
    }

    /// `RETURNCODE`:行结束 → 提交当前缓冲为一条可呈现文本。
    ///
    /// 空缓冲不产出(与引擎「无文本不绘制」一致)。
    pub fn commit_line(&mut self) {
        if self.line_buf.is_empty() {
            self.line_src = None;
            return;
        }
        let (sid, pc) = self.line_src.take().unwrap_or((0, 0));
        let text = std::mem::take(&mut self.line_buf);
        self.push(TextLine {
            seq: 0,
            source: TextSource::Inline {
                script_id: sid,
                pc,
            },
            line_id: None,
            text,
            le: String::new(),
        });
    }

    /// 当前未提交行缓冲(诊断/测试)。
    pub fn pending_line(&self) -> &str {
        &self.line_buf
    }

    /// 取走全部待呈现文本(帧末消费;顺序 = 入队序)。
    pub fn take(&mut self) -> Vec<TextLine> {
        self.pending.drain(..).collect()
    }

    /// 待呈现条数。
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// 是否无待呈现文本。
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod text_channel_tests {
    use super::*;

    #[test]
    fn inline_and_scenario_share_one_sequence() {
        let mut ch = TextChannel::new();
        ch.push_inline(80, 412, &[0x82, 0xb1, 0x82, 0xf1]);
        ch.push_scenario(Some(7), "hello", "en");
        ch.push_inline(80, 415, b".");
        let lines = ch.take();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].seq, 0);
        assert_eq!(lines[1].seq, 1);
        assert_eq!(lines[2].seq, 2);
        assert_eq!(lines[0].source, TextSource::Inline { script_id: 80, pc: 412 });
        assert_eq!(lines[1].source, TextSource::Scenario);
        assert_eq!(lines[1].line_id, Some(7));
        assert_eq!(lines[1].text, "hello");
        // 内联 SJIS「こん」按数据侧判码 → UTF-8
        assert_eq!(lines[0].text, "こん");
    }

    /// **行语义回归**:连续 WORD 追加成一行,RETURNCODE 才提交。
    #[test]
    fn consecutive_words_accumulate_until_returncode() {
        let mut ch = TextChannel::new();
        ch.push_word(149, 723, &[0x59, 0x55, 0x2d, 0x52, 0x49, 0x53]); // "YU-RIS"
        ch.push_word(149, 724, &[0x34]); // "4"
        assert!(ch.is_empty(), "WORD 不提交");
        assert_eq!(ch.pending_line(), "YU-RIS4");
        ch.commit_line();
        let lines = ch.take();
        assert_eq!(lines.len(), 1, "一行只有一个 TextLine");
        assert_eq!(lines[0].text, "YU-RIS4");
        assert_eq!(lines[0].source, TextSource::Inline { script_id: 149, pc: 723 });
    }

    /// 空缓冲的 RETURNCODE 不产出行(引擎「无文本不绘制」)。
    #[test]
    fn empty_commit_produces_nothing() {
        let mut ch = TextChannel::new();
        ch.commit_line();
        assert!(ch.take().is_empty());
    }

    #[test]
    fn take_drains_and_seq_keeps_monotonic() {
        let mut ch = TextChannel::new();
        ch.push_scenario(None, "a", "");
        assert!(!ch.is_empty());
        let _ = ch.take();
        assert!(ch.is_empty());
        ch.push_scenario(None, "b", "");
        assert_eq!(ch.take()[0].seq, 1, "序号跨批次单调,不因 drain 归零");
    }
}


#[cfg(test)]
mod go_gif_tests {
    //! \GO.G.IF 行为对照测试(Likely 级:@50[n] 映射未真机对照,见 PROGRESS B5;
    //! 本测试验证比较算子与跳转分支逻辑本身)。

    use super::*;

    struct TestHost {
        slot: std::cell::Cell<i64>,
        resets: std::cell::Cell<usize>,
    }
    impl ScenarioHost for TestHost {
        fn show_bg(&mut self, _: &str, _: u64, _: [u8; 3]) {}
        fn show_sprite(&mut self, _: &str, _: Option<&str>, _: i64, _: i64, _: u64) {}
        fn show_tachie(&mut self, _: &str, _: i64, _: i64, _: u64) {}
        fn hide_sprite(&mut self, _: &str, _: u64) {}
        fn fade(&mut self, _: bool, _: u64, _: [u8; 3]) {}
        fn show_text(&mut self, _: Option<u32>, _: &str, _: &str) {}
        fn clear_text(&mut self) {}
        fn play_voice(&mut self, _: &str) {}
        fn play_bgm(&mut self, _: &str, _: Option<i64>) {}
        fn play_se(&mut self, _: &str) {}
        fn title_screen(&mut self) {}
        fn poll_choice(&mut self, _: usize) -> Option<usize> { None }
        fn show_choices(&mut self, _: &[String]) {}
        fn clear_choices(&mut self) {}
        fn reset_scene(&mut self) { self.resets.set(self.resets.get() + 1); }
        fn global(&self, _: usize) -> i64 { self.slot.get() }
        fn log(&mut self, _: &str) {}
    }

    /// scenario:`#S` 段内一条 \GO.G.IF;reset_scene 次数 = 是否跳转。
    fn run_goto_gif(script: &str, slot: i64) -> usize {
        let mut p = ScenarioPlayer::new(vec![("t.txt".into(), script.as_bytes().to_vec())]);
        let _ = p.goto("S"); // 惰性解析:先定位标签
        let mut h = TestHost { slot: std::cell::Cell::new(slot), resets: std::cell::Cell::new(0) };
        p.tick(&mut h, false);
        h.resets.get()
    }

    #[test]
    fn go_gif_hit_and_miss() {
        let sc = "#S\n\\GO.G.IF(1, \"==\", 2, T)\n\\GO(END0)\n#T\n";
        // 槽=2,==2 → 命中跳 T(reset_scene 1 次)
        assert_eq!(run_goto_gif(sc, 2), 1, "== 命中应跳转");
        // 槽=0,==2 → 未命中,不跳(无 reset_scene)
        assert_eq!(run_goto_gif(sc, 0), 0, "== 未命中不应跳转");
    }

    #[test]
    fn go_gif_other_ops() {
        let base = |op: &str| format!("#S\n\\GO.G.IF(1, \"{op}\", 2, T)\n\\GO(END0)\n#T\n");
        assert_eq!(run_goto_gif(&base(">="), 2), 1);
        assert_eq!(run_goto_gif(&base(">="), 1), 0);
        assert_eq!(run_goto_gif(&base("!="), 3), 1);
        assert_eq!(run_goto_gif(&base("!="), 2), 0);
        assert_eq!(run_goto_gif(&base("<"), 1), 1);
        assert_eq!(run_goto_gif(&base("<="), 2), 1);
    }
}

#[cfg(test)]
mod title_menu_tests {
    //! 标题菜单 G1 写入回归(成果 78):\TITLE → 点 START → G1=1 →
    //! \GO.G.IF(1,"==",1,MAIN) 命中(非兜底 FALLBACK),对齐引擎原生流。

    use super::*;

    struct TitleHost {
        globals: std::cell::RefCell<std::collections::HashMap<usize, i64>>,
        resets: std::cell::Cell<usize>,
        /// poll_title_menu 返回的动作(模拟点击的按钮)。
        action: TitleMenuAction,
    }
    impl ScenarioHost for TitleHost {
        fn show_bg(&mut self, _: &str, _: u64, _: [u8; 3]) {}
        fn show_sprite(&mut self, _: &str, _: Option<&str>, _: i64, _: i64, _: u64) {}
        fn show_tachie(&mut self, _: &str, _: i64, _: i64, _: u64) {}
        fn hide_sprite(&mut self, _: &str, _: u64) {}
        fn fade(&mut self, _: bool, _: u64, _: [u8; 3]) {}
        fn show_text(&mut self, _: Option<u32>, _: &str, _: &str) {}
        fn clear_text(&mut self) {}
        fn play_voice(&mut self, _: &str) {}
        fn play_bgm(&mut self, _: &str, _: Option<i64>) {}
        fn play_se(&mut self, _: &str) {}
        fn title_screen(&mut self) {}
        fn poll_title_menu(&mut self) -> Option<TitleMenuAction> {
            Some(self.action) // 模拟点击指定按钮
        }
        fn poll_choice(&mut self, _: usize) -> Option<usize> {
            None
        }
        fn show_choices(&mut self, _: &[String]) {}
        fn clear_choices(&mut self) {}
        fn reset_scene(&mut self) {
            self.resets.set(self.resets.get() + 1);
        }
        fn global(&self, slot: usize) -> i64 {
            self.globals.borrow().get(&slot).copied().unwrap_or(0)
        }
        fn set_global(&mut self, slot: usize, value: i64) {
            self.globals.borrow_mut().insert(slot, value);
        }
        fn log(&mut self, _: &str) {}
    }

    /// scenario:`#TITLE` 段 = \TITLE + 双 \GO.G.IF + 兜底;点击 `action`
    /// 按钮后断言 G1 写入值与跳转落点(reset_scene 次数)。
    fn run_title_click(action: TitleMenuAction) -> (Option<i64>, usize) {
        let sc = "#TITLE\n\\TITLE\n\\GO.G.IF(1, \"==\", 1, MAIN)\n\\GO.G.IF(1, \"==\", 2, ARA)\n\\GO(FALLBACK)\n#MAIN\n\\END\n#ARA\n\\END\n#FALLBACK\n\\END\n";
        let mut p = ScenarioPlayer::new(vec![("t.txt".into(), sc.as_bytes().to_vec())]);
        let _ = p.goto("TITLE");
        let mut h = TitleHost {
            globals: Default::default(),
            resets: std::cell::Cell::new(0),
            action,
        };
        p.tick(&mut h, false); // \TITLE → Wait::TitleMenu
        p.tick(&mut h, false); // poll → action → 写 G1 → 继续执行循环
        let g1 = h.globals.borrow().get(&1).copied();
        (g1, h.resets.get())
    }

    #[test]
    fn title_start_writes_g1_and_branches() {
        // START → G1=1 → \GO.G.IF(1,"==",1,MAIN) 命中(1 次 reset_scene,非兜底)
        let (g1, resets) = run_title_click(TitleMenuAction::Start);
        assert_eq!(g1, Some(1), "START 应写 G1=1");
        assert_eq!(resets, 1, "应命中 \\GO.G.IF 跳 MAIN(1 次 reset_scene),而非兜底");
    }

    #[test]
    fn title_outline_writes_g2_and_branches() {
        // OUTLINE(あらすじ)→ G1=2 → \GO.G.IF(1,"==",2,ARA) 命中(成果 79)
        let (g1, resets) = run_title_click(TitleMenuAction::Outline);
        assert_eq!(g1, Some(2), "OUTLINE 应写 G1=2");
        assert_eq!(resets, 1, "应命中 \\GO.G.IF 跳 ARA(1 次 reset_scene),而非兜底");
    }
}

#[cfg(test)]
mod camera_tests {
    //! `\BG.CMXYZ` / `\EV.CMXYZ` 解析回归(layout-fix-plan P1-4):
    //! 命令名/修饰符分离模型下必须走相机通道,不得再被当作换背景
    //! (旧实现 name=="CMXYZ" 分支永不命中,已勘误)。

    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct CamHost {
        bgs: RefCell<Vec<(String, i64)>>,
        bcam: RefCell<Vec<(i64, i64, i64)>>,
        ecam: RefCell<Vec<(i64, i64, i64)>>,
        moves: RefCell<Vec<(String, i64, i64, i64, i64, u64)>>,
    }
    impl ScenarioHost for CamHost {
        fn show_bg(&mut self, name: &str, fade_ms: u64, _: [u8; 3]) {
            self.bgs.borrow_mut().push((name.to_string(), fade_ms as i64));
        }
        fn show_sprite(&mut self, _: &str, _: Option<&str>, _: i64, _: i64, _: u64) {}
        fn show_tachie(&mut self, _: &str, _: i64, _: i64, _: u64) {}
        fn hide_sprite(&mut self, _: &str, _: u64) {}
        fn fade(&mut self, _: bool, _: u64, _: [u8; 3]) {}
        fn show_text(&mut self, _: Option<u32>, _: &str, _: &str) {}
        fn clear_text(&mut self) {}
        fn play_voice(&mut self, _: &str) {}
        fn play_bgm(&mut self, _: &str, _: Option<i64>) {}
        fn play_se(&mut self, _: &str) {}
        fn title_screen(&mut self) {}
        fn poll_choice(&mut self, _: usize) -> Option<usize> {
            None
        }
        fn show_choices(&mut self, _: &[String]) {}
        fn clear_choices(&mut self) {}
        fn reset_scene(&mut self) {}
        fn global(&self, _: usize) -> i64 {
            0
        }
        fn log(&mut self, _: &str) {}
        fn bg_camera(&mut self, x: i64, y: i64, z: i64) {
            self.bcam.borrow_mut().push((x, y, z));
        }
        fn ev_camera(&mut self, x: i64, y: i64, z: i64) {
            self.ecam.borrow_mut().push((x, y, z));
        }
        fn move_tachie(&mut self, name: &str, x1: i64, y1: i64, x2: i64, y2: i64, ms: u64) {
            self.moves
                .borrow_mut()
                .push((name.to_string(), x1, y1, x2, y2, ms));
        }
    }

    fn run(script: &str) -> CamHost {
        let mut p = ScenarioPlayer::new(vec![("t.txt".into(), script.as_bytes().to_vec())]);
        let _ = p.goto("S");
        let mut h = CamHost::default();
        p.tick(&mut h, false);
        h
    }

    #[test]
    fn bg_cmxyz_routes_to_camera_not_bg() {
        let h = run("#S\n\\BG.CMXYZ(108, 0, -51)\n\\BG(bg04b , 260, 0, 0)\n");
        assert_eq!(*h.bcam.borrow(), vec![(108, 0, -51)]);
        let bgs = h.bgs.borrow();
        assert_eq!(bgs.len(), 1, "CMXYZ 不得触发 show_bg");
        assert_eq!(bgs[0].0, "bg04b", "相机行之后才是真背景");
    }

    #[test]
    fn ev_cmxyz_routes_to_ev_camera() {
        let h = run("#S\n\\EV.CMXYZ(-72, -216, -64)\n");
        assert_eq!(*h.ecam.borrow(), vec![(-72, -216, -64)]);
        assert!(h.bgs.borrow().is_empty());
    }

    #[test]
    fn t_eight_params_route_to_move() {
        // 8 参 \T = 位移动画(x1,y1,z1,x2,y2,z2)
        let h = run("#S\n\\T(A_HAN_1A0100, 500, 100, 10, 200, 300, 20, 200)\n");
        assert_eq!(
            *h.moves.borrow(),
            vec![("A_HAN_1A0100".to_string(), 100, 10, 300, 20, 500)]
        );
    }
}
