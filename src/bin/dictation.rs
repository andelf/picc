//! Push-to-Talk Dictation Tool
//!
//! Hold right Command key to dictate speech, then types recognized text at the cursor.
//! Supports two engines: SenseVoice (offline, via sherpa-onnx) and Apple Speech API.
//!
//! Usage:
//!   dictation --engine sensevoice --model-dir PATH
//!   dictation --engine apple

use std::cell::Cell;
#[cfg(feature = "sensevoice")]
use std::io::{Read as _, Write as _};
#[cfg(feature = "sensevoice")]
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, OnceLock};

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSMenu, NSStatusItem};
use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRunLoop};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{ns_string, NSDate, NSLocale, NSRunLoop, NSTimer};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus,
};

use picc_macos_app::{
    configure_accessory_app, new_menu_item, new_status_item, set_status_item_symbol,
};
use picc_macos_input::type_text;
use picc_speech::{
    begin_requested_session, clear_recording_state, resample_linear, take_stop_while_recording,
    AudioCaptureConfig, AudioEngineManager, HotkeyPolicy, HotkeyRuntime, HotkeySignal, HotkeyState,
    SessionSignals,
};

static SESSION_SIGNALS: SessionSignals = SessionSignals::new();
static RECOGNIZED_TEXT: Mutex<String> = Mutex::new(String::new());

fn hotkey_state() -> &'static Mutex<HotkeyState> {
    static HOTKEY_STATE: OnceLock<Mutex<HotkeyState>> = OnceLock::new();
    HOTKEY_STATE.get_or_init(|| Mutex::new(HotkeyState::new()))
}

#[derive(Parser)]
#[command(about = "Push-to-Talk Dictation — hold right Command to dictate")]
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

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    if event_type == CGEventType::FlagsChanged {
        let flags = CGEvent::flags(Some(event.as_ref()));
        let device_flags = flags.0 & 0xFFFF;
        if let Ok(mut state) = hotkey_state().lock() {
            for signal in state.handle_flags_changed(
                device_flags,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                HotkeyRuntime {
                    is_recording: SESSION_SIGNALS.is_recording(),
                    cancel_pending: SESSION_SIGNALS.cancel_pending(),
                },
                HotkeyPolicy::dictation(),
            ) {
                match signal {
                    HotkeySignal::Start(mode) => SESSION_SIGNALS.request_start(mode),
                    HotkeySignal::Stop => SESSION_SIGNALS.request_stop(),
                    HotkeySignal::ClearPendingStart => SESSION_SIGNALS.clear_pending_start(),
                    HotkeySignal::Cancel => SESSION_SIGNALS.request_cancel(),
                }
            }
        }
    }
    event.as_ptr()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DictationMenuDelegate"]
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

fn set_status_icon(item: &NSStatusItem, recording: bool, mtm: MainThreadMarker) {
    let name = if recording { "mic.fill" } else { "mic" };
    set_status_item_symbol(item, mtm, name, "Dictation");
}

#[cfg(feature = "sensevoice")]
const SENSEVOICE_MODEL_DIR: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17";
#[cfg(feature = "sensevoice")]
const SENSEVOICE_HF_BASE: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main";
#[cfg(feature = "sensevoice")]
const SENSEVOICE_GITHUB_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2";

#[cfg(feature = "sensevoice")]
fn download_with_progress(url: &str, dest: &Path, label: &str) -> Result<(), String> {
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .send()
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let total_mb = total as f64 / 1_048_576.0;
    let mut downloaded: u64 = 0;
    let mut file = std::fs::File::create(dest).map_err(|e| format!("create file: {e}"))?;
    let mut reader = resp;
    let mut buf = [0u8; 65536];
    let start = std::time::Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("write: {e}"))?;
        downloaded += n as u64;
        if total > 0 {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
            let mb = downloaded as f64 / 1_048_576.0;
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { mb / elapsed } else { 0.0 };
            let bar_len = 30;
            let filled = (bar_len as f64 * downloaded as f64 / total as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_len - filled);
            eprint!(
                "\r[dictation] {label}: {bar} {mb:.1}/{total_mb:.1} MB ({pct}%) {speed:.1} MB/s"
            );
        }
    }
    eprintln!();
    Ok(())
}

#[cfg(feature = "sensevoice")]
fn try_download_from_hf(model_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(model_dir).map_err(|e| format!("mkdir: {e}"))?;
    let files = ["model.int8.onnx", "tokens.txt"];
    for name in &files {
        let url = format!("{SENSEVOICE_HF_BASE}/{name}");
        let dest = model_dir.join(name);
        eprintln!("[dictation] downloading {name} from HuggingFace...");
        download_with_progress(&url, &dest, name)?;
    }
    Ok(())
}

#[cfg(feature = "sensevoice")]
fn try_download_from_github(base_dir: &Path) -> Result<(), String> {
    let archive = base_dir.join("sensevoice.tar.bz2");
    eprintln!("[dictation] downloading from GitHub releases...");
    download_with_progress(SENSEVOICE_GITHUB_URL, &archive, "tar.bz2")?;
    eprint!("[dictation] extracting...");
    let status = std::process::Command::new("tar")
        .args([
            "xjf",
            &archive.to_string_lossy(),
            "-C",
            &base_dir.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err("tar extraction failed".into());
    }
    std::fs::remove_file(&archive).ok();
    eprintln!(" done");
    Ok(())
}

#[cfg(feature = "sensevoice")]
fn ensure_sensevoice_model(base_dir: &Path) -> String {
    let model_dir = base_dir.join(SENSEVOICE_MODEL_DIR);
    let model_file = model_dir.join("model.int8.onnx");
    if model_file.exists() {
        return model_dir.to_string_lossy().into_owned();
    }

    eprintln!(
        "[dictation] SenseVoice model not found at {}",
        model_dir.display()
    );
    eprintln!("[dictation] first run — downloading model, this may take a few minutes...");
    std::fs::create_dir_all(base_dir).expect("failed to create model directory");

    // Try HuggingFace first (single files, ~230 MB total)
    match try_download_from_hf(&model_dir) {
        Ok(()) => {
            eprintln!("[dictation] model ready (from HuggingFace)");
            return model_dir.to_string_lossy().into_owned();
        }
        Err(e) => {
            eprintln!("[dictation] HuggingFace download failed: {e}");
            eprintln!("[dictation] falling back to GitHub releases...");
            // Clean up partial HF download
            std::fs::remove_dir_all(&model_dir).ok();
        }
    }

    // Fallback: GitHub tar.bz2 (~1 GB)
    try_download_from_github(base_dir).expect("failed to download model from both sources");
    eprintln!("[dictation] model ready (from GitHub)");
    model_dir.to_string_lossy().into_owned()
}

fn main() {
    let args = Args::parse();
    let use_sensevoice = args.engine == "sensevoice";

    #[cfg(not(feature = "sensevoice"))]
    if use_sensevoice {
        eprintln!("sensevoice engine requires building dictation with --features sensevoice");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("must run on main thread");

    let app = configure_accessory_app(mtm);

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
            eprintln!("[dictation] loading SenseVoice model from {model_dir}...");
            let recognizer = sherpa_rs::sense_voice::SenseVoiceRecognizer::new(config)
                .expect("failed to init SenseVoice — check --model-dir");
            eprintln!("[dictation] SenseVoice model loaded");
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
                    eprintln!("[dictation] speech recognition authorized");
                } else {
                    eprintln!(
                        "[dictation] speech recognition not authorized: {:?}",
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

    let run_loop_source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        .expect("failed to create run loop source");
    unsafe {
        let run_loop = CFRunLoop::current().expect("no current run loop");
        run_loop.add_source(Some(&run_loop_source), kCFRunLoopCommonModes);
    }

    // --- Menubar ---
    let status_item = new_status_item(-1.0);
    set_status_icon(&status_item, false, mtm);

    let delegate: Retained<MenuDelegate> =
        unsafe { objc2::msg_send![MenuDelegate::alloc(mtm), init] };
    let menu = NSMenu::new(mtm);
    let quit_item = new_menu_item(mtm, "Quit", Some(sel!(quit:)), "q");
    unsafe { quit_item.setTarget(Some(&delegate)) };
    menu.addItem(&quit_item);
    status_item.setMenu(Some(&menu));

    let engine_name = if use_sensevoice {
        "SenseVoice"
    } else {
        "Apple Speech"
    };
    eprintln!("[dictation] ready ({engine_name}) — hold right Command to dictate, release to type");

    // --- State ---
    let is_recording = Cell::new(false);
    let apple_request: Cell<Option<Retained<SFSpeechAudioBufferRecognitionRequest>>> =
        Cell::new(None);
    let accumulated_samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let native_sample_rate: Cell<u32> = Cell::new(16000);

    let _tap = tap;
    let _run_loop_source = run_loop_source;

    // --- Timer ---
    let _timer = unsafe {
        let samples_ref = accumulated_samples.clone();

        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            // --- START recording ---
            if let Some(_mode) = begin_requested_session(&SESSION_SIGNALS, &is_recording) {
                eprintln!("[dictation] recording started...");
                set_status_icon(&status_item, true, mtm);

                // Defensive: remove any stale tap from a previous failed start
                if use_sensevoice {
                    if let Ok(mut s) = samples_ref.lock() {
                        s.clear();
                    }
                    if let Err(e) = audio_engine.start_sample_capture(
                        samples_ref.clone(),
                        &native_sample_rate,
                        AudioCaptureConfig {
                            buffer_size: 4096,
                            use_none_format: false,
                            collect_gate: None,
                            rms_out: None,
                        },
                    ) {
                        eprintln!("[dictation] audio engine start error: {e}");
                        clear_recording_state(&SESSION_SIGNALS, &is_recording);
                        set_status_icon(&status_item, false, mtm);
                        return;
                    }
                    eprintln!(
                        "[dictation] native sample rate: {}Hz",
                        native_sample_rate.get()
                    );
                } else {
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
                            rms_out: None,
                        },
                    ) {
                        eprintln!("[dictation] audio engine start error: {e}");
                        SESSION_SIGNALS.set_recording(false);
                        set_status_icon(&status_item, false, mtm);
                        return;
                    }

                    let handler = RcBlock::new(
                        |result: *mut SFSpeechRecognitionResult,
                         error: *mut objc2_foundation::NSError| {
                            if !error.is_null() {
                                let error = &*error;
                                eprintln!(
                                    "[dictation] recognition error: {:?}",
                                    error.localizedDescription()
                                );
                            } else if !result.is_null() {
                                let result = &*result;
                                let text = result.bestTranscription().formattedString().to_string();
                                eprintln!("[dictation] partial: {}", text);
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
            }

            // --- STOP recording ---
            if take_stop_while_recording(&SESSION_SIGNALS, &is_recording) {
                #[cfg(feature = "sensevoice")]
                let stop_t0 = std::time::Instant::now();
                eprintln!("[dictation] recording stopped");
                set_status_icon(&status_item, false, mtm);

                audio_engine.stop_and_reset();

                if use_sensevoice {
                    let raw_samples = samples_ref.lock().unwrap();
                    if !raw_samples.is_empty() {
                        let sr = native_sample_rate.get();
                        // Downsample to 16kHz if needed
                        let samples_16k = resample_linear(&raw_samples, sr, 16000);
                        drop(raw_samples);

                        eprintln!(
                            "[dictation] transcribing {:.1}s of audio...",
                            samples_16k.len() as f64 / 16000.0
                        );
                        #[cfg(feature = "sensevoice")]
                        {
                            if let Some(mut recognizer) = sv_recognizer.take() {
                                let t0 = std::time::Instant::now();
                                let result = recognizer.transcribe(16000, &samples_16k);
                                let inference_ms = t0.elapsed().as_secs_f64() * 1000.0;
                                if !result.text.is_empty() {
                                    type_text(&result.text);
                                    let total_ms = stop_t0.elapsed().as_secs_f64() * 1000.0;
                                    eprintln!(
                                        "[dictation] result: {} (inference={:.0}ms, total={:.0}ms)",
                                        result.text, inference_ms, total_ms
                                    );
                                }
                                sv_recognizer.set(Some(recognizer));
                            }
                        }
                    }
                } else {
                    if let Some(req) = apple_request.take() {
                        req.endAudio();
                    }
                    NSRunLoop::currentRunLoop()
                        .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.5));

                    if let Ok(text) = RECOGNIZED_TEXT.lock() {
                        if !text.is_empty() {
                            eprintln!("[dictation] typing: {}", *text);
                            type_text(&text);
                        }
                    }
                    if let Ok(mut text) = RECOGNIZED_TEXT.lock() {
                        text.clear();
                    }
                }
            }
        });

        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &block)
    };

    app.run();
}
