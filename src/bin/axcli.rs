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
#[command(name = "axcli", version, about = "macOS Accessibility CLI tool", after_help = "\
Locator syntax:
  #id                       DOM ID           e.g. #root, #modal
  .class                    DOM class        e.g. .SearchButton, .msg-item
  .class1.class2            Multiple classes  e.g. .message-item.message-self
  Role                      AX role          e.g. AXButton, button, textarea
  Role.class                Role + class     e.g. AXGroup.feed-card
  Role[attr=\"val\"]          Attribute match  e.g. AXButton[title=\"Send\"]
  text=VALUE                Exact text       e.g. text=\"Hello\"
  text~=VALUE               Contains text    e.g. text~=\"partial\"
  text=/regex/flags         Regex text       e.g. text=/\\d+条新消息/, text=/Log\\s*in/i
  L >> R                    Chain (scope)    e.g. .sidebar >> AXButton
  L > R                     Direct child     e.g. AXWindow > AXGroup
  L >> nth=N                Pick Nth match   e.g. .item >> nth=0, nth=-1
  L >> first / last         Pick first/last  e.g. .item >> last

Pseudo-classes:
  :has-text(\"text\")         Subtree text     e.g. .card:has-text(\"会议\")
  :has(selector)            Has descendant   e.g. .item:has(.reaction)
  :visible                  Non-zero size    e.g. AXButton:visible
  :nth-child(N)             Nth child (0-based) e.g. AXGroup:nth-child(0)
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

/// Known attribute names for `get` command. Also accepts raw AX* attribute names.
#[derive(Clone, Debug)]
enum GetAttr {
    /// Subtree text (with newlines at block boundaries)
    Text,
    /// AXRole
    Role,
    /// AXTitle
    Title,
    /// AXDescription
    Description,
    /// AXValue
    Value,
    /// AXDOMIdentifier
    DomId,
    /// AXDOMClassList
    Classes,
    /// Available actions
    Actions,
    /// Screen position (x, y)
    Position,
    /// Element size (w, h)
    Size,
    /// Number of children
    ChildCount,
    /// Raw AX attribute (e.g. AXHelp, AXURL)
    Raw(String),
}

impl std::fmt::Display for GetAttr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::Role => write!(f, "role"),
            Self::Title => write!(f, "title"),
            Self::Description => write!(f, "description"),
            Self::Value => write!(f, "value"),
            Self::DomId => write!(f, "domid"),
            Self::Classes => write!(f, "classes"),
            Self::Actions => write!(f, "actions"),
            Self::Position => write!(f, "position"),
            Self::Size => write!(f, "size"),
            Self::ChildCount => write!(f, "child-count"),
            Self::Raw(s) => write!(f, "{s}"),
        }
    }
}

impl std::str::FromStr for GetAttr {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "text" => Self::Text,
            "role" => Self::Role,
            "title" | "axtitle" => Self::Title,
            "description" | "desc" | "axdescription" => Self::Description,
            "value" | "axvalue" => Self::Value,
            "domid" | "dom-id" | "axdomidentifier" => Self::DomId,
            "classes" | "class" | "axdomclasslist" => Self::Classes,
            "actions" => Self::Actions,
            "position" | "pos" => Self::Position,
            "size" => Self::Size,
            "children" | "child-count" | "childcount" => Self::ChildCount,
            _ => Self::Raw(s.to_string()),
        })
    }
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
        /// Max text/title display length (0 = no truncation)
        #[arg(long, default_value = "80")]
        max_text_len: usize,
        /// Simplify output: hide DOM IDs/classes, prune empty subtrees
        #[arg(long)]
        simplify: bool,
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
    #[command(after_help = "\
Known attributes:
  text         Subtree text (with newlines at block boundaries)
  role         AXRole (e.g. AXButton, AXStaticText)
  title        AXTitle
  desc         AXDescription (alias: description)
  value        AXValue
  domid        AXDOMIdentifier (alias: dom-id)
  classes      AXDOMClassList (alias: class)
  actions      Available AX actions
  position     Screen position as x,y (alias: pos)
  size         Element size as w,h
  child-count  Number of children (alias: children)
  AX*          Any raw AX attribute (e.g. AXHelp, AXURL)
")]
    Get {
        /// Attribute to read
        #[arg(value_name = "ATTR")]
        attr: GetAttr,
        locator: String,
    },
    /// List running applications visible to accessibility
    ListApps,
}

fn main() {
    let cli = Cli::parse();

    // list-apps doesn't need --app/--pid
    if matches!(cli.command, Command::ListApps) {
        cmd_list_apps();
        return;
    }

    let (pid, app) = resolve_app(&cli);

    if !accessibility::is_trusted() {
        eprintln!("error: accessibility not granted");
        std::process::exit(1);
    }

    match cli.command {
        Command::ListApps => unreachable!(),
        Command::Snapshot { locator, depth, all, max_text_len, simplify } => cmd_snapshot(&app, locator.as_deref(), depth, all, max_text_len, simplify),
        Command::Click { locator } => cmd_click(&app, pid, &locator),
        Command::Dblclick { locator } => cmd_dblclick(&app, pid, &locator),
        Command::Input { locator, text } => cmd_input(&app, pid, &locator, &text),
        Command::Fill { locator, text } => cmd_fill(&app, pid, &locator, &text),
        Command::Press { key } => cmd_press(pid, &key),
        Command::Hover { locator } => cmd_hover(&app, pid, &locator),
        Command::Focus { locator } => cmd_focus(&app, pid, &locator),
        Command::ScrollTo { locator } => cmd_scroll_to(&app, pid, &locator),
        Command::Scroll { locator, direction, pixels } => cmd_scroll(&app, pid, &locator, &direction, pixels),
        Command::Screenshot { locator, output } => cmd_screenshot(&app, pid, locator.as_deref(), output.as_deref()),
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

/// Strip known pseudo-class suffixes from a segment for validation.
/// Returns the base segment after removing `:has-text(...)`, `:has(...)`, `:visible`.
fn strip_pseudo_classes(s: &str) -> &str {
    let mut base = s;
    loop {
        if let Some(stripped) = base.strip_suffix(":visible") {
            base = stripped;
            continue;
        }
        if let Some(pos) = base.rfind(":nth-child(") {
            if base.ends_with(')') {
                base = &base[..pos];
                continue;
            }
        }
        if let Some(pos) = base.rfind(":has-text(") {
            if base.ends_with(')') {
                base = &base[..pos];
                continue;
            }
        }
        if let Some(pos) = base.rfind(":has(") {
            if base.ends_with(')') {
                base = &base[..pos];
                continue;
            }
        }
        break;
    }
    base
}

/// Validate a single locator segment (between `>>` or `>`).
/// Valid forms:
///   #id                        — DOM ID
///   .class1.class2             — DOM class(es)
///   Role.class                 — role + class
///   Role[attr="value"]         — role + attribute
///   text=VALUE / text~=VALUE   — text match
///   text=/regex/flags          — regex text match
///   role (plain word)          — AXRole or short role
///   nth=N / first / last       — pipeline index selectors
///   :has-text("text")          — pseudo-class (can be appended to any selector)
///   :has(selector)             — pseudo-class
///   :visible                   — pseudo-class
fn validate_segment(seg: &str) -> Result<(), String> {
    let s = seg.trim();
    if s.is_empty() {
        return Err("empty segment (double `>>` or trailing `>>`)".into());
    }
    // Strip pseudo-classes for base validation
    let base = strip_pseudo_classes(s);
    // If entire selector is pseudo-classes only (e.g. `:has-text("Hello")`)
    if base.is_empty() {
        return Ok(());
    }
    let s = base;
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
    // Dot selector: .class or Role.class (may include :not())
    if s.contains('.') {
        let without_not = s.split(":not(").next().unwrap_or(s);
        let has_class = without_not.split('.').skip(1).any(|c| !c.is_empty());
        return if has_class { Ok(()) } else { Err(format!("empty class name in `{s}`")) };
    }
    // Plain role name: must be alphanumeric (allow AXFoo or foo)
    if s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Ok(());
    }
    Err(format!("unrecognized locator syntax: `{s}`"))
}

/// Validate an entire locator string (may contain `>>` and `>` chains).
fn validate_locator(locator: &str) {
    // Split by >> first, then by > within each part
    for desc_part in locator.split(" >> ") {
        for seg in desc_part.split(" > ") {
            if let Err(msg) = validate_segment(seg) {
                eprintln!("error: invalid locator: {msg}");
                eprintln!("  locator: {locator}");
                std::process::exit(1);
            }
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
        "Resolved → role=\"{}\" title=\"{}\"",
        node.role().unwrap_or_default(),
        node.title().unwrap_or_default(),
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

fn cmd_list_apps() {
    use objc2_app_kit::NSRunningApplication;

    let mtm = objc2::MainThreadMarker::new().expect("must run on main thread");
    let _ = mtm;
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
    let workspace: objc2::rc::Retained<objc2::runtime::NSObject> =
        unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let apps: objc2::rc::Retained<objc2_foundation::NSArray<NSRunningApplication>> =
        unsafe { objc2::msg_send![&workspace, runningApplications] };

    let mut entries: Vec<(i32, String, String)> = Vec::new();
    for app in apps.iter() {
        let pid = app.processIdentifier();
        let bundle = app
            .bundleIdentifier()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let name = app
            .localizedName()
            .map(|n| n.to_string())
            .unwrap_or_default();
        if !bundle.is_empty() && !name.is_empty() {
            entries.push((pid, name, bundle));
        }
    }
    entries.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));

    for (pid, name, bundle) in &entries {
        println!("{pid:>6}  {name:<30} {bundle}");
    }
    eprintln!("\n({} apps)", entries.len());
}

fn cmd_snapshot(app: &AXNode, locator: Option<&str>, depth: usize, all: bool, max_text_len: usize, simplify: bool) {
    let mut printer = tree_fmt::TreePrinter::new();
    printer.max_text_len = max_text_len;
    printer.simplify = simplify;

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
                printer.print_with_ancestors(node, depth);
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
                "Resolved → role=\"{}\" title=\"{}\" children={}",
                node.role().unwrap_or_default(),
                node.title().unwrap_or_default(),
                node.child_count()
            );
            printer.print_with_ancestors(node, depth);
        }
    } else {
        printer.print_tree(app, 0, depth);
    }

    eprintln!("\n({} interactive elements)", printer.interactive_count());
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

fn cmd_screenshot(app: &AXNode, pid: i32, locator: Option<&str>, output: Option<&str>) {
    // Bring app to foreground so it's visible for screen capture
    input::activate_app(pid);
    std::thread::sleep(std::time::Duration::from_millis(300));
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
        // No locator: capture the app's main window area
        let windows = app.children();
        let win = windows.iter().find(|w| {
            w.role().as_deref() == Some("AXWindow")
                && w.size().map_or(false, |(w, h)| w > 0.0 && h > 0.0)
        });
        if let Some(win) = win {
            let (x, y) = win.position().unwrap_or((0.0, 0.0));
            let (w, h) = win.size().unwrap_or((0.0, 0.0));
            eprintln!("Capturing window {w:.0}x{h:.0} at ({x:.0},{y:.0}) → {path}");
            CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
        } else {
            eprintln!("warning: no visible window found, capturing full screen → {path}");
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0))
        }
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

/// Collect text from a subtree, inserting newlines at group boundaries.
fn collect_text(node: &AXNode, max_depth: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    collect_text_inner(node, max_depth, &mut parts);
    parts.join("")
}

fn collect_text_inner(node: &AXNode, max_depth: usize, parts: &mut Vec<String>) {
    if max_depth == 0 {
        return;
    }
    let role = node.role().unwrap_or_default();

    // Text leaf: emit value
    if role == "AXStaticText" || role == "AXTextArea" || role == "AXTextField" {
        if let Some(val) = node.value() {
            if !val.is_empty() {
                parts.push(val);
            }
        }
        return;
    }

    // Block-level elements: insert newline before if we already have content
    let is_block = role == "AXGroup" || role == "AXList" || role == "AXTable"
        || role == "AXRow" || role == "AXHeading" || role == "AXParagraph"
        || role == "AXBlockquote" || role == "AXArticle";

    if is_block && !parts.is_empty() {
        // Only add newline if last part doesn't already end with one
        if !parts.last().map_or(true, |s| s.ends_with('\n')) {
            parts.push("\n".to_string());
        }
    }

    for child in node.children() {
        collect_text_inner(&child, max_depth - 1, parts);
    }

    if is_block && !parts.is_empty() {
        if !parts.last().map_or(true, |s| s.ends_with('\n')) {
            parts.push("\n".to_string());
        }
    }
}

fn cmd_get(app: &AXNode, attr: &GetAttr, locator: &str) {
    let node = resolve_one(app, locator);

    match attr {
        GetAttr::Text => {
            println!("{}", collect_text(&node, 50));
        }
        GetAttr::Role => {
            println!("{}", node.role().unwrap_or_default());
        }
        GetAttr::Title => {
            println!("{}", node.title().unwrap_or_default());
        }
        GetAttr::Description => {
            println!("{}", node.description().unwrap_or_default());
        }
        GetAttr::Value => {
            println!("{}", node.value().unwrap_or_default());
        }
        GetAttr::DomId => {
            let id = accessibility::attr_string(&node.0, "AXDOMIdentifier").unwrap_or_default();
            println!("{id}");
        }
        GetAttr::Classes => {
            let classes = node.dom_classes();
            println!("{}", classes.join(" "));
        }
        GetAttr::Actions => {
            let actions = node.actions();
            println!("{}", actions.join(" "));
        }
        GetAttr::Position => {
            match node.position() {
                Some((x, y)) => println!("{x:.0},{y:.0}"),
                None => { eprintln!("(no position)"); std::process::exit(1); }
            }
        }
        GetAttr::Size => {
            match node.size() {
                Some((w, h)) => println!("{w:.0},{h:.0}"),
                None => { eprintln!("(no size)"); std::process::exit(1); }
            }
        }
        GetAttr::ChildCount => {
            println!("{}", node.child_count());
        }
        GetAttr::Raw(name) => {
            let ax_name = if name.starts_with("AX") {
                name.clone()
            } else {
                let mut c = name.chars();
                let cap = match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().to_string() + c.as_str(),
                };
                format!("AX{cap}")
            };
            match accessibility::attr_string(&node.0, &ax_name) {
                Some(val) => println!("{val}"),
                None => { eprintln!("(no value for {ax_name})"); std::process::exit(1); }
            }
        }
    }
}
