# 德语+英语双语 ASR 模型调研

> 调研日期: 2026-04-11
> 背景: 为德语 STT 输入法选型，基于 picc 项目现有技术栈 (sherpa-onnx + Rust FFI)

## 结论

首选 **NeMo Parakeet TDT 0.6B v3** + VAD 伪流式方案。

理由:
- 德语 WER 7.4%，目前 sherpa-onnx 生态内精度最高
- 已有 ONNX 格式，可直接通过 sherpa-rs-sys FFI 调用
- 与现有 dictation-ng 架构一致（离线模型 + VAD chunk），无需引入新的流式框架
- 支持 25 个欧洲语言，德英双语天然覆盖

## sherpa-onnx 生态内支持德语的模型

| 模型 | 类型 | 德语 WER | 参数量 | 语言覆盖 | 备注 |
|------|------|---------|--------|---------|------|
| **NeMo Parakeet TDT 0.6B v3** | 离线 transducer | **7.4%** | 600M | 25 欧洲语言 | HuggingFace Open ASR 排行榜 avg WER 6.34% |
| NeMo Canary 180M Flash | 离线 | 未公开 | 180M | EN+ES+DE+FR | 体积最小，四语模型 |
| Qwen3-ASR 0.6B | 离线 | 未公开 | 600M | 多语言 | 2026-03-25 发布 |
| Whisper medium | 离线 | ~10-15% | 769M | 90+ 语言 | 社区最成熟，精度偏低 |
| Whisper small | 离线 | ~15-20% | 244M | 90+ 语言 | 体积小，精度一般 |

### 关键限制: 无德语 streaming 模型

sherpa-onnx 当前所有 streaming (Online) 模型均不支持德语。streaming 模型只覆盖:
- 中文 (zipformer-zh-xlarge, zipformer-zh)
- 中英双语 (zipformer-bilingual-zh-en)
- 中粤英三语 (paraformer-trilingual)
- 韩语、孟加拉语

Parakeet TDT 0.6B v3 曾有人尝试伪流式 (sherpa-onnx#2918)，结论是随 buffer 增长越来越慢，不可行。

但这不影响输入法场景 — 我们现有的 dictation-ng 核心语音输入也是离线模型 + VAD 的伪流式方案。

## sherpa-onnx 生态外参考

| 模型 | 德语表现 | 参数量 | 适用性 |
|------|---------|--------|--------|
| Whisper Large V3 | 金标准 | 1.5B | 需 ~10GB VRAM，不适合本地输入法 |
| IBM Granite Speech 3.3 8B | avg WER ~5.85% | 8B | 太重，需 GPU 推理 |
| Moonshine | 德语未验证 | tiny/base | 边缘设备最快(107ms)，可能需 fine-tune |
| FunASR Nano | 31 语言覆盖 | 未知 | 德语精度未公开 |

## 实现方案

### 架构

复用 dictation-ng 的 VAD + chunk 模式，替换识别引擎为 Parakeet TDT:

```
Audio Input -> VAD (silero) -> Speech Chunks -> Parakeet TDT 0.6B v3 -> Text Output
```

### sherpa-onnx 模型文件

模型名: `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`

下载: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models

文件结构 (transducer 架构):
- `encoder.onnx` (或 int8 量化版)
- `decoder.onnx`
- `joiner.onnx`
- `tokens.txt`

### 与现有模型的对比

| 特性 | dictation (SenseVoice) | dictation-ng (FunASR-Nano) | 德语方案 (Parakeet TDT) |
|------|----------------------|--------------------------|----------------------|
| 引擎 | SenseVoice offline | FunASR-Nano offline | Parakeet TDT offline |
| 识别方式 | 录完再识别 | VAD + chunk 伪流式 | VAD + chunk 伪流式 |
| API 层 | sherpa-rs 高级封装 | sherpa-rs 高级封装 | sherpa-rs-sys FFI |
| 目标语言 | 中文 | 中文 | 德语+英语 |
| 模型大小 | ~200MB | ~1.5GB | ~600MB (int8) |
| 德语支持 | 无 | 无 | 原生支持 (WER 7.4%) |

## 待验证

- [ ] 下载 Parakeet TDT 0.6B v3 int8 模型，确认文件结构
- [ ] 通过 sherpa-rs-sys FFI 调用 offline transducer API 识别德语音频
- [ ] 测试德语+英语混合输入的识别效果
- [ ] 对比 Canary 180M Flash 的德语精度（体积更小，可能更适合轻量场景）
- [ ] 确认 VAD chunk 大小对德语识别精度的影响

## 参考链接

- [NeMo Parakeet TDT 0.6B v3 - HuggingFace](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
- [Canary-1B-v2 & Parakeet-TDT-0.6B-v3 论文](https://arxiv.org/html/2509.14128v1)
- [sherpa-onnx Pre-trained Models](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/index.html)
- [sherpa-onnx NeMo Transducer Models](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/offline-transducer/nemo-transducer-models.html)
- [Parakeet TDT streaming 讨论 #2918](https://github.com/k2-fsa/sherpa-onnx/issues/2918)
- [2026 开源 STT 模型对比 - Northflank](https://northflank.com/blog/best-open-source-speech-to-text-stt-model-in-2026-benchmarks)
