# dictation-streaming 实现计划

## 目标

新建 `dictation-streaming` binary，使用 **streaming zipformer transducer** 模型实现实时流式语音识别，通过 `sherpa-rs-sys` FFI 直接调用 Online API。

## 模型

- **sherpa-onnx-streaming-zipformer-zh-xlarge-2024-11-19**
- Encoder: 726MB（最高精度中文 streaming 模型）
- 架构: Zipformer transducer (online)
- 下载: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models
- 文件结构:
  - `encoder-epoch-99-avg-1.onnx` (或 int8 量化版)
  - `decoder-epoch-99-avg-1.onnx`
  - `joiner-epoch-99-avg-1.onnx`
  - `tokens.txt`

## 可行性确认

### sherpa-rs-sys Online API ✅ 可用

`sherpa-rs-sys` 0.6.8 的 bindings.rs 中已包含完整的 Online API：

**核心结构体:**
- `SherpaOnnxOnlineRecognizerConfig` — 总配置（含 model, feature, decoding, endpoint 配置）
- `SherpaOnnxOnlineModelConfig` — 模型配置（含 transducer/paraformer/zipformer2_ctc 子配置）
- `SherpaOnnxOnlineTransducerModelConfig` — transducer 模型路径（encoder, decoder, joiner）
- `SherpaOnnxFeatureConfig` — 特征配置（sample_rate, feature_dim）
- `SherpaOnnxOnlineCtcFstDecoderConfig` — CTC FST 解码器配置

**核心函数:**
```rust
SherpaOnnxCreateOnlineRecognizer(config: *const SherpaOnnxOnlineRecognizerConfig) -> *const SherpaOnnxOnlineRecognizer
SherpaOnnxDestroyOnlineRecognizer(recognizer: *const SherpaOnnxOnlineRecognizer)
SherpaOnnxCreateOnlineStream(recognizer: *const SherpaOnnxOnlineRecognizer) -> *const SherpaOnnxOnlineStream
SherpaOnnxDestroyOnlineStream(stream: *const SherpaOnnxOnlineStream)
SherpaOnnxOnlineStreamAcceptWaveform(stream, sample_rate: i32, samples: *const f32, n: i32)
SherpaOnnxIsOnlineStreamReady(recognizer, stream) -> i32
SherpaOnnxDecodeOnlineStream(recognizer, stream)
SherpaOnnxGetOnlineStreamResult(stream) -> *const SherpaOnnxOnlineRecognizerResult
SherpaOnnxDestroyOnlineRecognizerResult(r)
SherpaOnnxOnlineStreamReset(recognizer, stream)
SherpaOnnxOnlineStreamInputFinished(stream)
SherpaOnnxOnlineStreamIsEndpoint(recognizer, stream) -> i32
```

### Endpoint 检测 ✅ 内置

`SherpaOnnxOnlineRecognizerConfig` 内置 endpoint 检测参数：
- `enable_endpoint` — 开关
- `rule1_min_trailing_silence` — 无解码结果时的静音阈值
- `rule2_min_trailing_silence` — 有解码结果后的静音阈值
- `rule3_min_utterance_length` — 最大发言长度

## 架构设计

### 参考 dictation-ng 的模式

从 `crates/dictation-ng` 借鉴 UI 和音频管道结构，替换识别引擎为 Online API。

### 线程模型

```
┌─────────────┐     ┌──────────────────┐     ┌───────────────┐
│ Audio Thread │────►│ Recognition Loop │────►│  Main Thread   │
│ (AVAudioEngine    │ AcceptWaveform   │     │  (AppKit UI)   │
│  installTap)      │ IsReady→Decode   │     │  显示实时结果   │
│              │     │ GetResult        │     │               │
└─────────────┘     └──────────────────┘     └───────────────┘
       │                     │                       │
       │   samples (f32)     │   text (String)       │
       └────────ring buf─────┘   └────Mutex/channel──┘
```

1. **Audio Thread**: AVAudioEngine installTap 采集 16kHz PCM samples
2. **Recognition Thread**: 循环调用 AcceptWaveform → IsReady → Decode → GetResult
3. **Main Thread**: AppKit UI 显示实时识别文本，endpoint 检测到后输出最终结果

### 核心流程

```rust
// 1. 初始化
let config = SherpaOnnxOnlineRecognizerConfig {
    feat_config: SherpaOnnxFeatureConfig { sample_rate: 16000, feature_dim: 80 },
    model_config: SherpaOnnxOnlineModelConfig {
        transducer: SherpaOnnxOnlineTransducerModelConfig {
            encoder: "path/to/encoder.onnx",
            decoder: "path/to/decoder.onnx",
            joiner: "path/to/joiner.onnx",
        },
        tokens: "path/to/tokens.txt",
        num_threads: 4,
        provider: "cpu", // 或 "coreml"
        ..
    },
    decoding_method: "greedy_search",
    enable_endpoint: 1,
    rule1_min_trailing_silence: 2.4,
    rule2_min_trailing_silence: 1.2,
    rule3_min_utterance_length: 20.0,
    ..zeroed()
};
let recognizer = SherpaOnnxCreateOnlineRecognizer(&config);
let stream = SherpaOnnxCreateOnlineStream(recognizer);

// 2. 音频回调中
SherpaOnnxOnlineStreamAcceptWaveform(stream, 16000, samples.as_ptr(), samples.len() as i32);

// 3. 识别循环
while SherpaOnnxIsOnlineStreamReady(recognizer, stream) != 0 {
    SherpaOnnxDecodeOnlineStream(recognizer, stream);
}
let result = SherpaOnnxGetOnlineStreamResult(stream);
// 显示 result.text (partial result)

// 4. Endpoint 检测
if SherpaOnnxOnlineStreamIsEndpoint(recognizer, stream) != 0 {
    // 最终结果确认，重置 stream 继续下一句
    SherpaOnnxOnlineStreamReset(recognizer, stream);
}
```

## 文件结构

```
src/bin/dictation_streaming.rs   — 新 binary 入口
Cargo.toml                       — 添加 [[bin]] 条目
```

binary 名: `dictation-streaming`，feature gate: `sensevoice`（复用 sherpa-rs 依赖）

## 与 dictation / dictation-ng 的区别

| 特性 | dictation | dictation-ng | dictation-streaming |
|------|-----------|-------------|---------------------|
| 引擎 | SenseVoice (offline) | FunASR-Nano (offline) | Zipformer (online) |
| 识别方式 | 录完再识别 | 伪流式(1.5s chunk) | 真流式(实时) |
| API | sherpa-rs 高级封装 | sherpa-rs 高级封装 | sherpa-rs-sys FFI |
| 延迟 | 高(等录完) | 中(1.5s) | 低(实时) |
| 模型大小 | ~200MB | ~1.5GB (LLM) | ~726MB |

## 待确认

- [ ] 模型下载位置和路径管理（硬编码 vs CLI 参数 vs 环境变量）
- [ ] 是否复用 dictation-ng 的 UI（NSPanel + NSTextField），还是做纯 CLI
- [ ] 是否需要 CoreML provider 支持（Apple Silicon 加速）
