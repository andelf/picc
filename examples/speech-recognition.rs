use icrate::block2::ConcreteBlock;
use icrate::ns_string;
use icrate::objc2::rc::{Id, Shared};
use icrate::objc2::{msg_send, msg_send_id, ClassType};
use icrate::Foundation::{NSDate, NSError, NSLocale, NSRunLoop};
use icrate::Speech::{self, SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognizer};

use picc::avfaudio::*;

fn main() {
    let recongnizer = unsafe {
        //let locale = NSLocale::initWithLocaleIdentifier(NSLocale::alloc(), ns_string!("en-US"));
        let locale = NSLocale::initWithLocaleIdentifier(NSLocale::alloc(), ns_string!("zh-CN"));
        SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale).unwrap()
    };

    // aquire authorization
    unsafe {
        // use block
        let handler = ConcreteBlock::new(|status: isize| {
            if status == Speech::SFSpeechRecognizerAuthorizationStatusAuthorized {
                println!("speech recognition authorized");
            } else {
                println!("unauth status: {}", status);
            }
        })
        .copy();
        SFSpeechRecognizer::requestAuthorization(&handler);
    }

    unsafe {
        let request: Id<SFSpeechAudioBufferRecognitionRequest, Shared> =
            msg_send_id![SFSpeechAudioBufferRecognitionRequest::class(), new];

        let audio_engine = AVAudioEngine::new();

        // println!("input node: {:?}", audio_engine.input_node());

        let microphone = audio_engine.input_node();
        let format = microphone.output_format_for_bus(0);
        // println!("format: {:?}", format);

        {
            let request = request.clone();
            microphone.install_tap_on_bus(0, 1024, &format, move |buffer, time| {
                //  println!("buffer: {:?}", buffer);
                //  println!("time: {:?}", time);
                msg_send![&request, appendAudioPCMBuffer: buffer]
            });
        }

        audio_engine.prepare();
        audio_engine.start().unwrap();

        let handler = ConcreteBlock::new(
            |result: *mut Speech::SFSpeechRecognitionResult, error: *mut NSError| {
                if !error.is_null() {
                    let error = &*error;
                    println!("error: {:?}", error.localizedDescription());
                    // println!("error: {:?}", error.userInfo(), );
                } else {
                    let result = &*result;
                    if result.isFinal() {
                        //let meta = result.speechRecognitionMetadata();
                        //println!("meta: {:?}", meta);
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
                            // println!("=> {:?}", res.segments().last());
                            /*let segs = res.segments();
                            for seg in segs.iter() {
                                println!("seg: {:?}", seg);
                            }*/
                        }
                    }
                }
            },
        )
        .copy();

        let task = recongnizer.recognitionTaskWithRequest_resultHandler(&request, &*handler);

        for _ in 0..10000 {
            //  println!("running {}", audio_engine.running());
            if task.isFinishing() {
                break;
            }
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(1.0));
        }
    }
}
