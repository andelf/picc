//! Push-to-Talk Dictation Tool
//!
//! Hold right Command key to dictate speech, then types recognized text at the cursor.
//! Supports two engines: SenseVoice (offline, via sherpa-onnx) and Apple Speech API.
//!
//! Usage:
//!   dictation --engine sensevoice --model-dir PATH
//!   dictation --engine apple

use std::cell::Cell;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{ns_string, NSDate, NSLocale, NSRunLoop, NSString, NSTimer};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus,
};

use picc::input::type_text;

const NX_DEVICERCMDKEYMASK: u64 = 0x10;

static SHOULD_START: AtomicBool = AtomicBool::new(false);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
static RECOGNIZED_TEXT: Mutex<String> = Mutex::new(String::new());

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
    if let Some(button) = item.button(mtm) {
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            Some(&NSString::from_str("Dictation")),
        ) {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
    }
}

fn main() {
    let args = Args::parse();
    let use_sensevoice = args.engine == "sensevoice";

    let mtm = MainThreadMarker::new().expect("must run on main thread");

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // --- SenseVoice recognizer (if needed) ---
    let sv_recognizer: Cell<Option<sherpa_rs::sense_voice::SenseVoiceRecognizer>> = if use_sensevoice
    {
        let model_dir = args.model_dir.unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap();
            format!("{home}/.local/share/picc/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17")
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
            let locale =
                NSLocale::initWithLocaleIdentifier(NSLocale::alloc(), ns_string!("zh-CN"));
            SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale).unwrap()
        };
        unsafe {
            let handler = RcBlock::new(|status: SFSpeechRecognizerAuthorizationStatus| {
                if status == SFSpeechRecognizerAuthorizationStatus::Authorized {
                    eprintln!("[dictation] speech recognition authorized");
                } else {
                    eprintln!("[dictation] speech recognition not authorized: {:?}", status);
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

    let run_loop_source =
        CFMachPort::new_run_loop_source(None, Some(&tap), 0).expect("failed to create run loop source");
    unsafe {
        let run_loop = CFRunLoop::current().expect("no current run loop");
        run_loop.add_source(Some(&run_loop_source), kCFRunLoopCommonModes);
    }

    // --- Menubar ---
    let status_bar = NSStatusBar::systemStatusBar();
    let status_item = status_bar.statusItemWithLength(-1.0);
    set_status_icon(&status_item, false, mtm);

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
        "[dictation] ready ({engine_name}) — hold right Command to dictate, release to type"
    );

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
            if SHOULD_START.swap(false, Ordering::Relaxed) && !is_recording.get() {
                eprintln!("[dictation] recording started...");
                is_recording.set(true);
                set_status_icon(&status_item, true, mtm);

                let microphone = audio_engine.inputNode();

                if use_sensevoice {
                    // Clear sample buffer
                    if let Ok(mut s) = samples_ref.lock() {
                        s.clear();
                    }

                    // Use native format — we'll downsample to 16kHz later
                    let format = microphone.outputFormatForBus(0);
                    let sr = format.sampleRate() as u32;
                    native_sample_rate.set(sr);
                    eprintln!("[dictation] native sample rate: {sr}Hz");

                    let samples_tap = samples_ref.clone();
                    let tap_block = RcBlock::new(
                        move |buffer: NonNull<AVAudioPCMBuffer>,
                              _time: NonNull<AVAudioTime>| {
                            let buf = buffer.as_ref();
                            let float_data = buf.floatChannelData();
                            let frame_length = buf.frameLength();
                            if !float_data.is_null() && frame_length > 0 {
                                // Channel 0 (mono or first channel)
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
                    // Apple: clear previous text
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
                                    "[dictation] recognition error: {:?}",
                                    error.localizedDescription()
                                );
                            } else if !result.is_null() {
                                let result = &*result;
                                let text = result
                                    .bestTranscription()
                                    .formattedString()
                                    .to_string();
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

                audio_engine.prepare();
                if let Err(e) = audio_engine.startAndReturnError() {
                    eprintln!("[dictation] audio engine start error: {:?}", e);
                    is_recording.set(false);
                    set_status_icon(&status_item, false, mtm);
                    return;
                }
            }

            // --- STOP recording ---
            if SHOULD_STOP.swap(false, Ordering::Relaxed) && is_recording.get() {
                eprintln!("[dictation] recording stopped");
                is_recording.set(false);
                set_status_icon(&status_item, false, mtm);

                let microphone = audio_engine.inputNode();
                microphone.removeTapOnBus(0);
                audio_engine.stop();

                if use_sensevoice {
                    let raw_samples = samples_ref.lock().unwrap();
                    if !raw_samples.is_empty() {
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
                                    let b = raw_samples
                                        .get(idx + 1)
                                        .copied()
                                        .unwrap_or(a);
                                    a + (b - a) * frac as f32
                                })
                                .collect::<Vec<f32>>()
                        };
                        drop(raw_samples);

                        eprintln!(
                            "[dictation] transcribing {:.1}s of audio...",
                            samples_16k.len() as f64 / 16000.0
                        );
                        if let Some(mut recognizer) = sv_recognizer.take() {
                            let result = recognizer.transcribe(16000, &samples_16k);
                            if !result.text.is_empty() {
                                eprintln!("[dictation] result: {}", result.text);
                                type_text(&result.text);
                            }
                            sv_recognizer.set(Some(recognizer));
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
