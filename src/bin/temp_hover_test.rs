// Test Case 2: reply in thread for a card WITHOUT existing thread
// Hover → toolbar 4th button → wait for thread sidebar → type + send
use objc2::MainThreadMarker;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType,
    CGMouseButton, CGScrollEventUnit,
};
use picc::accessibility::{self, AXNode, AXQuery, role};
use std::thread;
use std::time::Duration;

fn find_lark_app(_mtm: MainThreadMarker) -> Option<(i32, objc2::rc::Retained<NSRunningApplication>)> {
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
    let workspace: objc2::rc::Retained<objc2::runtime::NSObject> =
        unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let apps: objc2::rc::Retained<objc2_foundation::NSArray<NSRunningApplication>> =
        unsafe { objc2::msg_send![&workspace, runningApplications] };
    for app in apps.iter() {
        if let Some(bundle_id) = app.bundleIdentifier() {
            let bundle_str = bundle_id.to_string();
            if bundle_str.contains("lark") || bundle_str.contains("Lark") || bundle_str.contains("feishu") {
                return Some((app.processIdentifier(), app.clone()));
            }
        }
    }
    None
}

fn move_mouse(x: f64, y: f64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let point = CGPoint { x, y };
    let ev = CGEvent::new_mouse_event(source.as_deref(), CGEventType::MouseMoved, point, CGMouseButton::Left);
    if let Some(ref ev) = ev { CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev)); }
}

fn click_at(x: f64, y: f64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let point = CGPoint { x, y };
    let down = CGEvent::new_mouse_event(source.as_deref(), CGEventType::LeftMouseDown, point, CGMouseButton::Left);
    let up = CGEvent::new_mouse_event(source.as_deref(), CGEventType::LeftMouseUp, point, CGMouseButton::Left);
    if let Some(ref ev) = down { CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev)); }
    thread::sleep(Duration::from_millis(50));
    if let Some(ref ev) = up { CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev)); }
}

fn scroll_at(x: f64, y: f64, delta: i32) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let event = CGEvent::new_scroll_wheel_event2(source.as_deref(), CGScrollEventUnit::Pixel, 1, delta, 0, 0);
    if let Some(ref ev) = event {
        CGEvent::set_location(Some(ev), CGPoint { x, y });
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

fn type_text(text: &str) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let utf16: Vec<u16> = text.encode_utf16().collect();
    for chunk in utf16.chunks(20) {
        let down = CGEvent::new_keyboard_event(source.as_deref(), 0, true);
        if let Some(ref ev) = down {
            unsafe { CGEvent::keyboard_set_unicode_string(Some(ev), chunk.len() as _, chunk.as_ptr()); }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        thread::sleep(Duration::from_millis(5));
        let up = CGEvent::new_keyboard_event(source.as_deref(), 0, false);
        if let Some(ref ev) = up {
            unsafe { CGEvent::keyboard_set_unicode_string(Some(ev), chunk.len() as _, chunk.as_ptr()); }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn press_return() {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let down = CGEvent::new_keyboard_event(source.as_deref(), 36, true);
    if let Some(ref ev) = down { CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev)); }
    thread::sleep(Duration::from_millis(20));
    let up = CGEvent::new_keyboard_event(source.as_deref(), 36, false);
    if let Some(ref ev) = up { CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev)); }
}

fn wait_for_thread_input(chat_wa: &AXNode, max_retries: usize) -> Option<AXNode> {
    for attempt in 0..max_retries {
        let text_areas: Vec<AXNode> = accessibility::find_all(
            &chat_wa.0,
            &AXQuery::new().filter(|n| n.role().as_deref() == Some("AXTextArea")),
            40,
        ).into_iter().map(AXNode::new).collect();

        eprintln!("  poll[{attempt}] found {} text areas", text_areas.len());
        for (i, ta) in text_areas.iter().enumerate() {
            let val: String = ta.value().unwrap_or_default().chars().take(40).collect();
            eprintln!("    [{i}] val={val:?}");
        }

        if text_areas.len() >= 2 {
            let thread_ta = text_areas.into_iter().find(|ta| {
                ta.value().map(|v| v.contains("Reply to thread")).unwrap_or(false)
            });
            if thread_ta.is_some() { return thread_ta; }
        }
        if attempt < max_retries - 1 {
            thread::sleep(Duration::from_millis(1000));
        }
    }
    None
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    let (pid, lark_app) = find_lark_app(mtm).expect("no lark");
    let app = AXNode::app(pid);
    let win = app.find_all(role("AXWindow")).into_iter()
        .find(|w| { let t = w.title().unwrap_or_default(); t == "Lark" || t == "飞书" || t == "Feishu" })
        .expect("no window");
    let chat_wa = win.select(&[AXQuery::new().role("AXWebArea").title_contains("messenger-chat")])
        .expect("no chat");

    // Activate Lark
    #[allow(deprecated)]
    lark_app.activateWithOptions(
        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    );
    thread::sleep(Duration::from_millis(500));

    // Scroll to bottom
    let (wa_x, wa_y) = chat_wa.position().unwrap_or((0.0, 0.0));
    let (wa_w, wa_h) = chat_wa.size().unwrap_or((0.0, 0.0));
    let cx = wa_x + wa_w / 2.0;
    let cy = wa_y + wa_h / 2.0;
    for _ in 0..5 { scroll_at(cx, cy, -300); thread::sleep(Duration::from_millis(100)); }
    thread::sleep(Duration::from_millis(500));

    // Find all message items
    let q = &AXQuery::new().dom_class("message-item");
    let items: Vec<AXNode> = accessibility::find_all(&chat_wa.0, q, 30)
        .into_iter().map(AXNode::new).collect();
    eprintln!("Found {} message items", items.len());

    // Find last OTHER message WITHOUT existing thread
    let target = items.iter().rev().find(|item| {
        let cls = item.dom_classes();
        !cls.contains(&"message-self".to_string())
            && !cls.contains(&"message-thread-container".to_string())
    });

    match target {
        Some(item) => {
            let body = item.find(AXQuery::new().dom_class("message-content"))
                .map(|c| {
                    let texts = c.find_all(role("AXStaticText"));
                    texts.iter().filter_map(|t| t.value()).collect::<Vec<_>>().join("")
                })
                .unwrap_or_default();
            let short: String = body.chars().take(50).collect();
            eprintln!("\n=== Case 2: Card WITHOUT thread ===");
            eprintln!("Message: {short:?}");

            // Hover to show toolbar
            let (x, y) = item.position().unwrap();
            let (w, h) = item.size().unwrap();
            eprintln!("Hovering at ({}, {})", x + w / 2.0, y + h / 2.0);
            move_mouse(x + w / 2.0, y + h / 2.0);
            thread::sleep(Duration::from_millis(1500));

            // Find toolbar
            let toolbar = item.find(AXQuery::new().dom_class("messageAction__toolbar"));
            match toolbar {
                Some(tb) => {
                    let children = tb.children();
                    eprintln!("Toolbar: {} buttons", children.len());
                    for (i, c) in children.iter().enumerate() {
                        let cls = c.dom_classes();
                        eprintln!("  btn[{i}] cls={cls:?}");
                    }
                    if children.len() < 4 {
                        eprintln!("ERROR: need >= 4 buttons");
                        return;
                    }
                    // Click 4th button (index 3) = reply in thread
                    let btn = &children[3];
                    let (bx, by) = btn.position().unwrap();
                    let (bw, bh) = btn.size().unwrap();
                    eprintln!("Clicking reply-in-thread button at ({}, {})", bx + bw / 2.0, by + bh / 2.0);
                    click_at(bx + bw / 2.0, by + bh / 2.0);
                    thread::sleep(Duration::from_millis(2000));

                    // Wait for thread input
                    eprintln!("Waiting for thread input...");
                    match wait_for_thread_input(&chat_wa, 8) {
                        Some(input) => {
                            eprintln!("Thread input found!");
                            input.set_focused(true);
                            thread::sleep(Duration::from_millis(200));
                            if let (Some((ix, iy)), Some((iw, ih))) = (input.position(), input.size()) {
                                click_at(ix + iw / 2.0, iy + ih / 2.0);
                                thread::sleep(Duration::from_millis(200));
                            }
                            type_text("[case2 test] new thread from toolbar");
                            thread::sleep(Duration::from_millis(200));
                            press_return();
                            eprintln!("Sent!");
                        }
                        None => eprintln!("ERROR: thread input not found after waiting"),
                    }
                }
                None => eprintln!("ERROR: toolbar not found after hover"),
            }
        }
        None => eprintln!("No card without thread found"),
    }
}
