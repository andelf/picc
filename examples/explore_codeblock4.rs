use picc::accessibility::{self, AXNode, AXQuery};

fn dump_with_classes(node: &AXNode, indent: usize, max_depth: usize) {
    if max_depth == 0 { return; }
    let role = node.role().unwrap_or_default();
    let val = node.value().unwrap_or_default();
    let classes = node.dom_classes();
    let cc = node.child_count();
    let pad = " ".repeat(indent);
    let mut info = format!("{pad}{role}");
    if cc > 0 { info.push_str(&format!(" (c={cc})")); }
    if !classes.is_empty() { info.push_str(&format!(" cls={classes:?}")); }
    if !val.is_empty() {
        let v: String = val.chars().take(60).collect();
        info.push_str(&format!(" val={v:?}"));
    }
    eprintln!("{info}");
    for child in node.children() {
        dump_with_classes(&child, indent + 2, max_depth - 1);
    }
}

fn main() {
    let _mtm = objc2::MainThreadMarker::new().unwrap();
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
    let workspace: objc2::rc::Retained<objc2::runtime::NSObject> =
        unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let apps: objc2::rc::Retained<objc2_foundation::NSArray<objc2_app_kit::NSRunningApplication>> =
        unsafe { objc2::msg_send![&workspace, runningApplications] };
    let mut pid = 0i32;
    for app in apps.iter() {
        if let Some(bid) = app.bundleIdentifier() {
            if bid.to_string().contains("lark") || bid.to_string().contains("Lark") {
                pid = app.processIdentifier();
                break;
            }
        }
    }
    if pid == 0 { eprintln!("No Lark"); return; }

    let app = AXNode::app(pid);
    let win = app.find_all(accessibility::role("AXWindow")).into_iter()
        .find(|w| {
            let t = w.title().unwrap_or_default();
            t == "Lark" || t == "飞书"
        }).unwrap();
    let chat_wa = win.select(&[
        AXQuery::new().role("AXWebArea").title_contains("messenger-chat"),
    ]).unwrap();

    let items: Vec<AXNode> = accessibility::find_all(
        &chat_wa.0, &AXQuery::new().dom_class("message-item"), 30
    ).into_iter().map(AXNode::new).collect();

    eprintln!("Found {} items", items.len());
    if items.len() < 20 { return; }

    // Dump #19 fully with classes
    eprintln!("\n=== message-item #19 full dump with DOM classes ===\n");
    dump_with_classes(&items[19], 0, 20);
}
