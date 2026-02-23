use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_foundation::{ns_string, NSDate, NSError, NSLocale, NSRunLoop};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus,
};

use picc::avfaudio::*;

fn main() {
    let recognizer = unsafe {
        let locale = NSLocale::initWithLocaleIdentifier(NSLocale::alloc(), ns_string!("zh-CN"));
        SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale).unwrap()
    };

    // acquire authorization
    unsafe {
        let handler = RcBlock::new(|status: SFSpeechRecognizerAuthorizationStatus| {
            if status == SFSpeechRecognizerAuthorizationStatus::Authorized {
                println!("speech recognition authorized");
            } else {
                println!("unauth status: {:?}", status);
            }
        });
        SFSpeechRecognizer::requestAuthorization(&handler);
    }

    unsafe {
        let request: Retained<SFSpeechAudioBufferRecognitionRequest> =
            SFSpeechAudioBufferRecognitionRequest::new();

        let audio_engine = AVAudioEngine::new();

        let microphone = audio_engine.inputNode();
        let format = microphone.outputFormatForBus(0);

        {
            let request = request.clone();
            let block = RcBlock::new(
                move |buffer: NonNull<AVAudioPCMBuffer>, _time: NonNull<AVAudioTime>| {
                    request.appendAudioPCMBuffer(buffer.as_ref());
                },
            );
            microphone.installTapOnBus_bufferSize_format_block(
                0,
                1024,
                Some(&format),
                &*block as *const _ as *mut _,
            );
        }

        audio_engine.prepare();
        audio_engine.startAndReturnError().unwrap();

        let handler = RcBlock::new(
            |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                if !error.is_null() {
                    let error = &*error;
                    println!("error: {:?}", error.localizedDescription());
                } else {
                    let result = &*result;
                    if result.isFinal() {
                        println!(
                            "final request: {:?}",
                            result.bestTranscription().formattedString()
                        );
                    } else {
                        let partial_results = result.transcriptions();
                        for res in partial_results.iter() {
                            let s = res.formattedString();
                            println!("partial: {:?}", s);
                            if s.to_string().contains("退出") {
                                println!("exit");
                                std::process::exit(0);
                            }
                        }
                    }
                }
            },
        );

        let task = recognizer.recognitionTaskWithRequest_resultHandler(&request, &*handler);

        for _ in 0..10000 {
            if task.isFinishing() {
                break;
            }
            NSRunLoop::currentRunLoop()
                .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(1.0));
        }
    }
}
