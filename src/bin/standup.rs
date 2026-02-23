use std::cell::{Cell, RefCell};
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

struct StandupState {
    remaining_secs: u32,
    work_secs: f64,
    break_secs: u32,
}

static STATE: Mutex<StandupState> = Mutex::new(StandupState {
    remaining_secs: 0,
    work_secs: 25.0 * 60.0,
    break_secs: 5 * 60,
});

// 定时器和窗口引用，仅在主线程访问
thread_local! {
    static COUNTDOWN_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    static WORK_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    static WINDOW: RefCell<Option<Retained<StandupWindow>>> = const { RefCell::new(None) };
}

// -- StandupWindow: NSPanel 子类 --

#[derive(Debug)]
pub struct StandupWindowIvars {
    _dummy: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[ivars = StandupWindowIvars]
    #[name = "StandupWindow"]
    #[derive(Debug, PartialEq)]
    pub struct StandupWindow;

    #[allow(non_snake_case)]
    impl StandupWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn canBecomeKeyWindow(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn keyDown(&self, event: &NSEvent) {
            if event.keyCode() == 53 {
                dismiss_countdown();
            }
        }
    }
);

impl StandupWindow {
    fn new(screen: &NSScreen, mtm: MainThreadMarker) -> Retained<Self> {
        let frame = screen.frame();
        let this = Self::alloc(mtm).set_ivars(StandupWindowIvars {
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

// -- CountdownView: NSView 子类 --

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[name = "CountdownView"]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct CountdownView;

    #[allow(non_snake_case)]
    impl CountdownView {
        #[unsafe(method(drawRect:))]
        fn drawRect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let remaining = STATE.lock().unwrap().remaining_secs;

            // 半透明黑色遮罩
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.7).setFill();
            NSBezierPath::fillRect(bounds);

            let cx = bounds.origin.x + bounds.size.width / 2.0;
            let cy = bounds.origin.y + bounds.size.height / 2.0;

            let mins = remaining / 60;
            let secs = remaining % 60;
            let time_str = format!("{:02}:{:02}", mins, secs);

            unsafe {
                draw_centered_text(&time_str, cx, cy + 20.0, 72.0);
                draw_centered_text("站起来走动一下", cx, cy - 50.0, 24.0);
                draw_centered_text("按 ESC 跳过", cx, bounds.origin.y + 40.0, 14.0);
            }
        }
    }
);

impl CountdownView {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }
}

/// 在指定位置居中绘制白色文字
unsafe fn draw_centered_text(text: &str, cx: f64, cy: f64, font_size: f64) {
    let label = NSString::from_str(text);
    let font_cls = AnyClass::get(c"NSFont").unwrap();
    let font: Retained<NSObject> =
        msg_send![font_cls, monospacedDigitSystemFontOfSize: font_size, weight: 0.0_f64];

    let dict_cls = AnyClass::get(c"NSMutableDictionary").unwrap();
    let dict: Retained<NSObject> = msg_send![dict_cls, new];
    let font_key = NSString::from_str("NSFont");
    let color_key = NSString::from_str("NSColor");
    let white = NSColor::whiteColor();
    let _: () = msg_send![&dict, setObject: &*font, forKey: &*font_key];
    let _: () = msg_send![&dict, setObject: &*white, forKey: &*color_key];

    let text_size: NSSize = msg_send![&*label, sizeWithAttributes: &*dict];
    let text_pt = NSPoint::new(cx - text_size.width / 2.0, cy - text_size.height / 2.0);
    let _: () = msg_send![&*label, drawAtPoint: text_pt, withAttributes: &*dict];
}

// -- 刷新窗口中的视图 --

fn refresh_view(window: &StandupWindow) {
    if let Some(content) = window.contentView() {
        if let Some(view) = content.subviews().firstObject() {
            view.display();
        }
    }
}

// -- 核心控制函数 --

fn show_countdown() {
    let break_secs = {
        let mut state = STATE.lock().unwrap();
        state.remaining_secs = state.break_secs;
        state.break_secs
    };

    WINDOW.with(|w| {
        let borrow = w.borrow();
        let window = borrow.as_ref().unwrap();

        let mtm = MainThreadMarker::new().unwrap();
        let frame = NSScreen::mainScreen(mtm).unwrap().frame();
        window.setFrame_display_animate(frame, true, false);
        if let Some(content) = window.contentView() {
            if let Some(view) = content.subviews().firstObject() {
                view.setFrame(frame);
            }
        }

        window.makeKeyAndOrderFront(None);
        let app = NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);

        refresh_view(window);
    });

    // 启动 1s 倒计时定时器
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        tick_countdown();
    });
    let timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block) };
    COUNTDOWN_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });

    println!("Break started: {} seconds", break_secs);
}

fn tick_countdown() {
    let remaining = {
        let mut state = STATE.lock().unwrap();
        if state.remaining_secs > 0 {
            state.remaining_secs -= 1;
        }
        state.remaining_secs
    };

    WINDOW.with(|w| {
        let borrow = w.borrow();
        if let Some(window) = borrow.as_ref() {
            refresh_view(window);
        }
    });

    if remaining == 0 {
        dismiss_countdown();
    }
}

fn dismiss_countdown() {
    STATE.lock().unwrap().remaining_secs = 0;

    // 停止倒计时定时器
    COUNTDOWN_TIMER.with(|t| {
        if let Some(timer) = t.borrow_mut().take() {
            timer.invalidate();
        }
    });

    WINDOW.with(|w| {
        let borrow = w.borrow();
        if let Some(window) = borrow.as_ref() {
            window.orderOut(None);
        }
    });

    // 重启工作定时器
    start_work_timer();

    println!("Break ended. Back to work!");
}

fn start_work_timer() {
    // 停止旧的工作定时器
    WORK_TIMER.with(|t| {
        if let Some(timer) = t.borrow_mut().take() {
            timer.invalidate();
        }
    });

    let work_secs = STATE.lock().unwrap().work_secs;
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        show_countdown();
    });
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(work_secs, false, &block)
    };
    WORK_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });
}

// -- 命令行参数解析 --

fn parse_args() -> (f64, f64) {
    let args: Vec<String> = std::env::args().collect();
    let mut work_mins = 25.0_f64;
    let mut break_mins = 5.0_f64;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--work" => {
                if i + 1 < args.len() {
                    if let Ok(v) = args[i + 1].parse::<f64>() {
                        work_mins = v;
                    }
                    i += 1;
                }
            }
            "--break" => {
                if i + 1 < args.len() {
                    if let Ok(v) = args[i + 1].parse::<f64>() {
                        break_mins = v;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    (work_mins, break_mins)
}

fn main() {
    let (work_mins, break_mins) = parse_args();

    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // 设置全局状态
    {
        let mut state = STATE.lock().unwrap();
        state.work_secs = work_mins * 60.0;
        state.break_secs = (break_mins * 60.0) as u32;
    }

    let window = {
        let screen = NSScreen::mainScreen(mtm).unwrap();
        let win = StandupWindow::new(&screen, mtm);

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
        win.setLevel(1000);
        win.setMovable(false);
        win
    };

    // 添加 CountdownView
    let frame = NSScreen::mainScreen(mtm).unwrap().frame();
    let countdown_view = CountdownView::new(frame, mtm);
    window.contentView().unwrap().addSubview(&countdown_view);

    // 存储窗口引用到 thread_local
    WINDOW.with(|w| {
        *w.borrow_mut() = Some(window);
    });

    // 启动工作定时器
    start_work_timer();

    println!(
        "Standup running. Break every {:.1} minutes for {:.1} minutes.",
        work_mins, break_mins
    );

    app.run();
}
