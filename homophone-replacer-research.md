# Homophone Replacer 研究笔记

## 目标

这份文档只做一件事：把 `sherpa-onnx` 里的 `homophone replacer` 实现拆开，整理成后续 Rust 版本可直接复现的行为说明和测试基线。

当前阶段不写 Rust 实现，只确认：

- 上游代码放在本地哪里
- `replace.fst` 是怎么生成的
- 运行时到底做了哪些步骤
- 哪些细节必须和上游保持一致

## 本地参考代码

上游仓库已下载到本地：

- `/Users/oker/Repos/picc/vendor/sherpa-onnx`

当前拉取到的提交：

- `352b751aac9e05f91ad0e24c0487a63f343cad0b`

本次重点阅读的文件：

- `/Users/oker/Repos/picc/vendor/sherpa-onnx/sherpa-onnx/csrc/homophone-replacer.cc`
- `/Users/oker/Repos/picc/vendor/sherpa-onnx/sherpa-onnx/csrc/homophone-replacer.h`
- `/Users/oker/Repos/picc/vendor/sherpa-onnx/sherpa-onnx/csrc/phrase-matcher.cc`
- `/Users/oker/Repos/picc/vendor/sherpa-onnx/sherpa-onnx/csrc/text-utils.cc`
- `/Users/oker/Repos/picc/vendor/sherpa-onnx/sherpa-onnx/csrc/offline-recognizer-impl.cc`
- `/Users/oker/Repos/picc/vendor/sherpa-onnx/sherpa-onnx/csrc/online-recognizer-impl.cc`
- `/Users/oker/Repos/picc/vendor/sherpa-onnx/cxx-api-examples/sense-voice-with-hr-cxx-api.cc`
- `/Users/oker/Repos/picc/vendor/sherpa-onnx/c-api-examples/sense-voice-with-hr-c-api.c`

网页说明对应的是：

- <https://k2-fsa.github.io/sherpa/onnx/homophone-replacer/index.html>

## 一句话结论

它不是“热词加权”，而是“识别后替换”。

实现分成两层：

1. 用 `pynini` 把“拼音串 -> 目标汉字词组”的规则编译成 `replace.fst`
2. 运行时把识别文本按词组转成拼音，再拿拼音序列去跑 `replace.fst`

所以后续 Rust 版如果要做到和上游行为一致，不能只复现 `pynini.cross(...)` 那几行脚本，还必须复现运行时这几件事：

- 文本切分
- 词组最长匹配
- 汉字词组转拼音
- 中英混排保留
- 只对连续中文片段做替换

## `pynini` 这一层到底做了什么

官方文档里的规则生成示例本质上很简单：

```python
import pynini
from pynini import cdrewrite
from pynini.lib import utf8

sigma = utf8.VALID_UTF8_CHAR.star

rule1 = pynini.cross("xuan2jie4xin1pian4", "玄戒芯片")
rule2 = pynini.cross("xuan2jie4xing1pian4", "玄戒芯片")
rule3 = pynini.cross("fu2nan2ren2", "湖南人")

rule = (rule1 | rule2 | rule3).optimize()
rule = cdrewrite(rule, "", "", sigma)
rule.write("replace.fst")
```

这里真正重要的结论有四个：

1. 规则的左边不是汉字，而是“无空格拼接后的拼音串”
2. 规则的右边是最终要输出的汉字词组
3. 多条规则可以指向同一个目标词组
4. 产物只有一个运行时文件：`replace.fst`

也就是说，`pynini` 层只负责“编译规则”，不负责“把识别文本转拼音”。

## 运行时整体流程

上游运行时的真实链路如下：

1. ASR 先正常输出文本
2. 如果配置了 `hr.lexicon` 和 `hr.rule_fsts`，则创建 `HomophoneReplacer`
3. 识别完成后，先做已有的 ITN
4. 再调用 `ApplyHomophoneReplacer(text)`
5. 最终输出替换后的文本

从 `offline-recognizer-impl.cc` 和 `online-recognizer-impl.cc` 看，`homophone replacer` 是后处理步骤，不参与声学解码。

这点很重要，因为这决定了 Rust 版后续也应该做成一个独立文本后处理库，而不是把逻辑绑死在某个模型里。

## 配置项和真实约束

运行时配置结构很小：

- `dict_dir`
- `lexicon`
- `rule_fsts`
- `debug`

但真实约束比表面更严格：

- `dict_dir` 现在已经不用了
- `lexicon` 必须存在
- `rule_fsts` 虽然接口写成逗号分隔，但当前实现只支持 1 个文件

上游代码里明确写了：

- 多于 1 个 `rule_fsts` 会直接报错退出
- 实际替换时也只取第一个 `TextNormalizer`

所以 Rust 版第一版不要设计成“多规则文件并行”，否则行为会和上游不一致。

## `lexicon.txt` 的实际作用

`lexicon.txt` 不是替换规则表，它是“词组 -> 拼音”的映射表。

初始化时，上游会逐行读取 `lexicon.txt`，做这些处理：

1. 取第一列作为 `word`
2. 把 `word` 转成小写
3. 后面的每一列都视为一个拼音音节
4. 把所有音节直接拼接起来，变成没有分隔符的串
5. 写入 `word2pron`

例如：

```text
湖南 hu2 nan2
玄戒 xuan2 jie4
芯片 xin1 pian4
```

会变成：

- `湖南 -> hu2nan2`
- `玄戒 -> xuan2jie4`
- `芯片 -> xin1pian4`

### 语气词/轻声处理

上游有个很具体的细节：

- 如果某个拼音 token 的最后一个字符大于 `'4'`
- 就自动在这个 token 后面补一个 `1`

这意味着：

- 它默认把不规范的声调结尾当成第一声处理
- 这和网页文档里“轻声用第一声代替”的规则是一致的

Rust 版必须保留这个行为，否则同一份 `lexicon.txt` 会产生不同结果。

### 重复项处理

如果 `lexicon.txt` 里同一个 `word` 出现多次：

- 只保留第一次
- 后面的重复项忽略

这是测试里必须覆盖的行为。

### 空拼音行处理

如果某一行只有词，没有拼音：

- 该行会被忽略
- 并输出 warning

Rust 版也应该做同样处理，而不是静默接受。

## 文本切分不是“逐字”那么简单

运行时不是直接把整句中文一个字一个字转拼音。

实际流程是：

1. `SplitUtf8(text)`
2. 先按 UTF-8 切开
3. 再把连续英文字母合并成词
4. 然后交给 `PhraseMatcher` 做最长匹配

这里有两个关键点：

### 1. 英文会被保留成词

比如输入里有 `CPU`、`OpenAI`、`hello`，不会被拆成单个字母参与中文替换。

### 2. 中文会做“最长词组匹配”

`PhraseMatcher` 会在当前位置向后看，默认最多看 10 个 token，优先匹配 `lexicon` 里最长的词组。

也就是说，如果 `lexicon` 里同时有：

- `湖南`
- `湖南人`

而文本里出现的是 `湖南人`，它会优先拿到 `湖南人`，不是先切成 `湖南` 再切 `人`。

这会直接影响最终拼音串，也会直接影响能不能命中 `replace.fst`。

Rust 版如果只做逐字扫描，最终效果会和上游偏差很大。

## 运行时如何把文本转成“可匹配拼音串”

`Apply(text)` 的核心逻辑可以拆成下面几步：

1. 把文本切成词/词组
2. 遍历这些词组
3. 遇到非中文或过短 token，就直接原样输出
4. 遇到中文词组，就转成拼音串
5. 把连续中文片段一起送进 `TextNormalizer::Normalize(words, pronunciations)`
6. 得到替换结果后再拼回原文

### 非中文片段怎么处理

如果 token 满足任一条件：

- 字节长度小于 3
- 首字节小于 128

它就不会走中文替换，而是直接输出。

另外有个上游细节：

- 如果 token 的第一个字符是英文字母
- 输出后会额外补一个空格

最后如果整句末尾有多余空格，会再去掉。

这说明上游在中英混排时对空格有自己的处理习惯。Rust 版如果忽略这点，字符串结果会不同。

### 中文词组怎么转拼音

规则如下：

1. 如果整个词组在 `word2pron` 里，直接用整词拼音
2. 否则，如果它是多字词组，就拆成 UTF-8 字符逐个查
3. 单个字查得到就替换成对应拼音
4. 查不到就保留原字符本身

这个回退策略非常重要。

例如一个词组整体没出现在 `lexicon.txt` 里，但每个字都能查到拼音，那么它仍然可以生成完整拼音串并参与后续替换。

## 真正发生替换的地方

中文片段最终不是拿“原始文本”去匹配，而是拿下面这两组数据一起送入：

- `words`
- `pronunciations`

调用的是：

```cpp
r->Normalize(words, pronunciations)
```

这说明 `replace.fst` 看到的不是简单字符串替换，而是基于“词组序列 + 拼音序列”的归一化流程。

因此，对 Rust 版来说，最接近上游的实现方式应该是：

- 保留“先分词组、再转拼音”的结构
- 不要偷懒成“全文先转拼音字符串，再做普通查表替换”

后者可能在简单例子上能跑通，但边界行为会不同。

## 行为边界

目前可以明确确认的边界如下：

### 只改中文

非中文字符不会被替换。

### 规则文件必须预先生成

运行时不会动态编译规则。

### 不支持热更新

`replace.fst` 和 `lexicon.txt` 是初始化时读入的，不是每次 `Apply()` 时重新加载。

### 多个 `rule_fsts` 目前不支持

接口看着像支持，实际不支持。

### 输出末尾空格会被清理

对字符串完全相等测试有影响。

### 会清理非法 UTF-8

`Apply()` 返回前会调用 `RemoveInvalidUtf8Sequences()`。

Rust 版如果使用 `String` 作为输入输出，天然会更严格，但仍然要决定是否保留“坏字节过滤”这层兼容语义。

## 文档示例里有一个小心点

官方调试段落里有一行：

- `Output text: '下面是一个测试玄戒芯片湖南人头安装机载传感器'`

但同一页最终结果又写成：

- `下面是一个测试玄戒芯片湖南人弓头安装机载传感器`

结合前文规则和正式结果，我更信正式结果，调试段那一行更像示例日志里的笔误或中间态展示。

所以后续做黄金测试时，不要拿这条调试日志当唯一真值，应以页面最终结果和可运行示例为准。

## 对 Rust 版的实现建议

### 第一阶段目标

先做“运行时替换库”，不做“规则生成器”。

原因很简单：

- 规则生成依赖 `pynini`
- `pynini` 官方安装主要面向 Linux
- 我们当前最关键的是复现运行时行为

所以第一阶段建议输入就是：

- `lexicon.txt`
- `replace.fst`

### 第一阶段建议拆分

建议拆成 4 个模块：

1. `lexicon`
   - 解析 `lexicon.txt`
   - 处理 lowercase、重复词、轻声补 `1`

2. `phrase_matcher`
   - 对输入 token 序列做最长匹配
   - 默认最大搜索长度对齐上游 `10`

3. `pronunciation`
   - 负责“整词命中”与“逐字回退”

4. `replacer`
   - 负责中文片段拼接
   - 负责调用 FST 归一化器
   - 负责中英混排保留

### 关于 `replace.fst`

这里是 Rust 版最大的技术风险点。

我们已经确认：

- 上游运行时依赖的是 `kaldifst::TextNormalizer`
- `replace.fst` 是 `pynini` 写出来的 FST 二进制

但在本阶段研究里，我还没有验证“纯 Rust 直接读取并执行这份 FST”是否能无缝对齐上游。

所以实现时建议分两步：

1. 先把“切词、拼音、片段组织”这部分用 Rust 纯实现
2. `replace.fst` 执行层先做一个可替换抽象

也就是先定义一个 trait，类似：

```rust
trait Normalizer {
    fn normalize(&self, words: &[String], prons: &[String]) -> String;
}
```

这样后面可以先接兼容层，再决定是否追求纯 Rust FST 执行。

## 测试基线建议

下面这些测试，建议在 Rust 版开工前先写成黄金测试。

### A. 文档同款主案例

输入：

- 文本：`下面是一个测试悬界芯片湖南人工投安装基载传感器`
- 规则：
  - `xuan2jie4xin1pian4 -> 玄戒芯片`
  - `xuan2jie4xing1pian4 -> 玄戒芯片`
  - `fu2nan2ren2 -> 湖南人`
  - `gong1tou2an1zhuang1 -> 弓头安装`
  - `ji1zai3chuan2gan3qi4 -> 机载传感器`
  - `ji1zai4chuan2gan3qi4 -> 机载传感器`

期望：

- `下面是一个测试玄戒芯片湖南人弓头安装机载传感器`

### B. 必须整条拼音都命中才替换

输入：

- 文本：`悬界芯片`
- 只配置 `xuan2jie4xin1pian4 -> 玄戒芯片`

期望：

- 不替换

原因：

- 文档明确说，规则只会在整条拼音都匹配时命中

### C. 多种误读映射到同一目标词

输入：

- `悬界芯片`
- `悬界星片`

期望都输出：

- `玄戒芯片`

### D. 非中文保持不变

输入：

- `OpenAI 玄界芯片 v2`

期望：

- 英文和版本号保持原样
- 只有中文命中后替换

### E. 词组优先于单字

给定 `lexicon` 同时包含：

- `湖南`
- `湖南人`

输入：

- `湖南人`

期望：

- 走最长匹配

### F. 整词查不到时逐字回退

输入一个整体不在 `lexicon`、但拆字都能查到拼音的词组。

期望：

- 仍能生成拼音串并参与替换

### G. 重复 lexicon 项

`lexicon.txt` 中同一个词出现多次。

期望：

- 保留第一条
- 后续重复项忽略

### H. 轻声补 `1`

给定一个音节 token 末尾不是 `1..4`。

期望：

- 自动补成 `1`

### I. 末尾空格处理

输入包含英文 token，触发中英混排路径。

期望：

- 输出不出现额外尾部空格

### J. 配置约束

输入多个 `rule_fsts`。

期望：

- 行为和上游一致
- 当前按“不支持多个文件”处理

## 建议的开发顺序

建议严格按这个顺序做：

1. 先把 `lexicon` 解析和 `phrase matcher` 做成纯 Rust
2. 用假 `Normalizer` 写第一批单元测试
3. 再接 `replace.fst` 的执行层
4. 用文档主案例做集成测试
5. 最后再考虑要不要自己替代 `pynini`

这样做的好处是：

- 先锁定我们最容易写错的上游行为
- 把最大风险压缩到 FST 执行层
- 后面即便更换 FST 后端，测试也能继续复用

## 当前阶段完成情况

已完成：

- 上游源码下载到本地
- 核心实现链路梳理完成
- Rust 版需要对齐的行为整理完成
- 后续测试基线整理完成

未完成：

- 还没有写 Rust 版本代码
- 还没有写测试代码
- 还没有验证哪种 Rust FST 方案最适合直接消费 `replace.fst`
