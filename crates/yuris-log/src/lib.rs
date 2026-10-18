//! # yuris-log — 统一日志与致命错误汇聚层
//!
//! 全仓库**唯一**的日志入口。目标：
//!
//! 1. **统一等级管理**：`YURIS_LOG_LEVEL` 一处控制全内核输出；
//! 2. **消费各 crate 的 error 事件**：`Result::Err` 经 [`log_err!`] /
//!    [`fatal_err!`] 统一落日志；
//! 3. **崩坏级错误可视化**：`Fatal` 级写入 [`FatalReport`]，播放器壳在
//!    **画布中央**渲染错误窗（数据源即本包，不在渲染层重复定义）；
//! 4. **无第三方依赖**：核心包不引入 tracing/log，保持 `yuris-core` 同级
//!    的稳定性；需要 subscriber 的工具可自行桥接。
//!
//! ## 等级
//!
//! | 等级 | 用途 | 缺省 |
//! |---|---|---|
//! | `Trace` | 逐指令/逐事件取证 | 关 |
//! | `Debug` | 诊断细节 | 关 |
//! | `Info` | 生命周期/状态变化 | **开** |
//! | `Warn` | 可恢复异常（资源缺失、未实装） | 开 |
//! | `Error` | 功能失败但可继续 | 开 |
//! | `Fatal` | **崩坏级**：画布中央错误窗 | 开 |
//!
//! ## 快速使用
//!
//! ```no_run
//! use yuris_log::{info, warn, fatal, log_err, Level};
//!
//! info!("启动: v{}", env!("CARGO_PKG_VERSION"));
//! warn!("资源缺失 {path}", path = "bg_01.png");
//! log_err!(Level::Warn, "解码失败", std::fs::read("x"));
//! fatal!("无法初始化图形后端");
//! ```
//!
//! 环境变量见 [`Config::from_env`]。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod global;
mod level;
mod logger;
mod record;
mod sink;

pub use global::{
    alert, alert_once, clear_fatal, enabled, fatal_report, global, has_fatal, init, init_from_env,
    init_with_file, level, log, log_forced, on_fatal, recent, report_fatal, report_fatal_error,
    set_level,
};
pub use level::Level;
pub use logger::{Config, Logger};
pub use record::{FatalReport, Record};
pub use sink::{color_auto, FileSink, MemorySink, Sink, StderrSink};

/// crate 版本（与 workspace 同步）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局单例：等级阈值运行期可调，宏遵守阈值。
    #[test]
    fn global_level_can_be_changed() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let before = level();
        set_level(Level::Error);
        assert!(!enabled(Level::Info));
        assert!(enabled(Level::Error));
        set_level(before);
    }

    /// 致命错误宏产报告，清除后复位。
    #[test]
    fn fatal_macro_round_trip() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap();
        clear_fatal();
        let report = report_fatal("test::mod", "测试崩溃", "细节");
        assert_eq!(report.title, "测试崩溃");
        assert!(has_fatal());
        assert!(fatal_report().is_some());
        clear_fatal();
        assert!(!has_fatal());
    }
}
