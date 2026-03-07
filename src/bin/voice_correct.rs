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

use std::cell::Cell;
#[cfg(feature = "sensevoice")]
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem,
};
use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFString, CFType, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{ns_string, NSDate, NSLocale, NSRunLoop, NSString, NSTimer};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus,
};

use objc2_application_services::AXUIElement;
use picc::accessibility;
use picc::input::{parse_key_combo, press_key_combo, type_text};

use serde::{Deserialize, Serialize};

// --- CLI ---

#[derive(Parser)]
#[command(about = "Voice Correct — hold right Cmd to dictate, tap+hold to correct")]
struct Args {
    /// Speech engine: "sensevoice" or "apple"
    #[arg(long, default_value = "apple")]
    engine: String,

    /// SenseVoice model directory (required for sensevoice engine)
    #[arg(long)]
    model_dir: Option<String>,

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

const MODE_DICTATION: u8 = 1;
const MODE_CORRECT: u8 = 2;

// --- Event tap state ---

const NX_DEVICERCMDKEYMASK: u64 = 0x10;

static SHOULD_START: AtomicBool = AtomicBool::new(false);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
/// Cancel current recording without processing (short tap)
static SHOULD_CANCEL: AtomicBool = AtomicBool::new(false);
static IS_RECORDING: AtomicBool = AtomicBool::new(false);
/// When the current press happened (ms)
static PRESS_MS: AtomicU64 = AtomicU64::new(0);
/// Last short-tap release timestamp (ms) — for double-tap detection
static LAST_TAP_RELEASE_MS: AtomicU64 = AtomicU64::new(0);
/// Current session mode: 0=none, 1=dictation, 2=correct
static SESSION_MODE: AtomicU8 = AtomicU8::new(0);
static RECOGNIZED_TEXT: Mutex<String> = Mutex::new(String::new());

/// LLM result from background thread: Some(Ok(corrected)) or Some(Err(msg))
static LLM_RESULT: Mutex<Option<Result<String, String>>> = Mutex::new(None);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    if event_type == CGEventType::FlagsChanged {
        let flags = CGEvent::flags(Some(event.as_ref()));
        let device_flags = flags.0 & 0xFFFF;
        let right_cmd_pressed = (device_flags & NX_DEVICERCMDKEYMASK) != 0;

        static WAS_DOWN: AtomicBool = AtomicBool::new(false);
        let was_down = WAS_DOWN.load(Ordering::Relaxed);

        if right_cmd_pressed && !was_down {
            // Press → always start recording immediately
            WAS_DOWN.store(true, Ordering::Relaxed);
            let now = now_ms();
            PRESS_MS.store(now, Ordering::Relaxed);

            if !IS_RECORDING.load(Ordering::Relaxed) {
                let last_tap = LAST_TAP_RELEASE_MS.load(Ordering::Relaxed);
                if (now - last_tap) < 300 {
                    // Press shortly after a tap → double-tap → correction mode
                    SESSION_MODE.store(MODE_CORRECT, Ordering::Relaxed);
                } else {
                    // Normal press → dictation mode
                    SESSION_MODE.store(MODE_DICTATION, Ordering::Relaxed);
                }
                SHOULD_START.store(true, Ordering::Relaxed);
            }
        } else if !right_cmd_pressed && was_down {
            // Release
            WAS_DOWN.store(false, Ordering::Relaxed);
            let now = now_ms();
            let hold = now - PRESS_MS.load(Ordering::Relaxed);

            // Short tap → always record as tap for double-tap detection,
            // regardless of whether timer has started recording yet.
            if hold < 300 {
                LAST_TAP_RELEASE_MS.store(now, Ordering::Relaxed);
            }

            if IS_RECORDING.load(Ordering::Relaxed) {
                if hold < 300 && SESSION_MODE.load(Ordering::Relaxed) == MODE_DICTATION {
                    // Short tap in dictation mode → cancel recording
                    SHOULD_CANCEL.store(true, Ordering::Relaxed);
                } else {
                    // Long hold or correction mode → stop and process
                    SHOULD_STOP.store(true, Ordering::Relaxed);
                }
            } else if hold < 300 {
                // Timer hasn't started recording yet → cancel pending start
                SHOULD_START.store(false, Ordering::Relaxed);
            }
        }
    }
    event.as_ptr()
}

// --- SenseVoice model download (same as dictation.rs) ---

#[cfg(feature = "sensevoice")]
const SENSEVOICE_MODEL_DIR: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17";
#[cfg(feature = "sensevoice")]
const SENSEVOICE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2";

#[cfg(feature = "sensevoice")]
fn ensure_sensevoice_model(base_dir: &Path) -> String {
    use std::io::{Read as _, Write as _};

    let model_dir = base_dir.join(SENSEVOICE_MODEL_DIR);
    let model_file = model_dir.join("model.int8.onnx");
    if model_file.exists() {
        return model_dir.to_string_lossy().into_owned();
    }

    eprintln!(
        "[voice-correct] SenseVoice model not found at {}",
        model_dir.display()
    );
    eprintln!("[voice-correct] first run — downloading model (~250 MB), this may take a few minutes...");
    std::fs::create_dir_all(base_dir).expect("failed to create model directory");

    let archive = base_dir.join("sensevoice.tar.bz2");
    let resp = reqwest::blocking::Client::new()
        .get(SENSEVOICE_URL)
        .send()
        .expect("failed to download model — check your network connection");
    let total = resp.content_length().unwrap_or(0);
    let total_mb = total as f64 / 1_048_576.0;
    let mut downloaded: u64 = 0;
    let mut file = std::fs::File::create(&archive).expect("failed to create archive file");
    let mut reader = resp;
    let mut buf = [0u8; 65536];
    let start = std::time::Instant::now();
    loop {
        let n = reader
            .read(&mut buf)
            .expect("download error — check your network connection");
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).expect("write error");
        downloaded += n as u64;
        if total > 0 {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
            let mb = downloaded as f64 / 1_048_576.0;
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { mb / elapsed } else { 0.0 };
            let bar_len = 30;
            let filled = (bar_len as f64 * downloaded as f64 / total as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_len - filled);
            eprint!("\r[voice-correct] {bar} {mb:.1}/{total_mb:.1} MB ({pct}%) {speed:.1} MB/s");
        }
    }
    eprintln!();
    drop(file);

    eprint!("[voice-correct] extracting...");
    let status = std::process::Command::new("tar")
        .args([
            "xjf",
            &archive.to_string_lossy(),
            "-C",
            &base_dir.to_string_lossy(),
        ])
        .status()
        .expect("failed to run tar");
    assert!(status.success(), "tar extraction failed");
    std::fs::remove_file(&archive).ok();
    eprintln!(" done");

    model_dir.to_string_lossy().into_owned()
}

// --- Menubar delegate ---

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VoiceCorrectMenuDelegate"]
    #[derive(Debug, PartialEq)]
    struct MenuDelegate;

    #[allow(non_snake_case)]
    impl MenuDelegate {
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            std::process::exit(0);
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

// --- AX: read focused element + text ---

fn read_focused_text() -> Option<(CFRetained<AXUIElement>, String)> {
    let system = unsafe { AXUIElement::new_system_wide() };
    let focused_cf = accessibility::attr_value(&system, "AXFocusedUIElement")?;
    let focused: CFRetained<AXUIElement> = unsafe { CFRetained::cast_unchecked(focused_cf) };
    let text = accessibility::attr_string(&focused, "AXValue")?;
    if text.is_empty() {
        return None;
    }
    // Skip if text matches placeholder
    if let Some(placeholder) = accessibility::attr_string(&focused, "AXPlaceholderValue") {
        if text == placeholder {
            return None;
        }
    }
    Some((focused, text))
}

// --- AX: write corrected text ---

/// Bundle IDs of apps where AXValue set doesn't work (browsers, Electron apps).
/// For these, always use clipboard paste fallback.
const CLIPBOARD_ONLY_BUNDLES: &[&str] = &[
    "com.google.Chrome",
    "org.chromium.Chromium",
    "com.apple.Safari",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "com.electron.",           // prefix match for Electron apps
    "us.zoom.xos",
    "com.larksuite.Lark",
    "com.larksuite.larkApp",
    "com.bytedance.lark.Feishu",
];

/// Get the frontmost app's bundle identifier.
fn frontmost_bundle_id() -> Option<String> {
    use objc2_app_kit::NSRunningApplication;
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace")?;
    let workspace: Retained<NSObject> = unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let app: Option<Retained<NSRunningApplication>> =
        unsafe { objc2::msg_send![&*workspace, frontmostApplication] };
    app.and_then(|a| a.bundleIdentifier().map(|b| b.to_string()))
}

/// Check if the frontmost app should use clipboard-only strategy.
fn should_use_clipboard() -> bool {
    if let Some(bundle) = frontmost_bundle_id() {
        CLIPBOARD_ONLY_BUNDLES.iter().any(|b| {
            if b.ends_with('.') {
                bundle.starts_with(b)
            } else {
                bundle == *b
            }
        })
    } else {
        false
    }
}

/// Replace text in the focused field.
/// For browsers/Electron/Lark: always clipboard paste.
/// For native apps: try AXValue set first, fall back to clipboard.
fn write_corrected_text(element: &AXUIElement, text: &str) -> bool {
    // Browsers & Electron: skip AX, go straight to clipboard
    if should_use_clipboard() {
        eprintln!("[voice-correct] using clipboard paste for this app");
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
        eprintln!("[voice-correct] AXValue set didn't take effect, using clipboard fallback");
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

const SYSTEM_PROMPT: &str = "你是一个文本纠错助手。用户会提供一段已输入的文本，以及一条语音纠错指令。\n\
请根据指令修改文本，只输出修改后的完整文本，不要添加任何解释。";

fn call_llm(original_text: &str, instruction: &str) -> Result<String, String> {
    let api_key =
        std::env::var("KIMI_API_KEY").map_err(|_| "KIMI_API_KEY not set".to_string())?;

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

// --- Main ---

fn main() {
    let args = Args::parse();
    let use_sensevoice = args.engine == "sensevoice";

    #[cfg(not(feature = "sensevoice"))]
    if use_sensevoice {
        eprintln!("[voice-correct] ERROR: sensevoice engine requires --features sensevoice");
        eprintln!("[voice-correct] cargo run --bin voice-correct --features sensevoice -- --engine sensevoice");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("must run on main thread");

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    if !accessibility::is_trusted() {
        eprintln!("[voice-correct] WARNING: accessibility not trusted — text read/write may fail");
    }
    if std::env::var("KIMI_API_KEY").is_err() {
        eprintln!("[voice-correct] WARNING: KIMI_API_KEY not set — correction mode will fail");
    }

    // --- SenseVoice recognizer (if needed) ---
    #[cfg(feature = "sensevoice")]
    let sv_recognizer: Cell<Option<sherpa_rs::sense_voice::SenseVoiceRecognizer>> =
        if use_sensevoice {
            let model_dir = args.model_dir.unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap();
                let base = Path::new(&home).join(".local/share/picc");
                ensure_sensevoice_model(&base)
            });
            let config = sherpa_rs::sense_voice::SenseVoiceConfig {
                model: format!("{model_dir}/model.int8.onnx"),
                tokens: format!("{model_dir}/tokens.txt"),
                language: args.lang.clone(),
                use_itn: true,
                ..Default::default()
            };
            eprintln!("[voice-correct] loading SenseVoice model from {model_dir}...");
            let recognizer = sherpa_rs::sense_voice::SenseVoiceRecognizer::new(config)
                .expect("failed to init SenseVoice — check --model-dir");
            eprintln!("[voice-correct] SenseVoice model loaded");
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
                    eprintln!("[voice-correct] speech recognition authorized");
                } else {
                    eprintln!(
                        "[voice-correct] speech recognition not authorized: {:?}",
                        status
                    );
                }
            });
            SFSpeechRecognizer::requestAuthorization(&handler);
        }
        Some(recognizer)
    } else {
        None
    };

    let audio_engine = unsafe { AVAudioEngine::new() };

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

    let run_loop_source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        .expect("failed to create run loop source");
    unsafe {
        let run_loop = CFRunLoop::current().expect("no current run loop");
        run_loop.add_source(Some(&run_loop_source), kCFRunLoopCommonModes);
    }

    // --- Menubar ---
    let status_bar = NSStatusBar::systemStatusBar();
    let status_item = status_bar.statusItemWithLength(-1.0);
    set_status_icon(&status_item, AppState::Idle, mtm);

    let delegate: Retained<MenuDelegate> =
        unsafe { objc2::msg_send![MenuDelegate::alloc(mtm), init] };
    let menu = NSMenu::new(mtm);
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit"),
            Some(sel!(quit:)),
            &NSString::from_str("q"),
        )
    };
    unsafe { quit_item.setTarget(Some(&delegate)) };
    menu.addItem(&quit_item);
    status_item.setMenu(Some(&menu));

    let engine_name = if use_sensevoice {
        "SenseVoice"
    } else {
        "Apple Speech"
    };
    eprintln!(
        "[voice-correct] ready ({engine_name}) — hold right Cmd: dictate | tap+hold: correct"
    );

    // --- State ---
    let is_recording = Cell::new(false);
    let is_processing = Cell::new(false);
    let processing_tick: Cell<u32> = Cell::new(0);
    let current_mode: Cell<u8> = Cell::new(0);
    // Original text saved when LLM correction is dispatched
    let correction_original: Cell<Option<String>> = Cell::new(None);
    let apple_request: Cell<Option<Retained<SFSpeechAudioBufferRecognitionRequest>>> =
        Cell::new(None);
    let accumulated_samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let native_sample_rate: Cell<u32> = Cell::new(16000);

    let _tap = tap;
    let _run_loop_source = run_loop_source;

    // --- Timer (50ms polling) ---
    let _timer = unsafe {
        let samples_ref = accumulated_samples.clone();

        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            // --- Animate processing icon (cycle every ~250ms = 5 ticks) ---
            if is_processing.get() {
                let tick = processing_tick.get() + 1;
                processing_tick.set(tick);
                if tick % 5 == 0 {
                    let frame = (tick / 5) as usize % PROCESSING_ICONS.len();
                    set_status_icon_name(&status_item, PROCESSING_ICONS[frame], mtm);
                }

                // Check if LLM result has arrived
                if let Ok(mut r) = LLM_RESULT.try_lock() {
                    if let Some(result) = r.take() {
                        is_processing.set(false);
                        let original = correction_original.take().unwrap_or_default();
                        match result {
                            Ok(corrected) if corrected != original => {
                                // Re-read focused element for writing
                                if let Some((element, _)) = read_focused_text() {
                                    write_corrected_text(&element, &corrected);
                                } else {
                                    // Field lost focus — use clipboard fallback
                                    replace_via_clipboard(&corrected);
                                }
                            }
                            Ok(_) => {
                                eprintln!("[voice-correct] no change needed");
                            }
                            Err(e) => {
                                eprintln!("[voice-correct] LLM error: {}", e);
                                play_error_sound();
                            }
                        }
                        set_status_icon(&status_item, AppState::Idle, mtm);
                    }
                }
            }

            // --- CANCEL recording (short tap) ---
            if SHOULD_CANCEL.swap(false, Ordering::Relaxed) && is_recording.get() {
                eprintln!("[voice-correct] tap detected, cancelled");
                is_recording.set(false);
                IS_RECORDING.store(false, Ordering::Relaxed);

                let microphone = audio_engine.inputNode();
                microphone.removeTapOnBus(0);
                audio_engine.stop();

                if use_sensevoice {
                    if let Ok(mut s) = samples_ref.lock() {
                        s.clear();
                    }
                } else {
                    if let Some(req) = apple_request.take() {
                        req.endAudio();
                    }
                    if let Ok(mut t) = RECOGNIZED_TEXT.lock() {
                        t.clear();
                    }
                }
                set_status_icon(&status_item, AppState::Idle, mtm);
            }

            // --- START recording ---
            if SHOULD_START.swap(false, Ordering::Relaxed) && !is_recording.get() {
                let mode = SESSION_MODE.load(Ordering::Relaxed);
                current_mode.set(mode);

                let mode_name = if mode == MODE_CORRECT {
                    "correct"
                } else {
                    "dictate"
                };
                eprintln!("[voice-correct] recording started (mode: {mode_name})");

                is_recording.set(true);
                IS_RECORDING.store(true, Ordering::Relaxed);
                let icon = if mode == MODE_CORRECT {
                    AppState::Correcting
                } else {
                    AppState::Recording
                };
                set_status_icon(&status_item, icon, mtm);
                play_start_sound();

                let microphone = audio_engine.inputNode();

                if use_sensevoice {
                    // SenseVoice: accumulate raw samples
                    if let Ok(mut s) = samples_ref.lock() {
                        s.clear();
                    }

                    let format = microphone.outputFormatForBus(0);
                    let sr = format.sampleRate() as u32;
                    native_sample_rate.set(sr);

                    let samples_tap = samples_ref.clone();
                    let tap_block = RcBlock::new(
                        move |buffer: NonNull<AVAudioPCMBuffer>,
                              _time: NonNull<AVAudioTime>| {
                            let buf = buffer.as_ref();
                            let float_data = buf.floatChannelData();
                            let frame_length = buf.frameLength();
                            if !float_data.is_null() && frame_length > 0 {
                                let channel0 = (*float_data).as_ptr();
                                let slice =
                                    std::slice::from_raw_parts(channel0, frame_length as usize);
                                if let Ok(mut samples) = samples_tap.lock() {
                                    samples.extend_from_slice(slice);
                                }
                            }
                        },
                    );
                    microphone.installTapOnBus_bufferSize_format_block(
                        0,
                        4096,
                        Some(&format),
                        &*tap_block as *const _ as *mut _,
                    );
                } else {
                    // Apple Speech: streaming recognition
                    if let Ok(mut text) = RECOGNIZED_TEXT.lock() {
                        text.clear();
                    }

                    let req = SFSpeechAudioBufferRecognitionRequest::new();
                    let format = microphone.outputFormatForBus(0);
                    {
                        let req_clone = req.clone();
                        let tap_block = RcBlock::new(
                            move |buffer: NonNull<AVAudioPCMBuffer>,
                                  _time: NonNull<AVAudioTime>| {
                                req_clone.appendAudioPCMBuffer(buffer.as_ref());
                            },
                        );
                        microphone.installTapOnBus_bufferSize_format_block(
                            0,
                            1024,
                            Some(&format),
                            &*tap_block as *const _ as *mut _,
                        );
                    }

                    let handler = RcBlock::new(
                        |result: *mut SFSpeechRecognitionResult,
                         error: *mut objc2_foundation::NSError| {
                            if !error.is_null() {
                                let error = &*error;
                                eprintln!(
                                    "[voice-correct] recognition error: {:?}",
                                    error.localizedDescription()
                                );
                            } else if !result.is_null() {
                                let result = &*result;
                                let text = result
                                    .bestTranscription()
                                    .formattedString()
                                    .to_string();
                                eprintln!("[voice-correct] partial: {}", text);
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

                audio_engine.prepare();
                if let Err(e) = audio_engine.startAndReturnError() {
                    eprintln!("[voice-correct] audio engine start error: {:?}", e);
                    is_recording.set(false);
                    IS_RECORDING.store(false, Ordering::Relaxed);
                    set_status_icon(&status_item, AppState::Idle, mtm);
                    play_error_sound();
                    return;
                }
            }

            // --- STOP recording ---
            if SHOULD_STOP.swap(false, Ordering::Relaxed) && is_recording.get() {
                let mode = current_mode.get();
                is_recording.set(false);
                IS_RECORDING.store(false, Ordering::Relaxed);

                let microphone = audio_engine.inputNode();
                microphone.removeTapOnBus(0);
                audio_engine.stop();

                play_stop_sound();

                // --- Get transcription ---
                let spoken: String;

                if use_sensevoice {
                    #[cfg(feature = "sensevoice")]
                    {
                        let raw_samples = samples_ref.lock().unwrap();
                        if raw_samples.is_empty() {
                            eprintln!("[voice-correct] no audio captured");
                            set_status_icon(&status_item, AppState::Idle, mtm);
                            return;
                        }
                        let sr = native_sample_rate.get();
                        // Downsample to 16kHz if needed
                        let samples_16k = if sr == 16000 {
                            raw_samples.clone()
                        } else {
                            let ratio = sr as f64 / 16000.0;
                            let out_len = (raw_samples.len() as f64 / ratio) as usize;
                            (0..out_len)
                                .map(|i| {
                                    let src = i as f64 * ratio;
                                    let idx = src as usize;
                                    let frac = src - idx as f64;
                                    let a = raw_samples[idx];
                                    let b =
                                        raw_samples.get(idx + 1).copied().unwrap_or(a);
                                    a + (b - a) * frac as f32
                                })
                                .collect::<Vec<f32>>()
                        };
                        drop(raw_samples);

                        eprintln!(
                            "[voice-correct] transcribing {:.1}s of audio...",
                            samples_16k.len() as f64 / 16000.0
                        );
                        if let Some(mut recognizer) = sv_recognizer.take() {
                            let t0 = std::time::Instant::now();
                            let result = recognizer.transcribe(16000, &samples_16k);
                            let ms = t0.elapsed().as_secs_f64() * 1000.0;
                            eprintln!(
                                "[voice-correct] ASR: \"{}\" ({:.0}ms)",
                                result.text, ms
                            );
                            spoken = result.text;
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
                    if let Some(req) = apple_request.take() {
                        req.endAudio();
                    }
                    // Wait for Apple Speech to finalize
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

                // Empty speech → skip entirely
                if spoken.is_empty() {
                    eprintln!("[voice-correct] no speech recognized, skipping");
                    set_status_icon(&status_item, AppState::Idle, mtm);
                    return;
                }

                if mode == MODE_DICTATION {
                    // --- Dictation mode: just type ---
                    eprintln!("[voice-correct] typing: {}", spoken);
                    type_text(&spoken);
                    set_status_icon(&status_item, AppState::Idle, mtm);
                } else {
                    // --- Correction mode ---
                    // Skip if a previous LLM call is still in flight
                    if is_processing.get() {
                        eprintln!("[voice-correct] still processing previous correction, skipping");
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }

                    let app_name = frontmost_bundle_id()
                        .unwrap_or_else(|| "unknown".to_string());
                    eprintln!("[voice-correct] app: {}", app_name);

                    match read_focused_text() {
                        Some((_element, original_text)) => {
                            eprintln!(
                                "[voice-correct] correcting: \"{}\" with instruction: \"{}\"",
                                original_text, spoken
                            );
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
                                    Ok(text) => eprintln!(
                                        "[voice-correct] result: \"{}\" ({:.0}ms)",
                                        text, elapsed
                                    ),
                                    Err(e) => eprintln!("[voice-correct] LLM error: {} ({:.0}ms)", e, elapsed),
                                }
                                if let Ok(mut r) = LLM_RESULT.lock() {
                                    *r = Some(result);
                                }
                            });
                        }
                        None => {
                            // Empty field → fall back to typing (no LLM call)
                            eprintln!(
                                "[voice-correct] field empty, typing instead: {}",
                                spoken
                            );
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
