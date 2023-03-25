use std::ffi::c_void;

pub use icrate::Foundation::CGRect;

pub type CGImageRef = *mut c_void;

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
}
