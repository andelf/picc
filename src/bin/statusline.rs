use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, NSObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};

use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSPanel, NSResponder, NSScreen,
    NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};
use picc_macos_app::configure_accessory_app;

// -- 全局状态 --

static TEXT: Mutex<String> = Mutex::new(String::new());

thread_local! {
    static WINDOW: RefCell<Option<Retained<StatusPanel>>> = const { RefCell::new(None) };
}

// -- StatusPanel: NSPanel 子类 --

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[name = "StatusPanel"]
    #[derive(Debug, PartialEq)]
    pub struct StatusPanel;

    #[allow(non_snake_case)]
    impl StatusPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn canBecomeKeyWindow(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn canBecomeMainWindow(&self) -> bool {
            false
        }
    }
);

impl StatusPanel {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let win: Retained<Self> = unsafe {
            msg_send![
                Self::alloc(mtm),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };
        win
    }
}

// -- StatusView: 绘制文字的 NSView --

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[name = "StatusView"]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct StatusView;

    #[allow(non_snake_case)]
    impl StatusView {
        #[unsafe(method(drawRect:))]
        fn drawRect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let text = TEXT.lock().unwrap().clone();

            // 圆角半透明黑色背景
            let radius = 6.0;
            let bg = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, radius, radius);
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.75).setFill();
            bg.fill();

            if text.is_empty() {
                return;
            }

            // 绘制白色文字
            let pad_x = 12.0;
            let pad_y = 6.0;

            unsafe {
                let (dict, _font) = make_text_attrs(FONT_SIZE);
                let label = NSString::from_str(&text);

                // 限定绘制区域
                let draw_rect = NSRect::new(
                    NSPoint::new(pad_x, pad_y),
                    NSSize::new(bounds.size.width - pad_x * 2.0, bounds.size.height - pad_y * 2.0),
                );
                let _: () = msg_send![&*label, drawInRect: draw_rect, withAttributes: &*dict];
            }
        }
    }
);

impl StatusView {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

const FONT_SIZE: f64 = 13.0;
const PAD_X: f64 = 12.0;
const PAD_Y: f64 = 6.0;
const MARGIN_BOTTOM: f64 = 4.0;
const MARGIN_RIGHT: f64 = 4.0;

/// 创建文字绘制属性字典，返回 (dict, font) 保持 font 存活
unsafe fn make_text_attrs(font_size: f64) -> (Retained<NSObject>, Retained<NSObject>) {
    let font_cls = AnyClass::get(c"NSFont").unwrap();
    let font: Retained<NSObject> =
        msg_send![font_cls, systemFontOfSize: font_size, weight: 0.0_f64];

    let dict_cls = AnyClass::get(c"NSMutableDictionary").unwrap();
    let dict: Retained<NSObject> = msg_send![dict_cls, new];

    let font_key = NSString::from_str("NSFont");
    let color_key = NSString::from_str("NSColor");
    let white = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.9);
    let _: () = msg_send![&dict, setObject: &*font, forKey: &*font_key];
    let _: () = msg_send![&dict, setObject: &*white, forKey: &*color_key];

    (dict, font)
}

/// 计算文字在指定宽度内的 bounding size
unsafe fn measure_text(text: &str, max_width: f64) -> NSSize {
    let label = NSString::from_str(text);
    let (dict, _font) = make_text_attrs(FONT_SIZE);
    let constraint = CGSize::new(max_width, 10000.0);
    let options: u64 = 1 << 0 | 1 << 1; // UsesLineFragmentOrigin | UsesFontLeading
    let rect: NSRect = msg_send![
        &*label,
        boundingRectWithSize: constraint,
        options: options,
        attributes: &*dict,
    ];
    rect.size
}

/// 根据当前文字内容重新计算窗口大小并更新位置
fn relayout() {
    let mtm = MainThreadMarker::new().unwrap();
    let Some(screen) = NSScreen::mainScreen(mtm) else {
        return;
    };
    let screen_frame = screen.frame();
    let text = TEXT.lock().unwrap().clone();

    let content_width = screen_frame.size.width / 2.0 - MARGIN_RIGHT;
    let text_width = content_width - PAD_X * 2.0;

    let text_height = if text.is_empty() {
        FONT_SIZE * 1.4 // 单行高度
    } else {
        let size = unsafe { measure_text(&text, text_width) };
        size.height.ceil()
    };

    let win_height = text_height + PAD_Y * 2.0;
    let win_width = content_width;

    // 底边固定在屏幕底部，从屏幕中点到右边
    let win_x = screen_frame.origin.x + screen_frame.size.width / 2.0;
    let win_y = screen_frame.origin.y + MARGIN_BOTTOM;

    let win_frame = NSRect::new(NSPoint::new(win_x, win_y), NSSize::new(win_width, win_height));

    WINDOW.with(|w| {
        if let Some(win) = w.borrow().as_ref() {
            win.setFrame_display_animate(win_frame, true, false);
            // 更新 view frame
            if let Some(content) = win.contentView() {
                if let Some(view) = content.subviews().firstObject() {
                    let local_frame =
                        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(win_width, win_height));
                    view.setFrame(local_frame);
                    view.display();
                }
            }
        }
    });
}

fn set_text(s: &str) {
    *TEXT.lock().unwrap() = s.to_string();
    relayout();
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let app = configure_accessory_app(mtm);

    let screen = NSScreen::mainScreen(mtm).expect("no main screen");
    let screen_frame = screen.frame();

    // 初始窗口：屏幕右半部分底端
    let win_width = screen_frame.size.width / 2.0 - MARGIN_RIGHT;
    let win_height = FONT_SIZE * 1.4 + PAD_Y * 2.0;
    let win_x = screen_frame.origin.x + screen_frame.size.width / 2.0;
    let win_y = screen_frame.origin.y + MARGIN_BOTTOM;

    let win_frame = NSRect::new(NSPoint::new(win_x, win_y), NSSize::new(win_width, win_height));
    let win = StatusPanel::new(win_frame, mtm);

    win.setFloatingPanel(true);
    win.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
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
    win.setLevel(25); // kCGStatusWindowLevel, 高于普通窗口但低于屏保
    win.setMovable(false);
    win.setIgnoresMouseEvents(true);

    // 添加绘制 view
    let local_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(win_width, win_height));
    let view = StatusView::new(local_frame, mtm);
    win.contentView().unwrap().addSubview(&view);

    win.orderFrontRegardless();

    WINDOW.with(|w| {
        *w.borrow_mut() = Some(win);
    });

    // 设置初始文字
    set_text("Statusline ready.");

    // 演示：定时切换文字内容
    let demo_step: Mutex<u32> = Mutex::new(0);
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        let mut step = demo_step.lock().unwrap();
        match *step {
            0 => set_text("Statusline ready."),
            1 => set_text("Voice input: 你好世界"),
            2 => set_text("Recognition result:\n这是一段多行文字测试"),
            3 => set_text("Line 1\nLine 2\nLine 3"),
            _ => {
                *step = 0;
                set_text("Statusline ready.");
                return;
            }
        }
        *step += 1;
    });
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(3.0, true, &block);
    }

    println!("Statusline running. Ctrl+C to quit.");
    app.run();
}
