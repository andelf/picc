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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem,
};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioEngineConfigurationChangeNotification, AVAudioPCMBuffer, AVAudioTime,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFString, CFType, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{
    ns_string, NSDate, NSLocale, NSNotification, NSNotificationCenter, NSRunLoop, NSString, NSTimer,
};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus,
};

use objc2_application_services::AXUIElement;
use picc::accessibility;
use picc::input::{parse_key_combo, press_key_combo, type_text};
use tracing::{debug, error, info, warn};

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

/// Pointer to the CGEvent tap CFMachPort — used to re-enable on timeout.
static EVENT_TAP_PTR: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(ptr::null_mut());

/// Gate for the audio tap callback — only accumulate samples when true.
static COLLECTING: AtomicBool = AtomicBool::new(false);

/// Set by AVAudioEngineConfigurationChangeNotification — signals the timer
/// loop to reset engine and tap before next start.
static AUDIO_CONFIG_CHANGED: AtomicBool = AtomicBool::new(false);

/// Audio engine state machine (sensevoice path, managed by background thread).
static AUDIO_STATE: AtomicU8 = AtomicU8::new(0);
const AUDIO_IDLE: u8 = 0;
const AUDIO_STARTING: u8 = 1;
const AUDIO_RUNNING: u8 = 2;
const AUDIO_STOPPING: u8 = 3;
const AUDIO_STOPPED: u8 = 4;
const AUDIO_ERROR: u8 = 5;

/// Sample rate discovered by the audio thread — read by main thread for resampling.
static NATIVE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(16000);

/// Commands sent to the audio management thread.
const CMD_START: u8 = 1;
const CMD_STOP: u8 = 2;
const CMD_CANCEL: u8 = 3;

// --- Dictation post-processing options ---

#[derive(Debug, Clone, Copy, Default)]
struct DictationOptions {
    fullwidth_to_halfwidth: bool,
    space_around_punct: bool,
    space_between_cjk: bool,
    strip_trailing_punct: bool,
}

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
        let right_cmd_pressed = (device_flags & NX_DEVICERCMDKEYMASK) != 0;

        static WAS_DOWN: AtomicBool = AtomicBool::new(false);
        let was_down = WAS_DOWN.load(Ordering::Relaxed);

        if right_cmd_pressed && !was_down {
            // Press → always start recording immediately
            WAS_DOWN.store(true, Ordering::Relaxed);
            let now = now_ms();
            PRESS_MS.store(now, Ordering::Relaxed);

            let is_rec = IS_RECORDING.load(Ordering::Relaxed);
            let cancel_pending = SHOULD_CANCEL.load(Ordering::Relaxed);
            let should_start = SHOULD_START.load(Ordering::Relaxed);
            let should_stop = SHOULD_STOP.load(Ordering::Relaxed);
            let mode = SESSION_MODE.load(Ordering::Relaxed);
            let last_tap = LAST_TAP_RELEASE_MS.load(Ordering::Relaxed);
            let gap = now - last_tap;

            debug!(
                now,
                is_rec,
                cancel_pending,
                should_start,
                should_stop,
                mode,
                gap,
                flags = format_args!("0x{:04x}", device_flags),
                "KEY_DOWN: right cmd pressed"
            );

            // Allow new press if not recording, or if cancel is pending
            // (recording is about to be cancelled by timer)
            if !is_rec || cancel_pending {
                if gap < 300 {
                    SESSION_MODE.store(MODE_CORRECT, Ordering::Relaxed);
                    debug!(gap, "KEY_DOWN → CORRECT mode, set SHOULD_START");
                } else {
                    SESSION_MODE.store(MODE_DICTATION, Ordering::Relaxed);
                    debug!(gap, "KEY_DOWN → DICTATION mode, set SHOULD_START");
                }
                SHOULD_START.store(true, Ordering::Relaxed);
            } else {
                debug!(is_rec, cancel_pending, "KEY_DOWN → IGNORED (already recording)");
            }
        } else if !right_cmd_pressed && was_down {
            // Release
            WAS_DOWN.store(false, Ordering::Relaxed);
            let now = now_ms();
            let press_ms = PRESS_MS.load(Ordering::Relaxed);
            let hold = now - press_ms;
            let is_rec = IS_RECORDING.load(Ordering::Relaxed);
            let should_start = SHOULD_START.load(Ordering::Relaxed);
            let should_stop = SHOULD_STOP.load(Ordering::Relaxed);
            let cancel_pending = SHOULD_CANCEL.load(Ordering::Relaxed);
            let mode = SESSION_MODE.load(Ordering::Relaxed);

            debug!(
                now,
                press_ms,
                hold,
                is_rec,
                should_start,
                should_stop,
                cancel_pending,
                mode,
                flags = format_args!("0x{:04x}", device_flags),
                "KEY_UP: right cmd released"
            );

            // Short tap → always record as tap for double-tap detection,
            // regardless of whether timer has started recording yet.
            if hold < 300 {
                LAST_TAP_RELEASE_MS.store(now, Ordering::Relaxed);
                debug!(hold, "KEY_UP → short tap, updated LAST_TAP_RELEASE_MS");
            }

            if is_rec {
                if hold < 300 && SESSION_MODE.load(Ordering::Relaxed) == MODE_DICTATION {
                    SHOULD_CANCEL.store(true, Ordering::Relaxed);
                    debug!(hold, "KEY_UP → set SHOULD_CANCEL (short tap in dictation)");
                } else {
                    SHOULD_STOP.store(true, Ordering::Relaxed);
                    debug!(hold, mode, "KEY_UP → set SHOULD_STOP");
                }
            } else {
                if hold < 300 {
                    SHOULD_START.store(false, Ordering::Relaxed);
                    debug!(hold, "KEY_UP → cleared SHOULD_START (short tap, not yet recording)");
                } else {
                    debug!(hold, is_rec, should_start, "KEY_UP → noop (not recording)");
                }
            }
        } else if event_type == CGEventType::FlagsChanged {
            // Other modifier changes while tracking right cmd
            let is_rec = IS_RECORDING.load(Ordering::Relaxed);
            debug!(
                right_cmd_pressed,
                was_down,
                is_rec,
                flags = format_args!("0x{:04x}", device_flags),
                "FLAGS_CHANGED: no right cmd transition"
            );
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

    info!(path = %model_dir.display(), "SenseVoice model not found, downloading (~250 MB)...");
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

// --- Dictation post-processing ---

/// Apply user-configured text transforms before text goes on screen.
///
/// Used in both paths:
/// - **Dictation mode**: transform speech recognition output before typing
/// - **Correction mode**: transform LLM output before writing back to text field
///
/// Transforms (each gated by its toggle):
/// 1. `fullwidth_to_halfwidth` — convert CJK fullwidth punctuation to ASCII halfwidth
/// 2. `space_around_punct` — insert spaces around half-width punctuation
///    (requires fullwidth_to_halfwidth to be on)
/// 3. `space_between_cjk` — insert spaces at CJK↔Latin/Digit boundaries (independent)
/// 4. `strip_trailing_punct` — remove trailing punctuation (speech engines often add "。")
fn apply_dictation_transforms(text: &str, opts: DictationOptions) -> String {
    let mut result = text.to_string();

    if opts.fullwidth_to_halfwidth {
        result = fullwidth_to_halfwidth(&result);
    }
    if opts.space_around_punct || opts.space_between_cjk {
        result = auto_insert_spaces(&result, opts.space_around_punct, opts.space_between_cjk);
    }
    if opts.strip_trailing_punct {
        result = strip_trailing_punctuation(&result);
    }

    result
}

fn fullwidth_to_halfwidth(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            // Fullwidth ASCII variants (Ａ→A, ，→, etc.)
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',  // ideographic space
            '。' => '.',        // U+3002
            '、' => ',',        // U+3001
            '【' => '[',        // U+3010
            '】' => ']',        // U+3011
            '「' => '"',        // U+300C
            '」' => '"',        // U+300D
            '《' => '<',        // U+300A
            '》' => '>',        // U+300B
            '\u{201C}' => '"',  // left double quote
            '\u{201D}' => '"',  // right double quote
            '\u{2018}' => '\'', // left single quote
            '\u{2019}' => '\'', // right single quote
            _ => c,
        })
        .collect()
}

/// Character category for spacing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Cjk,
    Latin,
    Digit,
    OpenBracket,
    CloseBracket,
    /// Delimiter punctuation: , . ! ? : ;
    Delimiter,
    Space,
    Other,
}

fn classify(c: char) -> CharKind {
    match c {
        'A'..='Z' | 'a'..='z' => CharKind::Latin,
        '0'..='9' => CharKind::Digit,
        '(' | '[' | '<' => CharKind::OpenBracket,
        ')' | ']' | '>' => CharKind::CloseBracket,
        ',' | '.' | '!' | '?' | ':' | ';' => CharKind::Delimiter,
        ' ' => CharKind::Space,
        c if is_cjk(c) => CharKind::Cjk,
        _ => CharKind::Other,
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{2E80}'..='\u{2EFF}'   // CJK Radicals Supplement
        | '\u{2F00}'..='\u{2FDF}' // Kangxi Radicals
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{3100}'..='\u{312F}' // Bopomofo
        | '\u{3200}'..='\u{32FF}' // Enclosed CJK Letters
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
    )
}

/// Does this pair require a CJK↔Latin/Digit boundary space?
fn is_cjk_boundary(left: CharKind, right: CharKind) -> bool {
    use CharKind::*;
    matches!(
        (left, right),
        (Cjk, Latin) | (Latin, Cjk) | (Cjk, Digit) | (Digit, Cjk)
    )
}

/// Does this pair require a punctuation-related space?
fn is_punct_space(left: CharKind, right: CharKind) -> bool {
    use CharKind::*;
    matches!(
        (left, right),
        // Delimiter/CloseBracket → content
        (Delimiter | CloseBracket, Cjk | Latin | Digit | Other) |
        // content → OpenBracket
        (Cjk | Latin | Digit | Other, OpenBracket)
    )
}

/// Auto-insert spaces based on character classification.
///
/// - `punct`: insert spaces around half-width punctuation (delimiters, brackets)
/// - `cjk`: insert spaces at CJK↔Latin/Digit boundaries
///
/// Single-pass: each character is classified into a [`CharKind`], and a space is
/// inserted between adjacent pairs where the rules apply.
fn auto_insert_spaces(s: &str, punct: bool, cjk: bool) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 32);

    for (i, &c) in chars.iter().enumerate() {
        let kind = classify(c);

        // Insert space *before* this character if needed.
        if i > 0 {
            let prev = chars[i - 1];
            let prev_kind = classify(prev);

            if prev_kind != CharKind::Space && kind != CharKind::Space {
                let want_cjk = cjk && is_cjk_boundary(prev_kind, kind);
                let want_punct = punct && is_punct_space(prev_kind, kind);

                if want_cjk || want_punct {
                    // Exception: decimal point — don't space after '.' when digit.digit
                    let is_decimal_dot = prev == '.' && prev_kind == CharKind::Delimiter
                        && classify(chars.get(i.wrapping_sub(2)).copied().unwrap_or(' ')) == CharKind::Digit
                        && kind == CharKind::Digit;
                    if !is_decimal_dot {
                        out.push(' ');
                    }
                }
            }
        }

        out.push(c);
    }

    out
}

fn strip_trailing_punctuation(s: &str) -> String {
    s.trim_end_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | '!' | '?' | ';' | ':' | '。' | '，' | '！' | '？' | '；' | '：' | '、'
                | '…'
        )
    })
    .to_string()
}

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
            info!(enabled = opts.space_around_punct, "space around punctuation");
        }

        #[unsafe(method(toggleSpaceBetweenCjk:))]
        fn toggle_space_between_cjk(&self, sender: &AnyObject) {
            let mut opts = self.ivars().options.get();
            opts.space_between_cjk = !opts.space_between_cjk;
            self.ivars().options.set(opts);

            let item: &NSMenuItem = unsafe { &*(sender as *const AnyObject as *const NSMenuItem) };
            item.setState(if opts.space_between_cjk { 1 } else { 0 });
            info!(enabled = opts.space_between_cjk, "space between CJK & Latin");
        }

        #[unsafe(method(toggleStripTrailingPunct:))]
        fn toggle_strip_trailing_punct(&self, sender: &AnyObject) {
            let mut opts = self.ivars().options.get();
            opts.strip_trailing_punct = !opts.strip_trailing_punct;
            self.ivars().options.set(opts);

            let item: &NSMenuItem = unsafe { &*(sender as *const AnyObject as *const NSMenuItem) };
            item.setState(if opts.strip_trailing_punct { 1 } else { 0 });
            info!(enabled = opts.strip_trailing_punct, "strip trailing punctuation");
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

/// Punctuation characters after which a space should be inserted before new text.
const SPACE_AFTER_PUNCT: &[char] = &[',', '.', ';', ':', '!', '?', ')', ']', '}', '"', '\''];

/// Get the focused AX UI element.
fn focused_ax_element() -> Option<CFRetained<AXUIElement>> {
    let system = unsafe { AXUIElement::new_system_wide() };
    let cf = accessibility::attr_value(&system, "AXFocusedUIElement")?;
    Some(unsafe { CFRetained::cast_unchecked(cf) })
}

/// Extract a CFRange from an AXValue attribute.
fn ax_value_as_cfrange(value: &CFRetained<CFType>) -> Option<objc2_core_foundation::CFRange> {
    let ax_value: &objc2_application_services::AXValue =
        unsafe { &*(value.as_ref() as *const CFType as *const objc2_application_services::AXValue) };
    let mut range = objc2_core_foundation::CFRange { location: 0, length: 0 };
    let ok = unsafe {
        ax_value.value(
            objc2_application_services::AXValueType::CFRange,
            NonNull::new_unchecked(&mut range as *mut _ as *mut std::ffi::c_void),
        )
    };
    ok.then_some(range)
}

/// Read the character immediately before the cursor in the focused text field.
/// Returns None if unable to determine (no focused element, empty text, or at position 0).
fn char_before_cursor() -> Option<char> {
    let focused = focused_ax_element()?;
    let text = accessibility::attr_string(&focused, "AXValue")?;
    if text.is_empty() {
        return None;
    }
    // Try to get cursor position from AXSelectedTextRange
    let range_cf = accessibility::attr_value(&focused, "AXSelectedTextRange")?;
    let pos = if let Some(range) = ax_value_as_cfrange(&range_cf) {
        range.location as usize
    } else {
        // Fallback: assume cursor is at end (UTF-16 length)
        text.encode_utf16().count()
    };
    if pos == 0 {
        return None;
    }
    // CFRange.location is a UTF-16 code unit offset; decode via UTF-16 to get
    // the correct character even when text contains emoji or supplementary CJK.
    let utf16: Vec<u16> = text.encode_utf16().collect();
    if pos > utf16.len() {
        return text.chars().last();
    }
    let unit = utf16[pos - 1];
    if (0xDC00..=0xDFFF).contains(&unit) && pos >= 2 {
        // Low surrogate — pair with the preceding high surrogate
        char::decode_utf16([utf16[pos - 2], unit]).next()?.ok()
    } else {
        char::decode_utf16([unit]).next()?.ok()
    }
}

fn read_focused_text() -> Option<(CFRetained<AXUIElement>, String)> {
    let focused = focused_ax_element()?;
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
/// Apps where AXValue returns the entire buffer (terminals).
/// Correction mode skips text reading and falls back to typing.
const SKIP_AX_READ_BUNDLES: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "io.alacritty",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "co.zeit.hyper",
    "net.kovidgoyal.kitty",
];

/// Apps where AXValue set doesn't work (browsers, Electron apps).
/// Text replacement uses clipboard paste instead.
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

fn matches_bundle_list(bundle: &str, list: &[&str]) -> bool {
    list.iter().any(|b| {
        if b.ends_with('.') {
            bundle.starts_with(b)
        } else {
            bundle == *b
        }
    })
}

/// Check if the frontmost app should use clipboard-only strategy.
fn should_use_clipboard() -> bool {
    frontmost_bundle_id()
        .map(|b| matches_bundle_list(&b, CLIPBOARD_ONLY_BUNDLES))
        .unwrap_or(false)
}

/// Replace text in the focused field.
/// For browsers/Electron/Lark: always clipboard paste.
/// For native apps: try AXValue set first, fall back to clipboard.
fn write_corrected_text(element: &AXUIElement, text: &str) -> bool {
    // Browsers & Electron: skip AX, go straight to clipboard
    if should_use_clipboard() {
        info!("using clipboard paste for this app");
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

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

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
            info!(path = %model_dir, "loading SenseVoice model");
            let recognizer = sherpa_rs::sense_voice::SenseVoiceRecognizer::new(config)
                .expect("failed to init SenseVoice — check --model-dir");
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

    let audio_engine = unsafe { AVAudioEngine::new() };

    // Listen for audio device changes (earphone plug/unplug, default device switch)
    let _config_observer = unsafe {
        let notification_block = RcBlock::new(|_notif: NonNull<NSNotification>| {
            warn!("audio device configuration changed — will reset engine on next start");
            AUDIO_CONFIG_CHANGED.store(true, Ordering::Relaxed);
        });
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(AVAudioEngineConfigurationChangeNotification),
            Some(&audio_engine),
            None,
            &*notification_block,
        )
    };

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
    EVENT_TAP_PTR.store(&*tap as *const CFMachPort as *mut std::ffi::c_void, Ordering::Relaxed);

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

    let delegate: Retained<MenuDelegate> = {
        let this = MenuDelegate::alloc(mtm).set_ivars(MenuDelegateIvars {
            options: Cell::new(DictationOptions::default()),
            punct_spaces_item: RefCell::new(None),
        });
        unsafe { objc2::msg_send![super(this), init] }
    };
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit"),
            Some(sel!(quit:)),
            &NSString::from_str("q"),
        )
    };
    unsafe { quit_item.setTarget(Some(&delegate)) };

    // Toggle: fullwidth → halfwidth
    let fw_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Fullwidth to Halfwidth"),
            Some(sel!(toggleFullwidthToHalfwidth:)),
            &NSString::from_str(""),
        )
    };
    unsafe { fw_item.setTarget(Some(&delegate)) };

    // Toggle: space around punctuation (sub-option of fullwidth→halfwidth)
    let punct_spaces_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("  Space Around Punctuation"),
            Some(sel!(toggleSpaceAroundPunct:)),
            &NSString::from_str(""),
        )
    };
    unsafe { punct_spaces_item.setTarget(Some(&delegate)) };
    punct_spaces_item.setEnabled(false); // disabled until fullwidth is turned on
    *delegate.ivars().punct_spaces_item.borrow_mut() = Some(punct_spaces_item.clone());

    // Toggle: space between CJK & Latin/Digit (independent)
    let cjk_spaces_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Space Between CJK & Latin"),
            Some(sel!(toggleSpaceBetweenCjk:)),
            &NSString::from_str(""),
        )
    };
    unsafe { cjk_spaces_item.setTarget(Some(&delegate)) };

    // Toggle: strip trailing punctuation
    let strip_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Strip Trailing Punctuation"),
            Some(sel!(toggleStripTrailingPunct:)),
            &NSString::from_str(""),
        )
    };
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
    info!(engine = engine_name, "ready — hold right Cmd: dictate | tap+hold: correct");

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
        let delegate = delegate.clone();

        // Counter for recording heartbeat (logs every ~2s)
        let heartbeat_tick: Cell<u32> = Cell::new(0);
        // Whether the SenseVoice audio tap has been installed (only done once)
        let sv_tap_installed: Cell<bool> = Cell::new(false);

        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            // --- Heartbeat: log state every ~2s while recording ---
            if is_recording.get() {
                let tick = heartbeat_tick.get() + 1;
                heartbeat_tick.set(tick);
                if tick % 40 == 0 {
                    let elapsed_s = tick as f64 * 0.05;
                    let should_stop = SHOULD_STOP.load(Ordering::Relaxed);
                    let should_cancel = SHOULD_CANCEL.load(Ordering::Relaxed);
                    let mode = current_mode.get();
                    let mode_name = if mode == MODE_CORRECT { "correct" } else { "dictate" };
                    debug!(
                        elapsed_s = format_args!("{:.1}", elapsed_s),
                        mode = mode_name,
                        should_stop,
                        should_cancel,
                        "HEARTBEAT: still recording"
                    );
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
                }

                // Check if LLM result has arrived
                if let Ok(mut r) = LLM_RESULT.try_lock() {
                    if let Some(result) = r.take() {
                        is_processing.set(false);
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
                                if let Some((element, _)) = read_focused_text() {
                                    write_corrected_text(&element, &corrected);
                                } else {
                                    // Field lost focus — use clipboard fallback
                                    replace_via_clipboard(&corrected);
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
            if SHOULD_CANCEL.swap(false, Ordering::Relaxed) && is_recording.get() {
                debug!(
                    is_recording = is_recording.get(),
                    is_processing = is_processing.get(),
                    "TIMER → CANCEL: stopping recording"
                );
                is_recording.set(false);
                IS_RECORDING.store(false, Ordering::Relaxed);

                if use_sensevoice {
                    COLLECTING.store(false, Ordering::Relaxed);
                    audio_engine.stop();
                    if let Ok(mut s) = samples_ref.lock() { s.clear(); }
                    debug!("TIMER → CANCEL: engine stopped (tap kept)");
                } else {
                    let microphone = audio_engine.inputNode();
                    microphone.removeTapOnBus(0);
                    audio_engine.stop();
                    audio_engine.reset();
                    debug!("TIMER → CANCEL: engine stopped and reset");
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
                debug!(
                    mode = mode_name,
                    is_processing = is_processing.get(),
                    "TIMER → START: beginning recording"
                );

                is_recording.set(true);
                IS_RECORDING.store(true, Ordering::Relaxed);
                let icon = if mode == MODE_CORRECT {
                    AppState::Correcting
                } else {
                    AppState::Recording
                };
                set_status_icon(&status_item, icon, mtm);
                play_start_sound();
                debug!("TIMER → START [1]: icon set, sound played");

                if use_sensevoice {
                    // Proactively reset engine if audio device changed (earphone plug/unplug)
                    if AUDIO_CONFIG_CHANGED.swap(false, Ordering::Relaxed) {
                        info!("TIMER → START: audio config changed, resetting engine and tap");
                        if sv_tap_installed.get() {
                            let mic = audio_engine.inputNode();
                            mic.removeTapOnBus(0);
                            sv_tap_installed.set(false);
                        }
                        audio_engine.stop();
                        audio_engine.reset();
                    }

                    // Call order: inputNode → installTap → prepare → start.
                    // Apple requires tap to be installed BEFORE engine start.
                    let microphone = audio_engine.inputNode();

                    // Clear samples
                    match samples_ref.try_lock() {
                        Ok(mut s) => s.clear(),
                        Err(_) => {
                            warn!("TIMER → START: samples lock contention, retry");
                            is_recording.set(false);
                            IS_RECORDING.store(false, Ordering::Relaxed);
                            SHOULD_START.store(true, Ordering::Relaxed);
                            set_status_icon(&status_item, AppState::Idle, mtm);
                            return;
                        }
                    }

                    // Install tap BEFORE prepare/start (Apple requirement)
                    if !sv_tap_installed.get() {
                        let format = microphone.outputFormatForBus(0);
                        let sr = format.sampleRate() as u32;
                        native_sample_rate.set(sr);
                        debug!(sample_rate = sr, "TIMER → START [2]: installing tap");

                        let samples_tap = samples_ref.clone();
                        let tap_block = RcBlock::new(
                            move |buffer: NonNull<AVAudioPCMBuffer>,
                                  _time: NonNull<AVAudioTime>| {
                                if !COLLECTING.load(Ordering::Relaxed) {
                                    return;
                                }
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
                        sv_tap_installed.set(true);
                        debug!("TIMER → START [2]: tap installed");
                    } else {
                        debug!("TIMER → START [2]: tap reused");
                    }

                    // prepare + start (tap already installed above)
                    debug!("TIMER → START [3]: prepare + start");
                    audio_engine.prepare();
                    if let Err(e) = audio_engine.startAndReturnError() {
                        error!(?e, "TIMER → START: engine failed, resetting for device change");
                        if sv_tap_installed.get() {
                            let mic = audio_engine.inputNode();
                            mic.removeTapOnBus(0);
                            sv_tap_installed.set(false);
                        }
                        audio_engine.reset();
                        is_recording.set(false);
                        IS_RECORDING.store(false, Ordering::Relaxed);
                        SHOULD_START.store(true, Ordering::Relaxed);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }

                    COLLECTING.store(true, Ordering::Relaxed);
                } else {
                    // Apple Speech path — always reinstalls tap, so just drain the flag
                    AUDIO_CONFIG_CHANGED.store(false, Ordering::Relaxed);
                    let microphone = audio_engine.inputNode();
                    microphone.removeTapOnBus(0);
                    audio_engine.prepare();
                    if let Err(e) = audio_engine.startAndReturnError() {
                        error!(?e, "TIMER → START: audio engine failed");
                        audio_engine.reset();
                        is_recording.set(false);
                        IS_RECORDING.store(false, Ordering::Relaxed);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        play_error_sound();
                        return;
                    }

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
                                warn!("recognition error: {:?}", error.localizedDescription());
                            } else if !result.is_null() {
                                let result = &*result;
                                let text = result
                                    .bestTranscription()
                                    .formattedString()
                                    .to_string();
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
            if SHOULD_STOP.swap(false, Ordering::Relaxed) && is_recording.get() {
                let mode = current_mode.get();
                let mode_name = if mode == MODE_CORRECT { "correct" } else { "dictate" };
                debug!(
                    mode = mode_name,
                    is_processing = is_processing.get(),
                    "TIMER → STOP: stopping recording"
                );
                is_recording.set(false);
                IS_RECORDING.store(false, Ordering::Relaxed);

                let spoken: String;

                if use_sensevoice {
                    COLLECTING.store(false, Ordering::Relaxed);
                    audio_engine.stop();
                    debug!("TIMER → STOP: engine stopped (tap kept)");
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
                                    let b = raw_samples.get(idx + 1).copied().unwrap_or(a);
                                    a + (b - a) * frac as f32
                                })
                                .collect::<Vec<f32>>()
                        };
                        drop(raw_samples);
                        info!(duration_s = format_args!("{:.1}", samples_16k.len() as f64 / 16000.0), "transcribing audio");
                        if let Some(mut recognizer) = sv_recognizer.take() {
                            let t0 = std::time::Instant::now();
                            let result = recognizer.transcribe(16000, &samples_16k);
                            let ms = t0.elapsed().as_secs_f64() * 1000.0;
                            info!(text = %result.text, elapsed_ms = format_args!("{:.0}", ms), "ASR result");
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
                    // Apple Speech path
                    let microphone = audio_engine.inputNode();
                    microphone.removeTapOnBus(0);
                    audio_engine.stop();
                    audio_engine.reset();
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
                    set_status_icon(&status_item, AppState::Idle, mtm);
                    return;
                }

                if mode == MODE_DICTATION {
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
                    if opts.fullwidth_to_halfwidth && opts.space_around_punct && !spoken.starts_with(' ') {
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
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }

                    let app_name = frontmost_bundle_id()
                        .unwrap_or_else(|| "unknown".to_string());
                    info!(app = %app_name, "correction mode");

                    // Terminals: AXValue returns entire buffer, skip reading
                    if matches_bundle_list(&app_name, SKIP_AX_READ_BUNDLES) {
                        info!(text = %spoken, "terminal app, typing instead");
                        type_text(&spoken);
                        set_status_icon(&status_item, AppState::Idle, mtm);
                        return;
                    }

                    match read_focused_text() {
                        Some((_element, original_text)) => {
                            // Limit text length to avoid sending huge payloads to LLM
                            let trimmed = original_text.trim();
                            if trimmed.chars().count() > 250 {
                                warn!(len = trimmed.chars().count(), "text too long (>250 chars), typing instead");
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
                                    Ok(text) => info!(text = %text, elapsed_ms = format_args!("{:.0}", elapsed), "LLM result"),
                                    Err(e) => error!(err = %e, elapsed_ms = format_args!("{:.0}", elapsed), "LLM error"),
                                }
                                if let Ok(mut r) = LLM_RESULT.lock() {
                                    *r = Some(result);
                                }
                            });
                        }
                        None => {
                            // Empty field → fall back to typing (no LLM call)
                            info!(text = %spoken, "field empty, typing instead");
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
    use super::{auto_insert_spaces, apply_dictation_transforms, DictationOptions};

    /// Helper: both punct and cjk spacing enabled (the common case).
    fn spaced(s: &str) -> String {
        auto_insert_spaces(s, true, true)
    }

    // --- CJK ↔ Latin/Digit ---

    #[test]
    fn cjk_latin_spacing() {
        assert_eq!(auto_insert_spaces("中文abc", false, true), "中文 abc");
        assert_eq!(auto_insert_spaces("abc中文", false, true), "abc 中文");
        assert_eq!(auto_insert_spaces("中文abc中文", false, true), "中文 abc 中文");
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
        assert_eq!(auto_insert_spaces("hello,world", true, false), "hello, world");
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
        assert_eq!(auto_insert_spaces("hello(world)test", true, false), "hello (world) test");
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
        assert_eq!(auto_insert_spaces("hello,world", false, true), "hello,world");
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
        assert_eq!(apply_dictation_transforms("中文abc中文", opts), "中文 abc 中文");
    }
}
