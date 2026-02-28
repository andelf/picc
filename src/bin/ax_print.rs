//! Print a simplified, DevTools-style accessibility tree for a macOS app.
//!
//! Usage:
//!   cargo run --bin ax_print -- --app Lark
//!   cargo run --bin ax_print -- --pid 1234
//!   cargo run --bin ax_print -- --pid 1234 --depth 15
//!   cargo run --bin ax_print -- --app Lark --locator '#root'
//!   cargo run --bin ax_print -- --app Lark --locator 'AXButton[title="Send"]'
//!   cargo run --bin ax_print -- --app Lark --locator '.feed-shortcut-item' --all
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --move-to
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --click
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --input 'hello'
//!   cargo run --bin ax_print -- --app Lark --press Enter
//!   cargo run --bin ax_print -- --app Lark --press 'Control+a'
//!   cargo run --bin ax_print -- --app Lark --press 'Command+Shift+v'
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --screenshot
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --screenshot /tmp/shot.png

use std::ffi::c_void;

use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType,
    CGImage, CGMouseButton,
};
use objc2_foundation::NSString;
use picc::accessibility::{self, AXNode};

const TEXT_ROLES: &[&str] = &["AXStaticText", "AXTextArea", "AXTextField"];

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let max_depth: usize = args
        .iter()
        .position(|a| a == "--depth")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let pid: i32;
    let app = if let Some(pos) = args.iter().position(|a| a == "--pid") {
        pid = args
            .get(pos + 1)
            .expect("--pid requires a value")
            .parse()
            .expect("invalid pid");
        AXNode::app(pid)
    } else if let Some(pos) = args.iter().position(|a| a == "--app") {
        let name = args.get(pos + 1).expect("--app requires a value");
        let mtm = objc2::MainThreadMarker::new().expect("must run on main thread");
        let (p, localized) =
            accessibility::find_app_by_name(mtm, name).expect("app not found");
        pid = p;
        eprintln!("Found app: {localized} (pid={pid})");
        AXNode::app(pid)
    } else {
        eprintln!("Usage: ax_print --app <name> | --pid <pid> [--depth N] [--locator SEL]");
        std::process::exit(1);
    };

    if !accessibility::is_trusted() {
        eprintln!("Accessibility not granted");
        std::process::exit(1);
    }

    let show_all = args.iter().any(|a| a == "--all");
    let do_move = args.iter().any(|a| a == "--move-to");
    let do_click = args.iter().any(|a| a == "--click");
    let input_text = args
        .iter()
        .position(|a| a == "--input")
        .map(|i| args.get(i + 1).expect("--input requires a TEXT value").clone());
    let press_key = args
        .iter()
        .position(|a| a == "--press")
        .map(|i| args.get(i + 1).expect("--press requires a key like Enter, Tab, Control+a").clone());
    let do_screenshot = args.iter().any(|a| a == "--screenshot");
    let screenshot_path = if do_screenshot {
        // Next arg after --screenshot is optional path (if it doesn't start with --)
        let pos = args.iter().position(|a| a == "--screenshot").unwrap();
        args.get(pos + 1)
            .filter(|s| !s.starts_with("--"))
            .cloned()
    } else {
        None
    };
    let has_action = do_move || do_click || input_text.is_some() || do_screenshot;
    let _needs_app = has_action || press_key.is_some();

    // --locator: resolve a locator string
    let roots = if let Some(pos) = args.iter().position(|a| a == "--locator") {
        let loc = args.get(pos + 1).expect("--locator requires a selector string");
        if show_all && !has_action {
            let nodes = app.locate_all(loc);
            if nodes.is_empty() {
                eprintln!("locator not found: {loc}");
                std::process::exit(1);
            }
            eprintln!("Found {} matches for {loc}", nodes.len());
            nodes
        } else {
            // For actions or single-select: use locate_all and validate
            let nodes = app.locate_all(loc);
            if nodes.is_empty() {
                eprintln!("locator not found: {loc}");
                std::process::exit(1);
            }
            if has_action && nodes.len() > 1 {
                eprintln!(
                    "error: locator matched {} elements, must be unique for actions",
                    nodes.len()
                );
                eprintln!("hint: use 'locator >> nth=N' to select one");
                std::process::exit(1);
            }
            let node = &nodes[0];
            eprintln!(
                "Resolved locator → role={:?} title={:?} children={}",
                node.role(),
                node.title(),
                node.child_count()
            );
            nodes
        }
    } else {
        if has_action {
            eprintln!("error: --move-to/--click/--input require --locator");
            std::process::exit(1);
        }
        vec![app]
    };

    // Execute action if requested
    if has_action {
        let node = &roots[0];

        let role = node.role().unwrap_or_default();
        let is_menu = role == "AXMenuItem" || role == "AXMenuBarItem";

        // Validate element is visible (menu items exempt — they can be AXPress'd without size)
        let (w, h) = node.size().unwrap_or((0.0, 0.0));
        if w == 0.0 && h == 0.0 && !is_menu {
            eprintln!("error: element has zero size (not visible)");
            std::process::exit(1);
        }
        let (x, y) = node.position().unwrap_or((0.0, 0.0));
        let center_x = x + w / 2.0;
        let center_y = y + h / 2.0;

        // Bring app to foreground
        activate_app(pid);
        std::thread::sleep(std::time::Duration::from_millis(200));

        if do_move {
            if is_menu {
                eprintln!("warning: --move-to not meaningful for menu items");
            } else {
                eprintln!("Moving mouse to ({center_x:.0}, {center_y:.0})");
                mouse_move(center_x, center_y);
            }
        }
        if do_click {
            if is_menu {
                eprintln!("Performing AXPress on {role}");
                if !accessibility::perform_action(&node.0, "AXPress") {
                    eprintln!("error: AXPress failed");
                    std::process::exit(1);
                }
            } else {
                eprintln!("Clicking at ({center_x:.0}, {center_y:.0})");
                mouse_move(center_x, center_y);
                std::thread::sleep(std::time::Duration::from_millis(50));
                mouse_click(center_x, center_y);
            }
        }
        if let Some(ref text) = input_text {
            // Focus element, then type text
            let focused = node.set_focused(true);
            if !focused {
                // Fallback: click to focus
                eprintln!("AXFocused failed, clicking to focus...");
                mouse_move(center_x, center_y);
                std::thread::sleep(std::time::Duration::from_millis(50));
                mouse_click(center_x, center_y);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            eprintln!("Typing: {text:?}");
            type_text(text);
        }
        if do_screenshot {
            let path = screenshot_path.clone().unwrap_or_else(|| {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                format!("/tmp/ax_screenshot_{ts}.png")
            });
            let rect = CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
            eprintln!("Capturing {w:.0}x{h:.0} at ({x:.0},{y:.0}) → {path}");
            let image = picc::screenshot(rect).expect("screenshot failed");
            save_cgimage(&image, &path);
        }
        return;
    }

    // --press: send key combo to the app (no locator needed)
    if let Some(ref combo) = press_key {
        activate_app(pid);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (keycode, flags) = parse_key_combo(combo);
        eprintln!("Pressing: {combo} (keycode={keycode}, flags=0x{flags:x})");
        press_key_combo(keycode, flags);
        return;
    }

    // Default: print tree
    let mut interactive = 0usize;
    let multi = roots.len() > 1;
    for (i, root) in roots.iter().enumerate() {
        if multi {
            if i > 0 { println!(); }
            eprintln!("--- match {}/{} ---", i + 1, roots.len());
            print_with_ancestors(root, max_depth, &mut interactive);
        } else {
            print_tree(root, 0, max_depth, &mut interactive);
        }
    }
    eprintln!("\n({interactive} interactive elements)");
}

fn activate_app(pid: i32) {
    let ns_app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid);
    if let Some(ns_app) = ns_app {
        #[allow(deprecated)]
        ns_app.activateWithOptions(
            objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
        );
    }
}

fn mouse_move(x: f64, y: f64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let point = CGPoint { x, y };
    let event = CGEvent::new_mouse_event(
        source.as_deref(),
        CGEventType::MouseMoved,
        point,
        CGMouseButton::Left,
    );
    if let Some(ref ev) = event {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

fn mouse_click(x: f64, y: f64) {
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

fn type_text(text: &str) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let utf16: Vec<u16> = text.encode_utf16().collect();
    for chunk in utf16.chunks(20) {
        let down = CGEvent::new_keyboard_event(source.as_deref(), 0, true);
        if let Some(ref ev) = down {
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(ev), chunk.len() as _, chunk.as_ptr());
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        let up = CGEvent::new_keyboard_event(source.as_deref(), 0, false);
        if let Some(ref ev) = up {
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(ev), chunk.len() as _, chunk.as_ptr());
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn parse_key_combo(combo: &str) -> (u16, u64) {
    let parts: Vec<&str> = combo.split('+').map(|s| s.trim()).collect();
    let mut flags: u64 = 0;
    let mut key_name = "";

    for part in &parts {
        match part.to_lowercase().as_str() {
            "control" | "ctrl" => flags |= 0x40000,
            "shift" => flags |= 0x20000,
            "option" | "alt" => flags |= 0x80000,
            "command" | "cmd" | "super" => flags |= 0x100000,
            _ => key_name = part,
        }
    }

    let keycode = match key_name.to_lowercase().as_str() {
        "return" | "enter" => 36,
        "tab" => 48,
        "space" => 49,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "home" => 115,
        "end" => 119,
        "pageup" => 116,
        "pagedown" => 121,
        "f1" => 122, "f2" => 120, "f3" => 99, "f4" => 118,
        "f5" => 96, "f6" => 97, "f7" => 98, "f8" => 100,
        "f9" => 101, "f10" => 109, "f11" => 103, "f12" => 111,
        // Single character keys
        s if s.len() == 1 => {
            let ch = s.chars().next().unwrap();
            match ch {
                'a' => 0, 's' => 1, 'd' => 2, 'f' => 3, 'h' => 4,
                'g' => 5, 'z' => 6, 'x' => 7, 'c' => 8, 'v' => 9,
                'b' => 11, 'q' => 12, 'w' => 13, 'e' => 14, 'r' => 15,
                'y' => 16, 't' => 17, '1' => 18, '2' => 19, '3' => 20,
                '4' => 21, '6' => 22, '5' => 23, '=' => 24, '9' => 25,
                '7' => 26, '-' => 27, '8' => 28, '0' => 29, ']' => 30,
                'o' => 31, 'u' => 32, '[' => 33, 'i' => 34, 'p' => 35,
                'l' => 37, 'j' => 38, '\'' => 39, 'k' => 40, ';' => 41,
                '\\' => 42, ',' => 43, '/' => 44, 'n' => 45, 'm' => 46,
                '.' => 47,
                _ => {
                    eprintln!("warning: unknown key '{ch}', using keycode 0");
                    0
                }
            }
        }
        _ => {
            eprintln!("warning: unknown key '{key_name}', using keycode 0");
            0
        }
    };

    (keycode, flags)
}

fn press_key_combo(keycode: u16, flags: u64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);

    let down = CGEvent::new_keyboard_event(source.as_deref(), keycode, true);
    if let Some(ref ev) = down {
        if flags != 0 {
            CGEvent::set_flags(Some(ev), CGEventFlags(flags));
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
    let up = CGEvent::new_keyboard_event(source.as_deref(), keycode, false);
    if let Some(ref ev) = up {
        if flags != 0 {
            CGEvent::set_flags(Some(ev), CGEventFlags(flags));
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

/// Format a one-line summary of a node (no trailing newline).
fn format_node_line(node: &AXNode) -> String {
    let role = node.role().unwrap_or_default();
    let title = node.title().unwrap_or_default();
    let desc = node.description().unwrap_or_default();
    let value = node.value().unwrap_or_default();
    let dom_id = accessibility::attr_string(&node.0, "AXDOMIdentifier").unwrap_or_default();
    let dom_classes = node.dom_classes();
    let is_text = TEXT_ROLES.iter().any(|r| role == *r);

    let short_role = role.strip_prefix("AX").unwrap_or(&role).to_lowercase();

    if is_text && !value.is_empty() {
        return format!("text: \"{}\"", truncate(&value, 60));
    }

    let mut s = short_role;
    if !dom_id.is_empty() {
        s.push_str(&format!("#{dom_id}"));
    }
    for cls in &dom_classes {
        if !cls.starts_with('_') {
            s.push_str(&format!(".{cls}"));
        }
    }
    if !title.is_empty() {
        s.push_str(&format!(" \"{}\"", truncate(&title, 40)));
    } else if !desc.is_empty() {
        s.push_str(&format!(" \"{}\"", truncate(&desc, 40)));
    }
    s
}

/// Print a matched node with up to 3 ancestor levels for context.
fn print_with_ancestors(node: &AXNode, max_depth: usize, interactive: &mut usize) {
    // Collect up to 3 ancestors
    let mut ancestors = Vec::new();
    let mut cur = node.parent();
    for _ in 0..3 {
        match cur {
            Some(p) => {
                ancestors.push(p);
                cur = ancestors.last().unwrap().parent();
            }
            None => break,
        }
    }
    ancestors.reverse();

    // Print ancestor chain
    for (i, anc) in ancestors.iter().enumerate() {
        let indent = "  ".repeat(i);
        println!("{indent}- {}:", format_node_line(anc));
    }

    // Print matched node + its subtree
    let base_depth = ancestors.len();
    let indent = "  ".repeat(base_depth);
    let line = format_node_line(node);
    println!("{indent}- {line}  ← matched");
    // Subtree children
    for child in node.children() {
        print_tree_inner(&child, base_depth + 1, base_depth + max_depth, interactive, false);
    }
}

fn print_tree(node: &AXNode, depth: usize, max_depth: usize, interactive: &mut usize) {
    print_tree_inner(node, depth, max_depth, interactive, depth == 0);
}

fn print_tree_inner(node: &AXNode, depth: usize, max_depth: usize, interactive: &mut usize, is_root: bool) {
    if depth > max_depth {
        return;
    }

    // size 为 0 → 跳过含子树（但不跳过 locator 指定的根节点）
    if !is_root {
        if let Some((w, h)) = node.size() {
            if w == 0.0 && h == 0.0 {
                return;
            }
        }
    }

    let role = node.role().unwrap_or_default();
    let title = node.title().unwrap_or_default();
    let desc = node.description().unwrap_or_default();
    let value = node.value().unwrap_or_default();
    let dom_id = accessibility::attr_string(&node.0, "AXDOMIdentifier").unwrap_or_default();
    let dom_classes = node.dom_classes();
    let actions = node.actions();
    let is_text = TEXT_ROLES.iter().any(|r| role == *r);

    let has_visible_class = dom_classes.iter().any(|c| !c.starts_with('_'));
    let has_identity = !title.is_empty()
        || !desc.is_empty()
        || !value.is_empty()
        || !dom_id.is_empty()
        || has_visible_class;

    let has_meaningful_actions = actions
        .iter()
        .any(|a| a != "AXScrollToVisible" && a != "AXShowMenu");

    let is_text_node = is_text && !value.is_empty();
    let is_interactive = !is_text_node && has_identity && has_meaningful_actions;
    let is_structural = !is_text_node && !is_interactive && has_identity;
    let keep = is_root || is_text_node || is_interactive || is_structural;

    if keep {
        let short_role = role
            .strip_prefix("AX")
            .unwrap_or(&role)
            .to_lowercase();

        let indent = "  ".repeat(depth);
        let mut line = String::new();

        if is_text_node {
            let display_value = truncate(&value, 80);
            line = format!("{indent}- text: \"{display_value}\"");
        } else {
            line.push_str(&format!("{indent}- {short_role}"));

            if !dom_id.is_empty() {
                line.push_str(&format!("#{dom_id}"));
            }
            for cls in &dom_classes {
                if !cls.starts_with('_') {
                    line.push_str(&format!(".{cls}"));
                }
            }

            if !title.is_empty() {
                line.push_str(&format!(" \"{}\"", truncate(&title, 60)));
            } else if !desc.is_empty() {
                line.push_str(&format!(" \"{}\"", truncate(&desc, 60)));
            }

            if is_interactive {
                *interactive += 1;
            } else {
                line.push(':');
            }
        }

        println!("{line}");

        if depth == max_depth {
            let remaining = count_descendants(node, 5);
            if remaining > 0 {
                let child_indent = "  ".repeat(depth + 1);
                let suffix = if remaining >= 100 { "+" } else { "" };
                println!("{child_indent}... {remaining}{suffix} more");
            }
        } else {
            for child in node.children() {
                print_tree_inner(&child, depth + 1, max_depth, interactive, false);
            }
        }
    } else {
        // 透传：跳过自身，子节点继承当前 depth
        for child in node.children() {
            print_tree_inner(&child, depth, max_depth, interactive, false);
        }
    }
}

/// Count descendants up to a shallow depth, capped at 100 for speed.
fn count_descendants(node: &AXNode, max_depth: usize) -> usize {
    if max_depth == 0 {
        return 0;
    }
    let mut total = 0;
    for child in node.children() {
        total += 1;
        if total >= 100 {
            return total;
        }
        total += count_descendants(&child, max_depth - 1);
        if total >= 100 {
            return total;
        }
    }
    total
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.replace('\n', "\\n")
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}...", truncated.replace('\n', "\\n"))
    }
}

fn save_cgimage(image: &CGImage, path: &str) {
    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageDestinationCreateWithURL(
            url: *const c_void,
            ty: *const c_void,
            count: usize,
            options: *const c_void,
        ) -> *mut c_void;
        fn CGImageDestinationAddImage(
            dest: *mut c_void,
            image: *const c_void,
            properties: *const c_void,
        );
        fn CGImageDestinationFinalize(dest: *mut c_void) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFURLCreateWithFileSystemPath(
            allocator: *const c_void,
            path: *const c_void,
            style: i32,
            is_dir: bool,
        ) -> *const c_void;
    }

    unsafe {
        let ns_path = NSString::from_str(path);
        let url = CFURLCreateWithFileSystemPath(
            std::ptr::null(),
            (&*ns_path as *const NSString).cast(),
            0, // kCFURLPOSIXPathStyle
            false,
        );
        let png_type = NSString::from_str("public.png");
        let dest = CGImageDestinationCreateWithURL(
            url,
            (&*png_type as *const NSString).cast(),
            1,
            std::ptr::null(),
        );
        CGImageDestinationAddImage(dest, (image as *const CGImage).cast(), std::ptr::null());
        let ok = CGImageDestinationFinalize(dest);
        if ok {
            eprintln!("Saved: {path}");
        } else {
            eprintln!("error: failed to save {path}");
            std::process::exit(1);
        }
    }
}
