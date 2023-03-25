use icrate::{
    AppKit::NSScreen,
    Foundation::{NSArray, NSDictionary, NSString},
};
use picc::vision;

fn main() {
    let req = vision::VNRecognizeTextRequest::new();

    // NOTE: According to the docs: when using multiple languages, the order of the languages in the array is significant.
    // The more complex language should be placed first in the array.
    let lang = NSArray::from_vec(vec![
        NSString::from_str("zh-Hans"),
        NSString::from_str("en-US"),
    ]);
    req.set_languages(&lang);

    let img = picc::screenshot(unsafe { NSScreen::mainScreen().unwrap().frame() });
    println!("pic => {:?}", img);

    let handler =
        vision::VNImageRequestHandler::new_with_cgimage(img.unwrap(), &NSDictionary::new());

    let reqs = NSArray::from_slice(&[req.clone()]);
    handler.perform(&reqs).unwrap();

    for item in req.results().iter() {
        for candidate in item.top_candidates(1).iter() {
            println!("candidate.string(): {:?}", candidate.string());
            // println!("candidate.confidence(): {:?}", candidate.confidence());
        }
    }
}
