use std::ffi::c_void;
use std::mem;

use icrate::objc2::{Encoding, RefEncode};
pub use icrate::Foundation::CGRect;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CGImage([u8; 0]); // to make it FFI-safe
pub type CGImageRef = *mut CGImage;

// Required for use CGImageRef in `msg_send!` macro
unsafe impl RefEncode for CGImage {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}

impl CGImage {
    pub fn width(&self) -> usize {
        unsafe { CGImageGetWidth(mem::transmute(self)) }
    }

    pub fn height(&self) -> usize {
        unsafe { CGImageGetHeight(mem::transmute(self)) }
    }
}

type CGWindowListOption = u32;
type CGWindowID = u32;
type CGWindowImageOption = u32;

type CFArrayRef = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGWindowListCreate(
        option: CGWindowListOption,
        relativeToWindow: CGWindowID,
    ) -> CFArrayRef;
    pub fn CGWindowListCreateImageFromArray(
        screenBounds: CGRect,
        windowArray: CFArrayRef,
        imageOption: CGWindowImageOption,
    ) -> CGImageRef;
    pub fn CGImageRetain(image: CGImageRef) -> CGImageRef;
    pub fn CGImageRelease(image: CGImageRef);
    pub fn CGImageIsMask(image: CGImageRef) -> bool;
    pub fn CGImageGetWidth(image: CGImageRef) -> usize;
    pub fn CGImageGetHeight(image: CGImageRef) -> usize;
}
