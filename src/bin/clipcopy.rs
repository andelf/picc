use std::io::Read;

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

fn main() {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).unwrap();

    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let ns_str = NSString::from_str(&buf);
    pb.setString_forType(&ns_str, unsafe { NSPasteboardTypeString });
}
