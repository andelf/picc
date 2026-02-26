use objc2::MainThreadMarker;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGScrollEventUnit,
};

use picc::accessibility::{self, AXNode, AXQuery, role};

fn find_lark_app(_mtm: MainThreadMarker) -> Option<(i32, objc2::rc::Retained<NSRunningApplication>)> {
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
    let workspace: objc2::rc::Retained<objc2::runtime::NSObject> =
        unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let apps: objc2::rc::Retained<objc2_foundation::NSArray<NSRunningApplication>> =
        unsafe { objc2::msg_send![&workspace, runningApplications] };

    for app in apps.iter() {
        if let Some(bundle_id) = app.bundleIdentifier() {
            let bundle_str = bundle_id.to_string();
            if bundle_str.contains("lark") || bundle_str.contains("Lark") || bundle_str.contains("feishu") {
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

// ---------------------------------------------------------------------------
// Structured message types
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ChatMessage {
    sender: String,
    is_bot: bool,
    body: String,
    replies: Vec<Reply>,
}

#[derive(Debug)]
struct Reply {
    sender: String,
    text: String,
}

// ---------------------------------------------------------------------------
// Message parsing using structural navigation
// ---------------------------------------------------------------------------

/// Parse a single message card from the message list.
///
/// Lark message card structure (from AX tree dump):
/// ```text
/// AXGroup (card)
///   └─ AXGroup > AXGroup(c=2)
///        ├─ [0] left area (avatar placeholder, often empty)
///        └─ [1] right area AXGroup
///             ├─ [0] sender area: AXGroup { "name", "BOT", "From ..." }
///             └─ [1] AXGroup(c=2)
///                  ├─ [0] content area (body + reply thread)
///                  │    ├─ AXGroup (main message body)
///                  │    └─ AXGroup (reply entries + "Reply" button)
///                  └─ [1] action area (empty)
/// ```
fn parse_card(card: &AXNode) -> Option<ChatMessage> {
    // Navigate to the first AXGroup with 2 children (left/right split)
    let split = find_split(card, 6)?;
    let right = split.child(1).or_else(|| split.child(0))?;

    let right_children = right.children();
    if right_children.is_empty() {
        return None;
    }

    // Parse sender area (first child of right)
    let (sender, is_bot) = if right_children.len() >= 2 {
        parse_sender_area(&right_children[0])
    } else {
        (String::new(), false)
    };

    // Content area: usually right_children[1] or right_children[0] if no sender
    let content_area = if right_children.len() >= 2 {
        &right_children[1]
    } else {
        &right_children[0]
    };

    // Inside content area, the first child with children is the message+replies container
    let inner = content_area.child(0).unwrap_or_else(|| AXNode::new(content_area.0.clone()));
    let inner_children = inner.children();

    // Separate main body from reply thread
    let mut body_parts = Vec::new();
    let mut replies = Vec::new();

    for child in &inner_children {
        // Reply area pattern: contains AXImage (avatar) and "Reply" button text
        if child.has_role("AXImage", 5) {
            // This is the reply/action area — parse reply entries
            parse_reply_area(child, &mut replies);
        } else {
            // This is body content — collect text parts, filter out source links
            let texts = child.texts(15);
            let mut j = 0;
            while j < texts.len() {
                let t = &texts[j];
                if t.trim().is_empty() { j += 1; continue; }
                // "From " + next text = source link, emit as single line
                if (t.starts_with("From ") || t.trim() == "From") && j + 1 < texts.len() {
                    let source = format!("From {}", texts[j + 1].trim());
                    body_parts.push(source);
                    j += 2;
                    continue;
                }
                body_parts.push(t.trim().to_string());
                j += 1;
            }
        }
    }

    let body = body_parts.join("\n");

    // Skip pure timestamps / date separators
    if sender.is_empty() && replies.is_empty() {
        let b = body.trim();
        if b.is_empty()
            || b == "Yesterday" || b == "Today" || b == "New"
            || b.chars().all(|c| c.is_ascii_digit() || c == ':')
        {
            return None;
        }
    }

    Some(ChatMessage {
        sender: sender.trim_end_matches(',').trim().to_string(),
        is_bot,
        body,
        replies,
    })
}

/// Move the mouse to a given screen position without clicking.
fn mouse_move_to(x: f64, y: f64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let event = CGEvent::new_mouse_event(
        source.as_deref(),
        objc2_core_graphics::CGEventType::MouseMoved,
        CGPoint { x, y },
        objc2_core_graphics::CGMouseButton::Left,
    );
    if let Some(ref ev) = event {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

/// Simulate a scroll wheel event at the given screen coordinates.
///
/// `delta` is in pixels: positive = scroll up (content moves down),
/// negative = scroll down (content moves up).
fn scroll_at(x: f64, y: f64, delta: i32) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let event = CGEvent::new_scroll_wheel_event2(
        source.as_deref(),
        CGScrollEventUnit::Pixel,
        1,     // wheel_count (1 = vertical only)
        delta, // wheel1 (vertical: negative = up)
        0,     // wheel2 (horizontal)
        0,     // wheel3
    );
    if let Some(ref ev) = event {
        CGEvent::set_location(Some(ev), CGPoint { x, y });
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

/// Find the first AXGroup with exactly 2 children (the left/right split).
fn find_split(node: &AXNode, max_depth: usize) -> Option<AXNode> {
    if max_depth == 0 { return None; }
    let children = node.children();
    if children.len() == 2
        && children.iter().all(|c| c.role().as_deref() == Some("AXGroup"))
    {
        return Some(AXNode::new(node.0.clone()));
    }
    for child in &children {
        if let Some(found) = find_split(child, max_depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Parse the sender area: extracts name and BOT flag.
///
/// Sender area has children like:
///   AXGroup { AXStaticText "name" }
///   AXGroup { AXStaticText "BOT" }
///   AXGroup { AXStaticText "From Lark Base" }
fn parse_sender_area(node: &AXNode) -> (String, bool) {
    let texts = node.texts(4);
    if texts.is_empty() {
        return (String::new(), false);
    }
    let is_bot = texts.iter().any(|t| t == "BOT");
    let name: String = texts.iter()
        .filter(|t| *t != "BOT" && !t.starts_with("From "))
        .cloned()
        .collect::<Vec<_>>()
        .join("");
    (name, is_bot)
}

/// Parse the reply/action area.
///
/// Reply area structure:
/// ```text
/// AXGroup (reply area)
///   ├─ AXGroup { AXStaticText "sender", AXStaticText "reply body" }
///   ├─ AXImage (avatar)
///   └─ AXStaticText "Reply"
/// ```
/// Multiple replies stack as additional groups before the final AXImage + "Reply".
fn parse_reply_area(node: &AXNode, replies: &mut Vec<Reply>) {
    let children = node.children();
    for child in &children {
        let r = child.role().unwrap_or_default();
        // Skip images and "Reply" button
        if r == "AXImage" { continue; }
        if r == "AXStaticText" {
            if child.value().as_deref() == Some("Reply") { continue; }
        }
        // This should be a reply entry — first text is sender, rest is body
        let texts = child.texts(6);
        if texts.is_empty() { continue; }
        let sender = texts[0].clone();
        let body: String = texts[1..].iter().cloned().collect::<Vec<_>>().join("");
        if !body.is_empty() || !sender.is_empty() {
            replies.push(Reply { sender, text: body });
        }
    }
}

/// Find the message list container using XPath-like structural navigation.
///
/// Strategy: inside messenger-chat AXWebArea, find the node with the most
/// children that are deep AXGroup trees (message cards).
fn find_message_list(node: &AXNode, depth: usize, max_depth: usize, best: &mut Option<(usize, AXNode)>) {
    if depth > max_depth { return; }
    let children = node.children();
    if children.len() >= 3 {
        let card_count = children.iter().filter(|c| {
            if c.role().as_deref() != Some("AXGroup") { return false; }
            // Must have depth (grandchildren)
            let gc_count: usize = c.children().iter().map(|gc| gc.child_count()).sum();
            if gc_count == 0 { return false; }
            c.text(15).trim().chars().count() > 10
        }).count();
        if card_count >= 3 {
            let dominated = match best {
                Some((best_count, _)) => card_count > *best_count,
                None => true,
            };
            if dominated {
                *best = Some((card_count, AXNode::new(node.0.clone())));
            }
        }
    }
    for child in &children {
        find_message_list(child, depth + 1, max_depth, best);
    }
}

fn main() {
    if !accessibility::is_trusted() {
        eprintln!("Accessibility permission not granted.");
        eprintln!("Go to System Settings > Privacy & Security > Accessibility and add this app.");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let (pid, lark_app) = match find_lark_app(mtm) {
        Some(v) => v,
        None => {
            eprintln!("Lark is not running.");
            std::process::exit(1);
        }
    };

    let app = AXNode::app(pid);

    // Find Lark main window
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
            eprintln!("No Lark main window found.");
            return;
        }
    };

    // XPath-like: //AXWebArea[title*="messenger-chat"]
    let chat_wa = win.select(&[
        AXQuery::new().role("AXWebArea").title_contains("messenger-chat"),
    ]);

    let chat_wa = match chat_wa {
        Some(wa) => {
            eprintln!("Found messenger-chat");
            wa
        }
        None => {
            eprintln!("messenger-chat not found. Is Lark showing a chat?");
            let web_areas = win.find_all(role("AXWebArea"));
            for wa in &web_areas {
                eprintln!("  AXWebArea: title={:?}", wa.title());
            }
            return;
        }
    };

    // Find message list container
    let mut best = None;
    find_message_list(&chat_wa, 0, 30, &mut best);
    let msg_list = match best {
        Some((count, node)) => {
            eprintln!("Message list: {} cards", count);
            node
        }
        None => {
            eprintln!("No message list found.");
            return;
        }
    };

    // Parse CLI args: [--scroll N] [filter_text]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut scroll_count: u32 = 0;
    let mut filter: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--scroll" {
            if i + 1 < args.len() {
                scroll_count = args[i + 1].parse().unwrap_or(1);
                i += 2;
                continue;
            }
        }
        filter = Some(args[i].clone());
        i += 1;
    }

    // Scroll up if requested
    if scroll_count > 0 {
        if let (Some(pos), Some(sz)) = (chat_wa.position(), chat_wa.size()) {
            let cx = pos.0 + sz.0 / 2.0;
            let cy = pos.1 + sz.1 / 2.0;
            eprintln!("Scrolling up {} times at ({:.0}, {:.0})", scroll_count, cx, cy);
            // Activate Lark and move mouse to chat area center
            #[allow(deprecated)]
            lark_app.activateWithOptions(
                objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
            );
            std::thread::sleep(std::time::Duration::from_millis(500));
            mouse_move_to(cx, cy);
            std::thread::sleep(std::time::Duration::from_millis(200));

            for j in 0..scroll_count {
                eprintln!("  scroll #{}/{}", j + 1, scroll_count);
                scroll_at(cx, cy, 800);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            // Wait for Lark to load content
            eprintln!("Waiting for content to load...");
            std::thread::sleep(std::time::Duration::from_secs(2));

            // Re-find message list after scroll
            let mut best_new = None;
            find_message_list(&chat_wa, 0, 30, &mut best_new);
            if let Some((count, new_list)) = best_new {
                eprintln!("Message list after scroll: {} cards", count);
                // Re-assign — we shadow msg_list below
                return run_parse(&new_list, &filter);
            }
        } else {
            eprintln!("Cannot scroll: chat area position/size unavailable");
        }
    }

    run_parse(&msg_list, &filter);
}

fn run_parse(msg_list: &AXNode, filter: &Option<String>) {
    // Select cards — if filter is set, use has_text to pre-filter at AX tree level
    let cards: Vec<AXNode> = if let Some(ref keyword) = filter {
        eprintln!("Filter: {:?}", keyword);
        let q = AXQuery::new().role("AXGroup").has_text(keyword);
        msg_list.children_matching(&q)
    } else {
        msg_list.children()
    };
    eprintln!("Cards to parse: {}", cards.len());

    // Parse each message card
    let mut parsed = Vec::new();
    for card in &cards {
        if let Some(msg) = parse_card(card) {
            parsed.push(msg);
        }
    }

    eprintln!("Parsed {} messages\n", parsed.len());

    for (i, msg) in parsed.iter().enumerate() {
        let bot_tag = if msg.is_bot { " [BOT]" } else { "" };
        println!("--- Message #{} ---", i);
        if !msg.sender.is_empty() {
            println!("From: {}{}", msg.sender, bot_tag);
        }
        if !msg.body.is_empty() {
            println!("{}", msg.body);
        }
        for reply in &msg.replies {
            println!("  > {}: {}", reply.sender, reply.text);
        }
        println!();
    }
}
