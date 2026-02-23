# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

PICC (PICCrop) is a macOS-only Rust application that provides interactive screenshot capture with OCR (text recognition) and speech recognition. It interfaces with Apple's native frameworks (CoreGraphics, Vision, AVFoundation) through Objective-C FFI using the `objc2` 0.6 ecosystem crates.

## Build & Run Commands

- `cargo build` — build the project
- `cargo run` — run the main interactive screenshot+OCR tool
- `cargo run --example screen-ocr` — full screen OCR
- `cargo run --example local-ocr` — OCR from local image file
- `cargo run --example speech-recognition` — real-time speech-to-text

No tests exist currently.

## Architecture

The project is a library (`src/lib.rs`) + binary (`src/main.rs`) crate.

### Library modules (`src/lib.rs`)

- **`core_graphics`** — Re-exports from `objc2-core-graphics` and `objc2-core-foundation` for screenshot capture (`CGImage`, `CGRect`, `CGWindowListCreateImage`).
- **`vision`** — Thin wrapper over `objc2-vision` providing convenience functions for OCR (`new_handler_with_cgimage`, `perform_requests`). Re-exports `VNRecognizeTextRequest`, `VNImageRequestHandler`, etc.
- **`avfaudio`** — Re-exports from `objc2-avf-audio` (`AVAudioEngine`, `AVAudioInputNode`, `AVAudioFormat`, etc.) for microphone input used by speech recognition.

Top-level `screenshot(rect)` function in `lib.rs` captures a screen region as `CFRetained<CGImage>`.

### Binary (`src/main.rs`)

Uses `define_class!` macro to create custom Objective-C classes:
- **`SnapWindow`** — Custom `NSPanel` subclass that captures the full screen, handles mouse/keyboard events for region selection, and triggers OCR on the selected area. Uses `Cell<NSPoint>` ivars for interior mutability.
- **`DrawPathView`** — Custom `NSView` that draws the selection rectangle overlay with semi-transparent mask.
- **`ocr()`** — Takes a `&CGImage`, runs Vision framework text recognition (Chinese + English).

### Key patterns

- Custom Objective-C classes use `define_class!` macro with `#[ivars = Struct]` and `Cell<T>` for interior mutability (all method receivers are `&self`).
- `MainThreadMarker` is required for AppKit classes (`NSApplication::sharedApplication`, `NSScreen::mainScreen`, class allocation).
- Framework types come from dedicated crates: `objc2-foundation`, `objc2-app-kit`, `objc2-core-graphics`, `objc2-vision`, `objc2-avf-audio`, `objc2-speech`.
- `Retained<T>` replaces the old `Id<T, Owned/Shared>` for reference-counted Objective-C objects.
- `CFRetained<T>` is used for Core Foundation types like `CGImage`.

## Dependencies

- `objc2` 0.6.3 — Core Objective-C interop
- `block2` 0.6.2 — Objective-C block support (`RcBlock`)
- `objc2-foundation` 0.3.2 — Foundation framework bindings
- `objc2-app-kit` 0.3.2 — AppKit framework bindings
- `objc2-core-foundation` 0.3.2 — Core Foundation types (`CGRect`, `CGPoint`, `CFRetained`)
- `objc2-core-graphics` 0.3.2 — CoreGraphics framework bindings
- `objc2-vision` 0.3.2 — Vision framework bindings (OCR)
- `objc2-avf-audio` 0.3.2 — AVFAudio framework bindings
- `objc2-speech` 0.3.2 — Speech framework bindings

## Platform

macOS only. Requires macOS frameworks: CoreGraphics, Vision, AVFoundation, AppKit, Speech.
