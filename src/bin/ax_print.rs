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

use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
};
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
    let has_action = do_move || do_click || input_text.is_some();

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
            eprintln!("error: actions require --locator");
            std::process::exit(1);
        }
        vec![app]
    };

    // Execute action if requested
    if has_action {
        let node = &roots[0];

        // Validate element is visible (has non-zero size and position)
        let (w, h) = node.size().unwrap_or((0.0, 0.0));
        if w == 0.0 && h == 0.0 {
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
            eprintln!("Moving mouse to ({center_x:.0}, {center_y:.0})");
            mouse_move(center_x, center_y);
        }
        if do_click {
            eprintln!("Clicking at ({center_x:.0}, {center_y:.0})");
            mouse_move(center_x, center_y);
            std::thread::sleep(std::time::Duration::from_millis(50));
            mouse_click(center_x, center_y);
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
