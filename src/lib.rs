pub mod accessibility;
pub mod actions;
pub mod avfaudio;
pub mod core_graphics;
pub mod error;
pub mod input;
pub mod locator;
pub mod screen_capture;
pub mod screenshot;
pub mod tree_fmt;
pub mod vision;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

pub fn screenshot(rect: core_graphics::CGRect) -> Option<CFRetained<CGImage>> {
    use self::core_graphics::*;

    #[allow(deprecated)]
    CGWindowListCreateImage(rect, CGWindowListOption(1), 0, CGWindowImageOption(0))
}
