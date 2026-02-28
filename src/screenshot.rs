//! Screenshot capture and image saving utilities.

use std::ffi::c_void;

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
#[allow(deprecated)]
use objc2_core_graphics::{CGImage, CGWindowImageOption, CGWindowListCreateImage, CGWindowListOption};
use objc2_foundation::NSString;

/// Capture a screenshot of the given screen rectangle.
#[allow(deprecated)]
pub fn capture(rect: CGRect) -> Option<CFRetained<CGImage>> {
    CGWindowListCreateImage(rect, CGWindowListOption(1), 0, CGWindowImageOption(0))
}

/// Capture a full-screen screenshot.
pub fn capture_full() -> Option<CFRetained<CGImage>> {
    capture(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0)))
}

/// Save a CGImage as PNG to the given file path. Returns true on success.
pub fn save_png(image: &CGImage, path: &str) -> bool {
    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageDestinationCreateWithURL(
            url: *const c_void,
            ty: *const c_void,
            count: usize,
            options: *const c_void,
        ) -> *mut c_void;
        fn CGImageDestinationAddImage(
            dest: *mut c_void,
            image: *const c_void,
            properties: *const c_void,
        );
        fn CGImageDestinationFinalize(dest: *mut c_void) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFURLCreateWithFileSystemPath(
            allocator: *const c_void,
            path: *const c_void,
            style: i32,
            is_dir: bool,
        ) -> *const c_void;
    }

    unsafe {
        let ns_path = NSString::from_str(path);
        let url = CFURLCreateWithFileSystemPath(
            std::ptr::null(),
            (&*ns_path as *const NSString).cast(),
            0, // kCFURLPOSIXPathStyle
            false,
        );
        let png_type = NSString::from_str("public.png");
        let dest = CGImageDestinationCreateWithURL(
            url,
            (&*png_type as *const NSString).cast(),
            1,
            std::ptr::null(),
        );
        CGImageDestinationAddImage(dest, (image as *const CGImage).cast(), std::ptr::null());
        CGImageDestinationFinalize(dest)
    }
}
