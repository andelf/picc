//! Print a simplified, DevTools-style accessibility tree for a macOS app.
//!
//! Usage:
//!   cargo run --example ax_print -- --app Lark
//!   cargo run --example ax_print -- --pid 1234
//!   cargo run --example ax_print -- --pid 1234 --depth 15

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

    let app = if let Some(pos) = args.iter().position(|a| a == "--pid") {
        let pid: i32 = args
            .get(pos + 1)
            .expect("--pid requires a value")
            .parse()
            .expect("invalid pid");
        AXNode::app(pid)
    } else if let Some(pos) = args.iter().position(|a| a == "--app") {
        let name = args.get(pos + 1).expect("--app requires a value");
        let mtm = objc2::MainThreadMarker::new().expect("must run on main thread");
        let (pid, localized) =
            accessibility::find_app_by_name(mtm, name).expect("app not found");
        eprintln!("Found app: {localized} (pid={pid})");
        AXNode::app(pid)
    } else {
        eprintln!("Usage: ax_print --app <name> | --pid <pid> [--depth N]");
        std::process::exit(1);
    };

    if !accessibility::is_trusted() {
        eprintln!("Accessibility not granted");
        std::process::exit(1);
    }

    let mut interactive = 0usize;
    print_tree(&app, 0, max_depth, &mut interactive);
    eprintln!("\n({interactive} interactive elements)");
}

fn print_tree(node: &AXNode, depth: usize, max_depth: usize, interactive: &mut usize) {
    if depth > max_depth {
        return;
    }

    // size 为 0 → 跳过含子树
    if let Some((w, h)) = node.size() {
        if w == 0.0 && h == 0.0 {
            return;
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

    let has_identity = !title.is_empty()
        || !desc.is_empty()
        || !value.is_empty()
        || !dom_id.is_empty()
        || !dom_classes.is_empty();

    let has_meaningful_actions = actions
        .iter()
        .any(|a| a != "AXScrollToVisible" && a != "AXShowMenu");

    let is_text_node = is_text && !value.is_empty();
    let is_interactive = !is_text_node && has_identity && has_meaningful_actions;
    let is_structural = !is_text_node && !is_interactive && has_identity;
    let keep = is_text_node || is_interactive || is_structural;

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
                line.push_str(&format!(".{cls}"));
            }

            if !title.is_empty() {
                line.push_str(&format!(" \"{}\"", truncate(&title, 60)));
            } else if !desc.is_empty() {
                line.push_str(&format!(" \"{}\"", truncate(&desc, 60)));
            }

            if is_interactive {
                *interactive += 1;
                // no ref tag for now
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
                print_tree(&child, depth + 1, max_depth, interactive);
            }
        }
    } else {
        // 透传：跳过自身，子节点继承当前 depth
        for child in node.children() {
            print_tree(&child, depth, max_depth, interactive);
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
