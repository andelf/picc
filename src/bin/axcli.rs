//! axcli — macOS Accessibility CLI tool.
//!
//! Usage:
//!   axcli --app Lark snapshot
//!   axcli --app Lark snapshot --depth 5
//!   axcli --app Lark click '.SearchButton'
//!   axcli --app Lark dblclick '.SearchButton'
//!   axcli --app Lark input '.SearchInput' 'hello'
//!   axcli --app Lark fill '.SearchInput' 'hello'
//!   axcli --app Lark press Enter
//!   axcli --app Lark press 'Control+a'
//!   axcli --app Lark hover '.SearchButton'
//!   axcli --app Lark focus '.SearchInput'
//!   axcli --app Lark scroll-to '.item'
//!   axcli --app Lark scroll '.chat-list' down 300
//!   axcli --app Lark screenshot -o /tmp/shot.png
//!   axcli --app Lark screenshot '.SearchButton' -o /tmp/btn.png
//!   axcli --app Lark wait '.loading'
//!   axcli --app Lark wait 500
//!   axcli --app Lark get AXValue '.SearchInput'

use clap::{Parser, Subcommand};
use objc2_core_foundation::{CGPoint, CGSize, CGRect};
use picc::accessibility::{self, AXNode};
use picc::{input, screenshot, tree_fmt};

#[derive(Parser)]
#[command(name = "axcli", about = "macOS Accessibility CLI tool", after_help = "\
Locator syntax:
  #id                       DOM ID           e.g. #root, #modal
  .class                    DOM class        e.g. .SearchButton, .msg-item
  .class1.class2            Multiple classes  e.g. .message-item.message-self
  Role                      AX role          e.g. AXButton, button, textarea
  Role.class                Role + class     e.g. AXGroup.feed-card
  Role[attr=\"val\"]          Attribute match  e.g. AXButton[title=\"Send\"]
  text=VALUE                Exact text       e.g. text=\"Hello\"
  text~=VALUE               Contains text    e.g. text~=\"partial\"
  L >> R                    Chain (scope)    e.g. .sidebar >> AXButton
  L >> nth=N                Pick Nth match   e.g. .item >> nth=0, nth=-1
  L >> first / last         Pick first/last  e.g. .item >> last
")]
struct Cli {
    /// Application name
    #[arg(long, global = true)]
    app: Option<String>,

    /// Process ID
    #[arg(long, global = true)]
    pid: Option<i32>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print accessibility tree (shows first match by default, use --all for all)
    Snapshot {
        /// Locator selector to focus on
        locator: Option<String>,
        /// Max tree depth
        #[arg(long, default_value = "10")]
        depth: usize,
        /// Show all matches instead of just the first
        #[arg(long)]
        all: bool,
    },
    /// Click element
    Click {
        locator: String,
    },
    /// Double-click element
    Dblclick {
        locator: String,
    },
    /// Focus element and type text
    Input {
        locator: String,
        text: String,
    },
    /// Clear field and type text (Cmd+A, Delete, type)
    Fill {
        locator: String,
        text: String,
    },
    /// Press key combo (Enter, Control+a, Command+Shift+v)
    Press {
        key: String,
    },
    /// Move mouse to element center
    Hover {
        locator: String,
    },
    /// Focus element (AXFocused + click fallback)
    Focus {
        locator: String,
    },
    /// Scroll element into view (AXScrollToVisible)
    ScrollTo {
        locator: String,
    },
    /// Scroll within an element (up/down/left/right)
    Scroll {
        /// Locator of the scrollable element
        locator: String,
        /// Direction: up, down, left, right
        direction: String,
        /// Pixels to scroll (default 300)
        #[arg(default_value = "300")]
        pixels: i32,
    },
    /// Capture screenshot
    Screenshot {
        /// Locator selector (optional, for element screenshot)
        locator: Option<String>,
        /// Output file path
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Wait for element or milliseconds
    Wait {
        /// Milliseconds (number) or locator string
        target: String,
    },
    /// Get element attribute value
    Get {
        /// Attribute name (e.g. AXValue, AXTitle, text, role)
        attr: String,
        locator: String,
    },
}

fn main() {
    let cli = Cli::parse();

    let (pid, app) = resolve_app(&cli);

    if !accessibility::is_trusted() {
        eprintln!("error: accessibility not granted");
        std::process::exit(1);
    }

    match cli.command {
        Command::Snapshot { locator, depth, all } => cmd_snapshot(&app, locator.as_deref(), depth, all),
        Command::Click { locator } => cmd_click(&app, pid, &locator),
        Command::Dblclick { locator } => cmd_dblclick(&app, pid, &locator),
        Command::Input { locator, text } => cmd_input(&app, pid, &locator, &text),
        Command::Fill { locator, text } => cmd_fill(&app, pid, &locator, &text),
        Command::Press { key } => cmd_press(pid, &key),
        Command::Hover { locator } => cmd_hover(&app, pid, &locator),
        Command::Focus { locator } => cmd_focus(&app, pid, &locator),
        Command::ScrollTo { locator } => cmd_scroll_to(&app, pid, &locator),
        Command::Scroll { locator, direction, pixels } => cmd_scroll(&app, pid, &locator, &direction, pixels),
        Command::Screenshot { locator, output } => cmd_screenshot(&app, locator.as_deref(), output.as_deref()),
        Command::Wait { target } => cmd_wait(&app, &target),
        Command::Get { attr, locator } => cmd_get(&app, &attr, &locator),
    }
}

// --- App resolution ---

fn resolve_app(cli: &Cli) -> (i32, AXNode) {
    if let Some(pid) = cli.pid {
        return (pid, AXNode::app(pid));
    }
    if let Some(ref name) = cli.app {
        let mtm = objc2::MainThreadMarker::new().expect("must run on main thread");
        let (pid, localized) =
            accessibility::find_app_by_name(mtm, name).expect("app not found");
        eprintln!("Found app: {localized} (pid={pid})");
        return (pid, AXNode::app(pid));
    }
    eprintln!("error: --app or --pid is required");
    std::process::exit(1);
}

/// Validate a single locator segment (between `>>`).
/// Valid forms:
///   #id                        — DOM ID
///   .class1.class2             — DOM class(es)
///   Role.class                 — role + class
///   Role[attr="value"]         — role + attribute
///   text=VALUE / text~=VALUE   — text match
///   role (plain word)          — AXRole or short role
///   nth=N / first / last       — pipeline index selectors
fn validate_segment(seg: &str) -> Result<(), String> {
    let s = seg.trim();
    if s.is_empty() {
        return Err("empty segment (double `>>` or trailing `>>`)".into());
    }
    // Pipeline selectors
    if s == "first" || s == "last" || s.starts_with("nth=") {
        return Ok(());
    }
    // DOM ID
    if s.starts_with('#') {
        return if s.len() > 1 { Ok(()) } else { Err("empty DOM ID after `#`".into()) };
    }
    // text= / text~=
    if s.starts_with("text=") || s.starts_with("text~=") {
        return Ok(());
    }
    // Bracket selector: Role[attr="value"]
    if s.contains('[') {
        if !s.ends_with(']') {
            return Err(format!("unclosed bracket in `{s}`"));
        }
        let inner = &s[s.find('[').unwrap() + 1..s.len() - 1];
        if !inner.contains('=') {
            return Err(format!("bracket selector missing `=` in `{s}`"));
        }
        return Ok(());
    }
    // Dot selector: .class or Role.class
    if s.contains('.') {
        // At least one class name after a dot
        let has_class = s.split('.').skip(1).any(|c| !c.is_empty());
        return if has_class { Ok(()) } else { Err(format!("empty class name in `{s}`")) };
    }
    // Plain role name: must be alphanumeric (allow AXFoo or foo)
    if s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Ok(());
    }
    Err(format!("unrecognized locator syntax: `{s}`"))
}

/// Validate an entire locator string (may contain `>>` chains).
fn validate_locator(locator: &str) {
    for seg in locator.split(" >> ") {
        if let Err(msg) = validate_segment(seg) {
            eprintln!("error: invalid locator: {msg}");
            eprintln!("  locator: {locator}");
            std::process::exit(1);
        }
    }
}

/// Resolve a locator to a single node, exit on error.
fn resolve_one(app: &AXNode, locator: &str) -> AXNode {
    validate_locator(locator);
    let nodes = app.locate_all(locator);
    if nodes.is_empty() {
        eprintln!("error: locator not found: {locator}");
        std::process::exit(1);
    }
    if nodes.len() > 1 {
        eprintln!(
            "error: locator matched {} elements, must be unique for actions",
            nodes.len()
        );
        eprintln!("hint: use 'locator >> nth=N' to select one");
        std::process::exit(1);
    }
    let node = &nodes[0];
    eprintln!(
        "Resolved → role={:?} title={:?}",
        node.role(),
        node.title(),
    );
    nodes.into_iter().next().unwrap()
}

/// Get element center coordinates. Exit if zero-size (unless is_menu).
fn element_center(node: &AXNode, allow_zero: bool) -> (f64, f64) {
    let (w, h) = node.size().unwrap_or((0.0, 0.0));
    if !allow_zero && w == 0.0 && h == 0.0 {
        eprintln!("error: element has zero size (not visible)");
        std::process::exit(1);
    }
    let (x, y) = node.position().unwrap_or((0.0, 0.0));
    (x + w / 2.0, y + h / 2.0)
}

fn is_menu_role(role: &str) -> bool {
    role == "AXMenuItem" || role == "AXMenuBarItem"
}

// --- Commands ---

fn cmd_snapshot(app: &AXNode, locator: Option<&str>, depth: usize, all: bool) {
    let mut interactive = 0usize;

    if let Some(loc) = locator {
        validate_locator(loc);
        let nodes = app.locate_all(loc);
        if nodes.is_empty() {
            eprintln!("error: locator not found: {loc}");
            std::process::exit(1);
        }
        if all {
            eprintln!("Found {} matches for {loc}", nodes.len());
            for (i, node) in nodes.iter().enumerate() {
                if i > 0 { println!(); }
                eprintln!("--- match {}/{} ---", i + 1, nodes.len());
                tree_fmt::print_with_ancestors(node, depth, &mut interactive);
            }
        } else {
            let node = &nodes[0];
            if nodes.len() > 1 {
                eprintln!(
                    "Matched {} elements, showing first. Use --all to see all.",
                    nodes.len()
                );
            }
            eprintln!(
                "Resolved → role={:?} title={:?} children={}",
                node.role(), node.title(), node.child_count()
            );
            tree_fmt::print_with_ancestors(node, depth, &mut interactive);
        }
    } else {
        tree_fmt::print_tree(app, 0, depth, &mut interactive);
    }

    eprintln!("\n({interactive} interactive elements)");
}

fn cmd_click(app: &AXNode, pid: i32, locator: &str) {
    let node = resolve_one(app, locator);
    let role = node.role().unwrap_or_default();

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    if is_menu_role(&role) {
        eprintln!("Performing AXPress on {role}");
        if !accessibility::perform_action(&node.0, "AXPress") {
            eprintln!("error: AXPress failed");
            std::process::exit(1);
        }
    } else {
        let (cx, cy) = element_center(&node, false);
        eprintln!("Clicking at ({cx:.0}, {cy:.0})");
        input::mouse_move(cx, cy);
        std::thread::sleep(std::time::Duration::from_millis(50));
        input::mouse_click(cx, cy);
    }
}

fn cmd_dblclick(app: &AXNode, pid: i32, locator: &str) {
    let node = resolve_one(app, locator);

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    let (cx, cy) = element_center(&node, false);
    eprintln!("Double-clicking at ({cx:.0}, {cy:.0})");
    input::mouse_move(cx, cy);
    std::thread::sleep(std::time::Duration::from_millis(50));
    input::mouse_dblclick(cx, cy);
}

fn cmd_input(app: &AXNode, pid: i32, locator: &str, text: &str) {
    let node = resolve_one(app, locator);

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Focus
    if !node.set_focused(true) {
        let (cx, cy) = element_center(&node, false);
        eprintln!("AXFocused failed, clicking to focus...");
        input::mouse_move(cx, cy);
        std::thread::sleep(std::time::Duration::from_millis(50));
        input::mouse_click(cx, cy);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    eprintln!("Typing: {text:?}");
    input::type_text(text);
}

fn cmd_fill(app: &AXNode, pid: i32, locator: &str, text: &str) {
    let node = resolve_one(app, locator);

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Focus
    if !node.set_focused(true) {
        let (cx, cy) = element_center(&node, false);
        eprintln!("AXFocused failed, clicking to focus...");
        input::mouse_move(cx, cy);
        std::thread::sleep(std::time::Duration::from_millis(50));
        input::mouse_click(cx, cy);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Select all + delete
    let (kc_a, fl_a) = input::parse_key_combo("Command+a");
    input::press_key_combo(kc_a, fl_a);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let (kc_del, fl_del) = input::parse_key_combo("Delete");
    input::press_key_combo(kc_del, fl_del);
    std::thread::sleep(std::time::Duration::from_millis(100));

    eprintln!("Filling: {text:?}");
    input::type_text(text);
}

fn cmd_press(pid: i32, key: &str) {
    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    let (keycode, flags) = input::parse_key_combo(key);
    eprintln!("Pressing: {key} (keycode={keycode}, flags=0x{flags:x})");
    input::press_key_combo(keycode, flags);
}

fn cmd_hover(app: &AXNode, pid: i32, locator: &str) {
    let node = resolve_one(app, locator);

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    let (cx, cy) = element_center(&node, false);
    eprintln!("Moving mouse to ({cx:.0}, {cy:.0})");
    input::mouse_move(cx, cy);
}

fn cmd_focus(app: &AXNode, pid: i32, locator: &str) {
    let node = resolve_one(app, locator);

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    if node.set_focused(true) {
        eprintln!("Focused via AXFocused");
    } else {
        let (cx, cy) = element_center(&node, false);
        eprintln!("AXFocused failed, clicking to focus...");
        input::mouse_move(cx, cy);
        std::thread::sleep(std::time::Duration::from_millis(50));
        input::mouse_click(cx, cy);
    }
}

fn cmd_scroll_to(app: &AXNode, pid: i32, locator: &str) {
    let node = resolve_one(app, locator);

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    eprintln!("Scrolling element into view...");
    if !accessibility::perform_action(&node.0, "AXScrollToVisible") {
        eprintln!("warning: AXScrollToVisible failed (element may not be in a scroll area)");
    }
}

fn cmd_scroll(app: &AXNode, pid: i32, locator: &str, direction: &str, pixels: i32) {
    let node = resolve_one(app, locator);

    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(200));

    let (cx, cy) = element_center(&node, false);

    let (dx, dy) = match direction {
        "up" => (0, pixels),
        "down" => (0, -pixels),
        "left" => (pixels, 0),
        "right" => (-pixels, 0),
        _ => {
            eprintln!("error: invalid direction '{direction}', use up/down/left/right");
            std::process::exit(1);
        }
    };
    eprintln!("Scrolling {direction} {pixels}px at ({cx:.0}, {cy:.0})");
    input::mouse_move(cx, cy);
    std::thread::sleep(std::time::Duration::from_millis(50));
    input::scroll_wheel(cx, cy, dx, dy);
}

fn cmd_screenshot(app: &AXNode, locator: Option<&str>, output: Option<&str>) {
    let path = output
        .map(String::from)
        .unwrap_or_else(|| {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            format!("/tmp/ax_screenshot_{ts}.png")
        });

    let rect = if let Some(loc) = locator {
        let node = resolve_one(app, loc);
        let (x, y) = node.position().unwrap_or((0.0, 0.0));
        let (w, h) = node.size().unwrap_or((0.0, 0.0));
        eprintln!("Capturing {w:.0}x{h:.0} at ({x:.0},{y:.0}) → {path}");
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    } else {
        eprintln!("Capturing full screen → {path}");
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0))
    };

    let image = screenshot::capture(rect).expect("screenshot failed");
    if screenshot::save_png(&image, &path) {
        eprintln!("Saved: {path}");
    } else {
        eprintln!("error: failed to save {path}");
        std::process::exit(1);
    }
}

fn cmd_wait(app: &AXNode, target: &str) {
    // Pure number = sleep ms
    if let Ok(ms) = target.parse::<u64>() {
        eprintln!("Waiting {ms}ms...");
        std::thread::sleep(std::time::Duration::from_millis(ms));
        return;
    }

    // Otherwise: poll for locator (timeout 10s)
    eprintln!("Waiting for '{target}'...");
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(10);
    loop {
        let nodes = app.locate_all(target);
        if !nodes.is_empty() {
            eprintln!("Found after {:.1}s", start.elapsed().as_secs_f64());
            return;
        }
        if start.elapsed() > timeout {
            eprintln!("error: timeout waiting for '{target}'");
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn cmd_get(app: &AXNode, attr: &str, locator: &str) {
    let node = resolve_one(app, locator);

    let result = match attr.to_lowercase().as_str() {
        "text" => {
            // Get subtree text
            Some(node.text(50))
        }
        "role" => node.role(),
        "title" | "axtitle" => node.title(),
        "description" | "axdescription" => node.description(),
        "value" | "axvalue" => node.value(),
        "domid" | "axdomidentifier" => {
            accessibility::attr_string(&node.0, "AXDOMIdentifier")
        }
        _ => {
            // Try as raw AX attribute
            let ax_attr = if attr.starts_with("AX") {
                attr.to_string()
            } else {
                format!("AX{}", capitalize(attr))
            };
            accessibility::attr_string(&node.0, &ax_attr)
        }
    };

    match result {
        Some(val) => println!("{val}"),
        None => {
            eprintln!("(no value)");
            std::process::exit(1);
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}
