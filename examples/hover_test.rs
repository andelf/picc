// Quick test: hover over a message card and dump AX tree to check for popup menu
use objc2::MainThreadMarker;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::*;
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
                let pid = app.processIdentifier();
                return Some((pid, app.clone()));
            }
        }
    }
    None
}

fn move_mouse(x: f64, y: f64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let point = CGPoint { x, y };
    let ev = CGEvent::new_mouse_event(source.as_deref(), CGEventType::MouseMoved, point, CGMouseButton::Left);
    if let Some(ref ev) = ev {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

fn dump(node: &AXNode, indent: usize, max_depth: usize) {
    if max_depth == 0 { return; }
    let r = node.role().unwrap_or_default();
    let title = node.title().unwrap_or_default();
    let value = node.value().unwrap_or_default();
    let desc = node.description().unwrap_or_default();
    let cc = node.child_count();
    let pad = " ".repeat(indent);
    let mut info = format!("{pad}{r}");
    if !title.is_empty() { info.push_str(&format!(" title={title:?}")); }
    if !value.is_empty() && value.len() < 80 { info.push_str(&format!(" val={value:?}")); }
    if !desc.is_empty() { info.push_str(&format!(" desc={desc:?}")); }
    if cc > 0 { info.push_str(&format!(" (c={cc})")); }
    let cls = node.dom_classes();
    if !cls.is_empty() { info.push_str(&format!(" cls={cls:?}")); }
    eprintln!("{info}");
    for child in node.children() {
        dump(&child, indent + 2, max_depth - 1);
    }
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    let (pid, _app) = find_lark_app(mtm).expect("no lark");
    let app = AXNode::app(pid);
    let win = app.find_all(role("AXWindow")).into_iter()
        .find(|w| { let t = w.title().unwrap_or_default(); t == "Lark" || t == "飞书" || t == "Feishu" })
        .expect("no window");
    let chat_wa = win.select(&[AXQuery::new().role("AXWebArea").title_contains("messenger-chat")])
        .expect("no chat");

    // Find last OTHER message-item
    let q = &AXQuery::new().dom_class("message-item");
    let items: Vec<AXNode> = accessibility::find_all(&chat_wa.0, q, 30).into_iter().map(AXNode::new).collect();
    let target = items.iter().rev().find(|item| {
        let cls = item.dom_classes();
        !cls.contains(&"message-self".to_string())
    }).expect("no OTHER message");

    let (x, y) = target.position().expect("no pos");
    let (w, h) = target.size().expect("no size");
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    eprintln!("Hovering over card at ({cx}, {cy})");

    // Move mouse to card center
    move_mouse(cx, cy);
    thread::sleep(Duration::from_millis(1500));

    // Now dump the card and its surroundings
    eprintln!("\n=== Card AX tree after hover ===");
    dump(target, 0, 15);

    // Also check for any popup/toolbar near the card - search for toolbar/menu/popover
    eprintln!("\n=== Searching for toolbar/popover/menu ===");
    for cls_name in &["toolbar", "popover", "action", "emoji", "reaction", "more"] {
        let found = chat_wa.find_all(AXQuery::new().dom_class(cls_name));
        if !found.is_empty() {
            eprintln!("Found {} nodes with dom_class={cls_name:?}", found.len());
            for (i, n) in found.iter().enumerate() {
                eprintln!("  [{i}]");
                dump(n, 4, 5);
            }
        }
    }

    // Also search by role
    for r in &["AXToolbar", "AXPopover", "AXMenu", "AXMenuBar"] {
        let found = chat_wa.find_all(role(r));
        if !found.is_empty() {
            eprintln!("Found {} nodes with role={r}", found.len());
            for n in &found {
                dump(n, 4, 5);
            }
        }
    }
}
