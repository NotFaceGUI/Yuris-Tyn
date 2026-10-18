//! 日志器：等级过滤、输出端分发、环形缓存与致命错误汇聚。

use std::collections::VecDeque;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::record::unix_ms_now;
use crate::sink::{color_auto, parse_bool, FileSink, Sink, StderrSink};
use crate::{FatalReport, Level, Record};

/// 日志器配置。
#[derive(Debug, Clone)]
pub struct Config {
    /// 过滤阈值（`Off` = 全部静默；Fatal 仍会进入错误窗）。
    pub level: Level,
    /// 是否写 stderr。
    pub stderr: bool,
    /// 追加写文件（None = 不写文件）。
    pub file: Option<PathBuf>,
    /// 环形缓存容量（错误窗据此回溯最近日志）。
    pub ring_capacity: usize,
    /// stderr 是否使用 ANSI 颜色。
    pub color: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            level: Level::Info,
            stderr: true,
            file: None,
            ring_capacity: 512,
            color: true,
        }
    }
}

impl Config {
    /// 从环境变量构造：
    ///
    /// | 变量 | 含义 | 缺省 |
    /// |---|---|---|
    /// | `YURIS_LOG_LEVEL` | `trace/debug/info/warn/error/fatal/off` | `info` |
    /// | `YURIS_LOG_STDERR` | `0` 关闭 stderr | 开 |
    /// | `YURIS_LOG_FILE` | 追加写文件路径 | 不写 |
    /// | `YURIS_LOG_COLOR` | `0/1` 强制颜色 | 自动 |
    /// | `YURIS_LOG_RING` | 环形缓存条数 | 512 |
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("YURIS_LOG_LEVEL") {
            if let Some(level) = Level::parse(&v) {
                cfg.level = level;
            }
        }
        if let Ok(v) = std::env::var("YURIS_LOG_STDERR") {
            cfg.stderr = parse_bool(&v);
        }
        if let Ok(v) = std::env::var("YURIS_LOG_FILE") {
            let p = v.trim();
            if !p.is_empty() {
                cfg.file = Some(PathBuf::from(p));
            }
        }
        if let Ok(v) = std::env::var("YURIS_LOG_RING") {
            if let Ok(n) = v.trim().parse::<usize>() {
                if n > 0 {
                    cfg.ring_capacity = n;
                }
            }
        }
        cfg.color = color_auto();
        cfg
    }
}

/// 日志器实例（全局单例见 [`crate::global`]）。
pub struct Logger {
    level: AtomicU8,
    sinks: Vec<Box<dyn Sink>>,
    ring: Mutex<VecDeque<Record>>,
    ring_capacity: usize,
    fatal: Mutex<Option<FatalReport>>,
    seq: AtomicU64,
    start: Instant,
}

impl Logger {
    /// 按配置创建（stderr / 文件输出端自动装配）。
    pub fn new(config: Config) -> Self {
        let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
        if config.stderr {
            sinks.push(Box::new(StderrSink::new(config.color)));
        }
        if let Some(path) = &config.file {
            match FileSink::create(path) {
                Ok(s) => sinks.push(Box::new(s)),
                Err(e) => {
                    // 日志系统自身失败不能递归写日志（会无限回环），
                    // 这里是对 stderr 的**唯一**兜底输出。
                    eprintln!("yuris-log: 无法打开日志文件 {}: {e}", path.display());
                }
            }
        }
        Self::with_sinks(config.level, config.ring_capacity, sinks)
    }

    /// 使用自定义输出端（测试/工具）。
    pub fn with_sinks(level: Level, ring_capacity: usize, sinks: Vec<Box<dyn Sink>>) -> Self {
        Self {
            level: AtomicU8::new(level as u8),
            sinks,
            ring: Mutex::new(VecDeque::with_capacity(ring_capacity.max(1))),
            ring_capacity: ring_capacity.max(1),
            fatal: Mutex::new(None),
            seq: AtomicU64::new(0),
            start: Instant::now(),
        }
    }

    /// 当前过滤阈值。
    pub fn level(&self) -> Level {
        Level::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// 设置过滤阈值（运行期可调）。
    pub fn set_level(&self, level: Level) {
        self.level.store(level as u8, Ordering::Relaxed);
    }

    /// 该等级是否通过过滤。
    ///
    /// 阈值语义 = 「**至少**这么严重才输出」：`Warn` 阈值放行 Warn/Error/Fatal。
    pub fn enabled(&self, level: Level) -> bool {
        let threshold = self.level();
        level != Level::Off && threshold != Level::Off && level >= threshold
    }

    /// 记录一条日志（不满足阈值时零格式化开销由宏保证）。
    pub fn log(&self, level: Level, target: &'static str, message: impl Into<String>) {
        if !self.enabled(level) {
            return;
        }
        let record = self.make_record(level, target, message.into());
        self.push(&record);
    }

    /// 记录一条日志，**绕过等级阈值**（`YURIS_TRACE_*` 等显式取证开关专用）。
    ///
    /// 用途：用户已经用环境变量点名打开了某类 trace（如 `YURIS_TRACE_CG=1`），
    /// 此时全局等级仍是 `info` 也不该把输出吞掉 —— 否则开关形同虚设。
    pub fn log_forced(&self, level: Level, target: &'static str, message: impl Into<String>) {
        if level == Level::Off {
            return;
        }
        let record = self.make_record(level, target, message.into());
        self.push(&record);
    }

    /// 记录一条可恢复错误。
    pub fn error(&self, target: &'static str, message: impl Into<String>) {
        self.log(Level::Error, target, message);
    }

    /// 记录一条警告。
    pub fn warn(&self, target: &'static str, message: impl Into<String>) {
        self.log(Level::Warn, target, message);
    }

    /// 消费一个外部错误：`context: error`。
    pub fn report<E: Display>(&self, level: Level, target: &'static str, context: &str, error: E) {
        self.log(level, target, format!("{context}: {error}"));
    }

    /// **崩坏级错误**：写入日志、环形缓存，并生成/更新错误窗报告。
    ///
    /// 与 [`Logger::log`] 不同，致命记录**无视等级阈值**：即使
    /// `YURIS_LOG_LEVEL=off`，错误窗仍应能描述发生了什么。
    pub fn fatal(
        &self,
        target: &'static str,
        title: &str,
        message: impl Into<String>,
    ) -> FatalReport {
        let message = message.into();
        let tail = self.tail_snapshot(12);
        let record = self.make_record(Level::Fatal, target, message.clone());
        self.push_ring(record.clone());
        for sink in &self.sinks {
            sink.emit(&record);
        }
        let mut report = FatalReport {
            seq: record.seq,
            title: title.to_string(),
            message,
            tail,
            unix_ms: record.unix_ms,
        };
        report.tail.push(record);
        if report.tail.len() > 16 {
            let drop = report.tail.len() - 16;
            report.tail.drain(..drop);
        }
        let mut slot = match self.fatal.lock() {
            Ok(g) => g,
            Err(_) => return report,
        };
        match slot.as_mut() {
            // 已有报告：保留首因（seq/标题/主消息），只追加后续致命记录。
            Some(existing) => {
                existing.tail.extend(report.tail);
                if existing.tail.len() > 16 {
                    let drop = existing.tail.len() - 16;
                    existing.tail.drain(..drop);
                }
                existing.clone()
            }
            None => {
                *slot = Some(report.clone());
                report
            }
        }
    }

    /// 消费一个外部错误并升级为崩坏级。
    pub fn fatal_error<E: Display>(
        &self,
        target: &'static str,
        context: &str,
        error: E,
    ) -> FatalReport {
        self.fatal(
            target,
            "致命错误 / FATAL ERROR",
            format!("{context}: {error}"),
        )
    }

    /// 当前致命错误报告（若有）。
    pub fn fatal_report(&self) -> Option<FatalReport> {
        self.fatal.lock().ok().and_then(|g| g.clone())
    }

    /// 是否已有未清除的致命错误。
    pub fn has_fatal(&self) -> bool {
        self.fatal.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// 清除致命错误（错误窗随之撤下）。
    pub fn clear_fatal(&self) {
        if let Ok(mut g) = self.fatal.lock() {
            *g = None;
        }
    }

    /// 最近的日志记录（按时间顺序，最多 `limit` 条）。
    pub fn recent(&self, limit: usize) -> Vec<Record> {
        let ring = match self.ring.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        ring.iter().rev().take(limit).rev().cloned().collect()
    }

    /// 冲刷全部输出端。
    pub fn flush(&self) {
        for sink in &self.sinks {
            sink.flush();
        }
    }

    fn make_record(&self, level: Level, target: &'static str, message: String) -> Record {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        Record {
            seq,
            level,
            target,
            message,
            elapsed_ms: self.start.elapsed().as_millis() as u64,
            unix_ms: unix_ms_now(),
        }
    }

    fn push(&self, record: &Record) {
        self.push_ring(record.clone());
        for sink in &self.sinks {
            sink.emit(record);
        }
    }

    fn push_ring(&self, record: Record) {
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= self.ring_capacity {
                ring.pop_front();
            }
            ring.push_back(record);
        }
    }

    /// 崩溃前的最近日志（优先 Warn 及以上；不足再补 Info）。
    fn tail_snapshot(&self, limit: usize) -> Vec<Record> {
        let ring = match self.ring.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut severe: Vec<Record> = ring
            .iter()
            .filter(|r| r.level >= Level::Warn)
            .rev()
            .take(limit)
            .cloned()
            .collect();
        severe.reverse();
        if severe.len() < limit {
            let need = limit - severe.len();
            let mut info: Vec<Record> = ring
                .iter()
                .filter(|r| r.level < Level::Warn)
                .rev()
                .take(need)
                .cloned()
                .collect();
            info.reverse();
            info.extend(severe);
            info
        } else {
            severe
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::MemorySink;
    use std::sync::Arc;

    fn logger(level: Level) -> (Arc<Logger>, Arc<MemorySink>) {
        let sink = Arc::new(MemorySink::new());
        let logger = Arc::new(Logger::with_sinks(
            level,
            32,
            vec![Box::new(SharedSink(sink.clone()))],
        ));
        (logger, sink)
    }

    struct SharedSink(Arc<MemorySink>);
    impl Sink for SharedSink {
        fn emit(&self, record: &Record) {
            self.0.emit(record);
        }
    }

    #[test]
    fn level_filter_drops_lower_records() {
        let (log, sink) = logger(Level::Warn);
        log.log(Level::Info, "t", "hidden");
        log.log(Level::Warn, "t", "shown");
        let got = sink.records();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message, "shown");
    }

    #[test]
    fn fatal_bypasses_filter_and_reports_tail() {
        let (log, sink) = logger(Level::Off);
        log.log(Level::Error, "t", "hidden by off");
        let report = log.fatal("t", "崩溃", "boom");
        assert_eq!(report.title, "崩溃");
        assert_eq!(report.message, "boom");
        assert!(report.tail.iter().any(|r| r.message == "boom"));
        assert_eq!(sink.records().len(), 1, "致命记录无视阈值写输出端");
        assert!(log.has_fatal());
        log.clear_fatal();
        assert!(!log.has_fatal());
    }

    #[test]
    fn second_fatal_keeps_first_cause() {
        let (log, _sink) = logger(Level::Info);
        let first = log.fatal("t", "首因", "first");
        let second = log.fatal("t2", "后续", "second");
        assert_eq!(second.seq, first.seq);
        assert_eq!(second.title, "首因");
        assert_eq!(second.message, "first");
        assert!(second.tail.iter().any(|r| r.message == "second"));
    }

    #[test]
    fn recent_returns_chronological_tail() {
        let (log, _sink) = logger(Level::Info);
        for i in 0..5 {
            log.log(Level::Info, "t", format!("m{i}"));
        }
        let recent = log.recent(3);
        assert_eq!(
            recent
                .iter()
                .map(|r| r.message.as_str())
                .collect::<Vec<_>>(),
            vec!["m2", "m3", "m4"]
        );
    }

    /// `log_forced` 绕过阈值（`YURIS_TRACE_*` 显式取证开关的语义）。
    #[test]
    fn forced_log_bypasses_threshold() {
        let (log, sink) = logger(Level::Error);
        log.log(Level::Trace, "t", "filtered");
        log.log_forced(Level::Trace, "t", "forced");
        let got = sink.records();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message, "forced");
    }

    /// 文件输出端：追加写、无颜色、flush 后内容可读。
    #[test]
    fn file_sink_appends_plain_lines() {
        let dir = std::env::temp_dir().join(format!(
            "yuris-log-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("test.log");
        {
            let log = Logger::new(Config {
                level: Level::Trace,
                stderr: false,
                file: Some(path.clone()),
                ring_capacity: 8,
                color: false,
            });
            log.log(Level::Info, "t", "first");
            log.log(Level::Fatal, "t", "second");
            log.flush();
        }
        let text = std::fs::read_to_string(&path).expect("读完日志");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.contains("first"), "应写入 Info 行");
        assert!(text.contains("FATAL"), "应写入 Fatal 行");
        assert!(!text.contains('\x1b'), "文件输出不应带 ANSI");
    }
}
