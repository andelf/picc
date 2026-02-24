use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, NSObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};

use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath, NSColor,
    NSEvent, NSPanel, NSResponder, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};

// -- 全局状态 --

struct ConfirmState {
    message: String,
    flash: bool,
}

static STATE: Mutex<ConfirmState> = Mutex::new(ConfirmState {
    message: String::new(),
    flash: false,
});

thread_local! {
    static EXIT_CODE: Cell<i32> = const { Cell::new(1) };
}

// -- ConfirmWindow: NSPanel 子类 --

#[derive(Debug)]
pub struct ConfirmWindowIvars {
    _dummy: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[ivars = ConfirmWindowIvars]
    #[name = "ConfirmWindow"]
    #[derive(Debug, PartialEq)]
    pub struct ConfirmWindow;

    #[allow(non_snake_case)]
    impl ConfirmWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn canBecomeKeyWindow(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn keyDown(&self, event: &NSEvent) {
            let key_code = event.keyCode();
            match key_code {
                // Y key
                16 => {
                    EXIT_CODE.with(|c| c.set(0));
                    std::process::exit(0);
                }
                // N key or ESC
                45 | 53 => {
                    std::process::exit(1);
                }
                // 其他键 → 闪动
                _ => {
                    {
                        STATE.lock().unwrap().flash = true;
                    }
                    refresh_view(self);

                    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
                        STATE.lock().unwrap().flash = false;
                        let mtm = MainThreadMarker::new().unwrap();
                        let app = NSApplication::sharedApplication(mtm);
                        if let Some(win) = app.keyWindow() {
                            if let Some(content) = win.contentView() {
                                if let Some(view) = content.subviews().firstObject() {
                                    view.display();
                                }
                            }
                        }
                    });
                    unsafe {
                        NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                            0.08, false, &block,
                        );
                    }
                }
            }
        }
    }
);

impl ConfirmWindow {
    fn new(screen: &NSScreen, mtm: MainThreadMarker) -> Retained<Self> {
        let frame = screen.frame();
        let this = Self::alloc(mtm).set_ivars(ConfirmWindowIvars {
            _dummy: Cell::new(false),
        });
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

// -- ConfirmView: NSView 子类 --

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[name = "ConfirmView"]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct ConfirmView;

    #[allow(non_snake_case)]
    impl ConfirmView {
        #[unsafe(method(drawRect:))]
        fn drawRect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let state = STATE.lock().unwrap();

            // 半透明黑色遮罩，闪动时降低 alpha
            let alpha = if state.flash { 0.5 } else { 0.75 };
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, alpha).setFill();
            NSBezierPath::fillRect(bounds);

            let cx = bounds.origin.x + bounds.size.width / 2.0;
            let cy = bounds.origin.y + bounds.size.height / 2.0;

            let message = state.message.clone();
            drop(state);

            // 警告图标 + 提示消息
            let warning_text = format!("\u{26a0}  {}", message);
            unsafe {
                draw_centered_text(&warning_text, cx, cy + 20.0, 28.0, true);
                draw_centered_text("[Y] Confirm    [N/ESC] Cancel", cx, cy - 40.0, 18.0, false);
            }
        }
    }
);

impl ConfirmView {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

/// 在指定位置居中绘制文字
unsafe fn draw_centered_text(text: &str, cx: f64, cy: f64, font_size: f64, white: bool) {
    let label = NSString::from_str(text);
    let font_cls = AnyClass::get(c"NSFont").unwrap();
    let font: Retained<NSObject> =
        msg_send![font_cls, monospacedDigitSystemFontOfSize: font_size, weight: 0.0_f64];

    let dict_cls = AnyClass::get(c"NSMutableDictionary").unwrap();
    let dict: Retained<NSObject> = msg_send![dict_cls, new];
    let font_key = NSString::from_str("NSFont");
    let color_key = NSString::from_str("NSColor");
    let color = if white {
        NSColor::whiteColor()
    } else {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.7, 0.7, 0.7, 1.0)
    };
    let _: () = msg_send![&dict, setObject: &*font, forKey: &*font_key];
    let _: () = msg_send![&dict, setObject: &*color, forKey: &*color_key];

    let text_size: NSSize = msg_send![&*label, sizeWithAttributes: &*dict];
    let text_pt = NSPoint::new(cx - text_size.width / 2.0, cy - text_size.height / 2.0);
    let _: () = msg_send![&*label, drawAtPoint: text_pt, withAttributes: &*dict];
}

fn refresh_view(window: &ConfirmWindow) {
    if let Some(content) = window.contentView() {
        if let Some(view) = content.subviews().firstObject() {
            view.display();
        }
    }
}

fn parse_message() -> String {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        return "Are you sure you want to continue?".to_string();
    }
    if args[1] == "--message" {
        if args.len() < 3 {
            return "Are you sure you want to continue?".to_string();
        }
        args[2].clone()
    } else {
        args[1].clone()
    }
}

fn main() {
    let message = parse_message();

    STATE.lock().unwrap().message = message;

    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let screen = NSScreen::mainScreen(mtm).unwrap();
    let window = ConfirmWindow::new(&screen, mtm);

    window.setFloatingPanel(true);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    window.setMovableByWindowBackground(false);
    window.setExcludedFromWindowsMenu(true);
    window.setAlphaValue(1.0);
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setHidesOnDeactivate(false);
    window.setRestorable(false);
    window.disableSnapshotRestoration();
    window.setLevel(1000);
    window.setMovable(false);

    // 添加 ConfirmView
    let frame = screen.frame();
    let confirm_view = ConfirmView::new(frame, mtm);
    window.contentView().unwrap().addSubview(&confirm_view);

    window.makeKeyAndOrderFront(None);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    app.run();

    std::process::exit(EXIT_CODE.with(|c| c.get()));
}
