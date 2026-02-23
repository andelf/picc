use objc2_foundation::{ns_string, NSArray, NSString, NSURL};
use picc::vision;

fn main() {
    let req = vision::VNRecognizeTextRequest::new();

    let supported_languages = unsafe { req.supportedRecognitionLanguagesAndReturnError() };
    println!("supported_languages: {:?}", supported_languages);

    let zh = NSString::from_str("zh-Hans");
    let en = NSString::from_str("en-US");
    let lang = NSArray::from_slice(&[&*zh, &*en]);
    req.setRecognitionLanguages(&lang);
    let languages = unsafe { req.recognitionLanguages() };
    println!("using languages: {:?}", languages);

    let url = NSURL::fileURLWithPath(ns_string!("./docker-sb.jpg"));

    let handler = vision::new_handler_with_url(&url);

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
