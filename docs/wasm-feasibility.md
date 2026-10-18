# wasm 可行性分析（2026-10-17）

> 范围：把本仓库（YU-RIS 兼容内核，20 个 crate / 约 22k 行 Rust）编译到
> `wasm32-unknown-unknown`（浏览器）与 `wasm32-wasip1`（无头）的可行性。
>
> 方法：**先编译、再运行时探针**，不靠推断。所有结论标注等级：
> **Confirmed**（本机实测/可复现）/ **Likely**（源码与依赖行为间接证明）/
> **Hypothesis**（未验证）/ **Unknown**（未测）。
>
> 环境：rustc/cargo 1.96.0，Windows x64，Node v22.23.2，
> wasm-bindgen 0.2.127；`wasm32-unknown-unknown` 原有，
> `wasm32-wasip1` 为本次分析新增（`rustup target add`）。
> 探针工程与产物在 `<side-repo>/workbench/tmp/`（仓库外，不入库）。
>
> **并发提醒**：分析期间工作区有其它任务在改动源码，个别行号/状态可能已漂移；
> 本文记录的是 2026-10-17 当次实测的树状态。全程未执行任何 git 写操作。

---

## 0. 结论（TL;DR）

**总体：可行。没有发现硬阻塞** —— 核心 crate 没有原生 FFI、没有业务线程、
VM 本来就是 suspend/resume 的非阻塞设计。真正的成本不在"能不能编"，而在
**三个桌面假设**：同步 GPU 初始化、`std::fs` 资产读取、`std::time::Instant` 时钟。

按三种产品形态分别看：

| 形态 | 结论 | 证据 |
|---|---|---|
| **A. WASI 无头 VM/工具**（脚本分析、CI、服务端） | **现在就能用** | 已构建 164 KB wasm，Node WASI 下跑通日志 + 封包扫描 |
| **B. 浏览器内 VM，无 GPU**（DOM/Canvas2D 文本演示） | **可行，中等改造** | 纯逻辑 crate 全部编译通过；只缺时钟/日志/资产字节源 |
| **C. 浏览器完整游戏**（wgpu + 音频 + 存档） | **可行，工作量大** | 渲染/音频/播放核心均可编译；需异步 GPU 初始化 + 资产分发 + 字体/存档适配 |

**唯一被实测证明会直接 trap 的**是 `std::time::Instant/SystemTime`
在 `wasm32-unknown-unknown` 上不可用（Rust std 的 wasm 后端是 unsupported 桩）。
这影响 56 处提及，是浏览器路线的**第一优先改造项**。

**最容易被误判的一点**：`cargo check` 通过 ≠ 能跑。wgpu 的
`request_adapter` / `request_device` 在 web 上是 JS Promise，而当前代码用
`pollster::block_on`（Condvar 等待）—— 编译完全通过，运行时必 trap。
这是形态 C 的核心重构点。

---

## 0.1 定位校正：跨平台主线是原生移植，wasm 是可选第四目标

**本仓库的跨平台模型不是"一套 wasm 跑所有平台"，而是"一套核心 + 每平台一个薄壳"。**
Android 的正确路线是**原生 NDK**（`aarch64-linux-android` 等），不是 wasm；
wasm 的价值是"零安装的网页演示 / 无头 CI 工具"，不是 Android 的替代品。

```text
        yuris-format / yuris-script / yuris-vm / yuris-scene / yuris-save
        yuris-scenario / yuris-runtime（trait 边界）
                              │
                    yuris-player-core（平台无关）
                    │            │            │
              yuris-cli     yuris-android  yuris-web（可选）
              （桌面）        （原生 NDK）    （wasm，非主线）
```

### Android 原生支持的机制（本仓库已具备的部件）

| 层 | 机制 | 仓库现状 |
|---|---|---|
| 核心 | 纯 Rust，`std::fs/thread/time` 在 Android 上**都是真的**（NDK libc），不需要 wasm 那套时钟/文件 shim | ✅ 核心 crate 零平台依赖 |
| 窗口/生命周期 | winit 0.30 + `android-activity 0.6`；`cdylib`、`lib name = "main"`、`#[no_mangle] fn android_main(app: AndroidApp)`、`EventLoop::builder().with_android_app(app)` | ❌ 无壳 crate、无 `android_main`（全仓 `grep android_main` 零命中） |
| 图形 | wgpu 24 → wgpu-hal 24 默认 features 含 **Vulkan + GLES**；surface 由 winit 窗口创建 | ✅ 渲染器可复用；❌ 无 Android surface 生命周期处理 |
| 音频 | cpal 0.15.3 在 Android 上走 **oboe 0.6**（`java-interface`），需 NDK/CMake 构建链 | ✅ 依赖就绪；本机 `NDK_HOME=E:\Software\Android\ndk\27.0.12077973` |
| 输入 | winit 发 `Touch` → `yuris_input::PlatformEvent::Touch` → `TouchAdapter` 合成鼠标语义（单指=左键、双指轻点=右键、双指上下滑=滚轮） | ✅ `crates/yuris-input/src/touch.rs` 已实现 |
| 字体 | `/system/fonts/NotoSansTC-Regular.otf`、`NotoSansCJK-Regular.ttc`、`NotoSansSC-Regular.otf` 已在候选表 | ✅ `player-core:158-176` 已含 Android 路径 |
| 资产/存档 | 原生可直接 `std::fs`；游戏数据放**真实目录**（app 专属外部目录 `/sdcard/Android/data/<pkg>/files/...` 或 OBB 目录），存档写可写目录 | ⚠️ `game_dir.join("save")` 写死；无 `save_dir` 配置；1.6 GB 数据分发未定 |
| 配置 | Android 没有 argv/env 启动上下文 | ⚠️ 当前 `yuris-cli` 用 `env::args` + 70 处 `env::var`，需换 `Config` 结构体 |

### Android 原生 vs wasm：成本结构不同

**原生便宜的地方：**

- `Instant/SystemTime`、`std::fs`、`std::thread` 全是真实实现 ⇒ **不需要** B2 时钟 shim、
  **不需要** B3 的 `ByteSource`（虽然做了也能服务 OBB/SAF）、**不需要** COOP/COEP；
- `pollster::block_on` 在原生上**能用**：`request_adapter/request_device` 的 future 由
  wgpu 自己的线程/事件驱动完成，阻塞等待不会死锁；wasm 上才因为"主线程阻塞 ⇒
  JS Promise 永不 resolve"而必须 async 化；
- 音频走 Oboe，没有浏览器"首次用户手势才能出声"的限制。

**原生贵的地方：**

- Activity 生命周期：`Resumed` 建 surface、`Suspended` 释放 surface/后端（当前
  `WgpuBackend` 只在 CLI 的 `resumed` 里创建，没有 suspend 路径）；
- NDK/Gradle/ABI 打包（4 个 target 已装，但 `cargo-apk`/`cargo-ndk`/`xbuild` **都未安装**）；
- 存储权限/数据导入（1.6 GB 不可能进 APK 基础包；需外部目录、OBB 或 Asset Delivery）；
- 存档根目录必须从只读游戏目录重定向到可写目录。

### 两条线共享的改造（做一次，两边都受益）

1. `Config` 结构体替换 `env::args` / `YURIS_*` 环境变量（B9）；
2. 存档根目录可配置，`StorageBackend` 真正接线（B5）；
3. 资源源抽象（原生可选、wasm 必需，B3）；
4. 字体兜底：系统字体缺失时随包附带子集字体（B6）。

> 因此本文档的 Phase 1（可移植接缝）**不是专为 wasm 服务的**：它同时也是
> Android 壳的前置条件。wasm 只是把同样的接缝用得更彻底（时钟/文件必须换）。

---

## 1. 验证环境与复现命令

```bash
# 沙箱：TEMP 重定向（AGENTS.md §2）
export TEMP="<side-repo>/workbench/tmp"
export TMP="$TEMP"

# 编译矩阵
cargo check --target wasm32-unknown-unknown \
  -p yuris-core -p yuris-log -p yuris-format -p yuris-value \
  -p yuris-script -p yuris-expr -p yuris-vm -p yuris-scene \
  -p yuris-runtime -p yuris-save -p yuris-scenario -p yuris-resource
cargo check --target wasm32-unknown-unknown -p yuris-audio --no-default-features
cargo check --target wasm32-unknown-unknown -p yuris-audio
cargo check --target wasm32-unknown-unknown -p yuris-render
cargo check --target wasm32-unknown-unknown -p yuris-player-core
cargo check --target wasm32-unknown-unknown -p yuris-input --features winit
cargo check --target wasm32-unknown-unknown -p yuris-debug
cargo check --target wasm32-unknown-unknown -p yuris-cli

# WASI
cargo check --target wasm32-wasip1 -p yuris-core ...（同上链路）
```

运行时探针：`wasm_probe`（unknown-unknown，无 import，Node 直接实例化）、
`wasi_probe`（wasip1，Node `node:wasi`）、`wasm_print`（stdio 行为）、
`wasm_size`（含渲染链的 cdylib 体积）、`wasi_vm`（真实
`yuris-log + yuris-format + yuris-vm` 的无头可执行）。

---

## 2. 编译矩阵

### 2.1 纯逻辑层：全部通过（Confirmed）

`cargo check --target wasm32-unknown-unknown` 对以下 12 个 crate
**Finished in 12.22s**（仅 `missing_docs` 警告）：

```
yuris-core  yuris-log   yuris-format  yuris-value
yuris-script yuris-expr yuris-vm     yuris-scene
yuris-runtime yuris-save yuris-scenario yuris-resource
```

`wasm32-wasip1` 同一链路同样通过（9.48s）。这说明格式解析、脚本译码、
VM、场景、存档序列化、剧本解析**没有任何目标相关的编译问题**。

### 2.2 平台层：通过，但有语义折扣

| crate | unknown-unknown | 说明 |
|---|---|---|
| `yuris-audio`（`--no-default-features`） | ✅ 1.43s | 纯解码/混音 |
| `yuris-audio`（默认，含 cpal） | ✅ 7.83s | cpal 0.15.3 的 WebAudio 后端受 `wasm-bindgen` feature 门控；未启用时 `default_host()` 回退 **Null host**（能编译、无声）。**Likely** |
| `yuris-render` | ✅ 15.84s | wgpu 24 默认 features = `wgsl, dx12, metal, webgpu`；**`webgl` 是 opt-in**，无 WebGPU 的浏览器不会自动回退 |
| `yuris-player-core` | ✅ 17.67s | 含 image/fontdue/winit/wgpu |
| `yuris-input --features winit` | ✅ 6.38s | 触摸→鼠标适配本来就为 wasm 设计 |
| `yuris-cli --lib` | ✅ 1.74s | 库部分干净 |

### 2.3 两处真实的编译失败（都是小修）

**(a) `yuris-debug`：bytemuck derive 缺失**（3 errors）

```
error[E0433]: cannot find `Pod` in `bytemuck`   --> crates/yuris-debug/src/gfx.rs:27
error[E0433]: cannot find `Zeroable` in `bytemuck` --> crates/yuris-debug/src/gfx.rs:27
error[E0277]: the trait bound `Vx: NoUninit` is not satisfied --> crates/yuris-debug/src/gfx.rs:610
```

根因（`cargo tree -e features -i bytemuck`，Confirmed）：native 下
bytemuck 的 `derive` 是 **wgpu-hal → wgpu-core** 顺带启用的；而 wasm 下
`wgpu-core/hal` 依赖被 target gate 掉（`cfg(not(target_arch = "wasm32"))`），
`yuris-debug` 自己的 `bytemuck = "1"` 没有 `derive` feature。
修法：`crates/yuris-debug/Cargo.toml` 改为
`bytemuck = { version = "1", features = ["derive"] }`（对 native 无副作用）。

**(b) `yuris-cli` bin：`Instant` 类型不匹配**（2 errors）

```
error[E0308]: expected `web_time::time::instant::Instant`, found `Instant`
  --> crates/yuris-cli/src/main.rs:612
  --> crates/yuris-cli/src/main.rs:664
```

根因：winit 0.30 在 wasm 上公开的 `ControlFlow::WaitUntil` 用的是
`web_time::Instant`（`Cargo.lock` 里已有 `web-time 1.1.0`，是 winit 的
wasm 依赖）。这正好指向下面的统一修法：**全仓库把 `std::time::Instant`
换成 `web_time::Instant`**（非 wasm 目标下 web-time 直接 re-export std 类型）。

### 2.4 并发状态说明（不计入结论）

分析末尾复检 `wasm32-wasip1 -p yuris-log` 时出现：

```
error[E0277]: `impl Into<String>` doesn't implement `std::fmt::Display`
   --> crates/yuris-log/src/global.rs:194   format!("{target}::{key}")
```

同一源码在几分钟前的 `wasm32-unknown-unknown` 全链检查是通过的，且该错误
与目标无关（纯类型约束问题）⇒ 最可能是**并发任务正在改这个文件**。
记录在此待复核；修法是 `let key = key.into();` 或给泛型补
`+ std::fmt::Display`。**不要把它当成 wasm 结论。**

---

## 3. 运行时语义：哪些 `std` 能力会炸

探针 `wasm_probe` 无任何 import，Node 直接实例化，逐个调用导出函数：

| API | `wasm32-unknown-unknown` | `wasm32-wasip1` | 后果 |
|---|---|---|---|
| `Instant::now()` / `elapsed()` | **TRAP: unreachable** | ✅ 正常 | 56 处提及；`yuris-log` 构造即触发 |
| `SystemTime::now()` | **TRAP: unreachable** | ✅ 正常 | 日志 `unix_ms` |
| `std::fs::read` | 返回 `ErrorKind::Unsupported`（探针值 7） | ✅ `NotFound` | 不会 trap，资产/存档路径会"静默失败" |
| `thread::spawn` | **TRAP** | **panic**：`failed to spawn thread: Unsupported` | 源码里无业务线程（仅 tests/examples） |
| `Condvar::wait_timeout` | **TRAP** | 未测 | `pollster::block_on` / `mpsc::recv` 的底层 |
| `env::args()` | 空（count 0） | ✅ 正常 | CLI 参数解析全失效 |
| `env::var()` | `Err` | ✅ 正常 | 70 处行为开关静默走默认 |
| `println!/eprintln!` | **静默 no-op**（不 trap、也无输出） | ✅ 输出 | 日志的 StderrSink 在浏览器里等于丢弃 |
| `Mutex::lock`（无竞争） | 可用（单线程） | 可用 | 仓库内 Mutex 均为单线程无竞争 |

依据：Rust std 的 `sys/pal/wasm/mod.rs` 明确写"currently all functions here
are basically stubs that immediately return errors"；`sys/stdio` 在 wasm 上
走 `unsupported` 模块。

**关键推论**：`wasm32-unknown-unknown` 是单线程模型，仓库源码没有
`thread::spawn` ⇒ **浏览器版不需要 SharedArrayBuffer / Atomics / COOP+COEP
响应头**。这是本仓库相对多数 Rust 游戏引擎的显著优势。

---

## 4. 阻碍项清单（按优先级）

### B1. GPU 初始化是同步的（形态 C 的头号重构）

**证据（Confirmed，读依赖源码 + 本仓库代码）：**

- `crates/yuris-render/src/lib.rs:248`、`:255` 用 `pollster::block_on` 等待
  `request_adapter` / `request_device`。
- wgpu 24 在 web 上把这两个 future 实现为
  `wasm_bindgen_futures::JsFuture::from(adapter_promise)`（`backend/webgpu.rs:1552`、
  `:1652`）—— 结果由浏览器事件循环回调驱动。
- `pollster::block_on` 用 `Condvar::wait` 阻塞当前线程；浏览器主线程阻塞时
  Promise 永远没机会 resolve ⇒ 探针实测 `Condvar::wait_timeout` 直接 trap。
- `crates/yuris-render/src/lib.rs:1558-1568` 的离屏回读同病：
  `map_async` + `device.poll(Maintain::wait_for(done))` + `rx.recv()`。
  而 wgpu 文档明确：**WebGPU 下 `Device::poll` 是 no-op，设备自动轮询**
  （`wgpu/src/api/device.rs:44`），`Maintain::Wait` 不会等 ⇒ `recv()` 会阻塞/trap。

**修法：**

1. `WgpuBackend` 增加 `async fn new_async(window)`（内部 `await` adapter/device），
   桌面壳用 `pollster::block_on` 调它，web 壳在 JS 驱动的 async 入口里 `.await`；
2. 离屏回读改为返回 future（`map_async` 回调 → oneshot channel + await），
   或 web 上直接禁用 `--shot` 等价功能；
3. `Instance::new(&InstanceDescriptor::default())`（`render/lib.rs:243`）
   在需要 WebGL2 回退时改用 `wgpu::util::new_instance_with_webgpu_detection`
   （wgpu 文档明确推荐；当前 workspace 依赖未开 `webgl` feature）；
4. 事件循环：winit 0.30 在 web 上 `run_app()` **靠抛 JS 异常"假装不返回"**
   （`event_loop.rs:249-260` 文档 + `platform_impl/web/event_loop/mod.rs:66`
   `backend::throw(...)` 实现），推荐 `EventLoopExtWebSys::spawn_app()`，
   它把控制权交还浏览器事件循环。当前 `crates/yuris-cli/src/main.rs:488`
   用的是 `run_app`，web 壳需要换。

**验收**：Chrome（WebGPU）首帧出画面；Firefox/Safari 在开 `webgl` feature
后走 WebGL2 回退；无 trap。

### B2. 时钟：56 处 `Instant` + 日志 `SystemTime`

**证据（Confirmed）：**

```
crates/yuris-log/src/logger.rs:87,119      start: Instant / Instant::now()
crates/yuris-log/src/record.rs:83          SystemTime::now()
crates/yuris-runtime/src/lib.rs:85         trait clock() -> Instant
crates/yuris-vm/src/lib.rs:304,517,800     resume_at / epoch / Instant::now()
crates/yuris-player-core/src/lib.rs:909,3861,3876,4146,4174
crates/yuris-cli/src/main.rs               19 处
```

**修法**：新建一个极薄的 `yuris-time`（或直接在 `yuris-core` 里）：

```rust
#[cfg(target_family = "wasm")]
pub use web_time::{Instant, SystemTime, UNIX_EPOCH};
#[cfg(not(target_family = "wasm"))]
pub use std::time::{Instant, SystemTime, UNIX_EPOCH};
```

全仓库替换导入（类型在非 wasm 上是同一个 std 类型，**行为零变化**），
顺带解决 winit `ControlFlow::WaitUntil` 的类型不匹配。
`web-time 1.1.0` 已在锁文件里（winit 的 wasm 依赖），不新增生态风险。

**验收**：`cargo test --workspace` 基线与改动前一致；wasm 下无 Instant trap。

### B3. 资产：44 处 `std::fs`，YPF 读取器绑死 `File`

**证据（Confirmed）：**

- `crates/yuris-format/src/ypf.rs:622` `YpfIndex::from_path`、
  `:490` `from_path_lenenc`、`:612` `from_path_auto` —— 索引解析**只有磁盘路径入口**；
  `:821` `YpfReader { file: std::fs::File }`，`:832/:838` 打开文件，
  `:848` `read()` 内部 `seek + read_exact`。
- `crates/yuris-vm/src/host.rs:300/356` 用 `read_dir` 扫包；
  `packs: Vec<(PathBuf, YpfIndex)>`；`:469/:507/:517` 每次读条目都
  `File::open + seek`。
- 但 **`YpfArchive::from_bytes`（`:119`）已经存在**，是全内存解析 + `read(name)`；
  索引区长度 = `first_data_off - 0x24`（对 12k 条目量级只有数百 KB），
  完全可以先取头部再按需 Range 取条目。
- `Cargo.lock` 的 flate2 走 miniz_oxide（纯 Rust），wasm 可用；zlib 解压不是问题。

**修法（推荐，桌面/Android 同时受益）：**

```rust
pub trait ByteSource {
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;
}
```

- 实现 `FileSource`（桌面）、`MemorySource`（测试/WASI）、
  `HttpRangeSource`（浏览器 `fetch` + `Range: bytes=a-b`）、
  `OpfsSource`（用户选目录，Chromium）；
- `YpfIndex` 增加 `from_bytes(&[u8], dialect)` / `from_reader`；
- `YpfReader`/`PacFileIndex` 把 `PathBuf` 换成 `Arc<dyn ByteSource>` + source id；
- 松散文件（save/、update）由 source 的命名空间实现。

**验收**：WASI 下用 `MemorySource` 跑同一套格式测试；浏览器下 Range 取一张
CG + 一段 OGG，字节哈希与桌面一致。

### B4. 日志：浏览器里既无时钟也无输出端

`yuris-log` 的 `Logger::new` 第一行就 `Instant::now()`（trap），
`Record::unix_ms` 又用 `SystemTime`（trap），stderr 在 wasm 上是 no-op。
**修法**：clock 走 B2；新增 `ConsoleSink`（`web_sys::console::log_1` 或
`wasm-bindgen` 的 `console_log`）；`FileSink` 在 web 上不启用；
`Config::from_env` 在 web 上改为从 JS 传入的 config 对象构造。

### B5. 存档：5 处 `std::fs`，且 `StorageBackend` trait 没接线

`crates/yuris-runtime/src/lib.rs` 已定义 `StorageBackend`（`read_slot/exists`），
但 `player-core` 的 `quick_save/quick_load/scan_save_entries`
（`lib.rs:4146/4174/2533`）直接走 `std::fs`。
**修法**：web 实现 OPFS（`navigator.storage.getDirectory()`）或 IndexedDB；
把 5 个调用点收敛到 `StorageBackend`。注意当前存档格式本来就是
项目自有的 JSON（原生 `save/*.sd` 链尚未实装），**不存在格式兼容包袱**，
这件事比想象中便宜。

### B6. 字体：系统路径在 web 上必然失败

`crates/yuris-player-core/src/lib.rs:158-176` 读
`C:\Windows\Fonts\...` / `/system/fonts/...` / `/usr/share/fonts/...`；
CLI 处还 `expect("系统 CJK 字体")`。浏览器没有这些路径。
**修法**：随 wasm 资源打包一份**子集化 CJK 字体**（fontdue 0.9 在 wasm 可用），
覆盖样本语料用到的繁中/日文字形；字体许可证单独声明（如 OFL），
与 GPL 代码分开。体积需要实测（全量 Noto CJK 约 16 MB，子集通常 1–3 MB）。
**Unknown**：本作语料的实际字形集大小未测。

### B7. 音频：能编，但要显式启用 WebAudio + 用户手势

- cpal 0.15.3 的 `host/webaudio` 模块受
  `cfg(all(target_arch = "wasm32", feature = "wasm-bindgen"))` 门控；
  未启用时 `default_host()` 是 Null host（**能编译、无声**）。
- 启用后 `Stream` 播放会 `AudioContext.resume()`（`host/webaudio/mod.rs:408`），
  但浏览器自动播放策略仍要求**首次用户手势**后才能出声。
- symphonia 解码走内存 `Cursor`，无文件/线程依赖（已编译通过）。

**修法**：workspace 给 cpal 加 wasm feature；web 壳在第一次点击时恢复音频；
**验收**：BGM 循环 + SE + voice 三路实听（当前设备无声卡时降级路径已有，
`yuris-audio/src/engine.rs:76-100` 会记 `device_error` 而不崩）。

### B8. `yuris-debug` 的 HUD

除 bytemuck 一行修复外，HUD 的 Windows 分层窗口路径本身是
`cfg(windows)`；非 Windows 分支走 wgpu surface，但同样依赖同步初始化。
**建议**：web 首版直接关闭 HUD（`--debug` 不暴露），把 B1 的异步初始化
做好后再接。

### B9. CLI 不能"cfg 一下"就变网页壳

`crates/yuris-cli/src/main.rs` 把桌面假设写死在 `main`：`env::args` 解析、
`std::process::exit`、`timeBeginPeriod`（windows-gated）、`EventLoop::run_app`、
目录扫描、`--shot` 写盘。wasm 下 `args` 为空、`process::exit` 不可用
（std wasm 为 unsupported 桩，**Likely**，未单独探针）、`run_app` 抛异常。
**修法**：新增 `crates/yuris-web`（`cdylib`，`wasm-bindgen` 入口），
把"参数/环境变量"换成 `Config` 结构体，由 JS/URL 传入；
`yuris-cli` 保持桌面专用。**不要试图把 main 改成双目标。**

---

## 5. 资产规模与分发（形态 C 的真问题）

本作封包约 **1.6 GB / 16 个 ypf**（`docs/01-runtime-architecture.md:95-109`）：

| 包 | 大小 |
|---|---|
| cg.ypf | 827 MB |
| update1.ypf | 491 MB |
| op.ypf / op_c.ypf | 各 125 MB |
| vo.ypf | 107 MB |
| cgsys_ec.ypf | 83 MB |
| bgm.ypf | 62 MB |
| se.ypf | 32 MB |
| sysvo.ypf | 5.5 MB |
| bn.ypf | 1.4 MB |

**不能整包进内存/整包下载。** 三种可行分发：

1. **原包 + HTTP Range（推荐）**：先取每个 ypf 的头部到 `first_data_off`
   （索引区，量级数百 KB），之后按条目 `offset/compressed_len` 发
   `Range` 请求。与引擎原生"索引 + seek"模型同构，服务端只需支持 Range
   （静态文件服务器默认支持）。需要 CORS。
2. **预解包 + 清单**：把条目导出成独立文件 + manifest，按需 fetch。
   实现更简单、缓存粒度更好，但文件数多，且要做与 YPF 一致的路径归一化。
3. **OPFS / File System Access**：用户本地选游戏目录，JavaScript 侧读切片。
   适合"本地玩"场景，Chromium 限定；Firefox/Safari 需要回退到 1 或 2。

**内存上限**：单张 CG 解码是主要峰值；按条目录入不需要 1.6 GB 常驻。
wasm32 实际可用堆受浏览器标签页限制（Chrome 远低于 4 GB 地址空间），
需要实测最大 CG 的峰值内存（**Unknown**）。纹理尺寸受 GPU 限制，
1100×825 画布与常规 CG 尺寸没有问题。

**不需要 COOP/COEP**（无 SharedArrayBuffer/线程，见 §3）。

**合规**：游戏数据不入库、不随 wasm 分发（GPL 不改变素材版权）；
wasm 包本身是二进制分发，按 GPL-3.0-or-later 提供对应源码即可。
字体按 OFL 单独声明。

---

## 6. 体积与性能

**体积（Confirmed，本机实测）**：

| 探针 | 配置 | 产物 |
|---|---|---|
| `wasm_size`：`yuris-player-core + yuris-render + fontdue + image + winit`（audio 关 device） | `opt-level="s"`, `lto`, `codegen-units=1`, `panic=abort` | **2,792,140 B**；`gzip -9` = **881,591 B** |
| `wasi_vm`：`yuris-log + yuris-format + yuris-vm` | 同上 | **167,464 B** |

注意：2.8 MB 是**未做 wasm-bindgen 后处理、未跑 wasm-opt** 的原始 wasm
（本机没有 wasm-opt/wabt）；体积里包含完整的 wgpu 渲染路径。
`image` 当前用默认 features，带 avif/exr/tiff/rayon；按 YSCM 图像族收窄到
`png,jpeg,bmp,gif` 可以显著减重。字体是最大的变量（见 B6）。

**性能（Hypothesis，需浏览器实测）**：

- VM 是 suspend/resume + 帧/时间配额设计（`driver.rs` 的 pump cap、
  `VmSuspend::Wait`），CPU 慢只会降帧，不会卡死事件循环——这是很适合 wasm 的形态；
- 2D 四边形 + 纹理采样的负载对 WebGPU/WebGL2 都很轻；
- wasm 相对 native 的整数/解压开销预计 1.5–3×（解压与图像解码是 CPU 热点），
  具体未知；
- 确定性问题：VM 已支持注入 MT19937 种子（`YURIS_MATH_SEED`，CLI:272）；
  但动画时钟由 `YURIS_FRAME_ANIM` 环境变量在"墙钟/帧量化"间切换
  （`player-core:1312`），web 上要改成配置项，否则截图对拍不可复现。

---

## 7. 建议路线（按投入从低到高）

### Phase 0 —— 固化已跑通的 WASI 无头线（低）

把 `wasi_vm` 探针产品化为 `crates/yuris-wasi`（bin）：加载 ypf、跑 boot、
导出文本/选择肢/场景快照 JSON。用途：CI 语料扫描、无 GPU 环境回归、
服务端工具。验收：`wasm32-wasip1` release 构建 + Node WASI 跑通；
体积保持在几十~几百 KB。

### Phase 1 —— 可移植接缝（中，但不改桌面行为）

1. `yuris-time` 时钟 shim（B2）；
2. `ByteSource` + `YpfIndex::from_bytes`（B3）；
3. `ConsoleSink` + 可注入 clock 的日志（B4）；
4. `Config` 结构体替换 `std::env`（B9 的前半）；
5. `StorageBackend` 接线（B5）；
6. `yuris-debug` bytemuck 一行修复（B8）。

验收（严格）：`cargo test --workspace` 与改动前完全一致（本机基线
165 passed / 0 failed，见 AGENTS.md §2）；`cargo check` 双 wasm 目标通过；
桌面 CLI 实机跑 M2 流程无差异。**这一阶段不做任何行为改动**，
符合"只搬运不改行为"的既有约定。

### Phase 2 —— 浏览器内 VM，无 GPU（中）

新 `crates/yuris-web-vm`（cdylib + wasm-bindgen）：JS 传入单个 `bn.ypf`
（File input 或 fetch），`MemorySource` 解析；`Bootstrap::boot_with_host`
走通；把文本行 / 选择肢 / 场景 JSON 暴露给 JS，用 DOM 渲染。
验收：浏览器里看到第一幕台词；与桌面 `--at` 输出逐行一致。

### Phase 3 —— wgpu 首帧（中高）

按 B1 改异步初始化 + `spawn_app` + canvas；`yuris-render` 现有管线基本可复用
（WGSL、混合模式、纹理上传都标准）。验收：Chrome WebGPU 出标题画面；
开 `webgl` feature 后 Firefox 走 WebGL2；无 trap。

### Phase 4 —— 音频 / 字体 / 存档（中）

B7 + B6 + B5。验收：BGM 循环、voice 逐行、F5/F9 存读档在浏览器内 round-trip。

### Phase 5 —— 资产分发与体验（中）

Range 或 OPFS（§5）、进度条、HTTP 缓存/OPFS 离线缓存、错误降级。

---

## 8. 与 Android 线的关系

Phase 1 的三个接缝（时钟、ByteSource、Storage）正是 Android 壳也需要的：
AGENTS.md 记录 Android 目前"零代码"，且 winit/wgpu/音频全要新写。
先把平台边界（`yuris-runtime` 的 trait）真正用起来，**一次重构同时服务
桌面、Android、wasm**，比单独为 web 打补丁更划算。
注意：本仓库代码与私有 旁线仓库 的单向边界（GPL）不变。

---

## 9. Unknown / 待实测清单

1. 浏览器实际帧率与 VM 每帧耗时（本机只有 Node，无浏览器 GPU 环境）。
2. wgpu 24 的 WebGPU 后端对本渲染器 WGSL / bind group layout 的验证结果
   （大概率通过，未实跑）。
3. WebGL2 回退下的纹理格式 / alpha_mode / 混合状态兼容性。
4. 最大 CG 的解码峰值内存与浏览器标签页上限。
5. 子集化字体的实际体积与缺字率（繁中/日文混合语料）。
6. cpal WebAudio 后端与现有 mixer 回调的实听效果。
7. `std::process::exit` 在 wasm 的具体行为（Likely 不可用，未探针）。
8. 并发改动后的 `yuris-log::global::alert_once` 状态（§2.4）。

---

## 10. 附录：探针原始输出

### 10.1 unknown-unknown 运行时探针

```
imports: []
exports: memory,probe_condvar_wait,probe_env_args,probe_env_var,probe_fs_read,
         probe_instant_now,probe_system_time,probe_thread_spawn,__data_end,__heap_base
probe_instant_now   => TRAP: RuntimeError: unreachable
probe_system_time   => TRAP: RuntimeError: unreachable
probe_fs_read       => 7                     # ErrorKind::Unsupported
probe_thread_spawn  => TRAP: RuntimeError: unreachable
probe_condvar_wait  => TRAP: RuntimeError: unreachable
probe_env_args      => 0
probe_env_var       => 2                     # Err
```

```
# wasm_print：println!/eprintln! 在 wasm32-unknown-unknown 上无输出、不 trap
js: p() returned without trap (println/eprintln are silent no-ops on wasm32-unknown-unknown)
```

### 10.2 WASI 探针

```
instant_ok true
system_time_ok true
fs_err_kind NotFound
thread 'main' panicked: failed to spawn thread: Os { code: 58, kind: Unsupported, ... }
```

### 10.3 WASI 无头 VM（真实依赖链）

```
[    0.000s] INFO  wasi_vm: [wasi] 启动，日志与时钟正常
[    0.000s] INFO  wasi_vm: [wasi] 扫描游戏目录: <repo>
[    0.000s] INFO  wasi_vm: [wasi] 封包扫描完成: 0 个（缺失目录=0 属正常）
[    0.000s] INFO  wasi_vm: [wasi] 结束
```

### 10.4 体积

```
wasm_size.wasm   2,792,140 B   (gzip -9: 881,591 B)
wasi_vm.wasm       167,464 B
```

---

## 11. 一句话回答

**wasm 可行，且门槛比预期低**：逻辑层已全部编译通过、WASI 无头线已实跑；
浏览器完整版的关键路径是"把同步 GPU 初始化改成 async + 把 `std::fs`
换成字节源 + 把 `Instant` 换成 web-time"，三者都是局部重构，不是架构重写。
真正需要产品决策的是**1.6 GB 资产怎么送到浏览器**，而不是引擎能不能编译。
