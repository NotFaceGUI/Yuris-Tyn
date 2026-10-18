//! 启动链编排器(P0 第3项)。
//!
//! 引擎启动链(成果 25,FUN_0046b63c,Confirmed):
//! `YSCM 命令表 → 变量描述符分配 → 载 YSVR → 应用初始值 → 哈希查入口标签
//! → 建任务 → 加载脚本 → 运行`。
//!
//! 本模块把现有件串联:[`yuris_format::ypf::YpfArchive`](格式化解析) +
//! [`YslbTable`](标签表查找入口) + [`YpfScriptHost`](加载目标脚本) +
//! [`GroupVm`](执行)。入口标签由调用方给定(缺省从 YSLB 找 `SYSTEM_START`)。
//!
//! **铁律**:Unknown 不猜;各步若缺证据则返回 `Error::Unimplemented`。

use std::collections::HashMap;

use yuris_core::{Error, Result};
use yuris_format::yslb::YslbTable;

use crate::GroupVm;
use yuris_value::Value;
use crate::host::{ScriptHost, YpfScriptHost};

/// 启动配置。
pub struct Bootstrap {
    /// 脚本封包字节(bn.ypf)。
    pub ypf_bytes: Vec<u8>,
    /// 封包文件名 XOR key(样本 0xC9)。
    pub name_key: u8,
    /// 脚本条目 XOR 密钥(如 `2b904f93`)。
    pub key: [u8; 4],
    /// 执行入口标签名(缺省从 YSLB 查 `SYSTEM_START`)。
    pub entry_label: Option<Vec<u8>>,
}

/// 启动结果:已初始化的 VM(尚未 run)。
pub struct Booted {
    /// 就绪的 GroupVm(已绑定 Host + 标签表 + 入口脚本)。
    pub vm: GroupVm,
    /// 当前脚本号(入口脚本)。
    pub script_id: u16,
    /// 入口组下标。
    pub entry_pc: u32,
    /// 已应用的 YSVR 条目数(若提供了 YSLB/YSVR,否则 0 —— 见 [`Bootstrap::boot`])。
    pub applied_ysvr: usize,
    /// 运行期命令分派表来源(诊断;应为游戏自身 YSCM)。
    pub command_profile_source: String,
    /// 游戏标题(`ysbin\yscfg.ybn` 的 caption 字段;缺失/解析失败 → None)。
    ///
    /// 引擎侧依据:`docs/01-runtime-architecture.md` C9 —— YSCF 含
    /// version/screen/**caption**;`crates/yuris-format/src/yscf.rs` 已解析该字段。
    /// 壳层用它给窗口起名,**不要**写死任何一款游戏的名字。
    pub caption: Option<String>,
    /// 游戏自述的**屏幕分辨率**(`ysbin\yscfg.ybn` 的 `screen_width/height`)。
    ///
    /// **勘误(2026-10-13,成果 121)**:逻辑画布的真值在这里 —— 内核曾把它写死
    /// 成 1920×1080(最初样本 v555 的值)。本作 <game-b> 实测 **1100×825**,
    /// 写死会让所有层坐标按错画布缩放(实机报告「分辨率有问题」「CG 混乱」)。
    pub screen: Option<(u32, u32)>,
}

impl Bootstrap {
    /// 编排启动链,返回就绪的 [`GroupVm`]。
    ///
    /// - 从 `ypf` 读 YSLB+脚本;若提供了标签表/初值表则一并接入。
    /// - 缺省入口标签 = `SYSTEM_START`。
    pub fn boot(self) -> Result<Booted> {
        let host = YpfScriptHost::from_ypf_bytes(self.ypf_bytes, self.name_key, self.key)?;
        Self::boot_with_host(host, self.entry_label)
    }

    /// **通用**启动:宿主已就绪(包发现 / 索引方言判别 / 脚本密钥猜测由调用方
    /// 经 [`YpfScriptHost::open`] 完成)。
    ///
    /// 与 [`Bootstrap::boot`] 同一条启动链,只是不要求把封包整载进内存 ——
    /// 大包(实测有 3.3 GB 的发行数据)必须走这条路径。
    pub fn boot_with_host(
        mut host: YpfScriptHost,
        entry_label: Option<Vec<u8>>,
    ) -> Result<Booted> {

        // —— 启动链第 1 步:载 YSCM 命令字典,建**运行期命令分派表** ——
        // 引擎顺序(FUN_0046b63c):YSCM 命令表 → 变量描述符 → YSVR → 入口标签
        // → 加载脚本 → 运行。命令序号逐游戏不同(实测两版本共有命令里 104/116
        // 条序号漂移),故分派只能来自**游戏自己的 ysc**,不得用硬编码常量表。
        let ysc_profile = load_command_profile(&mut host)?;

        let yslb_data = read_boot_entry(&mut host, "ysbin", "ysl.ybn")?;
        let yslb = YslbTable::from_bytes(&yslb_data)?;

        // 入口标签名
        let entry: &[u8] = entry_label.as_deref().unwrap_or(b"SYSTEM_START");
        let entry_idx = yslb.find(entry).ok_or_else(|| {
            Error::format(format!("入口标签不存在: {:?}", String::from_utf8_lossy(entry)))
        })?;
        let label = &yslb.labels()[entry_idx];
        let (entry_pc, script_id) = (label.target_pc, label.script_id);

        // 预读 YSTL 脚本名表(`SYSTEMINFO` 的 SCRIPTFILE/SCRIPTNUM/TEXTNUM 真值)。
        // 缺文件不是错误:`SYSTEMINFO` 会按「表未建立」答 0/空串,与引擎一致。
        let ystl = read_boot_entry(&mut host, "ysbin", "yst_list.ybn")
            .ok()
            .and_then(|d| match yuris_format::ystl::YstlTable::from_bytes(&d) {
                Ok(t) => Some(t),
                Err(e) => {
                    // 布局变化必须可观测(不能静默当"没有")。
                    yuris_log::alert_once(
                        module_path!(),
                        "ystl-parse-fail",
                        format!(
                            "yst_list.ybn 解析失败,SYSTEMINFO 查询将答 0/空串
                             错误: {e}
                             处置: 该文件的布局与本内核建模不一致(见 yuris-format::ystl)"
                        ),
                    );
                    None
                }
            });

        // 预读 YSVR(在 host move 进 Box 前完成所有借用)
        let ysvr_data = read_boot_entry(&mut host, "ysbin", "ysv.ybn")?;
        let ysvr = yuris_format::ysvr::YsvrTable::from_bytes(&ysvr_data)?;

        // 游戏自述(标题):`ysbin\yscfg.ybn` 的 caption 字段。缺失不算错误 ——
        // 不是所有发布都带该条目,壳层回退到目录名。
        let yscf = read_boot_entry(&mut host, "ysbin", "yscfg.ybn")
            .ok()
            .and_then(|d| yuris_format::yscf::YscfFile::from_bytes(&d).ok());
        let caption: Option<String> = yscf
            .as_ref()
            .map(|f| f.caption.clone())
            .filter(|c| !c.is_empty());
        // 屏幕分辨率 = **逻辑画布真值**(0/缺省视为未配置)。
        let screen: Option<(u32, u32)> = yscf
            .as_ref()
            .map(|f| (f.screen_width, f.screen_height))
            .filter(|(w, h)| *w > 0 && *h > 0);

        let ctx = host.load(script_id)?;

        // 构建单脚本 VM(装载入口脚本)
        let mut vm = GroupVm::load(ctx.script)?;
        // 分辨率注入(成果 123):`WINDOWINFO NO=0 SX/SY` 与 `@_SCREEN_SIZE_X/Y`
        // 的真值 = 游戏自身 yscfg;写死会让整套布局(es_First 的 @es.WSX/WSY)错位。
        if let Some((w, h)) = screen {
            vm.set_screen_size(w as i64, h as i64);
        }
        vm.set_script_id(script_id);

        // 注入游戏自身 YSCM 建的分派表(覆盖 load() 按 header 版本选的内置表)。
        let command_profile_source = ysc_profile
            .as_ref()
            .map(|p| p.source().to_string())
            .unwrap_or_else(|| "none (游戏 ysc.ybn 缺失)".into());
        vm.set_command_profile(ysc_profile);

        // 注入 yst_list 脚本名表(SYSTEMINFO 真值来源)。
        vm.set_ystl_table(ystl);

        // 注入标签表(name → (target_pc, script_id))
        let mut labels: HashMap<Vec<u8>, (u32, u16)> = HashMap::new();
        for l in yslb.labels() {
            labels.insert(l.name.clone(), (l.target_pc, l.script_id));
        }
        vm.set_labels(labels);

        // 声明消费 = **脚本装载时**做（引擎语义），不在这里扫全包。
        //
        // **勘误（2026-10-18，用户指出）**：旧实现 boot 时遍历**全部** 160 个
        // 脚本消费声明。那是错的：
        // - 引擎的声明消费在**加载器后处理**（`FUN_00451348`，随脚本装载触发），
        //   不是启动时把包里所有脚本都过一遍；
        // - 实测该作 160 个脚本里 5 个是 v479 头，而它们在 YSLB 里
        //   **一条标签都没有**（139/160 被引用）⇒ 引擎从不加载它们。
        //   全量扫描纯属把「永远不会跑的脚本」也拉进来解释，既慢又制造
        //   「同一份数据两套语义」的伪问题。
        //
        // 现在：入口脚本在此消费一次；其余脚本由 `switch_script`
        // （GO/GOSUB 跨脚本、TASK 装载）首次加载时消费。
        {
            let ctx = host.load(script_id)?;
            let d = vm.command_dispatch().clone();
            crate::consume_declarations_into(vm.store_mut(), &ctx, &d)?;
            vm.mark_declarations_consumed(script_id);
        }

        // 绑定 host(此后跨脚本可用)
        vm.set_host(Box::new(host));

        // 应用 YSVR 全局初值(启动链「应用初值」步;须在声明之后,否则覆盖初值)。
        // kind1+kind3 在此应用;kind2 暂存,由入口脚本/switch_script 首载应用。
        let applied_ysvr = vm.apply_ysvr(&ysvr)?;
        vm.apply_ysvr_for_script(script_id)?;

        // 注:$1045(存档目录串)引擎侧 = $105 = YSVR 空串(引擎经 VFS 原生
        // 前缀探测命中 save/*.sd,与 $1045 无关;成果 59e)—— 不在此种子。

        // 入口组:引擎先取后增,直接设入口组
        vm.set_pc(entry_pc as usize);
        // 引擎 TaskInit 第 8 步:任务体 = **可重入执行位**。基帧 RETURN 的
        // 引擎结局不是"任务终结",而是 `FUN_00451724` 清空"使命"指针后按
        // 优先级**重排队**(`0x42f7bc`→`0x452364` 解链+insert 实机读)——
        // 链上下一使命到来时,同一执行位从记录的 (pc, 脚本) 原地复跑
        // ([`GroupVm::RETURN` 发 `Wait{None,None}` 承载];见 lib.rs 成果 132)。
        vm.set_running();
        Ok(Booted {
            caption,
            screen,
            vm,
            script_id,
            entry_pc,
            applied_ysvr,
            command_profile_source,
        })
    }
}

impl GroupVm {
    /// 事件流推进到首个 Wait/Complete。供启动链驱动。
    pub fn set_pc(&mut self, pc: usize) {
        self.pc = pc;
    }

    /// 启动链把执行位置为 Running(成果 132:基帧 RETURN 是 wait-型让出,
    /// 引擎按 `FUN_00451724` 重排队;VM 这边只需 Idle/Running 可复读)。
    pub fn set_running(&mut self) {
        self.state = crate::VmState::Running;
    }

    /// P9.2 输入注入(播放器):按键名写入引擎输入串 `$55[1]`。
    ///
    /// `$55` 属帧系统变量族(成果 52:系统族无全局回退)——键链
    /// (s190 `STR $6409 = $55[1]`)在**其执行帧**内读取,注入时刻的
    /// 当前帧未必是同一帧(实测踩坑)。广播写入:全局 + 全部活动帧,
    /// 保证任一层级的键链本帧都能读到。
    pub fn inject_input_key(&mut self, key: &[u8]) -> Result<()> {
        let r = yuris_value::VarRef {
            space: yuris_value::VarSpace::Dollar,
            id: 55,
        };
        let v = Value::Str(key.to_vec());
        for f in &mut self.frames {
            if f.locals.has_array(&r) {
                let _ = f.locals.set_elem(&r, &[1], v.clone());
            }
        }
        self.store.set_elem(&r, &[1], v)
    }

    /// 诊断:读注入串(顶帧优先,与求值读路径对称)。
    pub fn input_key_snapshot(&self) -> String {
        let r = yuris_value::VarRef {
            space: yuris_value::VarSpace::Dollar,
            id: 55,
        };
        if let Some(f) = self.frames.last() {
            if let Ok(yuris_value::Value::Str(bs)) = f.locals.get_elem(&r, &[1]) {
                return String::from_utf8_lossy(bs).into_owned();
            }
        }
        match self.store.get_elem(&r, &[1]) {
            Ok(yuris_value::Value::Str(bs)) => String::from_utf8_lossy(bs).into_owned(),
            _ => "<undef>".into(),
        }
    }

    /// 清除输入注入(全帧 + 全局;见 [`Self::inject_input_key`])。
    pub fn clear_input_key(&mut self) -> Result<()> {
        let r = yuris_value::VarRef {
            space: yuris_value::VarSpace::Dollar,
            id: 55,
        };
        let v = Value::Str(Vec::new());
        for f in &mut self.frames {
            if f.locals.has_array(&r) {
                let _ = f.locals.set_elem(&r, &[1], v.clone());
            }
        }
        self.store.set_elem(&r, &[1], v)
    }
}

/// 从封包里找 `ysbin\ysc.ybn` 并建 [`yuris_core::CommandProfile`]。
///
/// 条目名的**虚拟根前缀**（`$` / `%` / `9` …）逐包不同，故按**后缀**匹配而不
/// 写死全名。找不到 → `Ok(None)`（调用方保持 header 版本内置表或未解析态）。
///
/// **参数名表一并取回**：YSTB 参数窗口的 `tag & 0xff` 是该命令在 YSCM 参数表
/// 里的下标（槽位），而槽位号逐版本漂移（实测 `CG.FILE`：v555 槽 46 /
/// v494 槽 33 / v479 槽 29）。只带名字的表无法按参数名解析槽位 —— 实测后果是
/// CG 的 `FILE` 永远读空、纹理不装载、画面全黑。
fn load_command_profile(
    host: &mut YpfScriptHost,
) -> Result<Option<yuris_core::CommandProfile>> {
    let suffix = r"ysbin\ysc.ybn";
    let name: Option<String> = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(suffix));

    let Some(name) = name else {
        return Ok(None);
    };
    let blob = host.read_entry(&name)?;
    let table = yuris_format::yscm::YscmTable::from_bytes(&blob)?;
    let version = table.version();
    let names: Vec<String> = table.commands().iter().map(|c| c.name.clone()).collect();
    let params: Vec<Vec<String>> = table
        .commands()
        .iter()
        .map(|c| c.params.iter().map(|p| p.name.clone()).collect())
        .collect();
    Ok(Some(yuris_core::CommandProfile::from_names_and_params(
        names,
        params,
        version,
        format!("game ysc.ybn ({name}, YSCM v{version})"),
    )))
}

/// 按「目录段 + 文件名」在封包内查启动链条目。
///
/// **不写死根前缀**:条目名的虚拟根前缀逐包不同(实测有 `$` / `%` 前缀的族,
/// 也有完全无前缀的族),故只按尾部两段匹配。
fn read_boot_entry(host: &mut YpfScriptHost, dir: &str, file: &str) -> Result<Vec<u8>> {
    let want = format!("{dir}{sep}{file}", sep = CHAR_SEP);
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.replace('/', &CHAR_SEP.to_string()).to_ascii_lowercase().ends_with(&want))
        .ok_or_else(|| Error::format(format!("启动链所需条目缺失: {want}")))?;
    host.read_entry(&name)
}

/// 路径分隔符(SJIS/ASCII 包内统一为反斜杠)。
const CHAR_SEP: char = '\\';
