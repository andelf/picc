//! Explore and operate Google Chrome via Accessibility API.
//!
//! Commands are executed sequentially, pipeline-style:
//!
//!   chrome_ax [--pid N | --dacs] CMD CMD CMD ...
//!
//! Commands:
//!   --list              List all Chrome/Chromium instances
//!   --activate          Bring window to front
//!   --sleep S           Sleep for S seconds (float)
//!   --tabs              List all tabs
//!   --tab N             Switch to tab N (by index)
//!   --click <text>      Click a button matching text (title/desc substring)
//!   --attrs             Dump attributes on window + first WebArea
//!   --dump N            Dump AX tree at depth N
//!   --text              Print page text content

use objc2::MainThreadMarker;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
};

use picc::accessibility::{self, AXNode, AXQuery, role};

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

fn find_all_chromes(
    _mtm: MainThreadMarker,
) -> Vec<(i32, String, String, objc2::rc::Retained<NSRunningApplication>)> {
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
    let workspace: objc2::rc::Retained<objc2::runtime::NSObject> =
        unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let apps: objc2::rc::Retained<objc2_foundation::NSArray<NSRunningApplication>> =
        unsafe { objc2::msg_send![&workspace, runningApplications] };

    let mut result = Vec::new();
    for app in apps.iter() {
        if let Some(bundle_id) = app.bundleIdentifier() {
            let bundle_str = bundle_id.to_string();
            if bundle_str.contains("com.google.Chrome") || bundle_str.contains("chromium") {
                let pid = app.processIdentifier();
                let name = app
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                result.push((pid, name, bundle_str, app.clone()));
            }
        }
    }
    result
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}...")
    }
}

fn dump(node: &AXNode, indent: usize, max_depth: usize) {
    if max_depth == 0 {
        let cc = node.child_count();
        if cc > 0 {
            let pad = " ".repeat(indent);
            eprintln!("{pad}... ({cc} children, depth limit)");
        }
        return;
    }
    let r = node.role().unwrap_or_default();
    let title = node.title().unwrap_or_default();
    let value = node.value().unwrap_or_default();
    let desc = node.description().unwrap_or_default();
    let cc = node.child_count();
    let pad = " ".repeat(indent);

    let mut info = format!("{pad}{r}");
    if !title.is_empty() {
        info.push_str(&format!(" title={:?}", trunc(&title, 60)));
    }
    if !value.is_empty() {
        info.push_str(&format!(" val={:?}", trunc(&value, 80)));
    }
    if !desc.is_empty() {
        info.push_str(&format!(" desc={:?}", trunc(&desc, 40)));
    }
    if cc > 0 {
        info.push_str(&format!(" (c={cc})"));
    }
    let classes = node.dom_classes();
    if !classes.is_empty() {
        info.push_str(&format!(" cls={classes:?}"));
    }
    eprintln!("{info}");
    for child in node.children() {
        dump(&child, indent + 2, max_depth - 1);
    }
}

fn dump_attrs(node: &AXNode) {
    let attrs = node.attr_names();
    eprintln!("Attributes ({}):", attrs.len());
    for attr in &attrs {
        let val = accessibility::attr_string(&node.0, attr).unwrap_or_default();
        if val.is_empty() {
            eprintln!("  {attr}");
        } else {
            eprintln!("  {attr} = {:?}", trunc(&val, 80));
        }
    }
}

// ---------------------------------------------------------------------------
// Command parsing & execution
// ---------------------------------------------------------------------------

enum Cmd {
    Activate,
    Sleep(f64),
    Tabs,
    Tab(usize),
    Click(String),
    Attrs,
    Dump(usize),
    Text,
}

/// Parse args into (target selector, commands).
/// Target selector: --pid N or --dacs (default: first chrome).
fn parse_args(args: &[String]) -> (Option<i32>, bool, Vec<Cmd>) {
    let mut pid: Option<i32> = None;
    let mut dacs = false;
    let mut cmds = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pid" => {
                pid = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--dacs" => {
                dacs = true;
                i += 1;
            }
            "--activate" => {
                cmds.push(Cmd::Activate);
                i += 1;
            }
            "--sleep" => {
                let secs: f64 = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(1.0);
                cmds.push(Cmd::Sleep(secs));
                i += 2;
            }
            "--tabs" => {
                cmds.push(Cmd::Tabs);
                i += 1;
            }
            "--tab" => {
                let idx: usize = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
                cmds.push(Cmd::Tab(idx));
                i += 2;
            }
            "--click" => {
                let target = args.get(i + 1).cloned().unwrap_or_default();
                cmds.push(Cmd::Click(target));
                i += 2;
            }
            "--attrs" => {
                cmds.push(Cmd::Attrs);
                i += 1;
            }
            "--dump" => {
                let depth: usize = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(5);
                cmds.push(Cmd::Dump(depth));
                i += 2;
            }
            "--text" => {
                cmds.push(Cmd::Text);
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    (pid, dacs, cmds)
}

fn main() {
    if !accessibility::is_trusted() {
        eprintln!("Accessibility not granted.");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("main thread");
    let chromes = find_all_chromes(mtm);

    if chromes.is_empty() {
        eprintln!("No Chrome/Chromium instances found.");
        std::process::exit(1);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();

    // --list is special: just print and exit
    if args.iter().any(|a| a == "--list") {
        eprintln!("Chrome/Chromium instances:");
        for (pid, name, bundle, _) in &chromes {
            eprintln!("  pid={pid} name={name:?} bundle={bundle}");
        }
        return;
    }

    let (target_pid, dacs, cmds) = parse_args(&args);

    // Resolve target Chrome instance
    let (pid, ns_app) = if let Some(tp) = target_pid {
        match chromes.iter().find(|(p, ..)| *p == tp) {
            Some((pid, name, bundle, app)) => {
                eprintln!("Found: {} (bundle={}, pid={})", name, bundle, pid);
                (*pid, app.clone())
            }
            None => {
                eprintln!("No Chrome with pid={tp}. Available:");
                for (pid, name, bundle, _) in &chromes {
                    eprintln!("  pid={pid} name={name:?} bundle={bundle}");
                }
                std::process::exit(1);
            }
        }
    } else if dacs {
        // Find by executable path containing "dacs" (since name is just "Google Chrome")
        let found = chromes.iter().find(|(p, ..)| {
            // Check executable path via /proc or lsof — simpler: just pick the non-Helper
            // Chrome that isn't the first one (pid != chromes[0].0) and isn't Chromium
            // Actually, let's use the known DACS path heuristic
            let Ok(output) = std::process::Command::new("ps")
                .args(["-p", &p.to_string(), "-o", "command="])
                .output()
            else {
                return false;
            };
            let cmd = String::from_utf8_lossy(&output.stdout);
            cmd.contains("DACS") || cmd.contains("dacs") || cmd.contains("datacube")
        });
        match found {
            Some((pid, name, bundle, app)) => {
                eprintln!("Found DACS: {} (bundle={}, pid={})", name, bundle, pid);
                (*pid, app.clone())
            }
            None => {
                eprintln!("No Chrome DACS found. Available:");
                for (pid, name, bundle, _) in &chromes {
                    eprintln!("  pid={pid} name={name:?} bundle={bundle}");
                }
                std::process::exit(1);
            }
        }
    } else {
        // Default: first non-Helper Chrome
        let entry = chromes
            .iter()
            .find(|(_, _, b, _)| !b.contains("helper") && !b.contains("Helper") && !b.contains("plugin"))
            .unwrap_or(&chromes[0]);
        let (pid, name, bundle, app) = entry;
        eprintln!("Found: {} (bundle={}, pid={})", name, bundle, pid);
        (*pid, app.clone())
    };

    let app = AXNode::app(pid);

    // Find first standard window
    let windows = app.find_all(role("AXWindow"));
    eprintln!("Windows ({})", windows.len());
    for (i, w) in windows.iter().enumerate() {
        eprintln!(
            "  [{}] {:?}",
            i,
            w.title().unwrap_or_default(),
        );
    }

    let win = match windows.into_iter().next() {
        Some(w) => w,
        None => {
            eprintln!("No windows found.");
            return;
        }
    };

    // Default: if no commands, dump tree at depth 3
    let cmds = if cmds.is_empty() {
        vec![Cmd::Dump(3)]
    } else {
        cmds
    };

    // Execute commands sequentially
    for cmd in &cmds {
        match cmd {
            Cmd::Activate => {
                unsafe {
                    let _: bool = objc2::msg_send![&ns_app, activateWithOptions: 3usize];
                }
                win.perform_action("AXRaise");
                eprintln!("[activate] {:?}", win.title().unwrap_or_default());
            }
            Cmd::Sleep(secs) => {
                eprintln!("[sleep] {secs}s");
                std::thread::sleep(std::time::Duration::from_secs_f64(*secs));
            }
            Cmd::Tabs => {
                let tabs = win.find_all(AXQuery::new().role("AXRadioButton").dom_class("Tab"));
                eprintln!("[tabs] {} tabs:", tabs.len());
                for (i, tab) in tabs.iter().enumerate() {
                    let desc = tab.description().unwrap_or_default();
                    let val = accessibility::attr_string(&tab.0, "AXValue").unwrap_or_default();
                    let marker = if val == "1" { " *" } else { "" };
                    eprintln!("  [{i}]{marker} {}", trunc(&desc, 60));
                }
            }
            Cmd::Tab(idx) => {
                let tabs = win.find_all(AXQuery::new().role("AXRadioButton").dom_class("Tab"));
                if *idx >= tabs.len() {
                    eprintln!("[tab] index {idx} out of range (have {} tabs)", tabs.len());
                    return;
                }
                let tab = &tabs[*idx];
                let desc = tab.description().unwrap_or_default();
                tab.perform_action("AXPress");
                eprintln!("[tab] -> [{idx}] {:?}", desc);
            }
            Cmd::Click(target) => {
                let buttons = win.find_all(role("AXButton"));
                let found = buttons.iter().find(|b| {
                    let t = b.title().unwrap_or_default();
                    let d = b.description().unwrap_or_default();
                    t.contains(target.as_str()) || d.contains(target.as_str())
                });
                match found {
                    Some(btn) => {
                        let t = btn.title().unwrap_or_default();
                        let d = btn.description().unwrap_or_default();
                        let label = if !t.is_empty() { &t } else { &d };
                        btn.perform_action("AXPress");
                        // Also CGEvent click at center
                        if let (Some((x, y)), Some((w, h))) = (btn.position(), btn.size()) {
                            let cx = x + w / 2.0;
                            let cy = y + h / 2.0;
                            click_at(cx, cy);
                            eprintln!("[click] {label:?} at ({cx:.0}, {cy:.0})");
                        } else {
                            eprintln!("[click] {label:?} (AXPress only, no position)");
                        }
                    }
                    None => {
                        eprintln!("[click] no button matching {target:?}. Available:");
                        for b in &buttons {
                            let t = b.title().unwrap_or_default();
                            let d = b.description().unwrap_or_default();
                            if !t.is_empty() || !d.is_empty() {
                                eprintln!("  title={t:?} desc={d:?}");
                            }
                        }
                    }
                }
            }
            Cmd::Attrs => {
                eprintln!("[attrs] Window:");
                dump_attrs(&win);
                if let Some(wa) = win.find(role("AXWebArea")) {
                    eprintln!("\n[attrs] WebArea:");
                    dump_attrs(&wa);
                }
            }
            Cmd::Dump(depth) => {
                eprintln!("[dump] depth={depth}");
                dump(&win, 0, *depth);
                // Also show WebArea summary
                let web_areas = win.find_all(role("AXWebArea"));
                if !web_areas.is_empty() {
                    eprintln!("\nWebAreas ({}):", web_areas.len());
                    for (i, wa) in web_areas.iter().enumerate() {
                        eprintln!("  [{i}] {:?}", wa.title().unwrap_or_default());
                    }
                }
            }
            Cmd::Text => {
                if let Some(wa) = win.find(role("AXWebArea")) {
                    let title = wa.title().unwrap_or_default();
                    eprintln!("[text] WebArea: {title:?}");
                    let texts = wa.texts(20);
                    for t in &texts {
                        let t = t.trim();
                        if !t.is_empty() {
                            println!("{t}");
                        }
                    }
                } else {
                    eprintln!("[text] No WebArea found.");
                }
            }
        }
    }
}
