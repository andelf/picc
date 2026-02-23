# PICC - PICCrop

A lightweight macOS screenshot & OCR tool written in Rust, using Apple native frameworks (CoreGraphics, Vision, AppKit) via `objc2` FFI.

## Features

- **Global hotkey** — `Ctrl+Cmd+A` to enter screenshot mode from anywhere
- **Interactive region selection** — drag to select, with real-time pixel resolution display
- **OCR** — text recognition powered by Apple Vision framework (Chinese + English)
- **Speech recognition** — real-time speech-to-text via Speech framework
- Runs in background with no Dock icon

## Usage

```sh
# Interactive screenshot + OCR (runs in background, press Ctrl+Cmd+A to activate)
cargo run

# Full screen OCR
cargo run --example screen-ocr

# OCR from local image file
cargo run --example local-ocr

# Real-time speech-to-text
cargo run --example speech-recognition
```

> Requires macOS. Global hotkey needs Accessibility permission (System Settings > Privacy & Security > Accessibility).

## License

MIT/Apache-2.0
