//! 全局日志器（进程内唯一）与便捷宏。
//!
//! 设计要点：
//! - **惰性初始化**：未调用 [`init`] 时仍可记日志（缺省 `Info` + stderr），
//!   库使用者不必改接线就能看到输出。
//! - **等级阈值可运行期调整**（`YURIS_LOG_LEVEL` / [`set_level`]）。
//! - **致命错误旁路**：即便 `Off`，[`fatal!`](crate::fatal) 仍产生错误窗报告。
//! - 宏在阈值不足时**先判再格式化**，热路径（逐指令 trace）无额外分配。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::{Config, FatalReport, Level, Logger};

static GLOBAL: OnceLock<Arc<Logger>> = OnceLock::new();
/// 等级覆盖（`u8`；`NO_OVERRIDE` = 未设置）。
///
/// 用原子而非 `Mutex`：`set_level` 可能在另一线程初始化 `GLOBAL` 期间被调用，
/// 两把锁互等会死锁。原子读写在初始化路径上是无锁的。
static LEVEL_OVERRIDE: AtomicU8 = AtomicU8::new(NO_OVERRIDE);
const NO_OVERRIDE: u8 = u8::MAX;
static FATAL_SEEN: AtomicBool = AtomicBool::new(false);

/// 致命错误的注册回调（错误窗/宿主据此唤醒主循环）。
///
/// 回调可能在任意线程被调用，实现方需自行同步。
struct CallbackSlot {
    f: Box<dyn Fn(&FatalReport) + Send + Sync>,
}

static CALLBACK: OnceLock<Mutex<Option<CallbackSlot>>> = OnceLock::new();

fn callback_slot() -> &'static Mutex<Option<CallbackSlot>> {
    CALLBACK.get_or_init(|| Mutex::new(None))
}

/// 全局日志器（首次访问时按环境变量初始化）。
pub fn global() -> &'static Arc<Logger> {
    GLOBAL.get_or_init(|| {
        let mut config = Config::from_env();
        let override_level = LEVEL_OVERRIDE.load(Ordering::Relaxed);
        if override_level != NO_OVERRIDE {
            config.level = Level::from_u8(override_level);
        }
        Arc::new(Logger::new(config))
    })
}

/// 初始化全局日志器（重复调用只保留第一次）。
///
/// 返回 `false` 表示已有实例（本次配置被忽略）。
pub fn init(mut config: Config) -> bool {
    if GLOBAL.get().is_some() {
        return false;
    }
    let override_level = LEVEL_OVERRIDE.load(Ordering::Relaxed);
    if override_level != NO_OVERRIDE {
        config.level = Level::from_u8(override_level);
    }
    GLOBAL.set(Arc::new(Logger::new(config))).is_ok()
}

/// 初始化全局日志器（从环境变量读配置）；重复调用只保留第一次。
pub fn init_from_env() -> bool {
    if GLOBAL.get().is_some() {
        return false;
    }
    GLOBAL
        .set(Arc::new(Logger::new(Config::from_env())))
        .is_ok()
}

/// 直接使用日志文件初始化（追加模式）。
pub fn init_with_file(path: impl Into<PathBuf>) -> bool {
    let mut cfg = Config::from_env();
    cfg.file = Some(path.into());
    init(cfg)
}

/// 设置全局过滤阈值（初始化前后都有效）。
pub fn set_level(level: Level) {
    LEVEL_OVERRIDE.store(level as u8, Ordering::Relaxed);
    if let Some(log) = GLOBAL.get() {
        log.set_level(level);
    }
}

/// 当前全局过滤阈值。
pub fn level() -> Level {
    global().level()
}

/// 注册致命错误回调（替换旧回调）。
pub fn on_fatal(f: impl Fn(&FatalReport) + Send + Sync + 'static) {
    if let Ok(mut slot) = callback_slot().lock() {
        *slot = Some(CallbackSlot { f: Box::new(f) });
    }
}

/// 记录致命错误并通知回调/唤醒标志。
///
/// 这是 `fatal!` 宏与库内错误升级的统一入口。
pub fn report_fatal(target: &'static str, title: &str, message: impl Into<String>) -> FatalReport {
    let report = global().fatal(target, title, message);
    FATAL_SEEN.store(true, Ordering::Release);
    if let Ok(slot) = callback_slot().lock() {
        if let Some(cb) = slot.as_ref() {
            (cb.f)(&report);
        }
    }
    report
}

/// 消费一个错误并升级为致命错误。
pub fn report_fatal_error<E: std::fmt::Display>(
    target: &'static str,
    context: &str,
    error: E,
) -> FatalReport {
    report_fatal(
        target,
        "致命错误 / FATAL ERROR",
        format!("{context}: {error}"),
    )
}

/// 是否已出现过致命错误（主循环据此停止推进）。
pub fn has_fatal() -> bool {
    FATAL_SEEN.load(Ordering::Acquire) || global().has_fatal()
}

/// 清除致命错误状态（错误窗关闭 / 恢复后调用）。
pub fn clear_fatal() {
    FATAL_SEEN.store(false, Ordering::Release);
    global().clear_fatal();
}

/// 当前致命错误报告。
pub fn fatal_report() -> Option<FatalReport> {
    global().fatal_report()
}

/// 最近日志（错误窗与诊断导出用）。
pub fn recent(limit: usize) -> Vec<crate::Record> {
    global().recent(limit)
}

/// 记录一条日志（宏 [`log!`](crate::log) 的底层入口）。
pub fn log(level: Level, target: &'static str, message: impl Into<String>) {
    global().log(level, target, message);
}

/// 是否启用某等级（热路径先判再格式化）。
pub fn enabled(level: Level) -> bool {
    global().enabled(level)
}

/// 记录一条**绕过阈值**的日志（显式取证开关专用；见 `Logger::log_forced`）。
pub fn log_forced(level: Level, target: &'static str, message: impl Into<String>) {
    global().log_forced(level, target, message);
}

/// **醒目告警横幅**：未知命令 / 未建模参数 / 被降级的未实现路径专用。
///
/// 与 [`warn!`](crate::warn) 的区别:
/// - **绕过等级阈值**（`YURIS_LOG_LEVEL=off` 也能看到）—— 缺什么不能因为
///   日志过滤而再次消失；
/// - 多行框线,实机滚动时一眼可辨;内容同时进入环形缓存,错误窗可回溯；
/// - **首行是摘要**（跟在平台日志前缀之后），其余行进框；
/// - **前后各留一个空行**：连续告警时不会糊成一坨（实机反馈：
///   「连续大告警分不清哪边是哪边」）。
///
/// 调用方负责去重（同一现场只打一次），避免每帧刷屏。
pub fn alert(target: &'static str, message: impl Into<String>) {
    const BAR: &str = "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!";
    let message = message.into();
    let mut lines = message.lines().skip_while(|l| l.trim().is_empty());
    // 摘要放在平台前缀那一行（`[t] WARN module: 摘要`），这样即使
    // 终端把多行记录折叠，最关键的「缺了什么」也不会丢。
    let head = lines.next().unwrap_or("运行时未实现 / 未知路径").trim_end();
    let mut out = String::with_capacity(message.len() + 320);
    out.push_str(head);
    out.push('\n');
    out.push('\n'); // 摘要与框体之间空一行
    out.push_str(BAR);
    out.push('\n');
    out.push_str("!!  YURIS ALERT\n");
    for line in lines {
        if line.trim().is_empty() {
            out.push_str("!!\n");
            continue;
        }
        out.push_str("!!  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(BAR);
    // 尾部换行 → 调用端 `writeln!` 再补一个 = 与下一条记录之间**空一行**。
    out.push('\n');
    global().log_forced(Level::Warn, target, out);
}

/// 进程级去重的 [`alert`]（同一 `key` 只打一次横幅）。
///
/// 用于没有自建去重集的对象（如 `scenario` 运行器、渲染侧效果槽）。
/// 键集合上限 4096,超出后不再打新横幅（宁漏报不涨内存）。
pub fn alert_once(target: &'static str, key: impl Into<String>, message: impl Into<String>) {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let key: String = key.into();
    let key = format!("{target}::{key}");
    let first = match seen.lock() {
        Ok(mut g) => {
            if g.len() >= 4096 {
                false
            } else {
                g.insert(key)
            }
        }
        Err(_) => true,
    };
    if first {
        alert(target, message);
    }
}

/// 记录一条指定等级的日志（阈值不足时不格式化）。
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {{
        if $crate::enabled($level) {
            $crate::log($level, module_path!(), format!($($arg)*));
        }
    }};
}

/// 记录 `Trace` 级日志（受全局等级过滤）。
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Trace, $($arg)*)
    };
}

/// 记录 `Trace` 级日志，**绕过等级阈值**。
///
/// 只用于已经被 `YURIS_TRACE_*` 之类环境变量显式门控的输出：开关打开
/// 就应该有输出，不能被 `YURIS_LOG_LEVEL` 再拦一层。
#[macro_export]
macro_rules! trace_forced {
    ($($arg:tt)*) => {
        $crate::log_forced($crate::Level::Trace, module_path!(), format!($($arg)*))
    };
}

/// 记录 `Debug` 级日志。
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Debug, $($arg)*)
    };
}

/// 记录 `Info` 级日志。
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Info, $($arg)*)
    };
}

/// 记录 `Warn` 级日志。
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Warn, $($arg)*)
    };
}

/// 记录 `Error` 级日志。
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::log!($crate::Level::Error, $($arg)*)
    };
}

/// 记录**崩坏级**错误并触发画布错误窗。
///
/// `fatal!("主消息")` 或 `fatal!(title: "标题", "主消息")`。
#[macro_export]
macro_rules! fatal {
    (title: $title:expr, $($arg:tt)*) => {
        $crate::report_fatal(module_path!(), $title, format!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::report_fatal(module_path!(), "致命错误 / FATAL ERROR", format!($($arg)*))
    };
}

/// **醒目告警横幅**（未知命令 / 未建模参数 / 被降级的未实现路径）。
///
/// 绕过等级阈值，多行框线，便于实机滚动时辨认。
#[macro_export]
macro_rules! alert {
    ($($arg:tt)*) => {
        $crate::alert(module_path!(), format!($($arg)*))
    };
}

/// 把 `Result` 的 `Err` 消费为日志（不中断流程）。
#[macro_export]
macro_rules! log_err {
    ($level:expr, $ctx:expr, $result:expr) => {
        if let Err(e) = $result {
            $crate::log!($level, "{}: {}", $ctx, e);
        }
    };
}

/// 把 `Result` 的 `Err` 消费为**崩坏级**错误并返回报告。
#[macro_export]
macro_rules! fatal_err {
    ($ctx:expr, $result:expr) => {
        match $result {
            Ok(v) => v,
            Err(e) => {
                $crate::report_fatal_error(module_path!(), $ctx, e);
                return;
            }
        }
    };
}
