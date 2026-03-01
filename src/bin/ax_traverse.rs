//! ax_traverse — Traverse macOS Accessibility tree and search for elements.
//!
//! Usage:
//!   ax_traverse --app Lark --text "hello"
//!   ax_traverse --app Lark --regexp "Reply.*thread"
//!   ax_traverse --app Lark --role AXTextArea
//!   ax_traverse --app Lark --class "message-item"
//!   ax_traverse --app Lark --dump --depth 3
//!   ax_traverse --pid 12345 --text "hello"

use clap::Parser;
use picc::accessibility::{self, AXNode, role};
use regex::Regex;

use objc2::MainThreadMarker;

#[derive(Parser)]
#[command(name = "ax_traverse", about = "Traverse and search macOS Accessibility tree")]
struct Args {
    /// Application name (partial match, e.g. "Lark", "Chrome")
    #[arg(long)]
    app: Option<String>,

    /// Process ID
    #[arg(long)]
    pid: Option<i32>,

    /// Search by text content (substring match in value/title/description)
    #[arg(long)]
    text: Option<String>,

    /// Search by regex pattern (matches against value/title/description)
    #[arg(long)]
    regexp: Option<String>,

    /// Filter by AX role (e.g. AXTextArea, AXButton, AXStaticText)
    #[arg(long)]
    role: Option<String>,

    /// Filter by DOM class (for web-based apps like Electron)
    #[arg(long, alias = "dom-class")]
    class: Option<String>,

    /// Dump full tree (no search, just print)
    #[arg(long)]
    dump: bool,

    /// Max traversal depth [default: 30]
    #[arg(short, long, default_value_t = 30)]
    depth: usize,

    /// Max results to show [default: 20]
    #[arg(short, long, default_value_t = 20)]
    max_results: usize,

    /// Window title filter (substring match)
    #[arg(short, long)]
    window: Option<String>,

    /// Show all attributes on matched nodes
    #[arg(long)]
    attrs: bool,
}

fn find_app_by_name(mtm: MainThreadMarker, name: &str) -> Option<(i32, String)> {
    accessibility::find_app_by_name(mtm, name)
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}...")
    }
}

/// Format a single node's attributes as a compact string.
fn fmt_node(node: &AXNode, show_attrs: bool) -> String {
    let r = node.role().unwrap_or_default();
    let mut parts = vec![r.clone()];

    if let Some(t) = node.title() {
        if !t.is_empty() {
            parts.push(format!("title={:?}", trunc(&t, 80)));
        }
    }
    if let Some(v) = node.value() {
        let v = v.replace('\u{200b}', "");
        if !v.is_empty() {
            parts.push(format!("val={:?}", trunc(&v, 80)));
        }
    }
    if let Some(d) = node.description() {
        if !d.is_empty() {
            parts.push(format!("desc={:?}", trunc(&d, 80)));
        }
    }

    let cc = node.child_count();
    if cc > 0 {
        parts.push(format!("children={cc}"));
    }

    let cls = node.dom_classes();
    if !cls.is_empty() {
        parts.push(format!("cls={cls:?}"));
    }

    if let Some((x, y)) = node.position() {
        if let Some((w, h)) = node.size() {
            parts.push(format!("pos=({x:.0},{y:.0}) size=({w:.0}x{h:.0})"));
        }
    }

    if show_attrs {
        let attr_names = node.attr_names();
        parts.push(format!("attrs={attr_names:?}"));
    }

    parts.join(" | ")
}

/// Check if a node matches the search criteria.
fn node_matches(node: &AXNode, text: Option<&str>, re: Option<&Regex>, role_filter: Option<&str>, class_filter: Option<&str>) -> bool {
    if let Some(r) = role_filter {
        if node.role().as_deref() != Some(r) {
            return false;
        }
    }
    if let Some(c) = class_filter {
        if !node.has_dom_class(c) {
            return false;
        }
    }

    // If no text/regexp filter, role/class match is enough
    if text.is_none() && re.is_none() {
        return role_filter.is_some() || class_filter.is_some();
    }

    let haystack = {
        let mut h = String::new();
        if let Some(v) = node.value() { h.push_str(&v); h.push('\n'); }
        if let Some(t) = node.title() { h.push_str(&t); h.push('\n'); }
        if let Some(d) = node.description() { h.push_str(&d); h.push('\n'); }
        h
    };

    if let Some(t) = text {
        if !haystack.contains(t) {
            return false;
        }
    }
    if let Some(r) = re {
        if !r.is_match(&haystack) {
            return false;
        }
    }
    true
}

/// Build path string from root to this node.
fn fmt_path(path: &[(String, usize)]) -> String {
    path.iter()
        .map(|(role, idx)| format!("{role}[{idx}]"))
        .collect::<Vec<_>>()
        .join(" > ")
}

struct SearchState {
    text: Option<String>,
    re: Option<Regex>,
    role_filter: Option<String>,
    class_filter: Option<String>,
    show_attrs: bool,
    max_results: usize,
    found: usize,
}

/// Recursively search the AX tree.
fn search(node: &AXNode, path: &mut Vec<(String, usize)>, depth: usize, max_depth: usize, state: &mut SearchState) {
    if depth > max_depth || state.found >= state.max_results {
        return;
    }

    if node_matches(
        node,
        state.text.as_deref(),
        state.re.as_ref(),
        state.role_filter.as_deref(),
        state.class_filter.as_deref(),
    ) {
        state.found += 1;
        println!("\n--- Match #{} ---", state.found);
        println!("Path: {}", fmt_path(path));
        println!("Node: {}", fmt_node(node, state.show_attrs));

        // Print path nodes' roles for context
        if path.len() > 1 {
            print!("Ancestors: ");
            let ancestors: Vec<String> = path.iter().map(|(r, _)| r.clone()).collect();
            println!("{}", ancestors.join(" > "));
        }
    }

    let children = node.children();
    for (i, child) in children.iter().enumerate() {
        let r = child.role().unwrap_or_else(|| "?".to_string());
        path.push((r, i));
        search(child, path, depth + 1, max_depth, state);
        path.pop();
        if state.found >= state.max_results {
            return;
        }
    }
}

/// Dump tree (no search).
fn dump_tree(node: &AXNode, indent: usize, max_depth: usize, show_attrs: bool) {
    if max_depth == 0 { return; }
    let pad = "  ".repeat(indent);
    println!("{pad}{}", fmt_node(node, show_attrs));
    for child in node.children() {
        dump_tree(&child, indent + 1, max_depth - 1, show_attrs);
    }
}

fn main() {
    let args = Args::parse();

    if args.app.is_none() && args.pid.is_none() {
        eprintln!("Error: specify --app <name> or --pid <pid>");
        std::process::exit(1);
    }

    if !args.dump && args.text.is_none() && args.regexp.is_none() && args.role.is_none() && args.class.is_none() {
        eprintln!("Error: specify at least one of --text, --regexp, --role, --class, or --dump");
        std::process::exit(1);
    }

    if !accessibility::is_trusted() {
        eprintln!("Error: Accessibility not granted.");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("main thread");

    let (pid, app_name) = if let Some(ref name) = args.app {
        match find_app_by_name(mtm, name) {
            Some(v) => v,
            None => {
                eprintln!("Error: app {:?} not found", name);
                std::process::exit(1);
            }
        }
    } else {
        (args.pid.unwrap(), String::from("?"))
    };

    eprintln!("App: {} (pid={})", app_name, pid);

    let app = AXNode::app(pid);

    // Optionally filter to a specific window
    let roots: Vec<AXNode> = if let Some(ref wf) = args.window {
        let wins = app.find_all(role("AXWindow"));
        let matched: Vec<AXNode> = wins.into_iter().filter(|w| {
            w.title().map(|t| t.contains(wf.as_str())).unwrap_or(false)
        }).collect();
        if matched.is_empty() {
            eprintln!("Error: no window matching {:?}", wf);
            std::process::exit(1);
        }
        eprintln!("Window: {:?}", matched[0].title());
        matched
    } else {
        vec![app]
    };

    let re = args.regexp.as_ref().map(|p| {
        Regex::new(p).unwrap_or_else(|e| {
            eprintln!("Error: invalid regex: {e}");
            std::process::exit(1);
        })
    });

    if args.dump {
        for root in &roots {
            dump_tree(root, 0, args.depth, args.attrs);
        }
        return;
    }

    let mut state = SearchState {
        text: args.text.clone(),
        re,
        role_filter: args.role.clone(),
        class_filter: args.class.clone(),
        show_attrs: args.attrs,
        max_results: args.max_results,
        found: 0,
    };

    for root in &roots {
        let r = root.role().unwrap_or_else(|| "?".to_string());
        let mut path = vec![(r, 0usize)];
        search(root, &mut path, 0, args.depth, &mut state);
    }

    eprintln!("\nFound {} match(es)", state.found);
}
