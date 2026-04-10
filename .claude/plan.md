# confirm — 全屏确认对话框工具

## 用途

用在 shell 脚本中，强制用户确认高危操作。全屏半透明遮罩打断当前工作流，必须明确按 Y 确认或 N/ESC 拒绝。

```bash
# 用法示例
confirm "drop production database" && psql -c "DROP DATABASE prod"
confirm -m "deploy to production" && ./deploy.sh
```

## 修改文件

- `Cargo.toml` — 添加 `[[bin]]` 段
- `src/bin/confirm.rs` — 新文件

## 设计

### 用法

```
confirm [OPTIONS] <message>
```

- 位置参数 `<message>` — 操作描述（如 "delete all data"）
- `-m <message>` — 同上，备选写法
- 退出码：0 = 确认 (Y)，1 = 拒绝 (N/ESC)

### UI 布局

```
┌─────────────────────────────────────────┐
│          (全屏半透明黑色遮罩 alpha 0.7)     │
│                                         │
│                                         │
│           ⚠ CONFIRMATION REQUIRED       │  ← 标题，黄色，36pt
│                                         │
│         Are you sure you want to:       │  ← 白色，20pt
│           drop production database      │  ← 操作描述，黄色加粗，24pt
│                                         │
│          [Y] Confirm  /  [N] Cancel     │  ← 白色，18pt
│                                         │
│                                         │
└─────────────────────────────────────────┘
```

### 按键处理

- **Y/y** — 确认，`app.terminate()` 退出码 0
- **N/n/ESC** — 取消，`app.terminate()` 退出码 1
- **其他按键** — 遮罩闪动反馈（短暂变亮再恢复），不做任何操作

### 闪动效果

用 NSTimer 实现：其他按键按下时 alpha 变为 0.9（变亮），100ms 后恢复 0.7。

### 退出码

用 `std::process::exit()` 设置退出码，0=确认，1=取消。

### 架构

复用 standup 模式：
- **ConfirmWindow** — NSPanel 子类，处理按键
- **ConfirmView** — NSView 子类，绘制遮罩和文字
- 消息文本存在 `static Mutex<String>` 中
- 退出码存在 `static AtomicI32` 中

## 实现步骤

1. Cargo.toml 添加 `[[bin]] name = "confirm"`
2. 创建 `src/bin/confirm.rs`：
   - 解析命令行参数获取 message
   - ConfirmWindow + ConfirmView
   - 按键处理 + 闪动效果
   - `std::process::exit(code)`
3. `cargo build --bin confirm` 验证编译
