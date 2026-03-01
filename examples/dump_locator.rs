//! Dump DFS locator for every node in an app's AX tree.
//!
//! Usage: cargo run --example dump_locator -- --pid 33262 [--depth 5]

use picc::accessibility::{self, AXNode};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pid: i32 = args
        .iter()
        .position(|a| a == "--pid")
        .and_then(|i| args.get(i + 1))
        .expect("--pid <pid> required")
        .parse()
        .expect("invalid pid");

    let max_depth: usize = args
        .iter()
        .position(|a| a == "--depth")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    if !accessibility::is_trusted() {
        eprintln!("Accessibility not granted");
        std::process::exit(1);
    }

    let app = AXNode::app(pid);
    eprintln!("App root: role={:?}, children={}", app.role(), app.child_count());
    eprintln!("Dumping locators (max_depth={max_depth})...\n");

    dump_dfs_locator(&app.0, &app, 0, max_depth);
}

fn dump_dfs_locator(
    root: &objc2_application_services::AXUIElement,
    node: &AXNode,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
        return;
    }

    let indent = "  ".repeat(depth);
    let role = node.role().unwrap_or_else(|| "?".into());
    let title = node.title().unwrap_or_default();
    let dom_id = accessibility::attr_string(&node.0, "AXDOMIdentifier").unwrap_or_default();

    // Generate locator using the app root
    let locator = accessibility::generate_locator(root, &node.0);

    // Also try resolving back
    let resolved = accessibility::resolve_locator(root, &locator);
    let roundtrip = match resolved {
        Some(ref found) => {
            if objc2_core_foundation::CFEqual(Some(found.as_ref()), Some(node.0.as_ref())) {
                "OK"
            } else {
                "MISMATCH"
            }
        }
        None => "NOT_FOUND",
    };

    let label = if !title.is_empty() {
        format!("{role} title={title:?}")
    } else if !dom_id.is_empty() {
        format!("{role} #{dom_id}")
    } else {
        role.clone()
    };

    println!("{indent}{label}  =>  {locator}  [{roundtrip}]");

    for child in node.children() {
        dump_dfs_locator(root, &child, depth + 1, max_depth);
    }
}
