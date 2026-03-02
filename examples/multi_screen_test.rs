//! 多屏幕环境测试：打印所有屏幕信息和坐标系行为
//!
//! cargo run --example multi_screen_test

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSScreen};

fn main() {
    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    _app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    println!("=== 多屏幕信息 ===\n");

    // 1. NSScreen::screens() 返回所有屏幕
    let screens = NSScreen::screens(mtm);
    println!("屏幕总数: {}", screens.count());

    for (i, screen) in screens.iter().enumerate() {
        let frame = screen.frame();
        let visible = screen.visibleFrame();
        let scale = screen.backingScaleFactor();
        let name = screen.localizedName();

        println!("\n--- Screen #{i} ---");
        println!("  名称: {}", name);
        println!(
            "  frame:   origin=({:.0}, {:.0})  size=({:.0} x {:.0})",
            frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
        );
        println!(
            "  visible: origin=({:.0}, {:.0})  size=({:.0} x {:.0})",
            visible.origin.x, visible.origin.y, visible.size.width, visible.size.height
        );
        println!("  scale: {scale}");

        // 判断是否为 primary screen
        if frame.origin.x == 0.0 && frame.origin.y == 0.0 {
            println!("  ** 这是 primary screen (菜单栏所在屏) **");
        }
    }

    // 2. NSScreen::mainScreen 返回当前鼠标所在屏或键盘焦点屏
    if let Some(main) = NSScreen::mainScreen(mtm) {
        let f = main.frame();
        println!(
            "\nmainScreen (当前焦点屏): origin=({:.0}, {:.0})  size=({:.0} x {:.0})",
            f.origin.x, f.origin.y, f.size.width, f.size.height
        );
    }

    // 3. 坐标系说明
    println!("\n=== 坐标系说明 ===");
    println!("AppKit 坐标系: 原点在 primary screen 左下角, Y 轴向上");
    println!("CoreGraphics 坐标系: 原点在 primary screen 左上角, Y 轴向下");
    println!("NSEvent::mouseLocation() 返回 AppKit 全局坐标");

    // 4. 计算所有屏幕的 union rect
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for screen in screens.iter() {
        let f = screen.frame();
        min_x = min_x.min(f.origin.x);
        min_y = min_y.min(f.origin.y);
        max_x = max_x.max(f.origin.x + f.size.width);
        max_y = max_y.max(f.origin.y + f.size.height);
    }
    println!(
        "\n所有屏幕 union rect (AppKit): ({:.0}, {:.0}) -> ({:.0}, {:.0}), size=({:.0} x {:.0})",
        min_x,
        min_y,
        max_x,
        max_y,
        max_x - min_x,
        max_y - min_y
    );

    // 5. 验证每个屏幕在 CG 坐标系中的位置
    let primary_h = screens
        .iter()
        .find(|s| {
            let f = s.frame();
            f.origin.x == 0.0 && f.origin.y == 0.0
        })
        .map(|s| s.frame().size.height)
        .unwrap_or(0.0);

    println!("\nPrimary screen height: {primary_h}");
    println!("\n=== CG 坐标系中各屏幕位置 ===");
    for (i, screen) in screens.iter().enumerate() {
        let f = screen.frame();
        let cg_x = f.origin.x;
        let cg_y = primary_h - (f.origin.y + f.size.height);
        println!(
            "Screen #{i}: CG origin=({:.0}, {:.0})  size=({:.0} x {:.0})",
            cg_x, cg_y, f.size.width, f.size.height
        );
    }
}
