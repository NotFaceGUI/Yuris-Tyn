//! 日志等级。
//!
//! 等级是**过滤阈值**而非独立的日志通道：低于阈值的记录不产生格式化开销。
//! `Off` 只作为阈值使用，从不写进 [`Record`](crate::Record)。

use std::fmt;
use std::str::FromStr;

/// 日志等级（数值越大越严重）。
///
/// 阈值语义 = 「**至少**这么严重才输出」：`record.level >= 阈值` 即放行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    /// 逐指令/逐事件追踪（YURIS_TRACE_* 取证）。
    Trace = 0,
    /// 诊断细节（正常游玩不输出）。
    Debug = 1,
    /// 生命周期与状态变化（启动、场景切换、存档）。
    Info = 2,
    /// 可恢复异常（资源缺失、解码失败、未实装特性）。
    Warn = 3,
    /// 错误（功能失败但仍可继续）。
    Error = 4,
    /// **崩坏级错误**：触发画布中央的错误窗，主循环停止推进。
    Fatal = 5,
    /// 关闭全部输出（仅阈值，不是记录级别）。
    Off = 6,
}

impl Level {
    /// 所有可输出的等级（不含 [`Level::Off`]）。
    pub const OUTPUT_LEVELS: [Level; 6] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
    ];

    /// 全大写名称（写日志行用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
            Level::Off => "OFF",
        }
    }

    /// 单字母缩写（错误窗窄列用）。
    pub const fn short(self) -> &'static str {
        match self {
            Level::Trace => "T",
            Level::Debug => "D",
            Level::Info => "I",
            Level::Warn => "W",
            Level::Error => "E",
            Level::Fatal => "F",
            Level::Off => "-",
        }
    }

    /// ANSI SGR 前缀（不含重置；调用方负责收尾）。
    pub const fn ansi(self) -> &'static str {
        match self {
            Level::Trace => "\x1b[90m",
            Level::Debug => "\x1b[36m",
            Level::Info => "\x1b[32m",
            Level::Warn => "\x1b[33m",
            Level::Error => "\x1b[1;31m",
            Level::Fatal => "\x1b[1;37;41m",
            Level::Off => "",
        }
    }

    /// 由 `repr(u8)` 值还原（越界 = [`Level::Off`]）。
    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Level::Trace,
            1 => Level::Debug,
            2 => Level::Info,
            3 => Level::Warn,
            4 => Level::Error,
            5 => Level::Fatal,
            _ => Level::Off,
        }
    }

    /// 解析等级名（大小写不敏感，接受常见缩写）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" | "tr" => Some(Level::Trace),
            "debug" | "dbg" => Some(Level::Debug),
            "info" | "inf" => Some(Level::Info),
            "warn" | "warning" | "wrn" => Some(Level::Warn),
            "error" | "err" => Some(Level::Error),
            "fatal" | "panic" => Some(Level::Fatal),
            "off" | "none" | "silent" => Some(Level::Off),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Level {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Level::parse(s).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_severity_order() {
        assert!(Level::Trace < Level::Debug);
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
        assert!(Level::Error < Level::Fatal);
    }

    #[test]
    fn parse_accepts_names_and_aliases() {
        assert_eq!(Level::parse("TRACE"), Some(Level::Trace));
        assert_eq!(Level::parse(" warning "), Some(Level::Warn));
        assert_eq!(Level::parse("err"), Some(Level::Error));
        assert_eq!(Level::parse("off"), Some(Level::Off));
        assert_eq!(Level::parse("verbose"), None);
    }
}
