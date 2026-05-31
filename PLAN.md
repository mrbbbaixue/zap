# 语音输入功能实现计划

## 状态总览

**音频采集管线**: 已完整 (cpal → mono → 16kHz 重采样 → WAV → base64)
**转录层**: App 注册仍禁用 (`VoiceTranscriber::disabled()`); Windows SAPI batch crate 已完成
**集成点**: `app/src/voice/transcriber.rs` 的 `Transcriber` trait（单方法 `transcribe(wav_base64) -> Result<String, TranscribeError>`）
**注册点**: `app/src/lib.rs:1534-1539`
**当前阶段**: Phase 2.1-Windows batch 最小闭环已完成；下一步是把 System Built-in 后端接入 App 注册点

## 参考项目

- [cjpais/Handy](https://github.com/cjpais/Handy) — 22.5k stars, MIT, Rust+Tauri, 纯本地 STT
  - whisper-rs (Whisper Small/Medium/Turbo/Large), GPU 加速
  - transcribe-rs (Parakeet V3, CPU 优化)
  - vad-rs (Silero VAD 静音检测)

---

## Phase 1: 重构转录层，支持多后端

### 1.1 添加后端枚举
- 新增 `TranscriptionBackend` 枚举: `System | Local | Api`
- 对应设置持久化

### 1.2 抽象改造
- `Transcriber` trait 保持不变，单方法已足够
- 将 `VoiceTranscriber` 从 `Option<Arc<dyn Transcriber>>` 改造为支持按后端选择

### 1.3 配置入口
- `app/src/settings/ai.rs` 添加 `voice_transcription_backend` 字段
- `app/src/settings_view/ai_page.rs` 添加后端选择 UI（下拉菜单）

---

## Phase 2: 三种后端实现

### 2.1 系统原生语音转文字（优先级: 中）

#### 核心卡点

**FFI 接入复杂度高**

- Windows SAPI `Win32_Media_Speech`: 走桌面 COM API，不走 WinRT `Windows.Media.SpeechRecognition`。现有管线已经产出整段 WAV，SAPI 可通过 `ISpStream`/`ISpRecognizer::SetInput` 读文件或内存流，先适配当前 batch trait；实时再接 SAPI 事件流
- macOS `SFSpeechRecognizer`: 需 `objc` crate 做 ObjC FFI，cpal `[f32]` → `SFSpeechAudioBufferRecognitionRequest.append()` 同样需要格式转换
- **Linux 无系统级语音 API**，此模式下需降级为本地模型

**batch vs streaming 架构不匹配**

当前 `Transcriber` trait 是整段式接口：
```rust
async fn transcribe(&self, wav_base64: String) -> Result<String, TranscribeError>;
```
系统 API 是流式的，两者都支持**实时返回中间结果**：
- Windows SAPI `SPEI_HYPOTHESIS` 事件 → 说话过程中持续吐出部分识别文本，`SPEI_RECOGNITION` 返回最终结果
- macOS `resultHandler` 回调 → `isFinal=false` 时返回临时结果

实时可做，但必须先把 trait 升级为流式接口（见 [Phase 3](#phase-3-实时流式转录后续)）。

**平台限制**

- macOS 单次识别最长约 1 分钟，需重建 session
- Windows 离线模式需提前安装对应语言包（zh-CN 支持取决于系统配置）
- 按 Ctrl 的 push-to-talk 模式够用，但长句可能有延迟感

**结论**: 系统原生方案功能最底层、最省资源，但 FFI 工作量大、需对应 trait 改造。建议排在本地方案之后。

#### macOS — `SFSpeechRecognizer`
- 使用 `SFSpeechRecognizer` (Speech framework)
- 需要 Info.plist 权限 `NSSpeechRecognitionUsageDescription`
- 通过 `objc` crate 做 ObjC FFI
- 格式桥接: cpal `[f32]` PCM → `AVAudioPCMBuffer` → `SFSpeechAudioBufferRecognitionRequest`
- 参考 Apple 文档 SFSpeechRecognizer API

#### Windows — SAPI `Win32_Media_Speech`
- 使用桌面 SAPI COM 接口：`ISpRecognizer` / `ISpRecoContext` / `ISpRecoGrammar` / `ISpStream`
- 直接依赖 workspace `windows = 0.62.2`，不引入 crates.io `sapi-lite` 的旧 `windows = 0.28`
- batch 格式桥接: `wav_base64` → 临时 WAV 文件或内存流 → `ISpStream` → `ISpRecognizer::SetInput`
- 任意听写需补上 `ISpRecoGrammar::LoadDictation` + `SetDictationState(SPRS_ACTIVE)`；`sapi-lite` 示例偏 grammar phrase，不够直接
- 实时可行但要新增流式 trait 或事件通道，详见 Windows 专项分析

#### 文件

`crates/voice_transcription/src/transcribers/system/`

### 2.2 本地模型（优先级: 高，离线能力核心）

参考 Handy 的架构：
- **whisper-rs** — Whisper 模型族，GPU 加速
- **transcribe-rs** — Parakeet V3，CPU 优化，自动语言检测
- **vad-rs** — Silero VAD，过滤静音段

Whisper GGUF 量化模型规格：
| 模型 | 磁盘 | 运行时内存 | CPU 实时比 | WER |
|------|------|-----------|-----------|-----|
| tiny.en | 75 MiB | ~1 GB | 12.8x | 18.7% |
| base | 142 MiB | ~1 GB | 6.5x | 11.2% |
| **small** | **466 MiB** | ~2 GB | 2.3x | 6.4% |
| medium | 1.5 GiB | ~5 GB | 0.9x | 3.8% |
| large-v3-turbo-q5_0 | 547 MiB | ~3 GB | 快(GPU) | 接近 v3 |

关键决策:
- **默认推荐 small (466MB)**: 桌面端甜点，体积和精度最佳平衡
- **GPU 用户可选 large-v3-turbo-q5_0 (547MB)**: 磁盘接近 small，GPU 推理极快
- 模型下载策略: 首次使用时从 HuggingFace 下载到 `$DATA_DIR/voice-models/`
- tiny/base 太小，WER 过高不推荐作为默认
- medium 及以上磁盘/内存太重

### 2.3 API ASR（优先级: 中，云端高精度）

遵循 OpenAI Whisper API 规范 (`/v1/audio/transcriptions`)，天然兼容:
- OpenAI Whisper API
- 阿里云百炼 ASR
- 豆包 ASR
- 任何兼容 `/v1/audio/transcriptions` 的提供商

配置项:
- API Base URL (默认 `https://api.openai.com/v1`)
- API Key
- Model name (默认 `whisper-1`)
- Language hint (可选)

文件: `crates/voice_transcription/src/transcribers/api/`

---

## Phase 3: 实时流式转录（后续）

当前 `Transcriber` trait 是整段 WAV 送进去、等结果回来：
```rust
async fn transcribe(&self, wav_base64: String) -> Result<String, TranscribeError>;
```

现有调用链也是 batch：

```
VoiceInput::start_listening
→ cpal 回调采集 f32
→ resample 到 16kHz mono，累积到 Vec<f32>
→ stop_listening 后整体转 WAV base64
→ Editor / CLI footer 调 transcriber.transcribe(wav_base64)
→ 一次性插入最终文本
```

实时转录要同时改两层：

1. `crates/voice_input` 要能在录音过程中输出有序 PCM chunk，而不是只在停止时输出整段 WAV。
2. `app/src/voice/transcriber.rs` 要提供 streaming 能力，让后端边接收音频边返回 partial/final 文本。

### 推荐 trait 形态

保持 batch 方法，新增可选 streaming 方法，避免一次性强迫 local/api/system 全部实现：

```rust
pub struct TranscriptionAudioChunk {
    pub pcm_i16_mono_16khz: Vec<i16>,
    pub is_final: bool,
}

pub struct TranscriptionChunk {
    pub text: String,
    pub is_final: bool,
    pub language: Option<String>,
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, wav_base64: String) -> Result<String, TranscribeError>;

    fn supports_streaming(&self) -> bool {
        false
    }

    async fn transcribe_stream(
        &self,
        audio_rx: tokio::sync::mpsc::Receiver<TranscriptionAudioChunk>,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<TranscriptionChunk, TranscribeError>>, TranscribeError> {
        Err(TranscribeError::StreamingUnsupported)
    }
}
```

不建议做成 `transcribe_stream(wav_base64)`：已经录完的 WAV 只能“边读文件边吐结果”，不是真正实时；真正实时必须在 cpal 采集时就把 PCM chunk 送给后端。

### `VoiceInput` 改造

新增 streaming session，而不是替换现有 batch session：

```rust
pub struct VoiceStreamSession {
    pub audio_rx: tokio::sync::mpsc::Receiver<TranscriptionAudioChunk>,
    pub stop_rx: oneshot::Receiver<VoiceStreamStopReason>,
}
```

实现要点：

- `start_streaming_listening()` 复用现有 cpal 设备选择、权限检查、16kHz mono 重采样。
- 当前 `on_audio_frame` 每帧再 `ctx.spawn(...)` 做重采样，batch 可以接受；streaming 应改成单个有序 audio worker，避免重采样任务并发导致 chunk 顺序不稳定。
- 重采样后立即把 `[f32]` 转成 `i16` PCM chunk 发到 `audio_tx`，同时可继续累积到 `resampled`，用于 fallback batch 或 debug dump。
- `stop_listening()` 在 streaming 模式下发送 `is_final = true` 的空 chunk 或显式 end signal，然后进入“等待最终识别”状态。
- `abort_listening()` 需要同时关闭 audio channel，通知 transcriber worker 停止，避免 SAPI 线程阻塞在 `Read`。

### UI / 输入框更新策略

Editor 和 CLI footer 都不能简单把每个 partial 追加进去，否则 hypothesis 改写时会重复文本。

需要引入“voice draft span”概念：

- `partial` chunk：替换当前 voice draft span，不提交为用户最终输入。
- `final` chunk：把 draft span 替换为最终文本，并把该 span 标记为已提交；后续 partial 从新的空 draft span 开始。
- stop 后如果没有 final，只清掉 draft span 并显示错误或保持为空。

CLI agent footer 有两条路径：

- Rich input / composer：可以像 editor 一样维护 draft span。
- 直接写 PTY：**不要写 partial**，只在 final chunk 到达后写入 PTY，避免把会被修正的中间文本发送给 shell。

### Windows SAPI 实时路线

SAPI 事件模型能提供真正 partial：

- `SPEI_HYPOTHESIS`：中间假设文本
- `SPEI_RECOGNITION`：最终识别文本
- `SPEI_FALSE_RECOGNITION`：识别失败或低置信度结果
- `SPEI_END_SR_STREAM` / `SPEI_END_INPUT_STREAM`：输入结束

SAPI 默认只排队 `SPEI_RECOGNITION`。要拿到 partial，必须在 `ISpEventSource::SetInterest` 中显式订阅 `SPEI_HYPOTHESIS`，例如：

```rust
let events =
    event_flag(SPEI_RECOGNITION)
    | event_flag(SPEI_HYPOTHESIS)
    | event_flag(SPEI_FALSE_RECOGNITION)
    | event_flag(SPEI_END_SR_STREAM)
    | event_flag(SPEI_END_INPUT_STREAM);
event_source.SetInterest(events, events)?;
```

`event_flag(event)` 等价于 SAPI `SPFEI(event)`，即 `1u64 << event.0`；迁移 `sapi-lite` 时保留其 `SPEI_RESERVED1` / `SPEI_RESERVED2` 保护位处理。

Microsoft SAPI 文档确认的约束：

- InProc recognizer 不会自动设置音频输入；必须先调用 `ISpRecognizer::SetInput`，否则 recognition 不会真正开始。
- `SetInput` 可以接 audio device token，也可以接实现 `ISpStreamFormat` 的实际对象；实时流对象通常还会实现 `IStream` 和 `ISpAudio`。
- `SetInterest` 如果不调用，SR engine 默认只通知并排队 `SPEI_RECOGNITION`；`SPEI_HYPOTHESIS` 必须显式订阅。
- `ISpAudio` 用于实时音频流；如果不是标准 Windows multimedia device，才需要自己实现它。

SAPI worker 建议独立 blocking thread：

```
spawn SAPI worker thread
→ CoInitializeEx
→ CoCreateInstance(SpInprocRecognizer)
→ SetInput(audio_source, false)
→ CreateRecoContext
→ SetNotifyWin32Event
→ SetInterest(RECOGNITION | HYPOTHESIS | FALSE_RECOGNITION | END...)
→ CreateGrammar(0)
→ LoadDictation(None, SPLO_STATIC)
→ SetDictationState(SPRS_ACTIVE)
→ loop WaitForNotifyEvent(50-100ms), drain GetEvents
→ SPEI_HYPOTHESIS -> TranscriptionChunk { is_final: false }
→ SPEI_RECOGNITION -> TranscriptionChunk { is_final: true }
→ EOF/abort -> inactive grammar, release COM
```

不要在 WarpUI/UI 线程里持有或调用 SAPI COM 对象；worker 只通过 channel 和 app 交互。

### SAPI 音频输入的三种实现等级

| 等级 | 路线 | 用途 | 风险 |
|------|------|------|------|
| A | SAPI 默认麦克风 `RecognitionInput::Default` | 最快验证 `SPEI_HYPOTHESIS`、事件循环、UI draft span | 绕过现有 cpal 设备选择、录音状态机和 VAD；不能保证和 Zap 选择的设备一致 |
| B | cpal → blocking `IStream` → `ISpStream::SetBaseStream` | 尝试用较少 COM 方法把现有 PCM chunk 喂给 SAPI | 文档对 real-time stream 更偏向 `ISpAudio` / `ISpStreamFormat`；`IStream` 包装可能只适合文件/内存流 |
| C | cpal → 自定义 `ISpAudio`/`ISpStreamFormat`/`IStream` | 生产级实时输入，完全复用现有 cpal 管线 | 工作量最大：要实现阻塞 `Read`、`EventHandle`、`GetStatus`、`SetState`、`GetFormat`、EOF/abort/backpressure |

建议按 A → B → C 验证，不要直接上 C。

等级 B/C 的核心数据结构：

```rust
struct AppendablePcmStream {
    buffer: Mutex<VecDeque<u8>>,
    available: Condvar,
    eof: AtomicBool,
    aborted: AtomicBool,
    format: WAVEFORMATEX, // 16kHz, 16-bit, mono PCM
}
```

`Read` 语义：

- 有数据：最多拷贝 `cb` 字节到 `pv`，写 `pcbread`，返回 `S_OK`。
- 暂无数据且未结束：阻塞等待 `available`。
- EOF 且 buffer 已空：写 `pcbread = 0`，返回 EOF/`S_FALSE` 语义，让 SAPI 收尾。
- abort：唤醒所有等待者并返回失败 HRESULT，让 worker 退出。

`GetFormat` 需要返回和 cpal chunk 一致的 `WAVEFORMATEX`。如果走 `ISpStreamFormat`/`ISpAudio`，返回内存需符合 COM 约定，由 SAPI 释放。

### 分阶段落地建议

1. **Phase 3a: SAPI 事件原型**
   - 在 `windows_sapi` 内扩展 `Event`：`Hypothesis(String)` / `Recognition(String)` / `FalseRecognition` / `End`.
   - 用 SAPI 默认麦克风跑通 `SPEI_HYPOTHESIS` → UI draft span，不接 cpal。

2. **Phase 3b: 通用 streaming trait + UI draft span**
   - 给 `Transcriber` 加默认 streaming 方法。
   - Editor 和 CLI rich input 支持 partial 替换、final 提交。
   - PTY 路径只消费 final。

3. **Phase 3c: 接回 cpal 音频管线**
   - `VoiceInput` 新增 ordered audio worker 和 streaming session。
   - Windows 先试 `IStream` + `ISpStream` 包装；失败或不稳定再实现完整 `ISpAudio`。

4. **Phase 3d: 稳定性**
   - hypothesis 节流（例如 50-100ms），避免 UI 每个 SAPI event 都重排。
   - stop 后保留 500-1000ms grace period 等 final recognition。
   - 处理无 final、空 final、false recognition、语言包缺失、worker panic、用户 abort。

### 验证方式

1. 用默认麦克风原型验证说话时有 `partial` 更新，停顿后有 `final`。
2. Editor 中 partial 文本不会重复追加，final 后继续输入位置正确。
3. CLI rich input 行为同 Editor；PTY fallback 只写 final。
4. 快速 start/stop、abort、窗口失焦、切换设置时 SAPI worker 不泄漏线程、不阻塞退出。
5. 未安装语言包或没有 SAPI engine 时，回退到 batch/local/api 或显示可诊断错误。

**卡点**: trait 和 UI 改造会影响 editor voice + CLI agent voice 两条调用链；cpal → SAPI 实时输入还涉及自定义 COM stream。建议在 Windows batch 后先做 Phase 3a 原型，再决定是否进入 Phase 3b/3c。

---

## Phase 3-Archive: 旧流式接口草案

早期草案是：
```rust
async fn transcribe_stream(
    &self,
    wav_base64: String,
) -> Result<tokio::sync::mpsc::Receiver<TranscriptionChunk>, TranscribeError>;

struct TranscriptionChunk {
    text: String,
    is_final: bool,
    language: Option<String>,
}
```

这个接口只能对“已经录完的 WAV”做伪流式，不适合真正实时转录。保留在这里仅作历史记录，不作为推荐实现。

---

## Phase 2.1-Windows: 系统原生实现（Windows 专项）

### 现状

| 项 | 状态 |
|---|---|
| Transcriber trait | 已定义，batch 模式 `transcribe(wav_base64) -> String` |
| TranscriptionBackend 设置 + UI | **已完成**（下拉菜单 + 说明文字） |
| 注册点 `lib.rs:1534` | **未改** — 仍是 `VoiceTranscriber::disabled()` |
| Transcriber 实现 | **部分完成** — `crates/voice_transcription` 已有 Windows SAPI batch recognizer；App adapter 未接 |
| 转录 crate | **已创建并提交** — `d7736598 feat: add Windows SAPI voice transcription crate` |
| Windows SAPI smoke test | **已通过** — `cargo run -p voice_transcription --example sapi_smoke -- <wav>` 返回非空文本 |
| 当前下一步 | **注册 System 后端** — `app/Cargo.toml` 加依赖，`app/src/voice/transcriber.rs` 加 adapter，`app/src/lib.rs` 按设置注入 |

### 结论

**改用桌面 SAPI，不采用 WinRT 临时 WAV 方案。**

旧方案里的 WinRT `SpeechRecognizer.RecognizeAsync()` 不消费我们打开的 `StorageFile` / `IRandomAccessStream`，它识别的是 recognizer 当前输入；`SpeechRecognitionGrammarFileConstraint` 也是语法约束文件，不是音频输入接口。因此 `wav_base64 -> 临时文件 -> RecognizeAsync()` 这条路线不成立。

更干净的 Windows 路线是把 `sapi-lite` 的 STT 最小子集复制到仓库内的 `crates/voice_transcription` 私有模块里，迁移到 workspace 已有的 `windows = 0.62.2`。不要直接依赖 crates.io `sapi-lite = 0.1.1`，因为它依赖 `windows = 0.28`，会带来双版本 `windows` 依赖和类型不兼容。

### `sapi-lite` 调研结论

`sapi-lite` 已经封装了 Microsoft SAPI 的核心形态：

- `Recognizer`：创建 in-process SAPI recognizer，并可 `set_input(RecognitionInput::Default | Stream(...))`
- `SyncContext`：通过 `SetNotifyWin32Event` + `WaitForNotifyEvent` 做阻塞式识别
- `EventfulContext`：通过 `ISpNotifySink` 接 SAPI 事件
- `AudioStream::open_file` / `MemoryStream`：把文件或内存流包装成 `ISpStream`
- `Phrase::from_sapi`：从 `ISpRecoResult::GetText` 提取识别文本

但原 crate 不是直接可升级依赖：

| 项 | 结论 |
|---|---|
| crates.io 版本 | `sapi-lite = 0.1.1` |
| 许可证 | Apache-2.0，复制源码时需保留原 LICENSE/NOTICE 归属 |
| 原依赖 | `windows = 0.28` |
| 当前仓库 | workspace `windows = 0.62.2` |
| 兼容性 | **不是源码兼容**，但可迁移 |

在 `target/sapi-lite-port-probe` 中把 `sapi-lite` 的 `windows` 版本改到 `0.62.2` 后，`cargo check --all-features` 已验证会失败，集中在这些 API 迁移点：

- `IntoParam` 已移除，`Param` 从旧版返回类型变为 trait，旧的 `into_param() -> Param<...>` 写法需要删掉或改写
- `PWSTR` / `PCWSTR` 应从 `windows::core` 使用，不再从 `Win32::Foundation` 导入
- `Abi` / `from_abi` 不再是旧路径，COM 指针恢复需改用新 `Interface` API
- `ToImpl` 改为新式实现访问方式；如果继续用 `#[implement]`，需要确保 `windows-core` crate 可见
- `SPSTATEHANDLE__` 更名为 `SPSTATEHANDLE`
- `VARIANT` / `VARENUM` 等迁到 `Win32_System_Variant`，但 STT 最小实现可以避开语义树和 TTS，从而少迁移这块
- `INFINITE` 位置变化只影响 TTS 同步合成；STT 最小实现不需要复制 TTS

结论：不要整包 fork `sapi-lite`。只复制 STT 必需路径，再按 `windows 0.62.2` API 重写薄封装。

### 推荐实现路线

#### Step 1: 创建 `voice_transcription` crate（已完成）

```
crates/voice_transcription/
├── Cargo.toml
├── LICENSES/
│   └── sapi-lite-APACHE-2.0.txt
└── src/
    ├── lib.rs
    ├── windows.rs                  # WindowsSpeechRecognizer，对外暴露 batch WAV 识别接口
    └── windows_sapi/               # 从 sapi-lite 复制并裁剪后的私有模块
        ├── audio.rs                # AudioFormat / AudioStream，先支持文件流
        ├── com.rs                  # CoInitializeEx / CoUninitialize 和少量 helper
        ├── event.rs                # EventSource，先支持 Recognition / EndInputStream
        ├── phrase.rs               # 只提取 String，不保留 semantic tree
        └── recognizer.rs           # Recognizer / SyncContext / dictation grammar
```

实际落地的 `Cargo.toml` 只使用 workspace 现有依赖：

```toml
[dependencies]
anyhow.workspace = true
thiserror.workspace = true

[target.'cfg(target_os = "windows")'.dependencies]
tempfile.workspace = true
windows = { workspace = true, features = [
    "Win32_Foundation",
    "Win32_Media_Audio",
    "Win32_Media_Speech",
    "Win32_System_Com",
] }
windows-core.workspace = true
```

如果后续实时原型保留 `#[implement]` / 自定义 `ISpNotifySink`，优先使用 `windows::core::implement`。只有宏展开明确要求外部 crate 时，再补 `windows-core.workspace = true`。

#### Step 2: batch 转录先跑通（已完成）

第一版不改 `Transcriber` trait，保持：

```rust
async fn transcribe(&self, wav_base64: String) -> Result<String, TranscribeError>;
```

数据流：

```
wav_base64
→ base64 解码
→ 写临时 WAV 文件或解析成内存流
→ ISpStream
→ ISpRecognizer::SetInput(stream, false)
→ ISpRecoContext::CreateGrammar(0)
→ ISpRecoGrammar::LoadDictation(...)
→ ISpRecoGrammar::SetDictationState(SPRS_ACTIVE)
→ WaitForNotifyEvent(timeout)
→ SPEI_RECOGNITION
→ ISpRecoResult::GetText
```

App adapter 阶段建议用 `tokio::task::spawn_blocking` 包住完整识别过程，在 blocking 线程内初始化和反初始化 COM。不要把 SAPI COM 对象跨 async await 或跨线程缓存。等功能稳定后，如需减少初始化成本，再改为专用 SAPI worker thread。

关键注意点：

- 现有音频管线已经产出 mono / 16kHz / WAV；当前 crate 也会读取 WAV `fmt ` chunk 构造实际 `WAVEFORMATEX`，本机已用 44.1kHz stereo 16-bit PCM WAV 验证链路可跑通
- `sapi-lite` 示例主要演示有限 grammar phrase。我们的通用听写必须补 `LoadDictation` / `SetDictationState`
- `Phrase` 第一版只返回文本，先不复制 semantic tree、grammar DSL、TTS、tokio wrappers
- 错误要区分：没有 SAPI engine、没有可用语言包、超时、识别为空、COM 初始化失败

#### Step 3: 注册点改造（当前阶段）

`app/src/lib.rs:1534-1539` 改为根据设置注入：

```rust
let backend = AISettings::as_ref(ctx).voice_transcription_backend.value();
let transcriber: Option<Arc<dyn Transcriber>> = match backend {
    TranscriptionBackend::System => {
        #[cfg(target_os = "windows")]
        { WindowsSpeechRecognizer::new().ok().map(|t| Arc::new(t) as _) }
        #[cfg(not(target_os = "windows"))]
        { None }
    }
    _ => None, // Local / Api 后续实现
};
VoiceTranscriber::from_option(transcriber)
```

需要在 `VoiceTranscriber` 上加一个 `from_option` 构造方法。

#### Step 4: 语言设置（后续）

当前先用系统默认语言。后续可加：
- `voice_recognition_language: Option<String>` 设置字段
- 支持 `zh-CN` / `en-US` 等 BCP-47 语言标签
- 通过 SAPI recognizer token / category 选择对应识别引擎

### 实时转录可行性

SAPI 本身支持实时事件，`windows 0.62.2` 中能看到这些事件常量：

- `SPEI_HYPOTHESIS`：中间假设文本
- `SPEI_RECOGNITION`：最终识别结果
- `SPEI_FALSE_RECOGNITION`：误识别/未确认结果
- `SPEI_END_SR_STREAM` / `SPEI_END_INPUT_STREAM`：输入结束

`sapi-lite` 当前 `EventfulContext` 只处理了 `SPEI_RECOGNITION`，而且只有传入 `interest` 时才调用 `SetInterest`。要做实时转录，需要扩展事件层，把 `SPEI_HYPOTHESIS` 映射成 `TranscriptionChunk { is_final: false }`，把 `SPEI_RECOGNITION` 映射成 `is_final: true`，并显式订阅 hypothesis / false recognition / end stream 事件。

事件层迁移点：

- `Event::from_sapi` 里 `SPEI_HYPOTHESIS` 和 `SPEI_RECOGNITION` 的 `lParam` 都按 `ISpRecoResult` 处理，可复用 `Phrase::from_sapi` 的 `GetText` 逻辑，但 `Phrase` 类型第一版只保留 `String`
- `SPEI_FALSE_RECOGNITION` 可先映射为 `FalseRecognition`，用于清空 draft 或等待后续 final
- `SPEI_END_SR_STREAM` / `SPEI_END_INPUT_STREAM` 映射为 `End`，用于关闭 output channel
- `SetNotifySink` 回调里不要直接触 UI，通过 `mpsc::Sender` 发到 app 层

可选路线：

| 路线 | 说明 | 取舍 |
|------|------|------|
| SAPI 默认麦克风实时 | `RecognitionInput::Default` 让 SAPI 自己采集麦克风，直接接事件 | 最快验证实时，但绕过现有 cpal 设备选择、VAD 和 push-to-talk 音频管线 |
| cpal → 自定义 COM stream | 实现可追加、阻塞读取的 `IStream` / `ISpStream`，把现有音频帧边录边喂给 SAPI | 架构最统一，但要处理 COM stream、线程阻塞、结束信号和 backpressure，复杂度高 |
| 小分段 batch | VAD 切小段，复用现有 `transcribe()` 多次调用 | 实现最简单，但不是 SAPI hypothesis 级实时，延迟和上下文连续性较差 |

建议顺序：

1. 先完成 batch 文件/内存流识别，证明 SAPI + `windows 0.62.2` 可用。
2. 再加 `EventSource` 对 `SPEI_HYPOTHESIS` 的解析，做默认麦克风实时原型。
3. 加 app 层 streaming trait 和 UI draft span，让 partial 能被正确替换而不是重复插入。
4. 最后决定是否值得实现 cpal appendable COM stream；如果默认麦克风原型体验已经足够，再评估是否要接回现有 cpal 设备选择和 VAD。

### 涉及文件清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `crates/voice_transcription/Cargo.toml` | **已完成** | crate 定义，复用 workspace `windows = 0.62.2` |
| `crates/voice_transcription/src/lib.rs` | **已完成** | 模块导出 |
| `crates/voice_transcription/src/windows.rs` | **已完成** | `WindowsSpeechRecognizer` batch 实现 |
| `crates/voice_transcription/src/windows_sapi/*` | **已完成** | 从 `sapi-lite` 裁剪并迁移的 SAPI STT 私有模块 |
| `crates/voice_transcription/examples/sapi_smoke.rs` | **已完成** | 固定 WAV smoke test 入口 |
| `crates/voice_transcription/LICENSES/sapi-lite-APACHE-2.0.txt` | **已完成** | 保留 vendored 源码许可证归属 |
| `Cargo.toml` (workspace) | **已完成** | 添加 workspace dependency |
| `app/Cargo.toml` | **修改** | 添加 `voice_transcription` 依赖 |
| `app/src/voice/transcriber.rs` | **修改** | 添加 `VoiceTranscriber::from_option` |
| `app/src/lib.rs` | **修改** | 注册点根据 backend 注入 transcriber |
| `app/src/settings/ai.rs` | 已完成 | TranscriptionBackend 枚举 |
| `app/src/settings_view/ai_page.rs` | 已完成后端下拉 | 后续可能需语言下拉 |

### 验证方式

1. `cargo check -p voice_transcription`（已通过）
2. `cargo run -p voice_transcription --example sapi_smoke -- <wav>`（已通过，返回非空文本）
3. `cargo check -p warp`
4. 选 System Built-in 后端，按触发键录音并停止，文本应进入终端输入框
5. 手动验证未安装目标语言包、静音输入、超时输入时不会崩溃，并给出可诊断错误

---

## 实现顺序建议

1. **Phase 2.1-Windows batch 最小闭环（已完成）** — 已创建 `crates/voice_transcription`，复制/裁剪 `sapi-lite` STT 子集，迁移到 workspace `windows = 0.62.2`，并跑通固定 WAV → SAPI dictation → 文本。
2. **注册 System 后端（当前阶段）** — 接入 `VoiceTranscriber` 注册点，让设置里的 System Built-in 后端能调用 Windows batch transcriber，并通过 `cargo check -p warp`。
3. **Phase 3a Windows 实时原型** — 在 batch 成功后扩展 SAPI event，订阅 `SPEI_HYPOTHESIS`，先用 SAPI 默认麦克风验证 partial/final 事件和 UI draft span。
4. **Phase 3b/3c 完整实时化** — 再改 `VoiceInput` streaming session 和 cpal → SAPI 音频流；先试 `IStream`，必要时实现 `ISpAudio`。
5. **Phase 2.2 / 2.3** — 本地模型和 API ASR 作为后续补充后端，不阻塞 Windows 原生方案验证。

## 涉及文件

```
crates/voice_input/src/lib.rs          # 音频采集管线 (已完整，无需改动)
app/src/voice/transcriber.rs           # Transcriber trait + VoiceTranscriber (需重构)
app/src/voice/mod.rs                   # 模块入口 (需扩展)
app/src/lib.rs:1534-1539               # 注册点 (需修改注入逻辑)
app/src/settings/ai.rs                 # 设置定义 (需新增字段)
app/src/settings_view/ai_page.rs       # 设置 UI (需新增下拉菜单)
app/src/editor/view/voice.rs           # 编辑器语音按钮逻辑 (透传，无需大改)
app/src/ai/blocklist/agent_view/agent_input_footer/mod.rs  # CLI agent 语音 (同上)

crates/voice_transcription/            # [新建] 转录后端 crate
crates/voice_transcription/src/transcribers/
crates/voice_transcription/src/transcribers/system/        # 系统原生
crates/voice_transcription/src/transcribers/local/         # whisper-rs + transcribe-rs
crates/voice_transcription/src/transcribers/api/           # OpenAI 兼容 API
```
