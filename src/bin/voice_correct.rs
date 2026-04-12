//! Voice Correct — dictation + voice-triggered text correction
//!
//! - Single hold right Command: dictation (type spoken text)
//! - Quick tap then hold right Command: correction mode
//!   - If focused field has text: LLM corrects it using spoken instruction
//!   - If focused field is empty: just type spoken text (no LLM call)
//!
//! Usage:
//!   KIMI_API_KEY=sk-... cargo run --bin voice-correct
//!   KIMI_API_KEY=sk-... cargo run --bin voice-correct --features sensevoice -- --engine sensevoice

use std::cell::{Cell, RefCell};
#[cfg(feature = "sensevoice")]
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAnimationContext, NSApplication, NSBackingStoreType, NSFont, NSImage, NSLineBreakMode,
    NSMenu, NSMenuItem, NSPanel, NSScreen, NSStatusItem, NSStatusWindowLevel, NSTextAlignment,
    NSTextField, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView, NSWindowStyleMask,
};
use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRunLoop, CFString, CFType};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{
    ns_string, NSDate, NSLocale, NSPoint, NSRect, NSRunLoop, NSSize, NSString, NSTimer,
};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus,
};

use objc2_application_services::AXUIElement;
use picc::accessibility;
use picc_macos_app::{
    configure_accessory_app, new_menu_item, new_status_item, set_status_item_symbol,
};
use picc_macos_input::{parse_key_combo, press_key_combo, type_text};
#[cfg(feature = "sensevoice")]
use picc_speech::models::resolve_repo_sensevoice_paths;
use picc_speech::postprocess::{apply_dictation_transforms, DictationOptions};
#[cfg(feature = "sensevoice")]
use picc_speech::resample_linear;
use picc_speech::{
    begin_requested_session, char_before_cursor, clear_recording_state, clipboard_replace_is_safe,
    frontmost_bundle_id, read_focused_text, should_skip_ax_read_for_bundle,
    should_use_clipboard_for_bundle, take_cancel_while_recording, take_stop_while_recording,
    AudioCaptureConfig, AudioEngineManager, FocusedText, HotkeyPolicy, HotkeyRuntime, HotkeySignal,
    HotkeyState, SessionMode, SessionSignals, SPACE_AFTER_PUNCT,
};
use tracing::{debug, error, info, warn};

use serde::{Deserialize, Serialize};

// --- CLI ---

#[derive(Parser)]
#[command(about = "Voice Correct — hold right Cmd to dictate, tap+hold to correct")]
struct Args {
    /// Speech engine: "sensevoice" or "apple"
    #[arg(long, default_value = "apple")]
    engine: String,

    /// SenseVoice model directory
    #[arg(long)]
    model_dir: Option<String>,

    /// SenseVoice model file name inside --model-dir
    #[arg(long, default_value = "model.int8.onnx")]
    model_file_name: String,

    /// Language hint for SenseVoice: auto, zh, en, ja, ko, yue
    #[arg(long, default_value = "auto")]
    lang: String,
}

// --- Audio feedback ---

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioServicesPlaySystemSound(sound_id: u32);
}

fn play_start_sound() {
    unsafe { AudioServicesPlaySystemSound(1054) }; // tink
}
fn play_stop_sound() {
    unsafe { AudioServicesPlaySystemSound(1057) }; // pop
}
fn play_error_sound() {
    unsafe { AudioServicesPlaySystemSound(1006) };
}

// --- Mode constants ---

static SESSION_SIGNALS: SessionSignals = SessionSignals::new();
static RECOGNIZED_TEXT: Mutex<String> = Mutex::new(String::new());

/// LLM result from background thread: Some(Ok(corrected)) or Some(Err(msg))
static LLM_RESULT: Mutex<Option<Result<String, String>>> = Mutex::new(None);

/// Pointer to the CGEvent tap CFMachPort — used to re-enable on timeout.
static EVENT_TAP_PTR: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(ptr::null_mut());

/// Gate for the audio tap callback — only accumulate samples when true.
static COLLECTING: AtomicBool = AtomicBool::new(false);

/// Current audio RMS level (f32 bits stored as u32) — updated by audio tap callback.
static AUDIO_RMS: AtomicU32 = AtomicU32::new(0);

struct MenuDelegateIvars {
    options: Cell<DictationOptions>,
    punct_spaces_item: RefCell<Option<Retained<NSMenuItem>>>,
}

impl std::fmt::Debug for MenuDelegateIvars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuDelegateIvars")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn hotkey_state() -> &'static Mutex<HotkeyState> {
    static HOTKEY_STATE: OnceLock<Mutex<HotkeyState>> = OnceLock::new();
    HOTKEY_STATE.get_or_init(|| Mutex::new(HotkeyState::new()))
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    // macOS disables the tap if our callback takes too long or if the user
    // switches to a secure input context.  Re-enable it immediately.
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        warn!(
            event_type = ?event_type,
            "CGEvent tap was disabled — re-enabling"
        );
        let tap_ptr = EVENT_TAP_PTR.load(Ordering::Relaxed);
        if !tap_ptr.is_null() {
            let tap = &*(tap_ptr as *const CFMachPort);
            CGEvent::tap_enable(tap, true);
        }
        return event.as_ptr();
    }

    if event_type == CGEventType::FlagsChanged {
        let flags = CGEvent::flags(Some(event.as_ref()));
        let device_flags = flags.0 & 0xFFFF;
        if let Ok(mut state) = hotkey_state().lock() {
            let signals = state.handle_flags_changed(
                device_flags,
                now_ms(),
                HotkeyRuntime {
                    is_recording: SESSION_SIGNALS.is_recording(),
                    cancel_pending: SESSION_SIGNALS.cancel_pending(),
                },
                HotkeyPolicy::voice_correct(),
            );
            for signal in signals {
                match signal {
                    HotkeySignal::Start(mode) => SESSION_SIGNALS.request_start(mode),
                    HotkeySignal::Stop => SESSION_SIGNALS.request_stop(),
                    HotkeySignal::Cancel => SESSION_SIGNALS.request_cancel(),
                    HotkeySignal::ClearPendingStart => SESSION_SIGNALS.clear_pending_start(),
                }
            }
        }
    }
    event.as_ptr()
}

// --- SenseVoice model download (same as dictation.rs) ---

// --- Menubar delegate ---

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VoiceCorrectMenuDelegate"]
    #[ivars = MenuDelegateIvars]
    #[derive(Debug, PartialEq)]
    struct MenuDelegate;

    #[allow(non_snake_case)]
    impl MenuDelegate {
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            std::process::exit(0);
        }

        #[unsafe(method(toggleFullwidthToHalfwidth:))]
        fn toggle_fullwidth_to_halfwidth(&self, sender: &AnyObject) {
            let mut opts = self.ivars().options.get();
            opts.fullwidth_to_halfwidth = !opts.fullwidth_to_halfwidth;
            self.ivars().options.set(opts);

            let item: &NSMenuItem = unsafe { &*(sender as *const AnyObject as *const NSMenuItem) };
            item.setState(if opts.fullwidth_to_halfwidth { 1 } else { 0 });

            // Enable/disable the punct spaces sub-option
            if let Some(punct_item) = self.ivars().punct_spaces_item.borrow().as_ref() {
                if opts.fullwidth_to_halfwidth {
                    punct_item.setEnabled(true);
                } else {
                    // Turn off and uncheck punct spaces when fullwidth is disabled
                    opts.space_around_punct = false;
                    self.ivars().options.set(opts);
                    punct_item.setEnabled(false);
                    punct_item.setState(0);
                }
            }
            info!(enabled = opts.fullwidth_to_halfwidth, "fullwidth→halfwidth");
        }

        #[unsafe(method(toggleSpaceAroundPunct:))]
        fn toggle_space_around_punct(&self, sender: &AnyObject) {
            let mut opts = self.ivars().options.get();
            opts.space_around_punct = !opts.space_around_punct;
            self.ivars().options.set(opts);

            let item: &NSMenuItem = unsafe { &*(sender as *const AnyObject as *const NSMenuItem) };
            item.setState(if opts.space_around_punct { 1 } else { 0 });
            info!(
                enabled = opts.space_around_punct,
                "space around punctuation"
            );
        }

        #[unsafe(method(toggleSpaceBetweenCjk:))]
        fn toggle_space_between_cjk(&self, sender: &AnyObject) {
            let mut opts = self.ivars().options.get();
            opts.space_between_cjk = !opts.space_between_cjk;
            self.ivars().options.set(opts);

            let item: &NSMenuItem = unsafe { &*(sender as *const AnyObject as *const NSMenuItem) };
            item.setState(if opts.space_between_cjk { 1 } else { 0 });
            info!(
                enabled = opts.space_between_cjk,
                "space between CJK & Latin"
            );
        }

        #[unsafe(method(toggleStripTrailingPunct:))]
        fn toggle_strip_trailing_punct(&self, sender: &AnyObject) {
            let mut opts = self.ivars().options.get();
            opts.strip_trailing_punct = !opts.strip_trailing_punct;
            self.ivars().options.set(opts);

            let item: &NSMenuItem = unsafe { &*(sender as *const AnyObject as *const NSMenuItem) };
            item.setState(if opts.strip_trailing_punct { 1 } else { 0 });
            info!(
                enabled = opts.strip_trailing_punct,
                "strip trailing punctuation"
            );
        }
    }
);

// --- Status icon ---

enum AppState {
    Idle,
    Recording,
    Correcting,
    Processing,
}

/// Animated processing icons — cycled every few timer ticks
const PROCESSING_ICONS: &[&str] = &[
    "ellipsis.circle",
    "ellipsis.circle.fill",
    "ellipsis.circle",
    "ellipsis",
];

fn set_status_icon(item: &NSStatusItem, state: AppState, mtm: MainThreadMarker) {
    let name = match state {
        AppState::Idle => "wand.and.stars",
        AppState::Recording => "mic.fill",
        AppState::Correcting => "mic.badge.plus",
        AppState::Processing => "ellipsis.circle", // initial, animated by timer
    };
    set_status_item_symbol(item, mtm, name, "Voice Correct");
}

fn set_status_icon_name(item: &NSStatusItem, name: &str, mtm: MainThreadMarker) {
    if let Some(button) = item.button(mtm) {
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            Some(&NSString::from_str("Voice Correct")),
        ) {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
    }
}

// --- AX: write corrected text ---

/// Replace text in the focused field.
/// For browsers/Electron/Lark: always clipboard paste.
/// For native apps: try AXValue set first, fall back to clipboard.
fn write_corrected_text(element: &AXUIElement, text: &str, original_text: &str) -> bool {
    let bundle = frontmost_bundle_id();
    if should_use_clipboard_for_bundle(bundle.as_deref()) {
        info!("using clipboard paste for this app");
        let current_text = read_focused_text().map(|focused| focused.text);
        if !clipboard_replace_is_safe(original_text, current_text.as_deref()) {
            warn!("refusing clipboard replace because focused text changed");
            return false;
        }
        return replace_via_clipboard(text);
    }

    // Try AX API (only works on native Cocoa controls)
    let cf_str = CFString::from_str(text);
    let cf_type: &CFType = cf_str.as_ref();
    if accessibility::set_attr_value(element, "AXValue", cf_type) {
        if let Some(readback) = accessibility::attr_string(element, "AXValue") {
            if readback.trim() == text.trim() {
                return true;
            }
        }
        info!("AXValue set didn't take effect, using clipboard fallback");
    }

    let current_text = read_focused_text().map(|focused| focused.text);
    if !clipboard_replace_is_safe(original_text, current_text.as_deref()) {
        warn!("refusing clipboard fallback because focused text changed");
        return false;
    }

    replace_via_clipboard(text)
}

/// Replace all text in the focused field via clipboard round-trip.
/// Uses NSPasteboard API: save → set → Cmd+A → Cmd+V → restore.
fn replace_via_clipboard(text: &str) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};

    let pb = NSPasteboard::generalPasteboard();
    let pb_type = unsafe { NSPasteboardTypeString };

    // Save current clipboard
    let old = pb.stringForType(pb_type);

    // Set clipboard to replacement text
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), pb_type);

    // Cmd+A (select all)
    let (keycode, flags) = parse_key_combo("Command+a");
    press_key_combo(keycode, flags);
    std::thread::sleep(Duration::from_millis(30));

    // Cmd+V (paste)
    let (keycode, flags) = parse_key_combo("Command+v");
    press_key_combo(keycode, flags);
    std::thread::sleep(Duration::from_millis(100));

    // Restore old clipboard
    pb.clearContents();
    if let Some(ref old_text) = old {
        pb.setString_forType(old_text, pb_type);
    }

    true
}

// --- LLM correction ---

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f64,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

const SYSTEM_PROMPT: &str =
    "你是一个文本纠错助手。用户会提供一段已输入的文本，以及一条语音纠错指令。\n\
请根据指令修改文本，只输出修改后的完整文本，不要添加任何解释。";

fn call_llm(original_text: &str, instruction: &str) -> Result<String, String> {
    let api_key = std::env::var("KIMI_API_KEY").map_err(|_| "KIMI_API_KEY not set".to_string())?;

    let original_text = original_text.trim();
    let instruction = instruction.trim();
    let user_msg = format!("已输入文本：{original_text}\n纠错指令：{instruction}");

    let request = ChatRequest {
        model: "kimi-k2-0905-preview".to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_msg,
            },
        ],
        temperature: 0.3,
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client error: {e}"))?;

    let resp = client
        .post("https://api.moonshot.cn/v1/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&request)
        .send()
        .map_err(|e| format!("HTTP error: {e}"))?;

    let status = resp.status();
    let body = resp.text().map_err(|e| format!("read error: {e}"))?;

    if !status.is_success() {
        return Err(format!("API {status}: {body}"));
    }

    let parsed: ChatResponse =
        serde_json::from_str(&body).map_err(|e| format!("response parse: {e}"))?;

    parsed
        .choices
        .first()
        .map(|c| c.message.content.trim().to_string())
        .ok_or_else(|| "no choices in response".to_string())
}

// --- Floating capsule overlay ---

/// Waveform bar weights (center-high, sides-low)
const BAR_WEIGHTS: [f32; 5] = [0.5, 0.8, 1.0, 0.75, 0.55];
/// Smoothed per-bar levels (updated each timer tick)
static BAR_LEVELS: [AtomicU32; 5] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

/// Create a capsule-shaped mask image for NSVisualEffectView.
/// Uses cap insets so the image stretches correctly when width changes.
fn make_capsule_mask(width: f64, height: f64, radius: f64) -> Retained<NSImage> {
    let size = NSSize::new(width, height);
    let handler = block2::RcBlock::new(move |rect: NSRect| -> objc2::runtime::Bool {
        objc2_app_kit::NSColor::whiteColor().setFill();
        let path = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect, radius, radius,
        );
        path.fill();
        objc2::runtime::Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(size, false, &handler);

    // Cap insets: the rounded corners are `radius` wide/tall; the middle stretches.
    image.setCapInsets(objc2_foundation::NSEdgeInsets {
        top: radius,
        left: radius,
        bottom: radius,
        right: radius,
    });
    // NSImageResizingModeStretch = 1
    image.setResizingMode(objc2_app_kit::NSImageResizingMode(1));

    image
}

// Custom NSView that draws 5 vertical waveform bars driven by audio RMS.
define_class!(
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[name = "WaveformView"]
    #[derive(Debug, PartialEq)]
    struct WaveformView;

    #[allow(non_snake_case)]
    impl WaveformView {
        #[unsafe(method(drawRect:))]
        fn drawRect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let bar_count = 5;
            let bar_width: f64 = 4.0;
            let bar_gap: f64 = 3.5;
            let total_w = bar_count as f64 * bar_width + (bar_count - 1) as f64 * bar_gap;
            let x_offset = (bounds.size.width - total_w) / 2.0;
            let max_height = bounds.size.height * 0.85;
            let min_height: f64 = 4.0;
            let cy = bounds.origin.y + bounds.size.height / 2.0;

            let mode = SESSION_SIGNALS.mode();
            let color = if mode == SessionMode::Correction {
                // Blue-purple for correction mode
                objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.45, 0.95, 0.9)
            } else {
                // Red for dictation mode
                objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(0.95, 0.22, 0.22, 0.9)
            };
            color.setFill();

            for (i, bar_level) in BAR_LEVELS.iter().enumerate().take(bar_count) {
                let level = f32::from_bits(bar_level.load(Ordering::Relaxed));
                let h = min_height + (max_height - min_height) * level as f64;
                let x = bounds.origin.x + x_offset + i as f64 * (bar_width + bar_gap);
                let y = cy - h / 2.0;
                let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(bar_width, h));
                let path = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    rect,
                    bar_width / 2.0,
                    bar_width / 2.0,
                );
                path.fill();
            }
        }
    }
);

/// Capsule overlay state — held by main thread during the app's lifetime.
struct CapsuleOverlay {
    panel: Retained<NSPanel>,
    waveform_view: Retained<NSView>,
    text_label: Retained<NSTextField>,
    /// Current target width for the capsule (animated)
    current_width: Cell<f64>,
}

const CAPSULE_HEIGHT: f64 = 56.0;
const CAPSULE_CORNER_RADIUS: f64 = 28.0;
const CAPSULE_PADDING_LEFT: f64 = 16.0;
const CAPSULE_PADDING_RIGHT: f64 = 16.0;
const WAVEFORM_WIDTH: f64 = 44.0;
const WAVEFORM_HEIGHT: f64 = 32.0;
const GAP: f64 = 10.0;
const TEXT_MIN_WIDTH: f64 = 160.0;
const TEXT_MAX_WIDTH: f64 = 560.0;
const CAPSULE_BOTTOM_MARGIN: f64 = 48.0;
const LABEL_HEIGHT: f64 = 26.0;
const LABEL_Y: f64 = (CAPSULE_HEIGHT - LABEL_HEIGHT) / 2.0;

fn create_capsule_overlay(mtm: MainThreadMarker) -> CapsuleOverlay {
    let min_width =
        CAPSULE_PADDING_LEFT + WAVEFORM_WIDTH + GAP + TEXT_MIN_WIDTH + CAPSULE_PADDING_RIGHT;
    let rect = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(min_width, CAPSULE_HEIGHT),
    );

    // NSPanel — nonactivating, borderless (use initWithContentRect via msg_send on NSWindow)
    let panel: Retained<NSPanel> = unsafe {
        let alloc = NSPanel::alloc(mtm);
        let style = NSWindowStyleMask::NonactivatingPanel;
        let backing = NSBackingStoreType::Buffered;
        let panel: Retained<NSPanel> = objc2::msg_send![
            alloc,
            initWithContentRect: rect,
            styleMask: style,
            backing: backing,
            defer: false,
        ];
        panel
    };

    panel.setLevel(NSStatusWindowLevel + 1);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
    panel.setHasShadow(true);
    panel.setIgnoresMouseEvents(true);
    panel.setMovableByWindowBackground(false);

    // NSVisualEffectView — HUD window material, rounded capsule
    let effect_view = NSVisualEffectView::initWithFrame(
        NSVisualEffectView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(min_width, CAPSULE_HEIGHT),
        ),
    );
    effect_view.setMaterial(NSVisualEffectMaterial::HUDWindow);
    effect_view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect_view.setState(NSVisualEffectState::Active);
    // Use maskImage for proper capsule clipping (CALayer cornerRadius doesn't
    // correctly clip the blur material on all window compositing modes).
    let mask = make_capsule_mask(min_width, CAPSULE_HEIGHT, CAPSULE_CORNER_RADIUS);
    effect_view.setMaskImage(Some(&mask));

    // Waveform view
    let wf_x = CAPSULE_PADDING_LEFT;
    let wf_y = (CAPSULE_HEIGHT - WAVEFORM_HEIGHT) / 2.0;
    let wf_rect = NSRect::new(
        NSPoint::new(wf_x, wf_y),
        NSSize::new(WAVEFORM_WIDTH, WAVEFORM_HEIGHT),
    );
    let waveform_view: Retained<NSView> = unsafe {
        let alloc = WaveformView::alloc(mtm);
        let view: Retained<WaveformView> = objc2::msg_send![alloc, initWithFrame: wf_rect];
        Retained::cast_unchecked(view)
    };

    // Text label — vertically centered
    let label_font = unsafe {
        // SF Pro Rounded, medium weight — softer look for the HUD capsule
        let base = NSFont::systemFontOfSize_weight(18.0, objc2_app_kit::NSFontWeightMedium);
        let desc = base.fontDescriptor();
        desc.fontDescriptorWithDesign(objc2_app_kit::NSFontDescriptorSystemDesignRounded)
            .and_then(|d| NSFont::fontWithDescriptor_size(&d, 18.0))
            .unwrap_or(base)
    };
    let label_x = CAPSULE_PADDING_LEFT + WAVEFORM_WIDTH + GAP;
    let label = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(
            NSPoint::new(label_x, LABEL_Y),
            NSSize::new(TEXT_MIN_WIDTH, LABEL_HEIGHT),
        ),
    );
    label.setBordered(false);
    label.setBezeled(false);
    label.setEditable(false);
    label.setSelectable(false);
    label.setDrawsBackground(false);
    label.setTextColor(Some(&objc2_app_kit::NSColor::whiteColor()));
    label.setFont(Some(&label_font));
    label.setAlignment(NSTextAlignment::Left);
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    label.setMaximumNumberOfLines(1);
    label.setStringValue(&NSString::from_str(""));

    // Assemble
    effect_view.addSubview(&waveform_view);
    effect_view.addSubview(&label);
    panel.setContentView(Some(&effect_view));

    // Position at bottom center of main screen
    position_capsule(&panel, min_width, mtm);

    CapsuleOverlay {
        panel,
        waveform_view,
        text_label: label,
        current_width: Cell::new(min_width),
    }
}

fn position_capsule(panel: &NSPanel, width: f64, mtm: MainThreadMarker) {
    let screen_frame = NSScreen::mainScreen(mtm)
        .map(|s| s.frame())
        .unwrap_or(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(1920.0, 1080.0),
        ));
    let x = screen_frame.origin.x + (screen_frame.size.width - width) / 2.0;
    let y = screen_frame.origin.y + CAPSULE_BOTTOM_MARGIN;
    panel.setFrame_display(
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, CAPSULE_HEIGHT)),
        true,
    );
}

fn show_capsule(overlay: &CapsuleOverlay, mode: SessionMode, mtm: MainThreadMarker) {
    let panel = &overlay.panel;
    // Reset to minimum width
    let min_width =
        CAPSULE_PADDING_LEFT + WAVEFORM_WIDTH + GAP + TEXT_MIN_WIDTH + CAPSULE_PADDING_RIGHT;
    overlay.current_width.set(min_width);
    let hint = if mode == SessionMode::Correction {
        "Correcting..."
    } else {
        "Dictating..."
    };
    overlay.text_label.setStringValue(&NSString::from_str(hint));
    position_capsule(panel, min_width, mtm);

    // Resize effect view
    if let Some(cv) = panel.contentView() {
        cv.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(min_width, CAPSULE_HEIGHT),
        ));
    }
    overlay.text_label.setFrame(NSRect::new(
        NSPoint::new(CAPSULE_PADDING_LEFT + WAVEFORM_WIDTH + GAP, LABEL_Y),
        NSSize::new(TEXT_MIN_WIDTH, LABEL_HEIGHT),
    ));

    // Start invisible for spring entrance
    panel.setAlphaValue(0.0);
    panel.orderFront(None);

    // Animate entrance (0.35s spring-like)
    NSAnimationContext::runAnimationGroup(&block2::RcBlock::new(
        |ctx: std::ptr::NonNull<NSAnimationContext>| {
            let ctx = unsafe { ctx.as_ref() };
            ctx.setDuration(0.35);
            ctx.setAllowsImplicitAnimation(true);
            panel.setAlphaValue(1.0);
        },
    ));
}

fn hide_capsule(overlay: &CapsuleOverlay) {
    let panel = &overlay.panel;
    // Animate exit (0.22s fade + scale)
    let panel_clone = panel.clone();
    NSAnimationContext::runAnimationGroup_completionHandler(
        &block2::RcBlock::new(|ctx: std::ptr::NonNull<NSAnimationContext>| {
            let ctx = unsafe { ctx.as_ref() };
            ctx.setDuration(0.22);
            ctx.setAllowsImplicitAnimation(true);
            panel.setAlphaValue(0.0);
        }),
        Some(&block2::RcBlock::new(move || {
            panel_clone.orderOut(None);
            // Reset bar levels
            for bar in &BAR_LEVELS {
                bar.store(0, Ordering::Relaxed);
            }
        })),
    );
}

/// Update smooth bar levels from current audio RMS. Called each timer tick (~50ms).
fn update_bar_levels() {
    let raw_rms = f32::from_bits(AUDIO_RMS.load(Ordering::Relaxed));
    // Convert to dB scale: map [-60dB, 0dB] → [0.0, 1.0]
    // This makes quiet speech visible and loud speech not clipped.
    let db = if raw_rms > 1e-6 {
        20.0 * raw_rms.log10()
    } else {
        -60.0
    };
    let rms = ((db + 60.0) / 60.0).clamp(0.0, 1.0);

    // Simple PRNG for jitter (xorshift32)
    static JITTER_SEED: AtomicU32 = AtomicU32::new(12345);
    let mut seed = JITTER_SEED.load(Ordering::Relaxed);

    for i in 0..5 {
        let target = rms * BAR_WEIGHTS[i];

        // Add ±4% random jitter
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let jitter = ((seed % 200) as f32 / 100.0 - 1.0) * 0.04; // -0.04 to +0.04
        let target = (target + jitter * target).clamp(0.0, 1.0);

        let prev = f32::from_bits(BAR_LEVELS[i].load(Ordering::Relaxed));
        let smoothed = if target > prev {
            // Attack: 40% toward target
            prev + (target - prev) * 0.4
        } else {
            // Release: 15% toward target
            prev + (target - prev) * 0.15
        };
        BAR_LEVELS[i].store(smoothed.to_bits(), Ordering::Relaxed);
    }
    JITTER_SEED.store(seed, Ordering::Relaxed);
}

/// Update capsule text label and animate width change.
fn update_capsule_text(overlay: &CapsuleOverlay, text: &str, mtm: MainThreadMarker) {
    overlay.text_label.setStringValue(&NSString::from_str(text));
    overlay.text_label.sizeToFit();

    let fitted_w = overlay.text_label.frame().size.width;
    let text_w = fitted_w.clamp(TEXT_MIN_WIDTH, TEXT_MAX_WIDTH);
    let new_width = CAPSULE_PADDING_LEFT + WAVEFORM_WIDTH + GAP + text_w + CAPSULE_PADDING_RIGHT;
    let old_width = overlay.current_width.get();

    if (new_width - old_width).abs() > 2.0 {
        overlay.current_width.set(new_width);

        let panel = &overlay.panel;
        // Animate width change (0.25s)
        NSAnimationContext::runAnimationGroup(&block2::RcBlock::new(
            |ctx: std::ptr::NonNull<NSAnimationContext>| {
                let ctx = unsafe { ctx.as_ref() };
                ctx.setDuration(0.25);
                ctx.setAllowsImplicitAnimation(true);

                // Reposition panel centered
                position_capsule(panel, new_width, mtm);

                // Resize effect view
                if let Some(cv) = panel.contentView() {
                    cv.setFrame(NSRect::new(
                        NSPoint::new(0.0, 0.0),
                        NSSize::new(new_width, CAPSULE_HEIGHT),
                    ));
                }
            },
        ));

        // Update label frame (not animated, just ensure correct size)
        overlay.text_label.setFrame(NSRect::new(
            NSPoint::new(CAPSULE_PADDING_LEFT + WAVEFORM_WIDTH + GAP, LABEL_Y),
            NSSize::new(text_w, LABEL_HEIGHT),
        ));
    }
}

// --- Main ---

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("voice_correct=debug".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let use_sensevoice = args.engine == "sensevoice";

    #[cfg(not(feature = "sensevoice"))]
    if use_sensevoice {
        error!("sensevoice engine requires --features sensevoice");
        error!("cargo run --bin voice-correct --features sensevoice -- --engine sensevoice");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("must run on main thread");

    let app: Retained<NSApplication> = configure_accessory_app(mtm);

    if !accessibility::is_trusted() {
        warn!("accessibility not trusted — text read/write may fail");
    }
    if std::env::var("KIMI_API_KEY").is_err() {
        warn!("KIMI_API_KEY not set — correction mode will fail");
    }

    // --- SenseVoice recognizer (if needed) ---
    #[cfg(feature = "sensevoice")]
    let sv_recognizer: Cell<Option<sherpa_rs::sense_voice::SenseVoiceRecognizer>> =
        if use_sensevoice {
            let home = std::env::var("HOME").unwrap();
            let base = Path::new(&home).join(".local/share/picc");
            let paths = resolve_repo_sensevoice_paths(
                &base,
                args.model_dir.as_deref(),
                &args.model_file_name,
            )
            .expect("failed to resolve SenseVoice model paths");
            let model_path = paths.model;
            let tokens_path = paths.tokens.expect("SenseVoice tokens path missing");
            let config = sherpa_rs::sense_voice::SenseVoiceConfig {
                model: model_path.clone(),
                tokens: tokens_path.clone(),
                language: args.lang.clone(),
                use_itn: true,
                ..Default::default()
            };
            info!(model = %model_path, tokens = %tokens_path, "loading SenseVoice model");
            let recognizer = sherpa_rs::sense_voice::SenseVoiceRecognizer::new(config)
                .expect("failed to init SenseVoice — check --model-dir/--model-file-name");
            info!("SenseVoice model loaded");
            Cell::new(Some(recognizer))
        } else {
            Cell::new(None)
        };

    // --- Apple speech recognizer (if needed) ---
    let apple_recognizer = if !use_sensevoice {
        let recognizer = unsafe {
            let locale = NSLocale::initWithLocaleIdentifier(NSLocale::alloc(), ns_string!("zh-CN"));
            SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale).unwrap()
        };
        unsafe {
            let handler = RcBlock::new(|status: SFSpeechRecognizerAuthorizationStatus| {
                if status == SFSpeechRecognizerAuthorizationStatus::Authorized {
                    info!("speech recognition authorized");
                } else {
                    warn!(?status, "speech recognition not authorized");
                }
            });
            SFSpeechRecognizer::requestAuthorization(&handler);
        }
        Some(recognizer)
    } else {
        None
    };

    let audio_engine = AudioEngineManager::new();

    // --- CGEventTap ---
    let event_mask: CGEventMask = 1 << CGEventType::FlagsChanged.0;
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            event_mask,
            Some(event_tap_callback),
            ptr::null_mut(),
        )
    }
    .expect("failed to create event tap — grant Accessibility permission");

    // Store tap pointer so the callback can re-enable it on timeout.
    EVENT_TAP_PTR.store(
        &*tap as *const CFMachPort as *mut std::ffi::c_void,
        Ordering::Relaxed,
    );

    let run_loop_source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        .expect("failed to create run loop source");
    unsafe {
        let run_loop = CFRunLoop::current().expect("no current run loop");
        run_loop.add_source(Some(&run_loop_source), kCFRunLoopCommonModes);
    }

    // --- Menubar ---
    let status_item: Retained<NSStatusItem> = new_status_item(-1.0);
    set_status_icon(&status_item, AppState::Idle, mtm);

    let delegate: Retained<MenuDelegate> = {
        let this = MenuDelegate::alloc(mtm).set_ivars(MenuDelegateIvars {
            options: Cell::new(DictationOptions::default()),
            punct_spaces_item: RefCell::new(None),
        });
        unsafe { objc2::msg_send![super(this), init] }
    };
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let quit_item = new_menu_item(mtm, "Quit", Some(sel!(quit:)), "q");
    unsafe { quit_item.setTarget(Some(&delegate)) };

    // Toggle: fullwidth → halfwidth
    let fw_item = new_menu_item(
        mtm,
        "Fullwidth to Halfwidth",
        Some(sel!(toggleFullwidthToHalfwidth:)),
        "",
    );
    unsafe { fw_item.setTarget(Some(&delegate)) };

    // Toggle: space around punctuation (sub-option of fullwidth→halfwidth)
    let punct_spaces_item = new_menu_item(
        mtm,
        "  Space Around Punctuation",
        Some(sel!(toggleSpaceAroundPunct:)),
        "",
    );
    unsafe { punct_spaces_item.setTarget(Some(&delegate)) };
    punct_spaces_item.setEnabled(false); // disabled until fullwidth is turned on
    *delegate.ivars().punct_spaces_item.borrow_mut() = Some(punct_spaces_item.clone());

    // Toggle: space between CJK & Latin/Digit (independent)
    let cjk_spaces_item = new_menu_item(
        mtm,
        "Space Between CJK & Latin",
        Some(sel!(toggleSpaceBetweenCjk:)),
        "",
    );
    unsafe { cjk_spaces_item.setTarget(Some(&delegate)) };

    // Toggle: strip trailing punctuation
    let strip_item = new_menu_item(
        mtm,
        "Strip Trailing Punctuation",
        Some(sel!(toggleStripTrailingPunct:)),
        "",
    );
    unsafe { strip_item.setTarget(Some(&delegate)) };

    menu.addItem(&fw_item);
    menu.addItem(&punct_spaces_item);
    menu.addItem(&cjk_spaces_item);
    menu.addItem(&strip_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&quit_item);
    status_item.setMenu(Some(&menu));

    let engine_name = if use_sensevoice {
        "SenseVoice"
    } else {
        "Apple Speech"
    };
    info!(
        engine = engine_name,
        "ready — hold right Cmd: dictate | tap+hold: correct"
    );

    // --- State ---
    let is_recording = Cell::new(false);
    let is_processing = Cell::new(false);
    let processing_tick: Cell<u32> = Cell::new(0);
    let current_mode: Cell<SessionMode> = Cell::new(SessionMode::None);
    // Original text saved when LLM correction is dispatched
    let correction_original: Cell<Option<String>> = Cell::new(None);
    let apple_request: Cell<Option<Retained<SFSpeechAudioBufferRecognitionRequest>>> =
        Cell::new(None);
    let accumulated_samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let native_sample_rate: Cell<u32> = Cell::new(16000);

    let _tap = tap;
    let _run_loop_source = run_loop_source;

    // --- Floating capsule overlay ---
    let capsule = create_capsule_overlay(mtm);

    // --- Timer (50ms polling) ---
    let _timer = unsafe {
        let samples_ref = accumulated_samples.clone();
        let delegate = delegate.clone();

        // Counter for recording heartbeat (logs every ~2s)
        let heartbeat_tick: Cell<u32> = Cell::new(0);
        // Cooldown ticks after audio device change (wait for HAL to settle)
        let config_change_cooldown: Cell<u32> = Cell::new(0);
        // Retry counter for engine start failures
        let start_retry_count: Cell<u32> = Cell::new(0);
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            // --- Heartbeat: log state every ~2s while recording ---
            if is_recording.get() {
                let tick = heartbeat_tick.get() + 1;
                heartbeat_tick.set(tick);
                if tick % 40 == 0 {
                    let elapsed_s = tick as f64 * 0.05;
                    let should_stop = SESSION_SIGNALS.stop_pending();
                    let should_cancel = SESSION_SIGNALS.cancel_pending();
                    let mode = current_mode.get();
                    let mode_name = if mode == SessionMode::Correction {
                        "correct"
                    } else {
                        "dictate"
                    };
                    debug!(
                        elapsed_s = format_args!("{:.1}", elapsed_s),
                        mode = mode_name,
                        should_stop,
                        should_cancel,
                        "HEARTBEAT: still recording"
                    );
                }

                // Update waveform bars and redraw
                update_bar_levels();
                capsule.waveform_view.setNeedsDisplay(true);

                // Update partial transcription text (Apple Speech path)
                if !use_sensevoice {
                    if let Ok(text) = RECOGNIZED_TEXT.lock() {
                        if !text.is_empty() {
                            update_capsule_text(&capsule, &text, mtm);
                        }
                    }
                }
            } else {
                heartbeat_tick.set(0);
            }

            // --- Animate processing icon (cycle every ~250ms = 5 ticks) ---
            if is_processing.get() {
                let tick = processing_tick.get() + 1;
                processing_tick.set(tick);
                if tick % 5 == 0 {
                    let frame = (tick / 5) as usize % PROCESSING_ICONS.len();
                    set_status_icon_name(&status_item, PROCESSING_ICONS[frame], mtm);

                    // Animate capsule text dots
                    let dots = match frame % 3 {
                        0 => "Correcting.",
                        1 => "Correcting..",
                        _ => "Correcting...",
                    };
                    update_capsule_text(&capsule, dots, mtm);
                }

                // Check if LLM result has arrived
                if let Ok(mut r) = LLM_RESULT.try_lock() {
                    if let Some(result) = r.take() {
                        is_processing.set(false);
                        hide_capsule(&capsule);
                        let original = correction_original.take().unwrap_or_default();
                        match result {
                            Ok(corrected) if corrected != original => {
                                // Apply dictation transforms to LLM output (e.g. fullwidth→halfwidth)
                                // so corrected text matches user's punctuation preferences.
                                let corrected = apply_dictation_transforms(
                                    &corrected,
                                    delegate.ivars().options.get(),
                                );
                                // Re-read focused element for writing
                                if let Some(FocusedText { element, .. }) = read_focused_text() {
                                    write_corrected_text(&element, &corrected, &original);
                                } else {
                                    warn!("focused field disappeared before correction could be applied");
                                    play_error_sound();
                                }
                            }
                            Ok(_) => {
                                info!("no change needed");
                            }
                            Err(e) => {
                                error!(err = %e, "LLM error");
                                play_error_sound();
                            }
                        }
                        set_status_icon(&status_item, AppState::Idle, mtm);
                    }
                }
            }

            // --- CANCEL recording (short tap) ---
            if take_cancel_while_recording(&SESSION_SIGNALS, &is_recording) {
                debug!(
                    is_recording = is_recording.get(),
                    is_processing = is_processing.get(),
                    "TIMER → CANCEL: stopping recording"
                );
                if use_sensevoice {
                    COLLECTING.store(false, Ordering::Relaxed);
                    audio_engine.stop();
                    if let Ok(mut s) = samples_ref.lock() {
                        s.clear();
                    }
                    debug!("TIMER → CANCEL: engine stopped");
                } else {
                    audio_engine.stop_and_reset();
                    debug!("TIMER → CANCEL: engine stopped and reset");
                    if let Some(req) = apple_request.take() {
                        req.endAudio();
                    }
                    if let Ok(mut t) = RECOGNIZED_TEXT.lock() {
                        t.clear();
                    }
                }
                set_status_icon(&status_item, AppState::Idle, mtm);
                hide_capsule(&capsule);
            }

            // --- START recording ---
            if let Some(mode) = begin_requested_session(&SESSION_SIGNALS, &is_recording) {
                current_mode.set(mode);

                let mode_name = if mode == SessionMode::Correction {
                    "correct"
                } else {
                    "dictate"
                };
                debug!(
                    mode = mode_name,
                    is_processing = is_processing.get(),
                    "TIMER → START: beginning recording"
                );

                let icon = if mode == SessionMode::Correction {
                    AppState::Correcting
                } else {
                    AppState::Recording
                };
                set_status_icon(&status_item, icon, mtm);
                play_start_sound();
                AUDIO_RMS.store(0, Ordering::Relaxed);
                show_capsule(&capsule, mode, mtm);
                debug!("TIMER → START [1]: icon set, sound played, capsule shown");

                if use_sensevoice {
                    if audio_engine.take_config_changed() {
                        info!("TIMER → START: audio config changed, recreating engine");
                        audio_engine.recreate_engine();
                        config_change_cooldown.set(20);
                    }
                    if config_change_cooldown.get() > 0 {
                        let remaining = config_change_cooldown.get() - 1;
                        config_change_cooldown.set(remaining);
                        if remaining % 5 == 0 {
                            debug!(remaining, "TIMER → START: cooldown tick");
                        }
                        clear_recording_state(&SESSION_SIGNALS, &is_recording);
                        SESSION_SIGNALS.request_start(mode);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }

                    match samples_ref.try_lock() {
                        Ok(mut s) => s.clear(),
                        Err(_) => {
                            warn!("TIMER → START: samples lock contention, retry");
                            clear_recording_state(&SESSION_SIGNALS, &is_recording);
                            SESSION_SIGNALS.request_start(mode);
                            set_status_icon(&status_item, AppState::Idle, mtm);
                            return;
                        }
                    }

                    if let Err(e) = audio_engine.start_sample_capture(
                        samples_ref.clone(),
                        &native_sample_rate,
                        AudioCaptureConfig {
                            buffer_size: 4096,
                            use_none_format: true,
                            collect_gate: Some(&COLLECTING),
                            rms_out: Some(&AUDIO_RMS),
                        },
                    ) {
                        error!(error = %e, "TIMER → START: engine start failed, will retry with new engine");
                        let start_retries = start_retry_count.get() + 1;
                        start_retry_count.set(start_retries);
                        if start_retries >= 3 {
                            error!("TIMER → START: giving up after {} retries", start_retries);
                            start_retry_count.set(0);
                            clear_recording_state(&SESSION_SIGNALS, &is_recording);
                            set_status_icon(&status_item, AppState::Idle, mtm);
                            play_error_sound();
                            return;
                        }
                        // Recreate engine and try again with cooldown
                        info!(
                            retry = start_retries,
                            "TIMER → START: recreating engine for retry"
                        );
                        audio_engine.recreate_engine();
                        config_change_cooldown.set(20); // 1s cooldown before retry
                        clear_recording_state(&SESSION_SIGNALS, &is_recording);
                        SESSION_SIGNALS.request_start(mode);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }
                    start_retry_count.set(0);

                    COLLECTING.store(true, Ordering::Relaxed);
                } else {
                    if audio_engine.take_config_changed() {
                        info!(
                            "TIMER → START: audio config changed, recreating engine (apple speech)"
                        );
                        audio_engine.recreate_engine();
                    }
                    if let Ok(mut text) = RECOGNIZED_TEXT.lock() {
                        text.clear();
                    }

                    let req = SFSpeechAudioBufferRecognitionRequest::new();
                    if let Err(e) = audio_engine.start_request_capture(
                        req.clone(),
                        AudioCaptureConfig {
                            buffer_size: 1024,
                            use_none_format: false,
                            collect_gate: None,
                            rms_out: Some(&AUDIO_RMS),
                        },
                    ) {
                        error!(error = %e, "TIMER → START: audio engine failed");
                        clear_recording_state(&SESSION_SIGNALS, &is_recording);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        play_error_sound();
                        return;
                    }

                    let handler = RcBlock::new(
                        |result: *mut SFSpeechRecognitionResult,
                         error: *mut objc2_foundation::NSError| {
                            if !error.is_null() {
                                let error = &*error;
                                warn!("recognition error: {:?}", error.localizedDescription());
                            } else if !result.is_null() {
                                let result = &*result;
                                let text = result.bestTranscription().formattedString().to_string();
                                debug!(text = %text, "partial");
                                if let Ok(mut stored) = RECOGNIZED_TEXT.lock() {
                                    *stored = text;
                                }
                            }
                        },
                    );

                    if let Some(ref recognizer) = apple_recognizer {
                        let _task =
                            recognizer.recognitionTaskWithRequest_resultHandler(&req, &*handler);
                    }
                    apple_request.set(Some(req));
                }

                debug!("TIMER → START: done");
            }

            // --- STOP recording ---
            if take_stop_while_recording(&SESSION_SIGNALS, &is_recording) {
                let mode = current_mode.get();
                let mode_name = if mode == SessionMode::Correction {
                    "correct"
                } else {
                    "dictate"
                };
                debug!(
                    mode = mode_name,
                    is_processing = is_processing.get(),
                    "TIMER → STOP: stopping recording"
                );
                // Dictation: hide capsule immediately.
                // Correction: keep capsule visible — will show "Correcting..." during LLM processing.
                if mode == SessionMode::Dictation {
                    hide_capsule(&capsule);
                } else {
                    // Stop waveform animation, show processing hint
                    for bar in &BAR_LEVELS {
                        bar.store(0, Ordering::Relaxed);
                    }
                    capsule.waveform_view.setNeedsDisplay(true);
                    update_capsule_text(&capsule, "Correcting...", mtm);
                }

                let spoken: String;

                if use_sensevoice {
                    COLLECTING.store(false, Ordering::Relaxed);
                    audio_engine.stop();
                    debug!("TIMER → STOP: engine stopped");
                    play_stop_sound();

                    #[cfg(feature = "sensevoice")]
                    {
                        let raw_samples = loop {
                            match samples_ref.try_lock() {
                                Ok(guard) => break guard,
                                Err(_) => std::thread::sleep(Duration::from_millis(1)),
                            }
                        };
                        if raw_samples.is_empty() {
                            warn!("no audio captured");
                            set_status_icon(&status_item, AppState::Idle, mtm);
                            return;
                        }
                        let sr = native_sample_rate.get();
                        let samples_16k = resample_linear(&raw_samples, sr, 16000);
                        drop(raw_samples);
                        info!(
                            duration_s = format_args!("{:.1}", samples_16k.len() as f64 / 16000.0),
                            "transcribing audio"
                        );
                        if let Some(mut recognizer) = sv_recognizer.take() {
                            let t0 = std::time::Instant::now();
                            let result = recognizer.transcribe(16000, &samples_16k);
                            let ms = t0.elapsed().as_secs_f64() * 1000.0;
                            info!(
                                text = %result.text,
                                lang = %result.lang,
                                token_count = result.tokens.len(),
                                timestamp_count = result.timestamps.len(),
                                elapsed_ms = format_args!("{:.0}", ms),
                                "ASR result"
                            );
                            debug!(
                                tokens = ?result.tokens,
                                timestamps = ?result.timestamps,
                                "ASR metadata"
                            );
                            spoken = result.text.clone();
                            sv_recognizer.set(Some(recognizer));
                        } else {
                            spoken = String::new();
                        }
                    }
                    #[cfg(not(feature = "sensevoice"))]
                    {
                        spoken = String::new();
                    }
                } else {
                    // Apple Speech path
                    audio_engine.stop_and_reset();
                    play_stop_sound();

                    if let Some(req) = apple_request.take() {
                        req.endAudio();
                    }
                    NSRunLoop::currentRunLoop()
                        .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.5));
                    spoken = RECOGNIZED_TEXT
                        .lock()
                        .map(|t| t.clone())
                        .unwrap_or_default();
                    if let Ok(mut t) = RECOGNIZED_TEXT.lock() {
                        t.clear();
                    }
                }

                debug!(spoken = %spoken, "TIMER → STOP: transcription done");

                // Empty speech → skip entirely
                if spoken.is_empty() {
                    debug!("TIMER → STOP: no speech recognized, resetting to idle");
                    hide_capsule(&capsule);
                    set_status_icon(&status_item, AppState::Idle, mtm);
                    return;
                }

                if mode == SessionMode::Dictation {
                    // --- Dictation mode: type spoken text at cursor ---
                    //
                    // Text processing pipeline:
                    // 1. apply_dictation_transforms: fullwidth→halfwidth, space around
                    //    punctuation, space between CJK & Latin, strip trailing punct
                    // 2. Context-aware space: if space_around_punct is on and the
                    //    character before cursor is halfwidth punctuation (e.g. ","),
                    //    prepend a space so "hello," + "world" → "hello, world"
                    // 3. type_text: simulate keyboard input at cursor position
                    let opts = delegate.ivars().options.get();
                    let mut spoken = apply_dictation_transforms(&spoken, opts);
                    if opts.fullwidth_to_halfwidth
                        && opts.space_around_punct
                        && !spoken.starts_with(' ')
                    {
                        if let Some(prev) = char_before_cursor() {
                            if SPACE_AFTER_PUNCT.contains(&prev) {
                                spoken.insert(0, ' ');
                            }
                        }
                    }
                    info!(text = %spoken, "typing");
                    type_text(&spoken);
                    set_status_icon(&status_item, AppState::Idle, mtm);
                } else {
                    // --- Correction mode: LLM corrects existing text ---
                    //
                    // Flow:
                    // 1. Read focused text field via AX API
                    // 2. Send (original_text, voice_instruction) to LLM
                    // 3. When LLM result arrives (checked in timer's is_processing block):
                    //    a. apply_dictation_transforms on LLM output (fullwidth→halfwidth etc.)
                    //    b. Write corrected text back via AX API or clipboard fallback
                    //
                    // Fallbacks that skip LLM and type spoken text directly:
                    // - Terminal apps (AXValue returns entire buffer)
                    // - Text too long (>250 chars)
                    // - Empty text field (nothing to correct)
                    //
                    // Skip if a previous LLM call is still in flight
                    if is_processing.get() {
                        warn!("still processing previous correction, skipping");
                        hide_capsule(&capsule);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }

                    let app_name = frontmost_bundle_id().unwrap_or_else(|| "unknown".to_string());
                    info!(app = %app_name, "correction mode");

                    // Terminals: AXValue returns entire buffer, skip reading
                    if should_skip_ax_read_for_bundle(Some(&app_name)) {
                        info!(text = %spoken, "terminal app, typing instead");
                        hide_capsule(&capsule);
                        type_text(&spoken);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }

                    match read_focused_text() {
                        Some(FocusedText {
                            text: original_text,
                            ..
                        }) => {
                            // Limit text length to avoid sending huge payloads to LLM
                            let trimmed = original_text.trim();
                            if trimmed.chars().count() > 250 {
                                warn!(
                                    len = trimmed.chars().count(),
                                    "text too long (>250 chars), typing instead"
                                );
                                hide_capsule(&capsule);
                                type_text(&spoken);
                                set_status_icon(&status_item, AppState::Idle, mtm);
                                return;
                            }
                            info!(original = %trimmed, instruction = %spoken, "correcting");
                            // Save original for comparison when result arrives
                            correction_original.set(Some(original_text.clone()));

                            // Start animated processing icon
                            set_status_icon(&status_item, AppState::Processing, mtm);
                            is_processing.set(true);
                            processing_tick.set(0);

                            // Dispatch LLM call to background thread
                            let orig = original_text.clone();
                            let instr = spoken.clone();
                            std::thread::spawn(move || {
                                let t0 = std::time::Instant::now();
                                let result = call_llm(&orig, &instr);
                                let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
                                match &result {
                                    Ok(text) => {
                                        info!(text = %text, elapsed_ms = format_args!("{:.0}", elapsed), "LLM result")
                                    }
                                    Err(e) => {
                                        error!(err = %e, elapsed_ms = format_args!("{:.0}", elapsed), "LLM error")
                                    }
                                }
                                if let Ok(mut r) = LLM_RESULT.lock() {
                                    *r = Some(result);
                                }
                            });
                        }
                        None => {
                            // Empty field → fall back to typing (no LLM call)
                            info!(text = %spoken, "field empty, typing instead");
                            hide_capsule(&capsule);
                            type_text(&spoken);
                            set_status_icon(&status_item, AppState::Idle, mtm);
                        }
                    }
                }
            }
        });

        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &block)
    };

    app.run();
}

#[cfg(test)]
mod tests {
    use super::{apply_dictation_transforms, DictationOptions};
    use picc_speech::postprocess::auto_insert_spaces;

    /// Helper: both punct and cjk spacing enabled (the common case).
    fn spaced(s: &str) -> String {
        auto_insert_spaces(s, true, true)
    }

    // --- CJK ↔ Latin/Digit ---

    #[test]
    fn cjk_latin_spacing() {
        assert_eq!(auto_insert_spaces("中文abc", false, true), "中文 abc");
        assert_eq!(auto_insert_spaces("abc中文", false, true), "abc 中文");
        assert_eq!(
            auto_insert_spaces("中文abc中文", false, true),
            "中文 abc 中文"
        );
    }

    #[test]
    fn cjk_digit_spacing() {
        assert_eq!(auto_insert_spaces("第3章", false, true), "第 3 章");
        assert_eq!(auto_insert_spaces("100个", false, true), "100 个");
    }

    #[test]
    fn cjk_only_off_no_effect() {
        // With cjk=false, CJK↔Latin boundaries are not spaced
        assert_eq!(auto_insert_spaces("中文abc", true, false), "中文abc");
    }

    // --- Punctuation ---

    #[test]
    fn delimiter_spacing() {
        assert_eq!(
            auto_insert_spaces("hello,world", true, false),
            "hello, world"
        );
        assert_eq!(auto_insert_spaces("a.b", true, false), "a. b");
        assert_eq!(auto_insert_spaces("ok!nice", true, false), "ok! nice");
    }

    #[test]
    fn decimal_point_no_space() {
        assert_eq!(spaced("3.14"), "3.14");
        assert_eq!(spaced("价格是9.99元"), "价格是 9.99 元");
    }

    #[test]
    fn bracket_spacing() {
        assert_eq!(
            auto_insert_spaces("hello(world)test", true, false),
            "hello (world) test"
        );
        assert_eq!(spaced("你好(世界)"), "你好 (世界)");
    }

    #[test]
    fn consecutive_punctuation() {
        assert_eq!(spaced("what?!ok"), "what?! ok");
        assert_eq!(spaced("a...b"), "a... b");
    }

    #[test]
    fn punct_only_off_no_effect() {
        // With punct=false, delimiter spacing is not applied
        assert_eq!(
            auto_insert_spaces("hello,world", false, true),
            "hello,world"
        );
    }

    // --- Both flags ---

    #[test]
    fn already_spaced_idempotent() {
        assert_eq!(spaced("中文 abc 中文"), "中文 abc 中文");
        assert_eq!(spaced("hello, world"), "hello, world");
    }

    #[test]
    fn no_spurious_spaces() {
        assert_eq!(spaced("你好世界"), "你好世界");
        assert_eq!(spaced("hello world"), "hello world");
        assert_eq!(spaced("12345"), "12345");
    }

    #[test]
    fn mixed_complex() {
        assert_eq!(spaced("用Vue3和React开发"), "用 Vue3 和 React 开发");
        assert_eq!(spaced("这是v2.0版本"), "这是 v2.0 版本");
    }

    // --- Integration: apply_dictation_transforms ---

    #[test]
    fn transforms_fullwidth_then_punct_spaces() {
        let opts = DictationOptions {
            fullwidth_to_halfwidth: true,
            space_around_punct: true,
            space_between_cjk: false,
            strip_trailing_punct: false,
        };
        // ，(fullwidth) → , (halfwidth) → ", " (space after)
        assert_eq!(apply_dictation_transforms("你好，世界", opts), "你好, 世界");
    }

    #[test]
    fn transforms_cjk_spacing_independent() {
        let opts = DictationOptions {
            fullwidth_to_halfwidth: false,
            space_around_punct: false,
            space_between_cjk: true,
            strip_trailing_punct: false,
        };
        assert_eq!(
            apply_dictation_transforms("中文abc中文", opts),
            "中文 abc 中文"
        );
    }
}
