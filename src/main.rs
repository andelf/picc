use std::cell::Cell;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};

use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSEvent,
    NSGraphicsContext, NSPanel, NSResponder, NSScreen, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSDate, NSPoint, NSRect, NSRunLoop, NSSize, NSString};
use picc::vision;

use std::sync::Mutex;
/// 选区坐标 (x, y, w, h)，AppKit 坐标系
static SEL_RECT: Mutex<[f64; 4]> = Mutex::new([0.0, 0.0, 0.0, 0.0]);

/// 保存 CGImage 到 PNG 文件
#[allow(dead_code)]
fn save_cgimage(image: &CGImage, path: &str) {
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
        let ok = CGImageDestinationFinalize(dest);
        println!("save_cgimage({}) => {}", path, ok);
    }
}

#[derive(Debug)]
pub struct SnapWindowIvars {
    start_pos: Cell<NSPoint>,
    end_pos: Cell<NSPoint>,
}

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[ivars = SnapWindowIvars]
    #[name = "SnapWindow"]
    #[derive(Debug, PartialEq)]
    pub struct SnapWindow;

    /// override NSResponder
    #[allow(non_snake_case)]
    impl SnapWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn canBecomeKeyWindow(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn canBecomeMainWindow(&self) -> bool {
            true
        }

        #[unsafe(method(mouseMoved:))]
        fn mouseMoved(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseDragged:))]
        fn mouseDragged(&self, _event: &NSEvent) {
            let loc = NSEvent::mouseLocation();
            let start_loc = self.ivars().start_pos.get();

            let x = f64::min(start_loc.x, loc.x);
            let y = f64::min(start_loc.y, loc.y);
            let w = f64::abs(start_loc.x - loc.x);
            let h = f64::abs(start_loc.y - loc.y);

            *SEL_RECT.lock().unwrap() = [x, y, w, h];

            let subviews = self.contentView().unwrap().subviews();
            let overlay_view = subviews.firstObject().unwrap();
            overlay_view.display();
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn acceptsFirstMouse(&self, _event: &NSEvent) -> bool {
            println!("acceptsFirstMouse");
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouseDown(&self, event: &NSEvent) {
            let loc = NSEvent::mouseLocation();
            if event.clickCount() == 2 {
                println!("double click {:?}", loc);
            } else {
                self.ivars().start_pos.set(loc);
            }
            // 重置选区
            *SEL_RECT.lock().unwrap() = [0.0, 0.0, 0.0, 0.0];
            self.contentView()
                .unwrap()
                .subviews()
                .firstObject()
                .unwrap()
                .display();
        }

        #[unsafe(method(mouseUp:))]
        fn mouseUp(&self, _event: &NSEvent) {
            let loc = NSEvent::mouseLocation();
            println!("mouseUp: {:?}", loc);
            self.ivars().end_pos.set(loc);

            let start_loc = self.ivars().start_pos.get();
            let end_loc = self.ivars().end_pos.get();

            let x = f64::min(start_loc.x, end_loc.x);
            let y = f64::min(start_loc.y, end_loc.y);
            let w = f64::abs(start_loc.x - end_loc.x);
            let h = f64::abs(start_loc.y - end_loc.y);
            if w < 10.0 || h < 10.0 {
                return;
            }

            // AppKit 坐标系 (左下角原点, y向上) -> CoreGraphics 坐标系 (左上角原点, y向下)
            let mtm = MainThreadMarker::new().unwrap();
            let screen_height = NSScreen::mainScreen(mtm).unwrap().frame().size.height;
            let cg_y = screen_height - (y + h);

            let rect = CGRect::new(CGPoint::new(x, cg_y), CGSize::new(w, h));
            println!("Crop Rect: {:?}", rect);

            // 隐藏覆盖窗口，避免截图时把遮罩层也截进去
            self.orderOut(None);
            NSRunLoop::currentRunLoop()
                .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.1));

            let crop_img = picc::screenshot(rect).unwrap();

            // 截图完成，恢复窗口
            self.makeKeyAndOrderFront(None);

            println!(
                "=> crop_img {}x{}",
                CGImage::width(Some(&crop_img)),
                CGImage::height(Some(&crop_img)),
            );

            ocr(&crop_img);
        }

        #[unsafe(method(keyDown:))]
        fn keyDown(&self, event: &NSEvent) {
            println!("FR => {:?}", self.frame());
            let mtm = MainThreadMarker::new().unwrap();
            if event.keyCode() == 53 {
                println!("ESC");
                self.orderOut(None);
                self.close();
                NSApplication::sharedApplication(mtm).terminate(None);
            } else if event.keyCode() == 36 {
                println!("ENTER");
                self.toggleFullScreen(None);
            } else if event.keyCode() == 12 {
                println!("quit");
                self.orderOut(None);
                self.close();
                NSApplication::sharedApplication(mtm).terminate(None);
            } else if event.keyCode() == 49 {
                println!("SPACE");
                self.contentView()
                    .unwrap()
                    .subviews()
                    .firstObject()
                    .unwrap()
                    .setHidden(true);
            }
            println!("keyDown: {:?}", event);
        }
    }
);

impl SnapWindow {
    fn new(screen: &NSScreen, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SnapWindowIvars {
            start_pos: Cell::new(NSPoint::new(0.0, 0.0)),
            end_pos: Cell::new(NSPoint::new(0.0, 0.0)),
        });
        let frame = screen.frame();
        unsafe {
            msg_send![
                super(this),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false,
                screen: screen,
            ]
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[name = "DrawPathView"]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct DrawPathView;

    #[allow(non_snake_case)]
    impl DrawPathView {
        #[unsafe(method(drawRect:))]
        fn drawRect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let [x, y, w, h] = *SEL_RECT.lock().unwrap();

            // 整个屏幕画半透明黑色遮罩
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.3).setFill();
            NSBezierPath::fillRect(bounds);

            // 如果有选区，挖空选区并画红色边框
            if w > 1.0 && h > 1.0 {
                let sel_rect = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

                // 用 Copy 模式 + clearColor 擦除选区内的遮罩
                let ctx = NSGraphicsContext::currentContext().unwrap();
                ctx.setCompositingOperation(NSCompositingOperation::Copy);
                NSColor::clearColor().setFill();
                NSBezierPath::fillRect(sel_rect);

                // 恢复正常模式，画红色边框
                ctx.setCompositingOperation(NSCompositingOperation::SourceOver);
                NSColor::colorWithSRGBRed_green_blue_alpha(0.4, 0.6, 1.0, 0.8).setStroke();
                let path = NSBezierPath::bezierPathWithRect(sel_rect);
                path.setLineWidth(2.);
                path.stroke();
            }
        }
    }
);

impl DrawPathView {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

fn ocr(img: &CGImage) {
    let req = vision::VNRecognizeTextRequest::new();

    let zh = NSString::from_str("zh-Hans");
    let en = NSString::from_str("en-US");
    let lang = NSArray::from_slice(&[&*zh, &*en]);
    req.setRecognitionLanguages(&lang);

    let handler = vision::new_handler_with_cgimage(img);

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

fn main() {
    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let app = NSApplication::sharedApplication(mtm);

    let window = {
        let screen = NSScreen::mainScreen(mtm).unwrap();
        println!("Screen size {:?}", screen.frame());
        let win = SnapWindow::new(&screen, mtm);

        win.setAcceptsMouseMovedEvents(true);
        win.setFloatingPanel(true);
        win.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        win.setMovableByWindowBackground(false);
        win.setExcludedFromWindowsMenu(true);
        win.setAlphaValue(1.0);
        win.setOpaque(false);
        win.setBackgroundColor(Some(&NSColor::clearColor()));
        win.setHasShadow(false);
        win.setHidesOnDeactivate(false);

        win.setRestorable(false);
        win.disableSnapshotRestoration();
        // kCGScreenSaverWindowLevel = 1000, 高于 Dock(~20) 和菜单栏(~24)
        win.setLevel(1000);

        win.setMovable(false);
        win
    };

    window.makeKeyAndOrderFront(None);

    let frame = NSScreen::mainScreen(mtm).unwrap().frame();
    window.setFrame_display_animate(frame, true, false);

    let frame = NSScreen::mainScreen(mtm).unwrap().frame();
    let path_view = DrawPathView::new(frame, mtm);
    window.contentView().unwrap().addSubview(&path_view);

    println!("=> subview {:?}", window.contentView());

    app.run();
}
