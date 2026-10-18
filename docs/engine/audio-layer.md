# 音频层（`yuris-audio`）

> 面向 `SOUND` / `SOUNDEND` / `SOUNDINFO` 三条命令的音频子系统。
> 本文记录**证据、语义、实现边界**。实现代码：`crates/yuris-audio/`。

## 1. 证据

| 来源 | 内容 | 等级 |
|---|---|---|
| 官方手册 SDK 0.495 `sound/sound.html` | `SOUND` 参数名、范围、示例 [1]–[12] | **Confirmed** |
| 官方手册 `sound/soundinfo.html` | 22 个查询项 + `CODEC` 取值 `0=WAVE/1=OGG/2=ACM WAVE` | **Confirmed（手册）** |
| 官方手册 `sound/soundend.html` | `SOUNDEND[ID]` = 删除声层 | **Confirmed** |
| v494 处理器 `<game-b>_v494.exe` `SOUND 0x44e07c` | 按参数槽依次**入队事件**（14 槽）；无 `FILE` 且声层不存在 → 空操作；`FILE` 路径读 YPF se 类型码 `5→FILETYPE 2`（WAVE）/`6→1`（OGG） | **Confirmed（反汇编）** |
| v494 处理器 `SOUNDINFO 0x44f350` | 24 槽 → 23 个查询字段映射（见 §3）；声层不存在 → 写回 0 | **Confirmed（反汇编）** |
| v494 处理器 `SOUNDEND 0x44f19c` | 按 ID 查找声层 → 停流 → 释放 | **Confirmed（反汇编）** |
| v494 声层结构（alloc `0x464cdc`，48+ 字节状态） | `+0x105` FILETYPE、`+0x106` E、`+0x107` CODEC、`+0x108` 播放态、`+0x109` 暂停、`+0x140` PAN、`+0x144` PITCH、`+0x148` 当前音量、`+0x150` 目标音量、`+0x10c` LOOP、`+0x110` LOOPCOUNT、`+0x105` 等 | **Confirmed（反汇编）** |
| v494 事件队列（`0x802ec8..0x802ee4`，1024 槽） | `SOUND` 把每个参数拆成独立事件（类型 1..14），由 worker `0x423734` 按队列序执行 | **Confirmed（反汇编）** |
| v494 音量换算 `0x424f18` / `0x425088` | 音量 `4000·log10(v/256)` 百分之一分贝；PAN `4000·log10(pan/256)` | **Confirmed（反汇编 + 常量 0.00390625/40/100）** |
| 官方手册与 v494 的 `CODEC` 差异 | 手册 `0=WAVE/1=OGG/2=ACM`；v494 解码器写入 `+0x107` 的值是 OGG=1 / WAVE=2（与 FILETYPE 同口径），SOUNDINFO 原样返回 | **Likely（两处独立写入点，未运行时对拍）** |
| 样本资源 | `bgm/se/vo/sysse/sysvo` 包内音频条目为 **stored OGG**（`OggS` 明文），少量 WAV | **Confirmed（成果 63/93）** |
| `SPEED` | v494 `SOUND` 处理器**完全不读**该槽；v555 YSCM 才有该参数 | **Confirmed（反汇编）** |

## 2. 数据模型：声层（SoundLayer）

声音不是固定的 BGM/SE/Voice 三通道，而是**以脚本给的 `ID` 字符串为键的声层**：

- 同一 ID 再次给 `FILE` = 替换源（手册示例 [8]：`BGM`、`BGM2` 同时播放）；
- 不同 ID 并行混音；
- `SOUNDEND[ID]` 删除声层；
- `SOUNDINFO[ID]` 按 ID 查询。

播放器侧桥接约定（**不是引擎真值**，供现有双脚本播放器平滑迁移）：

| 播放器语义 | 声层 ID | 循环 |
|---|---|---|
| BGM | `BGM` | 无限 |
| Voice（新替旧） | `VOICE` | 1 次 |
| SE（并发） | `SE#0`..`SE#7` 轮转 | 1 次 |

脚本驱动路径应直接构造 `SoundCommand`，不要经过这三个约定。

## 3. `SOUNDINFO` 槽位映射（v494）

| 槽 | 参数 | 引擎字段 | 本库查询 |
|---|---|---|---|
| 0 | `ID` | — | `query(id, …)` |
| 1 | `EXIST` | `+0x105 > 0`（已装载） | `Exist` |
| 2 | `E` | `+0x106` | `Enabled` |
| 3 | `PLAY` | `+0x108 ∈ [2,4]` | `Play` |
| 4 | `PAUSE` | `+0x109` | `Pause` |
| 5 | `VOLUME` | `+0x148` | `Volume`（淡变中为当前值） |
| 6 | `PAN` | `+0x140` | `Pan` |
| 7 | `PITCH` | `+0x144` | `Pitch` |
| 8 | `FILE` | `+0x0` 字符串 | `File`（脚本原始名） |
| 9 | `FILETYPE` | `+0x105`（OGG=1 / WAVE=2） | `FileType` |
| 10 | `START` | `+0x128` | `Start` |
| 11 | `RESTART` | `+0x12c` | `Restart` |
| 12 | `END` | `+0x134` | `End` |
| 13 | `LOOP` | `+0x10c` | `Loop` |
| 14 | `LOOPCOUNT` | `+0x110` | `LoopCount` |
| 15 | `TIME` | `+0x198/+0x19c` | `Time`（毫秒） |
| 16 | `TOTALTIME` | `+0x120/+0x124` | `TotalTime`（毫秒） |
| 17 | `CHANNEL` | `+0x1ba` | `Channel` |
| 18 | `BIT` | `+0x1c6` | `Bit` |
| 19 | `SAMPLING` | `+0x1bc` | `Sampling` |
| 20 | `DATALEFT` | 解码器当前左样本 | `DataLeft` |
| 21 | `DATARIGHT` | 解码器当前右样本 | `DataRight` |
| 22 | `CODEC` | `+0x107` | `Codec` |
| 23 | `LET` | 写回槽（调用方处理） | — |

**声层不存在时全部查询写回 `0`**（v494 `0x44fcc1`：`ebp=0` 后走同一套写回）。

## 4. 参数语义

### `SOUND`

| 参数 | 范围/单位 | 语义 | 实现 |
|---|---|---|---|
| `ID` | 字符串 | 声层键 | 必填；空 ID 显式报错 |
| `FILE` | 字符串 | 音频文件；可省略扩展名 | 搜索顺序：原名 → `.ogg` → `.OGG` → `.wav` → `.WAV`（手册顺序） |
| `E` | 整数 | **只有 `E == 1` 出声**；0/2/… 都静音（引擎 `cmp …,1; sete`，`0x424ec4`）。仍推进位置 | `enabled_raw`；`SOUNDINFO E` 返回原值 |
| `PLAY` | 0/1 | 1 播放 / 0 停止 | 播放中再给 1 = **不重播**，只按 `FADE` 滑到目标音量 |
| `PAUSE` | 0/1 | 1 暂停 / 0 恢复 | 暂停时位置冻结、输出静音。**`+0x109` 全 .text 只有此一处写点** ⇒ `PLAY=1` 不解除暂停，只有 `PAUSE=0` 能（手册示例 \[11\]） |
| `FADE` | 毫秒 | 淡入/淡出/音量斜坡 | 首次播放从 0 淡入；停止时淡出到 0 后停止；只改音量时从当前值斜坡 |
| `START` | 毫秒，可负 | 首次播放起点；负 = 延迟 | 起点 ≥ **生效终点** → 0（`0x4252a4` 用总时长、`0x424d42` 用 END，取更严者） |
| `RESTART` | 毫秒，可负 | 每次循环后的起点；负 = 循环前延迟 | 同上 |
| `END` | 毫秒 | 终点/循环点；0 = 文件末尾 | 超过总时长 → 总时长（引擎 `0x425364`） |
| `LOOP` | 0..N | 0 = 无限；N = 总播放次数；**未给时默认 1（播一次）** | 播满 N 次后停止，`LOOPCOUNT` 记录已完成循环数 |
| `VOLUME` | 0..256 | 原始音量 | 输出幅度 = `(v/256)²`（引擎换算成 `4000·log10(v/256)` 百分之一分贝） |
| `PAN` | 0..512 | 0 左 / 256 中 / 512 右 | 引擎转 DirectSound 声像值（百分之一分贝）；本库按 `10^(-|ds|/2000)` 衰减对侧（`PAN=128` → 对侧 ≈0.25，非线性） |
| `PITCH` | 0..512 | 0.5～1.5 倍 | 线性插值重采样 |
| `SPEED` | v555 only | **Unknown** | v494 不读取；本库解析但不应用 |

参数越界**显式报错**（`AudioError::InvalidParameter`），不静默钳制。

### `SOUNDEND`

删除声层。声层不存在 = 空操作（返回 `false`）。

## 5. 实现结构

```
crates/yuris-audio/src/
├── lib.rs      公开 API 与证据说明
├── info.rs     AudioInfo / AudioCodec / CODEC、FILETYPE 口径
├── decode.rs   symphonia：OGG Vorbis / PCM WAVE → f32 PCM
├── sound.rs    SoundCommand / SoundInfoQuery / SoundInfoValue / validate
├── mixer.rs    声层状态机 + 混音（无设备依赖）
├── engine.rs   AudioSystem 门面：资源装载 → 解码 → 命令 → 查询/渲染
├── output.rs   cpal 声卡输出（feature = device）
├── backend.rs  RuntimeAudio：L4 `AudioBackend` 适配器
└── compat.rs   与 player-core 旧 `audio::Audio` 同签名的迁移桥
```

- **解码**：`symphonia`（OGG/Vorbis + WAVE）。只接受 PCM WAVE 与 Vorbis；
  ACM/ADPCM 等“特殊コーデック形式の WAVE”（手册明确不支持）→
  `AudioError::UnsupportedCodec`。全量解码进内存（单曲数十 KB～数 MB）。
- **时间**：由输出帧数推进，不读墙钟 ⇒ 无头测试完全确定性。
- **输出**：`cpal` 默认设备；打开失败自动降级无头（`has_device()==false`，
  `device_error()` 说明原因），游戏逻辑不受影响。
- **无头**：`--no-default-features` 可完全去掉 cpal（Android 壳/服务端/CI）。

## 6. 已知边界（Unknown / UNVERIFIED）

| 项 | 状态 | 说明 |
|---|---|---|
| DirectSound 声像→左右声道 | **Likely** | 引擎传 DS 的值单位是百分之一分贝（`0x425088`）；按 DS 语义还原为「近侧 1.0、对侧 `10^(-|ds|/2000)`」。DS 内部是否另有等功率补偿未取证 |
| 重采样/变调音质 | **UNVERIFIED** | 线性插值；引擎（DirectSound）算法未取证 |
| 混音溢出 | **UNVERIFIED** | 硬限幅；DirectSound 混音器行为未逐样本对拍 |
| `CODEC` 的 WAVE/ACM 取值 | **Likely** | 手册 `0=WAVE/2=ACM` vs v494 `+0x107` 写 2=WAVE；本库按二进制取值（WAVE=2、OGG=1、ACM=0），常量 `CODEC_WAVE/CODEC_ACM_WAVE` 保留手册口径 |
| `SPEED` | **Unknown** | v494 不读；不实现 |
| MIDI 命令 | **Unknown** | v494 `MIDI 0x44a878` 未取证；音频层不提供 MIDI 解码 |
| 流式长音轨 | 工程限制 | 目前全量解码；未来可换流式而不改上层 API |
| `SOUNDINFO` 写回 `LET` | **未接线** | 本库提供 `SoundInfoValue`，但 VM 的 `SOUNDINFO` 仍归 `Subsystem`（只记录不写回）；接线属 VM 命令层任务 |
| `FADE` 与 `VOLUME` 同帧组合 | **Likely** | 按 v494 事件序（VOLUME 先写目标、PLAY 再起斜坡）实现；未逐样本对拍 |
| 多个 `SOUND` 同 ID 并发 | **Confirmed（引擎语义）** | 同 ID = 同声层，后 `FILE` 替换源；并发请用不同 ID |

## 7. 使用示例

```rust
use yuris_audio::{AudioSystem, SoundCommand, SoundInfoQuery, SoundInfoValue};

// 资源加载器由调用方注入：逻辑名 → YPF/VFS 条目字节。
let mut audio = AudioSystem::with_device(Box::new(|name| {
    // 例：ypf_index.read_bytes(name).ok()
    std::fs::read(name).ok()
}));

let mut bgm = SoundCommand::new("BGM");
bgm.file = Some(r"bgm\theme01".into());
bgm.loop_count = Some(0);
bgm.volume = Some(200);
bgm.fade_ms = Some(800);
bgm.play = Some(1);
audio.sound(&bgm)?;

let mut stop = SoundCommand::new("BGM");
stop.play = Some(0);
stop.fade_ms = Some(1500);
audio.sound(&stop)?;

assert_eq!(audio.sound_info("BGM", SoundInfoQuery::Play)?, SoundInfoValue::Int(0));
```

无头确定性测试：

```rust
let mut audio = AudioSystem::headless_at(Box::new(|_| None), 48_000);
audio.load_bytes("SE", "click.wav", &wav_bytes)?;
let mut cmd = SoundCommand::new("SE");
cmd.play = Some(1);
audio.sound(&cmd)?;
let pcm = audio.render_frames(48_000)?; // 1 秒立体声
```

## 8. 验证方式

```bash
export TEMP="<side-repo>/workbench/tmp" TMP="$TEMP"
cargo test -p yuris-audio
# 40 tests = 15 状态机 + 11 端到端 + 7 兼容层 + 6 L4 适配器 + 1 doctest
cargo test -p yuris-audio --no-default-features    # 同样 40 passed（去 cpal）
cargo run -p yuris-audio --example device_probe -- path/to/tone.ogg
cargo run -p yuris-audio --example decode_probe -- path/to/real_bgm.ogg
```

**真实封包实测**（`<game-b>/data.ypf`，条目 stored OGG）：

| 条目 | 解码结果 |
|---|---|
| `bgm\日常４.ogg`（2.9 MB） | 2 ch / 44100 Hz / 5,727,872 帧 / 129,883 ms / 解码 101 ms |
| `voice\kot\kot_4547.ogg`（139 KB） | 1 ch / 48000 Hz / 240,320 帧 / 5,006 ms / 解码 3.3 ms |

夹具：`crates/yuris-audio/tests/fixtures/tone.wav`（8 kHz mono PCM）
与 `tone.ogg`（8 kHz stereo Vorbis），由 ffmpeg 生成，已入库（共约 7 KB）。

## 9. 播放器接入（已实装，成果 162）

`yuris-player-core` 的 rodio 直连实现**已退役**：

- `crates/yuris-player-core/src/audio.rs` 现在只有一行转发
  `pub use yuris_audio::compat::Audio;`，调用点零改动；
- `rodio` 依赖已从 workspace / `yuris-player-core` / `yuris-cli` 移除，
  `Cargo.lock` 中不再有 rodio；
- `compat::Audio` 保留旧行为：同名 BGM 不重启、空数据只调音量、
  `se_log` 记录 32 条决策（含未命中）、voice 新替旧、SE 轮转 8 声层；
- 测试构造改用 `Audio::headless()`（不打开声卡，确定性）。

### 9.1 VM `SOUND` 事件 → 声层（成果 162）

原先 VM 只带出 `ID/FILE/PLAY` 三个槽，其余 11 个参数在 VM 层丢弃。现在：

- `VmEvent::Sound` 带全部 14 个参数（`E/FILE/PLAY/PAUSE/FADE/START/
  RESTART/END/LOOP/VOLUME/PAN/PITCH/SPEED`），槽位按**参数名**解析
  （`param_slot`，版本漂移安全）；
- `FILE` 是 **CP932 原始字节**（与 `Cg.file` 同约定）——真实语料的声音名
  含日文，先转 UTF-8 会毁掉名字；
- `player-core::apply_sound_event` 把事件原样映射成
  `yuris_audio::SoundCommand` 并调用 `AudioSystem`；
- `SOUNDEND` → `AudioSystem::sound_end(id)`。

### 9.2 真实语料的 `FILE` 形态（Confirmed）

`<game-b>` v494 全语料实测（`scripts` 探针 `sound_census`）：

| 观察 | 值 |
|---|---|
| `SOUND` 组总数 | 45 |
| `ID` 形态 | `"BGM/" + 变量`、`"VO/A1"`、`":/S0/" + …` |
| `FILE` 形态 | `bgm/xxx`、`voice/xxx`——**带目录、无扩展名** |
| 参数 | `FADE/LOOP/VOLUME/PAN/PITCH/START/RESTART/END` 全部出现在语料里 |
| 空 `ID` | `""`（初始化声层时常见，本库按空 ID 忽略） |

因此 `player-core` 的解析器按「剥 `:/段/` 前缀 → 六个音频根 → 原名/.ogg/.wav」
生成候选，全部在 CP932 归一化键空间里查（`normalize_request_bytes`）。

### 9.3 诊断

```bash
YURIS_TRACE_SOUND=1 yuris-cli run <游戏目录>
# [trace-sound] id="BGM/B1" file="bgm/..." play=Some(1) vol=Some(256) loop=Some(0) fade=Some(800)
```
