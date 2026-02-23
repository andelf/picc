use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_foundation::{NSArray, NSString};
use picc::vision;

fn main() {
    let req = vision::VNRecognizeTextRequest::new();

    let zh = NSString::from_str("zh-Hans");
    let en = NSString::from_str("en-US");
    let lang = NSArray::from_slice(&[&*zh, &*en]);
    req.setRecognitionLanguages(&lang);

    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let img = picc::screenshot(NSScreen::mainScreen(mtm).unwrap().frame());
    println!("pic => {:?}", img.is_some());

    let handler = vision::new_handler_with_cgimage(&img.unwrap());

    let reqs = NSArray::from_retained_slice(&[req.clone()]);
    let reqs: &NSArray<objc2_vision::VNRequest> =
        unsafe { &*((&*reqs) as *const _ as *const _) };
    vision::perform_requests(&handler, reqs).unwrap();

    if let Some(results) = req.results() {
        for item in results.iter() {
            let candidates = item.topCandidates(1);
            for candidate in candidates.iter() {
                println!("candidate.string(): {:?}", candidate.string());
            }
        }
    }
}
