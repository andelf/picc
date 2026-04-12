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
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --scroll-to
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --screenshot
//!   cargo run --bin ax_print -- --app Lark --locator '.SearchButton' --screenshot /tmp/shot.png

use objc2_core_foundation::{CFString, CFURLPathStyle, CGPoint, CGRect, CGSize, CFURL};
use objc2_core_graphics::CGImage;
use objc2_image_io::CGImageDestination;
use picc::accessibility::{self, AXNode};
use picc_macos_input as input;

const TEXT_ROLES: &[&str] = &["AXStaticText", "AXTextArea", "AXTextField"];

/// Sort class names: meaningful ones first, noisy ones (numeric, `-` or `_` prefixed) last.
fn sort_classes(classes: &[String]) -> Vec<&str> {
    let mut visible: Vec<&str> = classes
        .iter()
        .filter(|c| !c.starts_with('_'))
        .map(|s| s.as_str())
        .collect();
    visible.sort_by_key(|c| {
        if c.starts_with('-') || c.chars().next().map_or(true, |ch| ch.is_ascii_digit()) {
            1
        } else {
            0
        }
    });
    visible
}

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
        let (p, localized) = accessibility::find_app_by_name(mtm, name).expect("app not found");
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
    let do_scroll_to = args.iter().any(|a| a == "--scroll-to");
    let do_move = args.iter().any(|a| a == "--move-to");
    let do_click = args.iter().any(|a| a == "--click");
    let input_text = args.iter().position(|a| a == "--input").map(|i| {
        args.get(i + 1)
            .expect("--input requires a TEXT value")
            .clone()
    });
    let press_key = args.iter().position(|a| a == "--press").map(|i| {
        args.get(i + 1)
            .expect("--press requires a key like Enter, Tab, Control+a")
            .clone()
    });
    let do_screenshot = args.iter().any(|a| a == "--screenshot");
    let screenshot_path = if do_screenshot {
        // Next arg after --screenshot is optional path (if it doesn't start with --)
        let pos = args.iter().position(|a| a == "--screenshot").unwrap();
        args.get(pos + 1).filter(|s| !s.starts_with("--")).cloned()
    } else {
        None
    };
    let has_action = do_scroll_to || do_move || do_click || input_text.is_some() || do_screenshot;
    let _needs_app = has_action || press_key.is_some();

    // --locator: resolve a locator string
    let roots = if let Some(pos) = args.iter().position(|a| a == "--locator") {
        let loc = args
            .get(pos + 1)
            .expect("--locator requires a selector string");
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

        // Bring app to foreground
        input::activate_app(pid);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // --scroll-to: ask the scroll container to reveal this element
        if do_scroll_to {
            eprintln!("Scrolling element into view...");
            if !accessibility::perform_action(&node.0, "AXScrollToVisible") {
                eprintln!(
                    "warning: AXScrollToVisible failed (element may not be in a scroll area)"
                );
            } else {
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }

        // Get position/size (after scroll, so coordinates are up-to-date)
        let (w, h) = node.size().unwrap_or((0.0, 0.0));
        if w == 0.0 && h == 0.0 && !is_menu {
            eprintln!("error: element has zero size (not visible)");
            std::process::exit(1);
        }
        let (x, y) = node.position().unwrap_or((0.0, 0.0));
        let center_x = x + w / 2.0;
        let center_y = y + h / 2.0;

        if do_move {
            if is_menu {
                eprintln!("warning: --move-to not meaningful for menu items");
            } else {
                eprintln!("Moving mouse to ({center_x:.0}, {center_y:.0})");
                input::mouse_move(center_x, center_y);
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
                input::mouse_move(center_x, center_y);
                std::thread::sleep(std::time::Duration::from_millis(50));
                input::mouse_click(center_x, center_y);
            }
        }
        if let Some(ref text) = input_text {
            // Focus element, then type text
            let focused = node.set_focused(true);
            if !focused {
                // Fallback: click to focus
                eprintln!("AXFocused failed, clicking to focus...");
                input::mouse_move(center_x, center_y);
                std::thread::sleep(std::time::Duration::from_millis(50));
                input::mouse_click(center_x, center_y);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            eprintln!("Typing: {text:?}");
            input::type_text(text);
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
        input::activate_app(pid);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (keycode, flags) = input::parse_key_combo(combo);
        eprintln!("Pressing: {combo} (keycode={keycode}, flags=0x{flags:x})");
        input::press_key_combo(keycode, flags);
        return;
    }

    // Default: print tree
    let mut interactive = 0usize;
    let multi = roots.len() > 1;
    for (i, root) in roots.iter().enumerate() {
        if multi {
            if i > 0 {
                println!();
            }
            eprintln!("--- match {}/{} ---", i + 1, roots.len());
            print_with_ancestors(root, max_depth, &mut interactive);
        } else {
            print_tree(root, 0, max_depth, &mut interactive);
        }
    }
    eprintln!("\n({interactive} interactive elements)");
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
    for cls in sort_classes(&dom_classes) {
        s.push_str(&format!(".{cls}"));
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
        print_tree_inner(
            &child,
            base_depth + 1,
            base_depth + max_depth,
            interactive,
            false,
        );
    }
}

fn print_tree(node: &AXNode, depth: usize, max_depth: usize, interactive: &mut usize) {
    print_tree_inner(node, depth, max_depth, interactive, depth == 0);
}

fn print_tree_inner(
    node: &AXNode,
    depth: usize,
    max_depth: usize,
    interactive: &mut usize,
    is_root: bool,
) {
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
        let short_role = role.strip_prefix("AX").unwrap_or(&role).to_lowercase();

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
            for cls in sort_classes(&dom_classes) {
                line.push_str(&format!(".{cls}"));
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
    let cf_path = CFString::from_str(path);
    let url = CFURL::with_file_system_path(
        None,
        Some(&cf_path),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
    );
    let Some(url) = url else {
        eprintln!("error: failed to create URL for {path}");
        std::process::exit(1);
    };
    let png_type = CFString::from_str("public.png");
    let dest = unsafe { CGImageDestination::with_url(&url, &png_type, 1, None) };
    let Some(dest) = dest else {
        eprintln!("error: failed to create image destination for {path}");
        std::process::exit(1);
    };
    unsafe {
        dest.add_image(image, None);
        if dest.finalize() {
            eprintln!("Saved: {path}");
        } else {
            eprintln!("error: failed to save {path}");
            std::process::exit(1);
        }
    }
}
