use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::net::UdpSocket;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::Instant;

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::sel;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath, NSColor,
    NSEvent, NSImage, NSMenu, NSMenuItem, NSPanel, NSResponder, NSScreen, NSStatusBar,
    NSStatusItem, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSPoint, NSRect, NSSize, NSString, NSTimer};

#[derive(Parser)]
#[command(about = "Standup break reminder for macOS")]
struct Args {
    /// Work interval in minutes
    #[arg(long, default_value_t = 25.0)]
    work: f64,
    /// Break duration in minutes
    #[arg(long, alias = "break", default_value_t = 5.0)]
    brk: f64,
    /// Wake-from-sleep grace period in minutes
    #[arg(long, default_value_t = 10.0)]
    wake: f64,
    /// Disable LAN sync, run standalone
    #[arg(long)]
    solo: bool,
    /// LAN sync UDP port
    #[arg(long, default_value_t = 43210)]
    port: u16,
}

// -- 全局状态 --

struct StandupState {
    remaining_secs: u32,
    work_secs: f64,
    break_secs: u32,
    wake_secs: f64,
    is_breaking: bool,
    skip_next: bool,
    /// break 开始的墙钟时间，用于休眠唤醒后计算真实已过时间
    break_start_time: Option<std::time::SystemTime>,
    /// 防止 show_countdown 在休眠唤醒期间被 work_timer 和 menubar_timer 同时触发
    show_countdown_guard: bool,
}

static STATE: Mutex<StandupState> = Mutex::new(StandupState {
    remaining_secs: 0,
    work_secs: 25.0 * 60.0,
    break_secs: 5 * 60,
    wake_secs: 10.0 * 60.0,
    is_breaking: false,
    skip_next: false,
    break_start_time: None,
    show_countdown_guard: false,
});

// -- LAN Sync --

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Heartbeat {
    v: u8,
    id: u64,
    state: String,
    secs_to_break: u32,
    break_secs: u32,
    work_secs: u32,
    peers: u8,
}

#[allow(dead_code)]
struct PeerInfo {
    state: String,
    secs_to_break: u32,
    break_secs: u32,
    work_secs: u32,
    last_seen: Instant,
}

#[allow(dead_code)]
struct LanSync {
    socket: UdpSocket,
    node_id: u64,
    port: u16,
    broadcast_addrs: Vec<String>,
    peers: HashMap<u64, PeerInfo>,
    synced: bool,
}

static LAN_QUEUE: Mutex<VecDeque<Heartbeat>> = Mutex::new(VecDeque::new());
static LAN_SYNC: Mutex<Option<LanSync>> = Mutex::new(None);

// 定时器和窗口引用，仅在主线程访问
thread_local! {
    static COUNTDOWN_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    static WORK_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    static WINDOWS: RefCell<Vec<Retained<StandupWindow>>> = const { RefCell::new(Vec::new()) };
    static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
    static MENUBAR_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    static HEARTBEAT_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

// -- MenuDelegate: 菜单项 action 处理 --

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MenuDelegate"]
    #[derive(Debug, PartialEq)]
    pub struct MenuDelegate;

    #[allow(non_snake_case)]
    impl MenuDelegate {
        #[unsafe(method(breakNow:))]
        fn break_now(&self, _sender: &AnyObject) {
            info!("Menu action: break now");
            show_countdown();
        }

        #[unsafe(method(skipNext:))]
        fn skip_next(&self, _sender: &AnyObject) {
            let new_skip = {
                let mut state = STATE.lock().unwrap();
                state.skip_next = !state.skip_next;
                state.skip_next
            };
            info!("Menu action: skip_next toggled to {}", new_skip);
            // 更新菜单项标题
            update_skip_menu_title(new_skip);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            info!("Menu action: quit");
            std::process::exit(0);
        }
    }
);

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
            let key_code = event.keyCode();
            debug!(
                "keyDown: keyCode={}, isKeyWindow={}",
                key_code,
                self.isKeyWindow()
            );
            if key_code == 53 {
                info!("ESC pressed, dismissing countdown");
                dismiss_countdown();
            }
        }
    }
);

impl StandupWindow {
    fn new(screen: &NSScreen, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(StandupWindowIvars {
            _dummy: Cell::new(false),
        });
        // 先用小 rect 创建，再用 setFrame 设置精确位置
        // initWithContentRect 会被 backing scale 影响，导致非主屏窗口位置错误
        let init_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
        let win: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: init_rect,
                styleMask: NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false,
                screen: screen,
            ]
        };
        win.setFrame_display_animate(screen.frame(), false, false);
        win
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
                draw_centered_text("Time to stand up and stretch!", cx, cy - 50.0, 24.0);
                draw_centered_text("Press ESC to skip", cx, bounds.origin.y + 40.0, 14.0);
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

// -- thread_local 访问辅助函数 --

fn with_status_item(f: impl FnOnce(&NSStatusItem)) {
    STATUS_ITEM.with(|s| {
        if let Some(item) = s.borrow().as_ref() {
            f(item);
        }
    });
}

fn with_windows(f: impl Fn(&StandupWindow)) {
    WINDOWS.with(|w| {
        for win in w.borrow().iter() {
            f(win);
        }
    });
}

fn invalidate_timer(cell: &RefCell<Option<Retained<NSTimer>>>) {
    if let Some(timer) = cell.borrow_mut().take() {
        timer.invalidate();
    }
}

// -- 多屏窗口管理 --

fn refresh_view(window: &StandupWindow) {
    if let Some(content) = window.contentView() {
        if let Some(view) = content.subviews().firstObject() {
            view.display();
        }
    }
}

/// 为每个屏幕创建窗口并显示
fn create_and_show_windows() {
    let mtm = MainThreadMarker::new().unwrap();
    let screens = NSScreen::screens(mtm);
    let mut windows = Vec::new();

    for screen in screens.iter() {
        let frame = screen.frame();
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

        let local_frame = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
        let countdown_view = CountdownView::new(local_frame, mtm);
        win.contentView().unwrap().addSubview(&countdown_view);

        windows.push(win);
    }

    info!(
        "Created {} windows for {} screens",
        windows.len(),
        screens.len()
    );

    // 显示所有窗口
    for win in &windows {
        win.makeKeyAndOrderFront(None);
        refresh_view(win);
    }

    WINDOWS.with(|ws| {
        *ws.borrow_mut() = windows;
    });

    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// 隐藏并销毁所有窗口
fn destroy_all_windows() {
    WINDOWS.with(|ws| {
        let windows = ws.borrow_mut().drain(..).collect::<Vec<_>>();
        for win in &windows {
            win.orderOut(None);
        }
        // windows dropped here, releasing ObjC objects
    });
}

fn refresh_all_views() {
    with_windows(|win| refresh_view(win));
}

/// 用等宽数字字体设置 button title，避免数字变化时宽度晃动
unsafe fn set_button_monospaced_title(button: &NSObject, title: &str) {
    let font_cls = AnyClass::get(c"NSFont").unwrap();
    let font: *mut AnyObject =
        msg_send![font_cls, monospacedDigitSystemFontOfSize: 0.0_f64, weight: 0.0_f64];

    let dict_cls = AnyClass::get(c"NSMutableDictionary").unwrap();
    let dict: *mut AnyObject = msg_send![dict_cls, new];
    let font_key = NSString::from_str("NSFont");
    let _: () = msg_send![dict, setObject: font, forKey: &*font_key];

    let attr_cls = AnyClass::get(c"NSAttributedString").unwrap();
    let ns_title = NSString::from_str(title);
    let attr_str: *mut AnyObject = msg_send![attr_cls, alloc];
    let attr_str: *mut AnyObject =
        msg_send![attr_str, initWithString: &*ns_title, attributes: dict];
    let _: () = msg_send![button, setAttributedTitle: attr_str];
}

// -- 菜单栏辅助函数 --

fn new_menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

fn set_button_icon(button: &objc2_app_kit::NSStatusBarButton, name: &str) {
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str("Standup Timer")),
    ) {
        image.setTemplate(true);
        button.setImage(Some(&image));
    }
}

// -- 菜单栏 --

fn setup_menubar(mtm: MainThreadMarker) {
    let status_bar = NSStatusBar::systemStatusBar();
    let item = status_bar.statusItemWithLength(-1.0); // NSVariableStatusItemLength

    if let Some(button) = item.button(mtm) {
        set_button_icon(&button, "cup.and.saucer.fill");
        unsafe { set_button_monospaced_title(&button, " --:--") };
    }

    // 创建 MenuDelegate
    let delegate: Retained<MenuDelegate> = unsafe { msg_send![MenuDelegate::alloc(mtm), init] };

    // 创建菜单
    let menu = NSMenu::new(mtm);

    let info_item = new_menu_item(mtm, "Next break: --:--", None, "");
    info_item.setEnabled(false);
    menu.addItem(&info_item);

    let peer_item = new_menu_item(mtm, "Solo mode", None, "");
    peer_item.setEnabled(false);
    menu.addItem(&peer_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let break_item = new_menu_item(mtm, "Break now", Some(sel!(breakNow:)), "");
    unsafe { break_item.setTarget(Some(&delegate)) };
    menu.addItem(&break_item);

    let skip_item = new_menu_item(mtm, "Skip next break", Some(sel!(skipNext:)), "");
    unsafe { skip_item.setTarget(Some(&delegate)) };
    menu.addItem(&skip_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let quit_item = new_menu_item(mtm, "Quit", Some(sel!(quit:)), "q");
    unsafe { quit_item.setTarget(Some(&delegate)) };
    menu.addItem(&quit_item);

    item.setMenu(Some(&menu));

    // 保持强引用
    STATUS_ITEM.with(|s| {
        *s.borrow_mut() = Some(item);
    });
    std::mem::forget(delegate);
}

fn update_menubar_title() {
    // 先读取状态到局部变量，立即释放锁（避免死锁）
    let (is_breaking, remaining_secs, skip_next) = {
        let state = STATE.lock().unwrap();
        (state.is_breaking, state.remaining_secs, state.skip_next)
    };

    let (time_str, info_title, icon_name) = if is_breaking {
        let m = remaining_secs / 60;
        let s = remaining_secs % 60;
        let time = format!("{:02}:{:02}", m, s);
        let info = format!("Break: {}", time);
        (time, info, "figure.stand")
    } else {
        // 从 WORK_TIMER 的 fireDate 计算剩余秒数
        let (remaining, timer_expired) = WORK_TIMER.with(|t| match t.borrow().as_ref() {
            Some(timer) if timer.isValid() => {
                let diff = timer.fireDate().timeIntervalSinceDate(&NSDate::now());
                if diff > 0.0 {
                    (diff as u32, false)
                } else {
                    (0u32, true)
                }
            }
            _ => (0u32, true),
        });

        // 休眠恢复由 handle_wake 处理，这里只显示占位
        if timer_expired {
            let time = String::from("--:--");
            let info = String::from("Waking up...");
            (time, info, "cup.and.saucer.fill")
        } else {
            let m = remaining / 60;
            let s = remaining % 60;
            let time = format!("{:02}:{:02}", m, s);
            let skip_suffix = if skip_next { " (will skip)" } else { "" };
            let info = format!("Next break: {}{}", time, skip_suffix);
            (time, info, "cup.and.saucer.fill")
        }
    };

    let title_with_peers = format!(" {}", time_str);

    let peer_count = get_peer_count();
    let peer_status = if peer_count > 0 {
        format!(
            "Synced with {} peer{}",
            peer_count,
            if peer_count > 1 { "s" } else { "" }
        )
    } else if LAN_SYNC.lock().unwrap().is_some() {
        "No peers".to_string()
    } else {
        "Solo mode".to_string()
    };

    with_status_item(|item| {
        let mtm = MainThreadMarker::new().unwrap();
        if let Some(button) = item.button(mtm) {
            unsafe { set_button_monospaced_title(&button, &title_with_peers) };
            set_button_icon(&button, icon_name);
        }
        if let Some(menu) = item.menu(mtm) {
            if let Some(info_item) = menu.itemAtIndex(0) {
                info_item.setTitle(&NSString::from_str(&info_title));
            }
            // Update peer status item (index 1)
            if let Some(peer_item) = menu.itemAtIndex(1) {
                peer_item.setTitle(&NSString::from_str(&peer_status));
            }
        }
    });
}

fn start_menubar_timer() {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        process_lan_queue(); // 兜底处理 LAN 消息
        update_menubar_title();
    });
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block) };
    MENUBAR_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });
}

fn update_skip_menu_title(skip: bool) {
    with_status_item(|item| {
        let mtm = MainThreadMarker::new().unwrap();
        if let Some(menu) = item.menu(mtm) {
            if let Some(skip_item) = menu.itemAtIndex(4) {
                let title = if skip {
                    "Enable next break"
                } else {
                    "Skip next break"
                };
                skip_item.setTitle(&NSString::from_str(title));
            }
        }
    });
}

// -- 核心控制函数 --

fn show_countdown() {
    show_countdown_with(0);
}

/// 显示 break 倒计时。override_remaining > 0 时用该值作为倒计时，否则用 break_secs。
fn show_countdown_with(override_remaining: u32) {
    // 防重入：如果已经在 break 中，不再触发
    {
        let state = STATE.lock().unwrap();
        if state.is_breaking {
            warn!(
                "show_countdown called while already breaking (remaining={}s), ignoring",
                state.remaining_secs
            );
            return;
        }
        if state.show_countdown_guard {
            warn!("show_countdown re-entered, ignoring");
            return;
        }
    }

    // 设置 guard
    STATE.lock().unwrap().show_countdown_guard = true;

    // 检查 skip 标志
    let should_skip = {
        let mut state = STATE.lock().unwrap();
        if state.skip_next {
            state.skip_next = false;
            true
        } else {
            false
        }
    };
    if should_skip {
        update_skip_menu_title(false);
        start_work_timer();
        info!("Break skipped by user preference");
        STATE.lock().unwrap().show_countdown_guard = false;
        return;
    }

    let break_secs = {
        let mut state = STATE.lock().unwrap();
        state.remaining_secs = if override_remaining > 0 {
            override_remaining
        } else {
            state.break_secs
        };
        state.is_breaking = true;
        state.break_start_time = Some(std::time::SystemTime::now());
        state.remaining_secs
    };

    info!(
        "Break started: {}s ({}m{}s)",
        break_secs,
        break_secs / 60,
        break_secs % 60
    );

    // LAN sync: 快速广播 break 事件 (0/50/100ms)
    send_heartbeat();
    dispatch_after_ms(50, || send_heartbeat());
    dispatch_after_ms(100, || send_heartbeat());

    // 停止工作定时器
    WORK_TIMER.with(|t| invalidate_timer(t));
    debug!("Work timer invalidated for break");

    create_and_show_windows();

    // 启动 1s 倒计时定时器
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        tick_countdown();
    });
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block) };
    COUNTDOWN_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });

    update_menubar_title();

    STATE.lock().unwrap().show_countdown_guard = false;
}

fn tick_countdown() {
    let (remaining, is_breaking) = {
        let mut state = STATE.lock().unwrap();
        if !state.is_breaking {
            debug!("tick_countdown called but not breaking, ignoring");
            return;
        }
        if state.remaining_secs > 0 {
            state.remaining_secs -= 1;
        }
        (state.remaining_secs, state.is_breaking)
    };

    debug!(
        "tick: remaining={}s, is_breaking={}",
        remaining, is_breaking
    );

    refresh_all_views();

    // 确保至少有一个窗口是 key window
    let any_key = WINDOWS.with(|ws| ws.borrow().iter().any(|w| w.isKeyWindow()));
    if !any_key {
        warn!("No window is key during break, re-acquiring");
        WINDOWS.with(|ws| {
            if let Some(win) = ws.borrow().first() {
                win.makeKeyAndOrderFront(None);
            }
        });
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }

    if remaining == 0 {
        info!("Countdown reached zero, dismissing");
        dismiss_countdown();
    }
}

fn dismiss_countdown() {
    let was_breaking = {
        let mut state = STATE.lock().unwrap();
        let was = state.is_breaking;
        state.remaining_secs = 0;
        state.is_breaking = false;
        state.break_start_time = None;
        was
    };

    if !was_breaking {
        debug!("dismiss_countdown called but was not breaking, ignoring");
        return;
    }

    // 停止倒计时定时器
    COUNTDOWN_TIMER.with(|t| invalidate_timer(t));
    debug!("Countdown timer invalidated");

    destroy_all_windows();

    // 重启工作定时器
    start_work_timer();

    // LAN sync: 快速广播 dismiss 事件 (0/50/100ms)
    send_heartbeat();
    dispatch_after_ms(50, || send_heartbeat());
    dispatch_after_ms(100, || send_heartbeat());

    update_menubar_title();
    info!("Break ended, back to work");
}

fn start_work_timer() {
    let work_secs = STATE.lock().unwrap().work_secs;
    info!(
        "Starting work timer: {:.0}s ({:.1}m)",
        work_secs,
        work_secs / 60.0
    );
    start_work_timer_with(work_secs);
}

fn start_work_timer_with(secs: f64) {
    // 停止旧的工作定时器
    WORK_TIMER.with(|t| invalidate_timer(t));

    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        debug!("Work timer fired after {:.0}s", secs);
        show_countdown();
    });
    let timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) };
    WORK_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });
    debug!("Work timer scheduled for {:.0}s", secs);
}

// -- 休眠唤醒处理 --

fn ensure_menubar_timer() {
    let needs_restart =
        MENUBAR_TIMER.with(|t| t.borrow().as_ref().map_or(true, |timer| !timer.isValid()));
    if needs_restart {
        start_menubar_timer();
        info!("Menubar timer was invalid after wake, restarted");
    }
}

fn ensure_countdown_timer() {
    let needs_restart =
        COUNTDOWN_TIMER.with(|t| t.borrow().as_ref().map_or(true, |timer| !timer.isValid()));
    if needs_restart {
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            tick_countdown();
        });
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block) };
        COUNTDOWN_TIMER.with(|t| {
            *t.borrow_mut() = Some(timer);
        });
        info!("Countdown timer was invalid after wake, restarted");
    }
}

fn handle_wake() {
    info!("System wake detected");

    // 处理休眠期间堆积的 LAN 消息
    process_lan_queue();

    let (is_breaking, break_secs, break_start_time) = {
        let state = STATE.lock().unwrap();
        (state.is_breaking, state.break_secs, state.break_start_time)
    };

    if is_breaking {
        if let Some(start) = break_start_time {
            let elapsed = start.elapsed().unwrap_or_default().as_secs() as u32;
            if elapsed >= break_secs {
                info!(
                    "Wake during break: elapsed {}s >= break {}s, dismissing",
                    elapsed, break_secs
                );
                dismiss_countdown();
            } else {
                let new_remaining = break_secs - elapsed;
                info!("Wake during break: {}s remaining", new_remaining);
                STATE.lock().unwrap().remaining_secs = new_remaining;
                ensure_countdown_timer();
                refresh_all_views();
            }
        } else {
            info!("Wake during break: no start time recorded, dismissing");
            dismiss_countdown();
        }
    } else {
        let timer_ok = WORK_TIMER.with(|t| match t.borrow().as_ref() {
            Some(timer) if timer.isValid() => {
                let diff = timer.fireDate().timeIntervalSinceDate(&NSDate::now());
                if diff > 0.0 {
                    info!("Wake during work: {:.0}s remaining on work timer", diff);
                    true
                } else {
                    false
                }
            }
            _ => false,
        });

        if !timer_ok {
            let wake_secs = STATE.lock().unwrap().wake_secs;
            start_work_timer_with(wake_secs);
            info!(
                "Wake during work: timer expired, reset to {:.0}s",
                wake_secs
            );
        }
    }

    ensure_menubar_timer();
    update_menubar_title();
}

// -- LAN Sync 函数 --

/// dispatch_async 到主线程执行闭包
fn dispatch_async_main(f: impl Fn() + Send + 'static) {
    let block = RcBlock::new(move |_: NonNull<AnyObject>| {
        f();
    });
    unsafe {
        let queue: *mut AnyObject =
            msg_send![AnyClass::get(c"NSOperationQueue").unwrap(), mainQueue];
        let _: () = msg_send![queue, addOperationWithBlock: &*block];
    }
}

/// dispatch_after 延迟执行（毫秒），必须在主线程调用
fn dispatch_after_ms(ms: u64, f: impl Fn() + 'static) {
    let secs = ms as f64 / 1000.0;
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        f();
    });
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block);
    }
}

fn get_my_secs_to_break() -> u32 {
    WORK_TIMER.with(|t| match t.borrow().as_ref() {
        Some(timer) if timer.isValid() => {
            let diff = timer.fireDate().timeIntervalSinceDate(&NSDate::now());
            if diff > 0.0 {
                diff as u32
            } else {
                0
            }
        }
        _ => 0,
    })
}

fn get_peer_count() -> u8 {
    let guard = LAN_SYNC.lock().unwrap();
    match guard.as_ref() {
        Some(lan) => lan.peers.len() as u8,
        None => 0,
    }
}

fn send_heartbeat() {
    // 先读取 STATE（不持有 LAN_SYNC 锁），保持一致的锁顺序
    let (is_breaking, break_secs, work_secs) = {
        let state = STATE.lock().unwrap();
        (state.is_breaking, state.break_secs, state.work_secs as u32)
    };

    let my_state = if is_breaking { "breaking" } else { "working" };
    // breaking 时 secs_to_break = 剩余 break 秒数; working 时 = 距下次 break 秒数
    let secs_to_break = if is_breaking {
        STATE.lock().unwrap().remaining_secs
    } else {
        get_my_secs_to_break()
    };

    let guard = LAN_SYNC.lock().unwrap();
    let lan = match guard.as_ref() {
        Some(lan) => lan,
        None => return,
    };

    let hb = Heartbeat {
        v: 1,
        id: lan.node_id,
        state: my_state.to_string(),
        secs_to_break,
        break_secs,
        work_secs,
        peers: (lan.peers.len() + 1) as u8, // +1 for self
    };

    if let Ok(json) = serde_json::to_vec(&hb) {
        for addr in &lan.broadcast_addrs {
            match lan.socket.send_to(&json, addr.as_str()) {
                Ok(n) => debug!(
                    "Heartbeat sent: {} bytes to {}, state={}, secs_to_break={}, peers={}",
                    n, addr, my_state, secs_to_break, hb.peers
                ),
                Err(e) => warn!("Failed to send heartbeat to {}: {}", addr, e),
            }
        }
    }
}

fn update_peer(id: u64, hb: &Heartbeat) {
    let mut guard = LAN_SYNC.lock().unwrap();
    let lan = match guard.as_mut() {
        Some(lan) => lan,
        None => return,
    };

    lan.peers.insert(
        id,
        PeerInfo {
            state: hb.state.clone(),
            secs_to_break: hb.secs_to_break,
            break_secs: hb.break_secs,
            work_secs: hb.work_secs,
            last_seen: Instant::now(),
        },
    );
}

fn cleanup_stale_peers() {
    let mut guard = LAN_SYNC.lock().unwrap();
    let lan = match guard.as_mut() {
        Some(lan) => lan,
        None => return,
    };

    let before = lan.peers.len();
    lan.peers
        .retain(|_id, peer| peer.last_seen.elapsed().as_secs() < 9);
    let removed = before - lan.peers.len();
    if removed > 0 {
        info!(
            "Cleaned up {} stale peers, {} remaining",
            removed,
            lan.peers.len()
        );
    }
}

fn apply_sync_rules(hb: &Heartbeat) {
    // 分别获取锁，避免死锁
    let is_breaking = STATE.lock().unwrap().is_breaking;
    let synced = LAN_SYNC
        .lock()
        .unwrap()
        .as_ref()
        .map_or(true, |lan| lan.synced);

    // Rule 0: 首次同步 — 采纳 peer 的 config 和 timing
    if !synced && hb.state == "working" {
        info!(
            "Rule 0: first sync from peer {:x}, adopting config work={}s break={}s, break in {}s",
            hb.id, hb.work_secs, hb.break_secs, hb.secs_to_break
        );
        {
            let mut state = STATE.lock().unwrap();
            state.work_secs = hb.work_secs as f64;
            state.break_secs = hb.break_secs;
        }
        {
            let mut guard = LAN_SYNC.lock().unwrap();
            if let Some(lan) = guard.as_mut() {
                lan.synced = true;
            }
        }
        start_work_timer_with(hb.secs_to_break as f64);
        return;
    }

    if !synced && hb.state == "breaking" {
        info!(
            "Rule 0: first sync, peer {:x} is breaking ({}s remaining), joining",
            hb.id, hb.secs_to_break
        );
        {
            let mut guard = LAN_SYNC.lock().unwrap();
            if let Some(lan) = guard.as_mut() {
                lan.synced = true;
            }
        }
        // 采纳 peer 的 config，用 peer 的剩余时间作为倒计时
        {
            let mut state = STATE.lock().unwrap();
            state.work_secs = hb.work_secs as f64;
            state.break_secs = hb.break_secs;
        }
        show_countdown_with(hb.secs_to_break);
        return;
    }

    // Rule 3: breaking 时，如果 peer 是 working → 跟着退出 break
    if is_breaking {
        if hb.state == "working" {
            info!(
                "Rule 3: peer {:x} is working while we're breaking, dismissing",
                hb.id
            );
            dismiss_countdown();
        }
        return;
    }

    // Rule 2: peer 正在 break → 立刻 break，同步剩余时间
    if hb.state == "breaking" {
        {
            let mut state = STATE.lock().unwrap();
            if hb.break_secs > state.break_secs {
                state.break_secs = hb.break_secs;
            }
        }
        info!(
            "Rule 2: peer {:x} is breaking ({}s remaining), triggering break",
            hb.id, hb.secs_to_break
        );
        show_countdown_with(hb.secs_to_break);
        return;
    }

    // Rule 1: 跟随更早的 break
    if hb.state == "working" {
        let my_secs = get_my_secs_to_break();
        if my_secs > 0 && hb.secs_to_break + 5 < my_secs {
            info!(
                "Rule 1: synced to peer {:x}, break in {}s (was {}s)",
                hb.id, hb.secs_to_break, my_secs
            );
            start_work_timer_with(hb.secs_to_break as f64);
        }
    }
}

fn process_lan_queue() {
    let messages: Vec<Heartbeat> = LAN_QUEUE.lock().unwrap().drain(..).collect();
    // 每个 peer 只保留最后一条消息，避免处理过期状态导致震荡
    let mut latest: HashMap<u64, Heartbeat> = HashMap::new();
    for hb in messages {
        latest.insert(hb.id, hb);
    }
    for (_, hb) in latest {
        update_peer(hb.id, &hb);
        apply_sync_rules(&hb);
    }
    cleanup_stale_peers();
}

fn start_heartbeat_timer() {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        send_heartbeat();
    });
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(3.0, true, &block) };
    HEARTBEAT_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });
    info!("Heartbeat timer started (3s interval)");
}

/// 获取所有活跃网络接口的子网广播地址 (假设 /24)
fn detect_broadcast_addrs(port: u16) -> Vec<String> {
    use std::process::Command;
    let mut addrs = Vec::new();
    // 解析 ifconfig 获取所有 broadcast 地址
    if let Ok(output) = Command::new("ifconfig").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let line = line.trim();
            // macOS ifconfig 格式: "inet 192.168.0.100 netmask 0xffffff00 broadcast 192.168.0.255"
            if let Some(idx) = line.find("broadcast ") {
                let bcast = line[idx + 10..].split_whitespace().next().unwrap_or("");
                if !bcast.is_empty() && bcast != "127.255.255.255" {
                    let addr = format!("{}:{}", bcast, port);
                    if !addrs.contains(&addr) {
                        addrs.push(addr);
                    }
                }
            }
        }
    }
    if addrs.is_empty() {
        addrs.push(format!("255.255.255.255:{}", port));
    }
    info!("Broadcast addresses: {:?}", addrs);
    addrs
}

fn create_udp_socket(port: u16) -> Result<UdpSocket, Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind(("0.0.0.0", port))?;
    socket.set_broadcast(true)?;
    Ok(socket)
}

fn start_lan_sync(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let node_id: u64 = rand::rng().random();

    // 两个独立 socket：recv 绑定目标端口收包，send 用 ephemeral port 发包
    let recv_socket = create_udp_socket(port)?;
    let send_socket = UdpSocket::bind("0.0.0.0:0")?;
    send_socket.set_broadcast(true)?;
    let broadcast_addrs = detect_broadcast_addrs(port);

    info!("LAN sync started: node_id={:x}, port={}", node_id, port);

    *LAN_SYNC.lock().unwrap() = Some(LanSync {
        socket: send_socket,
        node_id,
        port,
        broadcast_addrs,
        peers: HashMap::new(),
        synced: false,
    });

    let my_node_id = node_id;
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match recv_socket.recv_from(&mut buf) {
                Ok((len, addr)) => {
                    debug!("UDP recv: {} bytes from {}", len, addr);
                    match serde_json::from_slice::<Heartbeat>(&buf[..len]) {
                        Err(e) => {
                            let raw = String::from_utf8_lossy(&buf[..len.min(200)]);
                            warn!("JSON parse error: {}, raw: {}", e, raw);
                        }
                        Ok(hb) => {
                            if hb.id == my_node_id {
                                debug!("Ignoring own heartbeat");
                                continue;
                            }
                            debug!(
                                "Heartbeat from {:x} @ {}: state={}, secs_to_break={}, peers={}",
                                hb.id, addr, hb.state, hb.secs_to_break, hb.peers
                            );

                            let is_breaking = hb.state == "breaking";
                            LAN_QUEUE.lock().unwrap().push_back(hb);

                            // break 事件立即 dispatch 到主线程处理
                            if is_breaking {
                                dispatch_async_main(|| process_lan_queue());
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("UDP recv error: {}", e);
                }
            }
        }
    });

    Ok(())
}

fn main() {
    let local_time = tracing_subscriber::fmt::time::OffsetTime::new(
        time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC),
        time::macros::format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
        ),
    );
    tracing_subscriber::fmt()
        .with_timer(local_time)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let args = Args::parse();

    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // 设置全局状态
    {
        let mut state = STATE.lock().unwrap();
        state.work_secs = args.work * 60.0;
        state.break_secs = (args.brk * 60.0) as u32;
        state.wake_secs = args.wake * 60.0;
    }

    // 设置菜单栏
    setup_menubar(mtm);

    // 启动工作定时器
    start_work_timer();

    // 启动菜单栏刷新定时器
    start_menubar_timer();
    update_menubar_title();

    // 注册系统唤醒通知
    unsafe {
        let workspace_cls = AnyClass::get(c"NSWorkspace").unwrap();
        let workspace: *mut AnyObject = msg_send![workspace_cls, sharedWorkspace];
        let nc: *mut AnyObject = msg_send![workspace, notificationCenter];
        let wake_name = NSString::from_str("NSWorkspaceDidWakeNotification");
        let wake_block = RcBlock::new(|_notif: NonNull<AnyObject>| {
            handle_wake();
        });
        let _observer: *mut AnyObject = msg_send![
            nc,
            addObserverForName: &*wake_name,
            object: std::ptr::null::<AnyObject>(),
            queue: std::ptr::null::<AnyObject>(),
            usingBlock: &*wake_block,
        ];
        std::mem::forget(wake_block);
    }
    info!("Registered NSWorkspaceDidWakeNotification handler");

    // 启动 LAN sync
    if !args.solo {
        match start_lan_sync(args.port) {
            Ok(()) => {
                start_heartbeat_timer();
            }
            Err(e) => {
                warn!("Failed to start LAN sync: {}, running solo", e);
            }
        }
    } else {
        info!("Solo mode, LAN sync disabled");
    }

    info!(
        "Standup running: work={:.1}m, break={:.1}m, wake={:.1}m, solo={}",
        args.work, args.brk, args.wake, args.solo
    );

    app.run();
}
