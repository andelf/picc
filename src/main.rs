use std::cell::Cell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, NSObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};

use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath, NSColor,
    NSCompositingOperation, NSEvent, NSEventMask, NSEventModifierFlags, NSGraphicsContext,
    NSPanel, NSResponder, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_image_io::CGImageDestination;
use objc2_foundation::{NSArray, NSDate, NSPoint, NSRect, NSRunLoop, NSSize, NSString};
use picc::vision;

use std::sync::Mutex;
/// 选区坐标 (x, y, w, h)，AppKit 全局坐标系
static SEL_RECT: Mutex<[f64; 4]> = Mutex::new([0.0, 0.0, 0.0, 0.0]);

/// 获取 primary screen（菜单栏所在屏，origin 为 (0,0)）的高度
fn primary_screen_height(mtm: MainThreadMarker) -> f64 {
    for screen in NSScreen::screens(mtm).iter() {
        let f = screen.frame();
        if f.origin.x == 0.0 && f.origin.y == 0.0 {
            return f.size.height;
        }
    }
    NSScreen::mainScreen(mtm).unwrap().frame().size.height
}

/// 判断窗口是否为 SnapWindow
fn is_snap_window(win: &NSWindow) -> bool {
    let cls_name: Retained<NSString> = unsafe { msg_send![win, className] };
    cls_name.to_string() == "SnapWindow"
}

/// 遍历所有 SnapWindow 并执行操作
fn for_each_snap_window(f: impl Fn(&NSWindow)) {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    for win in app.windows().iter() {
        if is_snap_window(&win) {
            f(&win);
        }
    }
}

/// 刷新所有 SnapWindow 的 overlay view
fn refresh_all_overlays() {
    for_each_snap_window(|win| {
        if let Some(content) = win.contentView() {
            if let Some(overlay) = content.subviews().firstObject() {
                overlay.display();
            }
        }
    });
}

/// 隐藏所有 SnapWindow
fn hide_all_windows() {
    for_each_snap_window(|win| {
        win.orderOut(None);
    });
}

/// 隐藏所有 SnapWindow 的 overlay
fn hide_all_overlays() {
    for_each_snap_window(|win| {
        if let Some(content) = win.contentView() {
            if let Some(overlay) = content.subviews().firstObject() {
                overlay.setHidden(true);
            }
        }
    });
}

/// 保存 CGImage 到 PNG 文件
#[allow(dead_code)]
fn save_cgimage(image: &CGImage, path: &str) {
    let cf_path = CFString::from_str(path);
    let url = CFURL::with_file_system_path(None, Some(&cf_path), CFURLPathStyle::CFURLPOSIXPathStyle, false);
    let Some(url) = url else {
        println!("save_cgimage({}) => false (bad url)", path);
        return;
    };
    let png_type = CFString::from_str("public.png");
    let dest = unsafe { CGImageDestination::with_url(&url, &png_type, 1, None) };
    let Some(dest) = dest else {
        println!("save_cgimage({}) => false (no dest)", path);
        return;
    };
    unsafe {
        dest.add_image(image, None);
        let ok = dest.finalize();
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
        fn mouseMoved(&self, _event: &NSEvent) {
            // 鼠标移入时自动成为 key window，避免跨屏需要点击两次
            if !self.isKeyWindow() {
                self.makeKeyWindow();
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouseDragged(&self, _event: &NSEvent) {
            let loc = NSEvent::mouseLocation();
            let start_loc = self.ivars().start_pos.get();

            let x = f64::min(start_loc.x, loc.x);
            let y = f64::min(start_loc.y, loc.y);
            let w = f64::abs(start_loc.x - loc.x);
            let h = f64::abs(start_loc.y - loc.y);

            *SEL_RECT.lock().unwrap() = [x, y, w, h];

            refresh_all_overlays();
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn acceptsFirstMouse(&self, _event: &NSEvent) -> bool {
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
            refresh_all_overlays();
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
            // 使用 primary screen 高度，多屏幕下也能正确转换
            let mtm = MainThreadMarker::new().unwrap();
            let primary_h = primary_screen_height(mtm);
            let cg_y = primary_h - (y + h);

            let rect = CGRect::new(CGPoint::new(x, cg_y), CGSize::new(w, h));
            println!("Crop Rect: {:?}", rect);

            // 隐藏所有覆盖窗口，避免截图时把遮罩层也截进去
            hide_all_windows();
            NSRunLoop::currentRunLoop()
                .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.1));

            let crop_img = picc::screenshot(rect).unwrap();

            println!(
                "=> crop_img {}x{}",
                CGImage::width(Some(&crop_img)),
                CGImage::height(Some(&crop_img)),
            );

            ocr(&crop_img);

            // OCR 完成后保持隐藏，重置选区，回到待命状态
            *SEL_RECT.lock().unwrap() = [0.0, 0.0, 0.0, 0.0];
        }

        #[unsafe(method(keyDown:))]
        fn keyDown(&self, event: &NSEvent) {
            if event.keyCode() == 53 || event.keyCode() == 12 {
                // ESC (53) 或 Q (12) — 隐藏所有窗口，回到待命状态
                hide_all_windows();
                *SEL_RECT.lock().unwrap() = [0.0, 0.0, 0.0, 0.0];
                refresh_all_overlays();
            } else if event.keyCode() == 49 {
                // SPACE — 隐藏所有屏幕的遮罩层
                hide_all_overlays();
            }
        }
    }
);

impl SnapWindow {
    fn new(screen: &NSScreen, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SnapWindowIvars {
            start_pos: Cell::new(NSPoint::new(0.0, 0.0)),
            end_pos: Cell::new(NSPoint::new(0.0, 0.0)),
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
                // 全局 AppKit 坐标 → 视图本地坐标
                let win_frame = self.window().unwrap().frame();
                let local_x = x - win_frame.origin.x;
                let local_y = y - win_frame.origin.y;
                let sel_rect = NSRect::new(NSPoint::new(local_x, local_y), NSSize::new(w, h));

                // 用 Copy 模式 + clearColor 擦除选区内的遮罩
                let ctx = NSGraphicsContext::currentContext().unwrap();
                ctx.setCompositingOperation(NSCompositingOperation::Copy);
                NSColor::clearColor().setFill();
                NSBezierPath::fillRect(sel_rect);

                // 恢复正常模式，画蓝色边框
                ctx.setCompositingOperation(NSCompositingOperation::SourceOver);
                NSColor::colorWithSRGBRed_green_blue_alpha(0.4, 0.6, 1.0, 0.8).setStroke();
                let path = NSBezierPath::bezierPathWithRect(sel_rect);
                path.setLineWidth(2.);
                path.stroke();

                // 在选框右下角显示分辨率大小
                let scale = self
                    .window()
                    .map(|w| w.backingScaleFactor())
                    .unwrap_or(2.0);
                let pixel_w = (w * scale) as u32;
                let pixel_h = (h * scale) as u32;
                let label = NSString::from_str(&format!("{} × {}", pixel_w, pixel_h));

                unsafe {
                    let font_cls = AnyClass::get(c"NSFont").unwrap();
                    let font: Retained<NSObject> =
                        msg_send![font_cls, monospacedDigitSystemFontOfSize: 12.0_f64, weight: 0.0_f64];

                    let dict_cls = AnyClass::get(c"NSMutableDictionary").unwrap();
                    let dict: Retained<NSObject> = msg_send![dict_cls, new];
                    let font_key = NSString::from_str("NSFont");
                    let color_key = NSString::from_str("NSColor");
                    let white = NSColor::whiteColor();
                    let _: () = msg_send![&dict, setObject: &*font, forKey: &*font_key];
                    let _: () = msg_send![&dict, setObject: &*white, forKey: &*color_key];

                    let text_size: NSSize = msg_send![&*label, sizeWithAttributes: &*dict];

                    let pad_x = 6.0_f64;
                    let pad_y = 3.0_f64;
                    let gap = 4.0_f64;
                    let bg_w = text_size.width + pad_x * 2.0;
                    let bg_h = text_size.height + pad_y * 2.0;

                    // 默认放在选框右下角下方，如果太靠近屏幕底部则放到上方
                    let bg_x = local_x + w - bg_w;
                    let bg_y = if local_y > bg_h + gap {
                        local_y - bg_h - gap
                    } else {
                        local_y + h + gap
                    };

                    let bg_rect =
                        NSRect::new(NSPoint::new(bg_x, bg_y), NSSize::new(bg_w, bg_h));

                    NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.75).setFill();
                    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bg_rect, 4.0, 4.0)
                        .fill();

                    let text_pt = NSPoint::new(bg_x + pad_x, bg_y + pad_y);
                    let _: () =
                        msg_send![&*label, drawAtPoint: text_pt, withAttributes: &*dict];
                }
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
    let text_req = vision::VNRecognizeTextRequest::new();

    let zh = NSString::from_str("zh-Hans");
    let en = NSString::from_str("en-US");
    let lang = NSArray::from_slice(&[&*zh, &*en]);
    text_req.setRecognitionLanguages(&lang);

    let barcode_req = unsafe { objc2_vision::VNDetectBarcodesRequest::new() };

    let handler = vision::new_handler_with_cgimage(img);

    // 同时执行 OCR 和二维码检测
    let text_req_ref: &objc2_vision::VNRequest =
        unsafe { &*((&*text_req) as *const _ as *const objc2_vision::VNRequest) };
    let barcode_req_ref: &objc2_vision::VNRequest =
        unsafe { &*((&*barcode_req) as *const _ as *const objc2_vision::VNRequest) };
    let reqs = NSArray::from_slice(&[text_req_ref, barcode_req_ref]);
    vision::perform_requests(&handler, &reqs).unwrap();

    // 处理二维码结果
    if let Some(results) = unsafe { barcode_req.results() } {
        for item in results.iter() {
            if let Some(payload) = unsafe { item.payloadStringValue() } {
                let symbology = unsafe { item.symbology() };
                println!("[QRCode] ({}) {}", symbology, payload);
            }
        }
    }

    // 处理 OCR 结果
    if let Some(results) = text_req.results() {
        for item in results.iter() {
            let candidates = item.topCandidates(1);
            for candidate in candidates.iter() {
                println!("candidate.string(): {:?}", candidate.string());
            }
        }
    }
}

/// 检测是否为 Ctrl+Cmd+A 快捷键
fn is_hotkey(event: &NSEvent) -> bool {
    let flags = event.modifierFlags();
    let has_ctrl = flags.contains(NSEventModifierFlags::Control);
    let has_cmd = flags.contains(NSEventModifierFlags::Command);
    // keyCode 0 = 'A' key
    event.keyCode() == 0 && has_ctrl && has_cmd
}

/// 显示所有屏幕的截图窗口
fn show_snap_windows() {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);

    // 重置选区
    *SEL_RECT.lock().unwrap() = [0.0, 0.0, 0.0, 0.0];

    // 通过 frame origin 匹配窗口到屏幕（app.windows() 顺序按 z-order，不可靠）
    let screens = NSScreen::screens(mtm);
    let windows = app.windows();

    for screen in screens.iter() {
        let screen_frame = screen.frame();
        // 找到 origin 匹配的 SnapWindow
        for win in windows.iter() {
            if !is_snap_window(&win) {
                continue;
            }
            let win_frame = win.frame();
            if (win_frame.origin.x - screen_frame.origin.x).abs() < 1.0
                && (win_frame.origin.y - screen_frame.origin.y).abs() < 1.0
            {
                win.setFrame_display_animate(screen_frame, true, false);

                // 刷新 overlay view（使用本地坐标，origin 为 0,0）
                if let Some(content) = win.contentView() {
                    if let Some(overlay) = content.subviews().firstObject() {
                        let content_frame =
                            NSRect::new(NSPoint::new(0.0, 0.0), screen_frame.size);
                        overlay.setFrame(content_frame);
                        overlay.setHidden(false);
                        overlay.display();
                    }
                }

                win.makeKeyAndOrderFront(None);
                break;
            }
        }
    }

    // 激活应用，使其获得焦点
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let app = NSApplication::sharedApplication(mtm);

    // Accessory 模式：不显示 Dock 图标
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // 为所有屏幕创建覆盖窗口
    let screens = NSScreen::screens(mtm);
    let mut windows: Vec<Retained<SnapWindow>> = Vec::new();

    for screen in screens.iter() {
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

        // 添加 overlay view（使用本地坐标，origin 为 0,0）
        let screen_frame = screen.frame();
        let content_frame = NSRect::new(NSPoint::new(0.0, 0.0), screen_frame.size);
        let path_view = DrawPathView::new(content_frame, mtm);
        win.contentView().unwrap().addSubview(&path_view);

        windows.push(win);
    }

    // 注册全局快捷键监听（app 没有焦点时）
    let global_block = RcBlock::new(move |event: NonNull<NSEvent>| {
        let event = unsafe { event.as_ref() };
        if is_hotkey(event) {
            show_snap_windows();
        }
    });
    let _global_monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::KeyDown,
        &global_block,
    );

    // 注册本地快捷键监听（app 有焦点时）
    let local_block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        if is_hotkey(event_ref) {
            show_snap_windows();
            std::ptr::null_mut() // 消费掉该事件
        } else {
            event.as_ptr() // 传递事件
        }
    });
    let _local_monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::KeyDown,
            &local_block,
        )
    };

    println!("PICC running. Press Ctrl+Cmd+A to capture screenshot.");

    // 保持 monitor token 和窗口存活
    let _keep_alive = (_global_monitor, _local_monitor, windows);

    app.run();
}
