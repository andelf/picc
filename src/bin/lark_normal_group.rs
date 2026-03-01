//! Parse Lark group chat messages using DOM class queries.
//!
//! Usage:
//!   cargo run --bin lark_normal_group                    # parse and print messages
//!   cargo run --bin lark_normal_group -- --list          # list sidebar chats
//!   cargo run --bin lark_normal_group -- --goto <name>   # click a chat by name (substring)
//!   cargo run --bin lark_normal_group -- --dump N        # dump full AX tree at depth N
//!   cargo run --bin lark_normal_group -- --dom-query     # debug: list message-item nodes
//!   cargo run --bin lark_normal_group -- --cards S C     # debug: dump S..S+C message-item trees

use objc2::MainThreadMarker;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
};

use picc::accessibility::{self, AXNode, AXQuery, role};

/// Simulate a mouse click at the given screen coordinates.
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

/// Click the center of an AXNode.
fn click_node(node: &AXNode) -> bool {
    let Some((x, y)) = node.position() else { return false };
    let Some((w, h)) = node.size() else { return false };
    click_at(x + w / 2.0, y + h / 2.0);
    true
}

fn find_lark_app(
    _mtm: MainThreadMarker,
) -> Option<(i32, objc2::rc::Retained<NSRunningApplication>)> {
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
    let workspace: objc2::rc::Retained<objc2::runtime::NSObject> =
        unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let apps: objc2::rc::Retained<objc2_foundation::NSArray<NSRunningApplication>> =
        unsafe { objc2::msg_send![&workspace, runningApplications] };

    for app in apps.iter() {
        if let Some(bundle_id) = app.bundleIdentifier() {
            let bundle_str = bundle_id.to_string();
            if bundle_str.contains("lark")
                || bundle_str.contains("Lark")
                || bundle_str.contains("feishu")
            {
                let pid = app.processIdentifier();
                let name = app
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                eprintln!("Found: {} (bundle={}, pid={})", name, bundle_str, pid);
                return Some((pid, app.clone()));
            }
        }
    }
    None
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
    let role = node.role().unwrap_or_default();
    let title = node.title().unwrap_or_default();
    let value = node.value().unwrap_or_default();
    let desc = node.description().unwrap_or_default();
    let cc = node.child_count();
    let pad = " ".repeat(indent);

    let mut info = format!("{pad}{role}");
    if !title.is_empty() {
        info.push_str(&format!(" title={:?}", trunc(&title, 50)));
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

/// A chat entry in the sidebar list.
struct ChatEntry {
    node: AXNode,
    name: String,
    badge: Option<u32>,
}

/// Find the sidebar chat list from the messenger-next WebArea.
fn find_chat_list(win: &AXNode) -> Option<Vec<ChatEntry>> {
    let sidebar = win.find(
        AXQuery::new()
            .role("AXWebArea")
            .title("messenger-next"),
    )?;
    let list = sidebar.find(AXQuery::new().role("AXGroup").filter(|n| {
        n.description().as_deref() == Some("scrollable content")
    }))?;

    let children = list.children();
    let mut entries = Vec::new();
    for entry in &children {
        let texts = entry.find_all(role("AXStaticText"));
        let name = texts
            .iter()
            .find_map(|t| {
                let v = t.value().unwrap_or_default();
                if !v.is_empty() && v.parse::<u32>().is_err() {
                    Some(v)
                } else {
                    None
                }
            })
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let badge = texts
            .iter()
            .find_map(|t| t.value().and_then(|v| v.parse::<u32>().ok()));
        entries.push(ChatEntry {
            node: AXNode::new(entry.0.clone()),
            name,
            badge,
        });
    }
    Some(entries)
}

/// Find the editable input field inside messenger-chat.
fn find_input(chat_wa: &AXNode) -> Option<AXNode> {
    let q = AXQuery::new().filter(|n| {
        let r = n.role().unwrap_or_default();
        if r != "AXTextArea" && r != "AXTextField" {
            return false;
        }
        let attrs = n.attr_names();
        attrs.contains(&"AXFocused".to_string())
            && attrs.contains(&"AXEditableAncestor".to_string())
    });
    chat_wa.find(q)
}

// ---------------------------------------------------------------------------
// Structured message types
// ---------------------------------------------------------------------------

struct ChatMessage {
    sender: String,
    body: String,
    reply_to: Option<String>,
    reactions: Vec<Reaction>,
    reply_count: u32,
    is_self: bool,
}

struct Reaction {
    emoji: String,
    users: Vec<String>,
}

// ---------------------------------------------------------------------------
// Card parsing
// ---------------------------------------------------------------------------

/// Parse sender area: name + optional status.
/// Structure: AXGroup (c=3)
///   ├─ AXStaticText val="Name" (or AXGroup > AXStaticText)
///   ├─ AXGroup > AXImage desc="status"
///   └─ AXGroup > AXStaticText val="signature"
fn parse_sender(node: &AXNode) -> String {
    // First static text child is the name
    let texts = node.find_all(role("AXStaticText"));
    if let Some(first) = texts.first() {
        if let Some(val) = first.value() {
            let val = val.trim();
            if !val.is_empty() {
                return val.to_string();
            }
        }
    }
    String::new()
}

/// Collect leaf text from a subtree. Only handles terminal nodes:
/// AXStaticText, AXLink, AXImage, and AXGroup with title="\n".
fn extract_body(node: &AXNode) -> String {
    let mut parts = Vec::new();
    collect_body_text(node, &mut parts, 15);
    parts.join("")
}

fn collect_body_text(node: &AXNode, parts: &mut Vec<String>, max_depth: usize) {
    if max_depth == 0 {
        return;
    }
    let r = node.role().unwrap_or_default();

    if r == "AXStaticText" {
        if let Some(val) = node.value() {
            let val = val.replace('\u{200b}', "");
            if !val.is_empty() {
                parts.push(val);
            }
        }
        return;
    }

    if r == "AXLink" {
        if let Some(desc) = node.description() {
            if !desc.is_empty() {
                parts.push(format!("[{}]", trunc(&desc, 60)));
                return;
            }
        }
    }

    if r == "AXImage" {
        if let Some(desc) = node.description() {
            let desc = desc.trim();
            if !desc.is_empty()
                && !desc.starts_with("http")
                && !desc.starts_with("/image")
                && desc != "In meeting"
                && desc != "Glance"
            {
                parts.push(format!("[{desc}]"));
            }
        }
        return;
    }

    // AXGroup with title="\n" → explicit line break marker
    if r == "AXGroup" {
        if let Some(title) = node.title() {
            if title == "\n" {
                parts.push("\n".to_string());
                return;
            }
        }
    }

    for child in node.children() {
        collect_body_text(&child, parts, max_depth - 1);
    }
}

// ---------------------------------------------------------------------------
// Selector-driven content extraction
// ---------------------------------------------------------------------------

/// Extract message body by dispatching on message type (DOM classes on item).
fn extract_message_body(content: &AXNode, item_classes: &[String]) -> String {
    let is_post = item_classes.iter().any(|c| c == "post-message");

    if is_post {
        extract_post_body(content)
    } else {
        extract_body(content)
    }
}

/// Extract rich-text post body: iterate message-post children by DOM class.
fn extract_post_body(content: &AXNode) -> String {
    let post = content.find(AXQuery::new().dom_class("message-post"));
    let post = match post {
        Some(p) => p,
        None => return extract_body(content), // fallback
    };

    let mut parts = Vec::new();
    for child in post.children() {
        let cls = child.dom_classes();
        if cls.iter().any(|c| c == "richTextDocs-codeBlockV2-wrapper") {
            parts.push(extract_code_block(&child));
        } else {
            // rich-text-paragraph or unknown → leaf text
            parts.push(extract_body(&child));
        }
    }
    parts.join("\n")
}

/// Extract code block: find richTextDocs-code-line nodes, skip first child (line number).
fn extract_code_block(wrapper: &AXNode) -> String {
    let lines = wrapper.find_all(AXQuery::new().dom_class("richTextDocs-code-line"));
    let mut result = Vec::new();
    for line in &lines {
        let children = line.children();
        // skip(1): first child is the line number
        let text: String = children
            .iter()
            .skip(1)
            .map(|c| extract_body(c))
            .collect::<Vec<_>>()
            .join("");
        if !text.is_empty() {
            result.push(text);
        }
    }
    result.join("\n")
}

/// Extract reactions from a message-reactions container using DOM classes.
fn extract_reactions(reactions_node: &AXNode) -> Vec<Reaction> {
    let items = reactions_node.find_all(AXQuery::new().dom_class("reaction-item"));
    items
        .iter()
        .filter_map(|item| {
            let emoji_img = item.find(role("AXImage"))?;
            let emoji = emoji_img.description().unwrap_or_default().trim().to_string();
            if emoji.is_empty() {
                return None;
            }
            let user_node = item.find(AXQuery::new().dom_class("reaction-user"))?;
            let users: Vec<String> = user_node
                .texts(3)
                .into_iter()
                .map(|v| v.trim().trim_end_matches(',').trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
            Some(Reaction { emoji, users })
        })
        .collect()
}

/// Check if a node is a reply indicator ("N reply" / "N replies").
fn is_reply_indicator(node: &AXNode) -> Option<u32> {
    let texts = node.texts(3);
    texts
        .iter()
        .find(|t| t.contains("reply") || t.contains("replies"))
        .and_then(|t| t.split_whitespace().next()?.parse().ok())
}

/// Parse a single message-item node using DOM class selectors.
///
/// Uses CSS class semantics from Lark's Electron DOM instead of
/// recursive heuristics. Key classes:
///   - `message-info` → sender area
///   - `message-content` → body content
///   - `message-reactions` → reactions container
///   - `post-message` / `text-message` → message type (on item)
fn parse_card(item: &AXNode) -> Option<ChatMessage> {
    let classes = item.dom_classes();
    let is_self = classes.contains(&"message-self".to_string());
    let is_first = classes.contains(&"message-item-first".to_string());

    // Sender: use DOM class to locate message-info area
    let sender = if is_first {
        item.find(AXQuery::new().dom_class("message-info"))
            .map(|n| parse_sender(&n))
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Body: locate message-content via DOM class
    let body = item
        .find(AXQuery::new().dom_class("message-content"))
        .map(|c| extract_message_body(&c, &classes))
        .unwrap_or_default();

    // Reactions: locate message-reactions via DOM class
    let reactions = item
        .find(AXQuery::new().dom_class("message-reactions"))
        .map(|r| extract_reactions(&r))
        .unwrap_or_default();

    // Reply count
    let reply_count = item
        .find(AXQuery::new().filter(|n| is_reply_indicator(n).is_some()))
        .and_then(|n| is_reply_indicator(&n))
        .unwrap_or(0);

    // Reply-to: check for "Reply to" text in content area
    let reply_to = item
        .find(AXQuery::new().filter(|n| {
            n.texts(3).iter().any(|t| t.starts_with("Reply to "))
        }))
        .and_then(|n| {
            n.texts(3)
                .into_iter()
                .find(|t| t.starts_with("Reply to "))
                .map(|t| t.strip_prefix("Reply to ").unwrap_or(&t).to_string())
        });

    let body = body.replace('\u{200b}', "").trim().to_string();

    if body.is_empty() && sender.is_empty() && reactions.is_empty() && reply_to.is_none() {
        return None;
    }

    Some(ChatMessage {
        sender,
        body,
        reply_to,
        reactions,
        reply_count,
        is_self,
    })
}

fn main() {
    if !accessibility::is_trusted() {
        eprintln!("Accessibility not granted.");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("main thread");
    let (pid, _) = match find_lark_app(mtm) {
        Some(v) => v,
        None => {
            eprintln!("Lark not running.");
            std::process::exit(1);
        }
    };

    let app = AXNode::app(pid);
    let win = app
        .find_all(role("AXWindow"))
        .into_iter()
        .find(|w| {
            let t = w.title().unwrap_or_default();
            t == "Lark" || t == "飞书" || t == "Feishu"
        });

    let win = match win {
        Some(w) => w,
        None => {
            eprintln!("No Lark main window.");
            return;
        }
    };
    eprintln!("Window: {:?}", win.title());

    let args: Vec<String> = std::env::args().skip(1).collect();

    // --list: list sidebar chat entries
    if args.first().map(|a| a == "--list").unwrap_or(false) {
        match find_chat_list(&win) {
            Some(entries) => {
                for (i, e) in entries.iter().enumerate() {
                    let badge = e.badge.map(|n| format!(" ({n})")).unwrap_or_default();
                    println!("#{i} {}{badge}", e.name);
                }
            }
            None => eprintln!("Chat list not found."),
        }
        return;
    }

    // --goto <name>: click a chat entry by name (substring match)
    if args.first().map(|a| a == "--goto").unwrap_or(false) {
        let target = match args.get(1) {
            Some(t) => t,
            None => {
                eprintln!("Usage: --goto <name>");
                return;
            }
        };
        match find_chat_list(&win) {
            Some(entries) => {
                let found = entries.iter().find(|e| {
                    e.name.to_lowercase().contains(&target.to_lowercase())
                });
                match found {
                    Some(entry) => {
                        eprintln!("Clicking: {}", entry.name);
                        if click_node(&entry.node) {
                            eprintln!("  clicked at center of entry");
                        } else {
                            eprintln!("  failed to get position/size");
                        }
                    }
                    None => {
                        eprintln!("No chat matching {:?}", target);
                        eprintln!("Available:");
                        for e in &entries {
                            eprintln!("  {}", e.name);
                        }
                    }
                }
            }
            None => eprintln!("Chat list not found."),
        }
        return;
    }

    let chat_wa = win.select(&[
        AXQuery::new()
            .role("AXWebArea")
            .title_contains("messenger-chat"),
    ]);

    let chat_wa = match chat_wa {
        Some(wa) => {
            eprintln!("Found messenger-chat");
            wa
        }
        None => {
            eprintln!("messenger-chat not found.");
            return;
        }
    };

    // Check for --dump mode
    if args.first().map(|a| a == "--dump").unwrap_or(false) {
        let depth: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
        eprintln!("\n=== messenger-chat AX tree (depth {depth}) ===\n");
        dump(&chat_wa, 0, depth);
        return;
    }

    // Check for --dom-query mode: test direct query by AXDOMClassList
    if args.first().map(|a| a == "--dom-query").unwrap_or(false) {
        let q = &AXQuery::new().dom_class("message-item");
        let items: Vec<AXNode> = accessibility::find_all(&chat_wa.0, q, 30)
            .into_iter().map(AXNode::new).collect();
        eprintln!("\n=== All message-item: {} ===\n", items.len());
        for (i, item) in items.iter().enumerate() {
            let classes = item.dom_classes();
            let is_self = classes.contains(&"message-self".to_string());
            let is_first = classes.contains(&"message-item-first".to_string());
            let msg_type = classes.iter()
                .find(|c| c.ends_with("-message"))
                .cloned()
                .unwrap_or_default();
            let text = trunc(&item.text(8), 40);
            let marker = if is_self { "SELF" } else { "OTHER" };
            let first = if is_first { " [1st]" } else { "" };
            eprintln!("  #{i} [{marker}]{first} {msg_type} | {text:?}");
        }
        return;
    }

    // Check for --cards mode: dump message-item nodes with full AX tree
    if args.first().map(|a| a == "--cards").unwrap_or(false) {
        let start: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
        let q = &AXQuery::new().dom_class("message-item");
        let items: Vec<AXNode> = accessibility::find_all(&chat_wa.0, q, 30)
            .into_iter().map(AXNode::new).collect();
        for (i, item) in items.iter().enumerate().skip(start).take(count) {
            let classes = item.dom_classes();
            eprintln!("\n=== message-item #{i} classes={:?} ===", classes);
            dump(item, 0, 20);
        }
        return;
    }

    // --- Main mode: query all message-item nodes via DOM class ---

    // Extract chat target name from input placeholder
    if let Some(input) = find_input(&chat_wa) {
        let placeholder = input.value().unwrap_or_default();
        if !placeholder.is_empty() {
            eprintln!("Chat input: {:?}", trunc(&placeholder, 40));
        }
    }

    let q = &AXQuery::new().dom_class("message-item");
    let items: Vec<AXNode> = accessibility::find_all(&chat_wa.0, q, 30)
        .into_iter().map(AXNode::new).collect();
    eprintln!("Found {} message-item nodes\n", items.len());

    // Parse and print structured messages
    let mut last_sender = String::new();
    for (i, item) in items.iter().enumerate() {
        match parse_card(item) {
            Some(mut msg) => {
                // For continuation messages, inherit previous sender
                if msg.sender.is_empty() {
                    msg.sender = last_sender.clone();
                } else {
                    last_sender = msg.sender.clone();
                }
                let marker = if msg.is_self { "SELF" } else { "OTHER" };
                println!("--- #{i} [{marker}] ---");
                if !msg.sender.is_empty() {
                    println!("From: {}", msg.sender);
                }
                if !msg.body.is_empty() {
                    println!("{}", msg.body);
                }
                if let Some(ref reply_to) = msg.reply_to {
                    println!("  (reply to: {})", reply_to);
                }
                for r in &msg.reactions {
                    println!("  [{}: {}]", r.emoji, r.users.join(", "));
                }
                if msg.reply_count > 0 {
                    println!("  ({} replies)", msg.reply_count);
                }
                println!();
            }
            None => {}
        }
    }
}
