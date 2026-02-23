# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

PICC (PICCrop) is a macOS-only Rust application that provides interactive screenshot capture with OCR (text recognition) and speech recognition. It interfaces with Apple's native frameworks (CoreGraphics, Vision, AVFoundation) through Objective-C FFI using `objc2`/`icrate` crates.

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

- **`core_graphics`** — FFI bindings to macOS CoreGraphics for screenshot capture. Defines `CGImage`/`CGImageRef` types and screen capture functions.
- **`vision`** — FFI bindings to macOS Vision framework for OCR. Wraps `VNImageRequestHandler`, `VNRecognizeTextRequest`, `VNRecognizedTextObservation`. Supports multi-language recognition.
- **`avfaudio`** — FFI bindings to AVFoundation audio engine (`AVAudioEngine`, `AVAudioInputNode`) for microphone input, used by speech recognition.

Top-level `screenshot(rect)` function in `lib.rs` captures a screen region as `CGImage`.

### Binary (`src/main.rs`)

Uses `declare_class!` macro to create custom Objective-C classes:
- **`SnapWindow`** — Custom `NSPanel` subclass that captures the full screen, handles mouse/keyboard events for region selection, and triggers OCR on the selected area.
- **`DrawPathView`** — Custom `NSView` that draws the red selection rectangle overlay.
- **`ocr()`** — Crops the captured image to the selection and runs Vision framework text recognition (Chinese + English).

### Key patterns

- All Apple framework interaction uses `unsafe` blocks with `objc2` message sending (`msg_send!`, `msg_send_id!`).
- Custom Objective-C classes are declared via `declare_class!` macro with `ClassType`, `DeclaredClass` implementations.
- `icrate` v0.0.2 provides typed Rust wrappers around Foundation/AppKit classes (`NSApplication`, `NSPanel`, `NSView`, `NSEvent`, etc.).

## Dependencies Note

The project uses pre-release versions of `objc2` (0.3.0-beta.5) and `icrate` (0.0.2). These APIs are unstable — refer to actual crate docs rather than assuming stable API patterns.

## Platform

macOS only. Requires macOS frameworks: CoreGraphics, Vision, AVFoundation, AppKit, Speech.
