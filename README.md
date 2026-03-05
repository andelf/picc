# PICC

A collection of macOS automation & productivity tools written in Rust, built on Apple native frameworks (CoreGraphics, Vision, AppKit, Accessibility) via the `objc2` ecosystem.

## Tools

### picc — Screenshot + OCR

Interactive screenshot tool with region selection and text recognition.

- `Ctrl+Cmd+A` global hotkey to activate
- Drag to select region, real-time pixel resolution display
- OCR powered by Vision framework (Chinese + English)
- Runs in background with no Dock icon

```sh
cargo run --bin picc
```

### axcli — Accessibility CLI

Playwright-style CLI for macOS app automation via the Accessibility API. Supports element locating, clicking, typing, scrolling, screenshots with OCR, and more.

```sh
axcli --app Lark snapshot              # Print accessibility tree
axcli --app Lark click '.SearchButton' # Click element by locator
axcli --app Lark input '.SearchInput' 'hello'
axcli --app Lark screenshot --ocr -o /tmp/shot.png
axcli --app Lark get AXValue '.SearchInput'
```

### standup — Break Reminder

Pomodoro-style break reminder with menubar countdown. Supports LAN sync across multiple devices via UDP broadcast.

```sh
cargo run --bin standup -- --work 25 --break 5
cargo run --bin standup -- --solo  # Disable LAN sync
```

### ax_print / ax_tui — Accessibility Tree Viewers

Print or interactively browse the accessibility tree of any macOS app.

```sh
cargo run --bin ax_print -- --app Lark
cargo run --bin ax_tui -- --app Chrome
```

### hidden — Menubar Manager

Hidden Bar clone — collapse/expand menubar icons by toggling a separator status item.

```sh
cargo run --bin hidden
```

### dictation — Push-to-Talk Dictation

Hold right Command key to dictate speech, recognized text is typed at the cursor. Supports SenseVoice (offline, via sherpa-onnx) and Apple Speech API engines.

```sh
# Install
cargo install --git https://github.com/andelf/picc --bin dictation

# Apple Speech API (default, no setup needed)
dictation --engine apple

# SenseVoice (offline, requires model download)
# Download model first:
#   mkdir -p ~/.local/share/picc && cd ~/.local/share/picc
#   wget https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2
#   tar xjf sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2
dictation --engine sensevoice
```

### claude_menubar — Claude Code Status Hook

Menubar indicator showing Claude Code session status, driven by Claude Code hooks.

```sh
cargo run --bin claude_menubar
```

## Requirements

- macOS
- Accessibility permission (System Settings > Privacy & Security > Accessibility)

## License

MIT/Apache-2.0
