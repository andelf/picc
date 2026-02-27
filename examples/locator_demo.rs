//! Demo: exercise the Playwright-style Locator API against a running app.
//!
//! Usage:
//!   cargo run --example locator_demo -- --pid <pid>
//!   cargo run --example locator_demo -- --app <name>

use picc::accessibility::{self, role, AXNode};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let app = if let Some(pos) = args.iter().position(|a| a == "--pid") {
        let pid: i32 = args[pos + 1].parse().expect("invalid pid");
        AXNode::app(pid)
    } else if let Some(pos) = args.iter().position(|a| a == "--app") {
        let name = &args[pos + 1];
        let mtm = objc2::MainThreadMarker::new().expect("must run on main thread");
        let (pid, localized) =
            accessibility::find_app_by_name(mtm, name).expect("app not found");
        println!("Found app: {localized} (pid {pid})");
        AXNode::app(pid)
    } else {
        eprintln!("Usage: locator_demo --pid <pid>  OR  --app <name>");
        std::process::exit(1);
    };

    println!("=== Locator API Demo ===\n");

    // 1. get_by_role — find all buttons
    let buttons = app.get_by_role("AXButton", "");
    let count = buttons.count();
    println!("[get_by_role] Found {count} AXButton(s)");
    for (i, btn) in buttons.all().iter().enumerate().take(5) {
        println!(
            "  [{i}] title={:?}  desc={:?}",
            btn.title(),
            btn.description()
        );
    }
    if count > 5 {
        println!("  ... and {} more", count - 5);
    }
    println!();

    // 2. get_by_role with name
    let close_btn = app.get_by_role("AXButton", "关闭");
    if let Some(node) = close_btn.resolve() {
        println!("[get_by_role+name] Found '关闭' button: role={:?}", node.role());
    } else {
        // Try English
        let close_btn = app.get_by_role("AXButton", "Close");
        if let Some(node) = close_btn.resolve() {
            println!("[get_by_role+name] Found 'Close' button: role={:?}", node.role());
        } else {
            println!("[get_by_role+name] No '关闭'/'Close' button found");
        }
    }
    println!();

    // 3. get_by_title
    let windows = app.get_by_role("AXWindow", "");
    println!("[windows] Found {} window(s)", windows.count());
    for w in windows.all().iter().take(3) {
        println!("  title={:?}", w.title());
    }
    println!();

    // 4. Chaining: find static text within first window
    let texts_in_window = app
        .get_by_role("AXWindow", "")
        .first()
        .get_by_role("AXStaticText", "");
    let text_count = texts_in_window.count();
    println!("[chaining] {text_count} AXStaticText in first window");
    for (i, t) in texts_in_window.all().iter().enumerate().take(5) {
        println!("  [{i}] value={:?}", t.value());
    }
    println!();

    // 5. filter with has_text
    let groups = app
        .get_by_role("AXGroup", "")
        .first();
    if let Some(g) = groups.resolve() {
        println!("[filter] First AXGroup: title={:?}, children={}", g.title(), g.child_count());
    } else {
        println!("[filter] No AXGroup found");
    }
    println!();

    // 6. nth / first / last
    let second_btn = app.get_by_role("AXButton", "").nth(1);
    if let Some(btn) = second_btn.resolve() {
        println!("[nth(1)] Second button: title={:?}", btn.title());
    }
    let last_btn = app.get_by_role("AXButton", "").last();
    if let Some(btn) = last_btn.resolve() {
        println!("[last] Last button: title={:?}", btn.title());
    }
    println!();

    // 7. Action methods
    let first_btn = app.get_by_role("AXButton", "").first();
    println!("[bounding_box] First button bbox: {:?}", first_btn.bounding_box());
    println!("[title] First button title: {:?}", first_btn.title());
    println!();

    // 8. query() with AXQuery
    let web_areas = app.query(role("AXWebArea"));
    let wa_count = web_areas.count();
    println!("[query(AXQuery)] Found {wa_count} AXWebArea(s)");
    println!();

    // 9. get_by_dom_id — test uniqueness of #root
    let roots = app.get_by_dom_id("root");
    let root_count = roots.count();
    println!("[get_by_dom_id] Found {root_count} element(s) with id='root'");
    for (i, el) in roots.all().iter().enumerate() {
        println!(
            "  [{i}] role={:?}  title={:?}  parent_role={:?}",
            el.role(),
            el.title(),
            el.parent().and_then(|p| p.role()),
        );
    }
    println!();

    println!("=== Done ===");
}
