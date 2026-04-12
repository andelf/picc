//! Pause-aware dictation experiment for SenseVoice.
//!
//! Hold right Command to record. When a short pause is detected, the tool
//! transcribes all captured audio so far and rewrites the current session text.
//! Releasing the key runs one final transcription and overwrites the temporary
//! text with the final result.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSPasteboard,
    NSPasteboardTypeString, NSRunningApplication, NSStatusBar, NSStatusItem,
};
use objc2_application_services::AXUIElement;
use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};
use objc2_core_foundation::{
    kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop, CFString, CFType,
};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{NSString, NSTimer};

use picc::accessibility;
use picc_macos_input::{parse_key_combo, press_key_combo, type_text};

const NX_DEVICERCMDKEYMASK: u64 = 0x10;

static SHOULD_START: AtomicBool = AtomicBool::new(false);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
static COLLECTING: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(about = "Pause-aware SenseVoice dictation experiment")]
struct Args {
    /// SenseVoice model directory
    #[arg(long)]
    model_dir: Option<String>,

    /// Language hint for SenseVoice: auto, zh, en, ja, ko, yue
    #[arg(long, default_value = "auto")]
    lang: String,

    /// Pause duration before a partial rewrite is triggered
    #[arg(long, default_value_t = 450)]
    pause_ms: u32,

    /// Minimum captured speech before partial rewrite is allowed
    #[arg(long, default_value_t = 350)]
    min_speech_ms: u32,

    /// RMS energy threshold for simple VAD
    #[arg(long, default_value_t = 0.009)]
    vad_threshold: f32,

    /// Minimum voiced time after a partial before another partial is allowed
    #[arg(long, default_value_t = 250)]
    resume_speech_ms: u32,
}

#[derive(Debug, Clone)]
struct FocusSession {
    element: CFRetained<AXUIElement>,
    base_text: String,
    selection_start_utf16: usize,
    selection_len_utf16: usize,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DictationPauseMenuDelegate"]
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
            WAS_DOWN.store(true, Ordering::Relaxed);
            SHOULD_START.store(true, Ordering::Relaxed);
        } else if !right_cmd_pressed && was_down {
            WAS_DOWN.store(false, Ordering::Relaxed);
            SHOULD_STOP.store(true, Ordering::Relaxed);
        }
    }
    event.as_ptr()
}

fn default_model_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.local/share/picc/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17")
}

fn set_status_icon(item: &NSStatusItem, state: &str, mtm: MainThreadMarker) {
    let symbol = match state {
        "recording" => "mic.fill",
        "processing" => "waveform",
        _ => "mic",
    };
    if let Some(button) = item.button(mtm) {
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            Some(&NSString::from_str("Dictation Pause")),
        ) {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
    }
}

fn focused_ax_element() -> Option<CFRetained<AXUIElement>> {
    let system = unsafe { AXUIElement::new_system_wide() };
    let cf = accessibility::attr_value(&system, "AXFocusedUIElement")?;
    Some(unsafe { CFRetained::cast_unchecked(cf) })
}

fn ax_value_as_cfrange(value: &CFRetained<CFType>) -> Option<objc2_core_foundation::CFRange> {
    let ax_value: &objc2_application_services::AXValue = unsafe {
        &*(value.as_ref() as *const CFType as *const objc2_application_services::AXValue)
    };
    let mut range = objc2_core_foundation::CFRange {
        location: 0,
        length: 0,
    };
    let ok = unsafe {
        ax_value.value(
            objc2_application_services::AXValueType::CFRange,
            NonNull::new_unchecked(&mut range as *mut _ as *mut std::ffi::c_void),
        )
    };
    ok.then_some(range)
}

fn read_focus_session() -> Option<FocusSession> {
    let element = focused_ax_element()?;
    let base_text = accessibility::attr_string(&element, "AXValue").unwrap_or_default();
    let range_cf = accessibility::attr_value(&element, "AXSelectedTextRange");
    let (selection_start_utf16, selection_len_utf16) =
        match range_cf.and_then(|v| ax_value_as_cfrange(&v)) {
            Some(range) => (range.location.max(0) as usize, range.length.max(0) as usize),
            None => (base_text.encode_utf16().count(), 0),
        };
    Some(FocusSession {
        element,
        base_text,
        selection_start_utf16,
        selection_len_utf16,
    })
}

fn utf16_to_byte_index(text: &str, utf16_offset: usize) -> usize {
    let mut seen = 0usize;
    for (idx, ch) in text.char_indices() {
        if seen >= utf16_offset {
            return idx;
        }
        seen += ch.len_utf16();
        if seen > utf16_offset {
            return idx + ch.len_utf8();
        }
    }
    text.len()
}

fn compose_session_text(session: &FocusSession, inserted: &str) -> String {
    let start = utf16_to_byte_index(&session.base_text, session.selection_start_utf16);
    let end = utf16_to_byte_index(
        &session.base_text,
        session.selection_start_utf16 + session.selection_len_utf16,
    );
    let mut out = String::with_capacity(session.base_text.len() + inserted.len());
    out.push_str(&session.base_text[..start]);
    out.push_str(inserted);
    out.push_str(&session.base_text[end..]);
    out
}

fn frontmost_bundle_id() -> Option<String> {
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

const CLIPBOARD_ONLY_BUNDLES: &[&str] = &[
    "com.google.Chrome",
    "org.chromium.Chromium",
    "com.apple.Safari",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "com.electron.",
    "us.zoom.xos",
    "com.larksuite.Lark",
    "com.larksuite.larkApp",
    "com.bytedance.lark.Feishu",
];

fn should_use_clipboard() -> bool {
    frontmost_bundle_id()
        .map(|b| matches_bundle_list(&b, CLIPBOARD_ONLY_BUNDLES))
        .unwrap_or(false)
}

fn replace_via_clipboard(text: &str) -> bool {
    let pb = NSPasteboard::generalPasteboard();
    let pb_type = unsafe { NSPasteboardTypeString };
    let old = pb.stringForType(pb_type);

    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), pb_type);

    let (keycode, flags) = parse_key_combo("Command+a");
    press_key_combo(keycode, flags);
    std::thread::sleep(Duration::from_millis(30));

    let (keycode, flags) = parse_key_combo("Command+v");
    press_key_combo(keycode, flags);
    std::thread::sleep(Duration::from_millis(100));

    pb.clearContents();
    if let Some(ref old_text) = old {
        pb.setString_forType(old_text, pb_type);
    }

    true
}

fn write_text(element: &AXUIElement, text: &str) -> bool {
    if should_use_clipboard() {
        return replace_via_clipboard(text);
    }

    let cf_str = CFString::from_str(text);
    let cf_type: &CFType = cf_str.as_ref();
    if accessibility::set_attr_value(element, "AXValue", cf_type) {
        if let Some(readback) = accessibility::attr_string(element, "AXValue") {
            if readback == text {
                return true;
            }
        }
    }

    replace_via_clipboard(text)
}

fn write_session_result(session: &FocusSession, inserted: &str) -> bool {
    let current = accessibility::attr_string(&session.element, "AXValue").unwrap_or_default();
    if current != session.base_text {
        eprintln!("[dictation-pause] refusing rewrite because focused text changed externally");
        return false;
    }
    let full_text = compose_session_text(session, inserted);
    write_text(&session.element, &full_text)
}

fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples
        .iter()
        .map(|s| (*s as f64) * (*s as f64))
        .sum::<f64>();
    (sum / samples.len() as f64).sqrt() as f32
}

fn resample_to_16k(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16000 {
        return samples.to_vec();
    }
    let ratio = source_rate as f64 / 16000.0;
    let out_len = (samples.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let src = i as f64 * ratio;
            let idx = src as usize;
            let frac = src - idx as f64;
            let a = samples[idx];
            let b = samples.get(idx + 1).copied().unwrap_or(a);
            a + (b - a) * frac as f32
        })
        .collect()
}

fn transcribe_all(
    recognizer: &mut sherpa_rs::sense_voice::SenseVoiceRecognizer,
    native_rate: u32,
    samples: &[f32],
) -> sherpa_rs::sense_voice::SenseVoiceRecognizerResult {
    let samples_16k = resample_to_16k(samples, native_rate);
    recognizer.transcribe(16000, &samples_16k)
}

fn main() {
    let args = Args::parse();
    let mtm = MainThreadMarker::new().expect("must run on main thread");

    let model_dir = args.model_dir.unwrap_or_else(default_model_dir);
    assert!(
        Path::new(&model_dir).join("model.int8.onnx").exists(),
        "SenseVoice model not found in {}",
        model_dir
    );
    assert!(
        Path::new(&model_dir).join("tokens.txt").exists(),
        "tokens.txt not found in {}",
        model_dir
    );

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    if !accessibility::is_trusted() {
        eprintln!("warning: Accessibility not trusted");
    }

    let delegate: Retained<MenuDelegate> =
        unsafe { objc2::msg_send![MenuDelegate::alloc(mtm), init] };
    let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0);
    set_status_icon(&status_item, "idle", mtm);
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

    let recognizer = RefCell::new(
        sherpa_rs::sense_voice::SenseVoiceRecognizer::new(
            sherpa_rs::sense_voice::SenseVoiceConfig {
                model: format!("{model_dir}/model.int8.onnx"),
                tokens: format!("{model_dir}/tokens.txt"),
                language: args.lang.clone(),
                use_itn: true,
                ..Default::default()
            },
        )
        .expect("failed to init SenseVoice"),
    );

    let audio_engine = unsafe { AVAudioEngine::new() };
    let sample_buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

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
    .expect("failed to create event tap");

    let run_loop_source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        .expect("failed to create run loop source");
    unsafe {
        let current = CFRunLoop::current().expect("no current runloop");
        current.add_source(Some(&run_loop_source), kCFRunLoopCommonModes);
    }

    let is_recording = Cell::new(false);
    let session: RefCell<Option<FocusSession>> = RefCell::new(None);
    let native_sample_rate = Cell::new(16000u32);
    let all_samples: RefCell<Vec<f32>> = RefCell::new(Vec::new());
    let partial_text: RefCell<String> = RefCell::new(String::new());
    let speech_started = Cell::new(false);
    let silence_ms = Cell::new(0u32);
    let pause_fired = Cell::new(false);
    let voiced_ms_since_partial = Cell::new(0u32);

    let _timer = unsafe {
        let samples_ref = sample_buf.clone();
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            if SHOULD_START.swap(false, Ordering::Relaxed) && !is_recording.get() {
                let microphone = audio_engine.inputNode();
                if let Ok(mut buf) = samples_ref.lock() {
                    buf.clear();
                }
                all_samples.borrow_mut().clear();
                partial_text.borrow_mut().clear();
                speech_started.set(false);
                silence_ms.set(0);
                pause_fired.set(false);
                voiced_ms_since_partial.set(0);
                *session.borrow_mut() = read_focus_session();

                let tap_samples = samples_ref.clone();
                let tap_block = RcBlock::new(
                    move |buffer: NonNull<AVAudioPCMBuffer>, _time: NonNull<AVAudioTime>| {
                        if !COLLECTING.load(Ordering::Relaxed) {
                            return;
                        }
                        let buf = buffer.as_ref();
                        let float_data = buf.floatChannelData();
                        let frame_length = buf.frameLength();
                        if !float_data.is_null() && frame_length > 0 {
                            let channel0 = (*float_data).as_ptr();
                            let slice = std::slice::from_raw_parts(channel0, frame_length as usize);
                            if let Ok(mut samples) = tap_samples.lock() {
                                samples.extend_from_slice(slice);
                            }
                        }
                    },
                );
                microphone.installTapOnBus_bufferSize_format_block(
                    0,
                    2048,
                    None,
                    &*tap_block as *const _ as *mut _,
                );
                native_sample_rate.set(microphone.outputFormatForBus(0).sampleRate() as u32);
                audio_engine.prepare();
                audio_engine
                    .startAndReturnError()
                    .expect("failed to start audio engine");
                COLLECTING.store(true, Ordering::Relaxed);
                is_recording.set(true);
                set_status_icon(&status_item, "recording", mtm);
                eprintln!("[dictation-pause] start");
            }

            if is_recording.get() {
                let new_chunk = {
                    let mut buf = match samples_ref.lock() {
                        Ok(guard) => guard,
                        Err(_) => return,
                    };
                    if buf.is_empty() {
                        Vec::new()
                    } else {
                        std::mem::take(&mut *buf)
                    }
                };

                if !new_chunk.is_empty() {
                    let mut collected = all_samples.borrow_mut();
                    collected.extend_from_slice(&new_chunk);

                    let sr = native_sample_rate.get().max(1);
                    let chunk_ms = ((new_chunk.len() as f64 / sr as f64) * 1000.0) as u32;
                    let energy = rms_energy(&new_chunk);
                    if energy >= args.vad_threshold {
                        speech_started.set(true);
                        silence_ms.set(0);
                        if pause_fired.get() {
                            let resumed = voiced_ms_since_partial.get().saturating_add(chunk_ms);
                            voiced_ms_since_partial.set(resumed);
                            if resumed >= args.resume_speech_ms {
                                pause_fired.set(false);
                                voiced_ms_since_partial.set(0);
                            }
                        } else {
                            voiced_ms_since_partial.set(0);
                        }
                    } else if speech_started.get() {
                        silence_ms.set(silence_ms.get().saturating_add(chunk_ms));
                    }

                    let snapshot = collected.clone();
                    drop(collected);
                    let total_ms = ((snapshot.len() as f64 / sr as f64) * 1000.0) as u32;
                    if speech_started.get()
                        && !pause_fired.get()
                        && silence_ms.get() >= args.pause_ms
                        && total_ms >= args.min_speech_ms
                    {
                        set_status_icon(&status_item, "processing", mtm);
                        let result = transcribe_all(
                            &mut recognizer.borrow_mut(),
                            native_sample_rate.get(),
                            &snapshot,
                        );
                        eprintln!(
                            "[dictation-pause] partial lang={} text={}",
                            result.lang, result.text
                        );
                        if !result.text.is_empty() {
                            if let Some(ref focused) = *session.borrow() {
                                let _ = write_session_result(focused, &result.text);
                            }
                            *partial_text.borrow_mut() = result.text;
                        }
                        pause_fired.set(true);
                        voiced_ms_since_partial.set(0);
                        set_status_icon(&status_item, "recording", mtm);
                    }
                }
            }

            if SHOULD_STOP.swap(false, Ordering::Relaxed) && is_recording.get() {
                COLLECTING.store(false, Ordering::Relaxed);
                let microphone = audio_engine.inputNode();
                microphone.removeTapOnBus(0);
                audio_engine.stop();
                is_recording.set(false);
                set_status_icon(&status_item, "processing", mtm);

                let samples = all_samples.borrow().clone();
                eprintln!(
                    "[dictation-pause] stop duration_s={:.2}",
                    samples.len() as f64 / native_sample_rate.get().max(1) as f64
                );
                if !samples.is_empty() {
                    let result = transcribe_all(
                        &mut recognizer.borrow_mut(),
                        native_sample_rate.get(),
                        &samples,
                    );
                    eprintln!(
                        "[dictation-pause] final lang={} text={}",
                        result.lang, result.text
                    );
                    if let Some(ref focused) = *session.borrow() {
                        if !result.text.is_empty() {
                            let _ = write_session_result(focused, &result.text);
                        } else {
                            let partial = partial_text.borrow().clone();
                            if !partial.is_empty() {
                                let _ = write_session_result(focused, &partial);
                            }
                        }
                    } else if !result.text.is_empty() {
                        type_text(&result.text);
                    }
                }

                *session.borrow_mut() = None;
                partial_text.borrow_mut().clear();
                all_samples.borrow_mut().clear();
                speech_started.set(false);
                silence_ms.set(0);
                pause_fired.set(false);
                voiced_ms_since_partial.set(0);
                set_status_icon(&status_item, "idle", mtm);
            }
        });

        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &block)
    };

    app.run();
}
