use objc2::sel;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{define_class, MainThreadMarker, MainThreadOnly};

use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem,
};
use objc2_foundation::NSString;

// 用于持有 status item 的全局引用，防止被释放
thread_local! {
    static STATUS_ITEM: std::cell::RefCell<Option<Retained<NSStatusItem>>> =
        const { std::cell::RefCell::new(None) };
}

// 菜单 action target
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MenuTarget"]
    #[derive(Debug, PartialEq)]
    pub struct MenuTarget;

    #[allow(non_snake_case)]
    impl MenuTarget {
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &NSObject) {
            println!("Quit clicked");
            std::process::exit(0);
        }

        #[unsafe(method(hello:))]
        fn hello(&self, _sender: &NSObject) {
            println!("Hello clicked!");
        }
    }
);

fn main() {
    let mtm = MainThreadMarker::new().expect("must be on the main thread");

    // 1. 初始化 NSApplication（必须在 NSStatusBar 之前）
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    println!("[1] NSApplication initialized, policy=Accessory");

    // 2. 创建 status item，用字面量 -1.0 代替 NSVariableStatusItemLength
    let status_bar = NSStatusBar::systemStatusBar();
    let item = status_bar.statusItemWithLength(-1.0);
    println!("[2] NSStatusItem created");

    // 3. 配置 button
    if let Some(button) = item.button(mtm) {
        // 尝试 SF Symbol 图标
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("cup.and.saucer.fill"),
            Some(&NSString::from_str("Test")),
        ) {
            image.setTemplate(true);
            button.setImage(Some(&image));
            println!("[3] SF Symbol image set");
        } else {
            println!("[3] SF Symbol not available, using text only");
        }
        button.setTitle(&NSString::from_str(" 25:00"));
        println!("[3] Button title set");
    } else {
        println!("[3] ERROR: button() returned None!");
    }

    // 4. 创建菜单
    let menu = NSMenu::new(mtm);
    let target: Retained<MenuTarget> = unsafe { objc2::msg_send![MenuTarget::alloc(mtm), init] };

    let hello_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Hello"),
            Some(sel!(hello:)),
            &NSString::from_str(""),
        )
    };
    unsafe { hello_item.setTarget(Some(&target)) };
    menu.addItem(&hello_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

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

    item.setMenu(Some(&menu));
    println!("[4] Menu attached");

    // 5. 保持强引用
    STATUS_ITEM.with(|s| {
        *s.borrow_mut() = Some(item);
    });
    // target 也要保持存活（leak 到 static）
    std::mem::forget(target);
    println!("[5] References stored");

    // 6. 运行事件循环
    println!("[6] Starting app.run() — check your menu bar!");
    app.run();
}
