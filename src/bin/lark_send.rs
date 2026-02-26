use objc2::MainThreadMarker;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
};

use picc::accessibility::{self, AXNode, AXQuery, role};

fn find_lark_app(
    _mtm: MainThreadMarker,
) -> Option<(i32, objc2::rc::Retained<NSRunningApplication>)> {
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
    let workspace: objc2::rc::Retained<objc2::runtime::NSObject> =
        unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let apps: objc2::rc::Retained<objc2_foundation::NSArray<NSRunningApplication>> =
        unsafe { objc2::msg_send![&workspace, runningApplications] };

    for app in apps.iter() {
        if let Some(bundle_id) = app.bundleIdentifier() {
            let bundle_str = bundle_id.to_string();
            if bundle_str.contains("lark")
                || bundle_str.contains("Lark")
                || bundle_str.contains("feishu")
            {
                let pid = app.processIdentifier();
                let name = app
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                eprintln!("Found: {} (bundle={}, pid={})", name, bundle_str, pid);
                return Some((pid, app.clone()));
            }
        }
    }
    None
}

/// Simulate a mouse click at the given screen coordinates.
fn click_at(x: f64, y: f64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let point = CGPoint { x, y };

    let down = CGEvent::new_mouse_event(
        source.as_deref(),
        CGEventType::LeftMouseDown,
        point,
        CGMouseButton::Left,
    );
    let up = CGEvent::new_mouse_event(
        source.as_deref(),
        CGEventType::LeftMouseUp,
        point,
        CGMouseButton::Left,
    );
    if let Some(ref ev) = down {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    if let Some(ref ev) = up {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

/// Type a Unicode string via CGEvent keyboard events.
///
/// Sends text in small chunks using `keyboard_set_unicode_string`,
/// which supports arbitrary Unicode including CJK characters.
fn type_text(text: &str) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    // Send in chunks of up to 20 UTF-16 code units
    let utf16: Vec<u16> = text.encode_utf16().collect();

    for chunk in utf16.chunks(20) {
        // Key down with unicode string
        let down = CGEvent::new_keyboard_event(source.as_deref(), 0, true);
        if let Some(ref ev) = down {
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    Some(ev),
                    chunk.len() as _,
                    chunk.as_ptr(),
                );
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Key up
        let up = CGEvent::new_keyboard_event(source.as_deref(), 0, false);
        if let Some(ref ev) = up {
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    Some(ev),
                    chunk.len() as _,
                    chunk.as_ptr(),
                );
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Send Return key (keycode 36).
fn press_return() {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let down = CGEvent::new_keyboard_event(source.as_deref(), 36, true);
    if let Some(ref ev) = down {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
    let up = CGEvent::new_keyboard_event(source.as_deref(), 36, false);
    if let Some(ref ev) = up {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

/// Print a subtree of the AX tree for debugging (explore mode).
fn dump_tree(node: &AXNode, indent: usize, max_depth: usize) {
    if max_depth == 0 {
        return;
    }
    let role = node.role().unwrap_or_default();
    let title = node.title().unwrap_or_default();
    let value = node.value().unwrap_or_default();
    let desc = node.description().unwrap_or_default();
    let actions = node.actions();
    let child_count = node.child_count();

    let pad = " ".repeat(indent);
    let mut info = format!("{pad}{role}");
    if !title.is_empty() {
        info.push_str(&format!(" title={:?}", truncate(&title, 60)));
    }
    if !value.is_empty() {
        info.push_str(&format!(" value={:?}", truncate(&value, 60)));
    }
    if !desc.is_empty() {
        info.push_str(&format!(" desc={:?}", truncate(&desc, 60)));
    }
    if child_count > 0 {
        info.push_str(&format!(" (c={})", child_count));
    }
    if !actions.is_empty() {
        info.push_str(&format!(" actions={:?}", actions));
    }
    println!("{}", info);

    for child in node.children() {
        dump_tree(&child, indent + 2, max_depth - 1);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}...", t)
    }
}

/// Find the input field in the messenger-chat area.
/// Filters by focusable + editable AXTextArea/AXTextField.
fn find_input_field(chat_wa: &AXNode) -> Option<AXNode> {
    let editable_text = AXQuery::new().filter(|n| {
        let r = n.role().unwrap_or_default();
        if r != "AXTextArea" && r != "AXTextField" {
            return false;
        }
        // Must have AXFocused attribute (i.e., it's focusable)
        let attrs = n.attr_names();
        attrs.contains(&"AXFocused".to_string())
            && attrs.contains(&"AXEditableAncestor".to_string())
    });
    chat_wa.find(editable_text)
}

/// Extract the chat target name from the input field placeholder, window title, or chat area.
fn find_chat_target(win: &AXNode, chat_wa: &AXNode) -> Option<String> {
    // Try the input field's value/placeholder: "Message <chat_name>"
    if let Some(input) = find_input_field(chat_wa) {
        // Check AXPlaceholderValue first
        if let Some(ph) = accessibility::attr_string(&input.0, "AXPlaceholderValue") {
            if let Some(name) = ph.strip_prefix("Message ").or_else(|| ph.strip_prefix("发送给 ")) {
                let name = name.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
        // Fallback: parse from AXValue which contains "Message <name>\n..."
        if let Some(val) = input.value() {
            if let Some(first_line) = val.lines().next() {
                let first_line = first_line.trim();
                if let Some(name) = first_line.strip_prefix("Message ").or_else(|| first_line.strip_prefix("发送给 ")) {
                    let name = name.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    // Try the window title — Lark sometimes shows "Name - Lark"
    if let Some(title) = win.title() {
        let title = title.trim().to_string();
        if title != "Lark" && title != "飞书" && title != "Feishu" && !title.is_empty() {
            let name = title
                .strip_suffix(" - Lark")
                .or_else(|| title.strip_suffix(" - 飞书"))
                .or_else(|| title.strip_suffix(" - Feishu"))
                .unwrap_or(&title);
            return Some(name.to_string());
        }
    }
    None
}

fn main() {
    if !accessibility::is_trusted() {
        eprintln!("Accessibility permission not granted.");
        eprintln!("Go to System Settings > Privacy & Security > Accessibility and add this app.");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let (pid, lark_app) = match find_lark_app(mtm) {
        Some(v) => v,
        None => {
            eprintln!("Lark is not running.");
            std::process::exit(1);
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let explore = args.first().map(|a| a == "--explore").unwrap_or(false);
    let message = if explore {
        None
    } else {
        args.first().cloned()
    };

    let app = AXNode::app(pid);

    // Find Lark main window
    let win = app
        .find_all(role("AXWindow"))
        .into_iter()
        .find(|w| {
            let t = w.title().unwrap_or_default();
            // Accept any Lark window — the main one or a chat-specific one
            t == "Lark"
                || t == "飞书"
                || t == "Feishu"
                || t.contains("Lark")
                || t.contains("飞书")
        });

    let win = match win {
        Some(w) => w,
        None => {
            eprintln!("No Lark window found.");
            return;
        }
    };
    eprintln!("Window: {:?}", win.title());

    // Find messenger-chat AXWebArea
    let chat_wa = win.select(&[
        AXQuery::new()
            .role("AXWebArea")
            .title_contains("messenger-chat"),
    ]);

    let chat_wa = match chat_wa {
        Some(wa) => {
            eprintln!("Found messenger-chat");
            wa
        }
        None => {
            eprintln!("messenger-chat not found. Is Lark showing a chat?");
            let web_areas = win.find_all(role("AXWebArea"));
            for wa in &web_areas {
                eprintln!("  AXWebArea: title={:?}", wa.title());
            }
            return;
        }
    };

    // Explore mode: dump the AX tree to discover input field structure
    if explore {
        eprintln!("=== Explore mode: dumping messenger-chat subtree ===\n");
        dump_tree(&chat_wa, 0, 8);

        eprintln!("\n=== Looking for input field ===");
        // Search for text areas and text fields
        let text_areas = chat_wa.find_all(role("AXTextArea"));
        let text_fields = chat_wa.find_all(role("AXTextField"));
        eprintln!("AXTextArea count: {}", text_areas.len());
        for (i, ta) in text_areas.iter().enumerate() {
            eprintln!(
                "  [{}] title={:?} value={:?} desc={:?} actions={:?}",
                i,
                ta.title(),
                ta.value(),
                ta.description(),
                ta.actions()
            );
            eprintln!("      attrs={:?}", ta.attr_names());
        }
        eprintln!("AXTextField count: {}", text_fields.len());
        for (i, tf) in text_fields.iter().enumerate() {
            eprintln!(
                "  [{}] title={:?} value={:?} desc={:?} actions={:?}",
                i,
                tf.title(),
                tf.value(),
                tf.description(),
                tf.actions()
            );
        }

        eprintln!("\n=== Chat target ===");
        let target = find_chat_target(&win, &chat_wa);
        eprintln!("Chat target: {:?}", target);
        return;
    }

    // Send mode
    let message = match message {
        Some(m) => m,
        None => {
            eprintln!("Usage: lark_send <message>");
            eprintln!("       lark_send --explore");
            std::process::exit(1);
        }
    };

    // Find chat target
    let target = find_chat_target(&win, &chat_wa);
    if let Some(ref name) = target {
        eprintln!("Chat target: {}", name);
    } else {
        eprintln!("Warning: could not determine chat target");
    }

    // Find input field
    let input = find_input_field(&chat_wa);
    let input = match input {
        Some(f) => {
            eprintln!(
                "Input field: role={:?} desc={:?}",
                f.role(),
                f.description()
            );
            f
        }
        None => {
            eprintln!("Input field not found in messenger-chat.");
            eprintln!("Try running with --explore to inspect the AX tree.");
            return;
        }
    };

    // Activate Lark window
    #[allow(deprecated)]
    lark_app.activateWithOptions(
        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    );
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Focus the input field: try AX focus first, then click as fallback
    let focused = input.set_focused(true);
    if focused {
        eprintln!("Focused input via AXFocused");
    } else {
        eprintln!("AXFocused failed, clicking input field...");
        if let (Some(pos), Some(sz)) = (input.position(), input.size()) {
            let cx = pos.0 + sz.0 / 2.0;
            let cy = pos.1 + sz.1 / 2.0;
            click_at(cx, cy);
        } else {
            // Try AXPress action
            input.perform_action("AXPress");
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Type the message
    eprintln!("Typing message: {:?}", truncate(&message, 50));
    type_text(&message);
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Press Return to send
    eprintln!("Sending...");
    press_return();
    std::thread::sleep(std::time::Duration::from_millis(100));

    if let Some(ref name) = target {
        eprintln!("Message sent to {}", name);
    } else {
        eprintln!("Message sent");
    }
}
