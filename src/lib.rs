pub mod avfaudio;
pub mod core_graphics;
pub mod vision;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

pub fn screenshot(rect: core_graphics::CGRect) -> Option<CFRetained<CGImage>> {
    use self::core_graphics::*;

    #[allow(deprecated)]
    CGWindowListCreateImage(rect, CGWindowListOption(1), 0, CGWindowImageOption(0))
}
