//! Hidden Bar clone — 通过调整 separator status item 的 length 来折叠/展开菜单栏图标
//!
//! 原理：创建两个 status item:
//!   1. btnToggle（最右）— 点击切换折叠/展开，显示 < 或 > 箭头
//!   2. btnSeparator（左边）— 正常时宽 20px 显示 "|"，折叠时宽 10000 把左边图标推出屏幕

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::sel;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};

use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEventMask, NSImage, NSMenu, NSMenuItem,
    NSStatusBar, NSStatusItem,
};
use objc2_foundation::NSString;

static COLLAPSED: AtomicBool = AtomicBool::new(false);

const SEPARATOR_NORMAL_LENGTH: f64 = 8.0;
const SEPARATOR_COLLAPSED_LENGTH: f64 = 10000.0;

thread_local! {
    static TOGGLE_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
    static SEPARATOR_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
}

// -- ToggleTarget: 处理点击 --

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ToggleTarget"]
    #[derive(Debug, PartialEq)]
    pub struct ToggleTarget;

    #[allow(non_snake_case)]
    impl ToggleTarget {
        #[unsafe(method(toggle:))]
        fn toggle(&self, _sender: &AnyObject) {
            let was_collapsed = COLLAPSED.load(Ordering::Relaxed);
            let new_collapsed = !was_collapsed;
            COLLAPSED.store(new_collapsed, Ordering::Relaxed);

            SEPARATOR_ITEM.with(|s| {
                let borrow = s.borrow();
                if let Some(item) = borrow.as_ref() {
                    if new_collapsed {
                        item.setLength(SEPARATOR_COLLAPSED_LENGTH);
                    } else {
                        item.setLength(SEPARATOR_NORMAL_LENGTH);
                    }
                }
            });

            // 更新 toggle 按钮图标
            TOGGLE_ITEM.with(|t| {
                let borrow = t.borrow();
                if let Some(item) = borrow.as_ref() {
                    let mtm = MainThreadMarker::new().unwrap();
                    if let Some(button) = item.button(mtm) {
                        let icon_name = if new_collapsed {
                            "chevron.compact.right"
                        } else {
                            "chevron.compact.left"
                        };
                        if let Some(image) =
                            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                                &NSString::from_str(icon_name),
                                None,
                            )
                        {
                            image.setTemplate(true);
                            button.setImage(Some(&image));
                        }
                    }
                }
            });

            println!(
                "{}",
                if new_collapsed {
                    "Collapsed"
                } else {
                    "Expanded"
                }
            );
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            std::process::exit(0);
        }
    }
);

fn main() {
    let mtm = MainThreadMarker::new().expect("must be on the main thread");

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let status_bar = NSStatusBar::systemStatusBar();

    // -- 1. Toggle 按钮（最右侧）--
    let toggle_item = status_bar.statusItemWithLength(-1.0);
    if let Some(button) = toggle_item.button(mtm) {
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("chevron.compact.left"),
            Some(&NSString::from_str("Toggle")),
        ) {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }

        let target: Retained<ToggleTarget> = unsafe { msg_send![ToggleTarget::alloc(mtm), init] };
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(toggle:)));
        }
        // 响应左键点击
        button.sendActionOn(NSEventMask::LeftMouseUp);

        // 右键菜单
        let menu = NSMenu::new(mtm);
        let quit_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Quit"),
                Some(sel!(quit:)),
                &NSString::from_str("q"),
            )
        };
        unsafe { quit_item.setTarget(Some(&target)) };
        menu.addItem(&quit_item);

        // target 需要保持存活
        std::mem::forget(target);

        // 不能直接 setMenu（会覆盖左键 action），需要通过右键事件手动弹出
        // 暂时只用左键 toggle，右键 quit 后面再加
    }

    // -- 2. Separator（分隔线，在 toggle 左边）--
    let separator_item = status_bar.statusItemWithLength(SEPARATOR_NORMAL_LENGTH);
    if let Some(button) = separator_item.button(mtm) {
        button.setTitle(&NSString::from_str("\u{2502}")); // │ (box drawing vertical)
    }

    // autosaveName: macOS 记住用户 ⌘+拖拽 后的位置，跨启动保持
    toggle_item.setAutosaveName(Some(&NSString::from_str("hidden_toggle")));
    separator_item.setAutosaveName(Some(&NSString::from_str("hidden_separator")));

    // 存储引用
    TOGGLE_ITEM.with(|t| {
        *t.borrow_mut() = Some(toggle_item);
    });
    SEPARATOR_ITEM.with(|s| {
        *s.borrow_mut() = Some(separator_item);
    });

    println!("Hidden Bar running.");
    println!("  Click < to collapse icons left of |");
    println!("  Cmd+drag | to reposition the separator");
    app.run();
}
