pub mod core_graphics;
pub mod vision;
pub mod avfaudio;

pub fn screenshot(rect: core_graphics::CGRect) -> Option<core_graphics::CGImageRef> {
    use self::core_graphics::*;

    // kCGWindowListOptionOnScreenOnly=2, kCGNullWindowID=0
    let window_list = unsafe { CGWindowListCreate(2, 0) };
    if window_list.is_null() {
        return None;
    }
    let image = unsafe { CGWindowListCreateImageFromArray(rect, window_list, 0) };
    if image.is_null() {
        return None;
    }
    Some(image)
}
