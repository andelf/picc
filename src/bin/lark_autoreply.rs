//! Lark auto-reply: poll current chat, detect new messages, reply via Kimi API.
//!
//! Usage:
//!   cargo run --bin lark_autoreply -- --dry-run -v
//!   cargo run --bin lark_autoreply
//!   KIMI_API_KEY=sk-... cargo run --bin lark_autoreply -- -s "你是一个友善的助手"

use std::thread;
use std::time::Duration;

use clap::Parser;
use objc2::MainThreadMarker;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
    CGScrollEventUnit,
};
use serde::{Deserialize, Serialize};

use picc::accessibility::{self, AXNode, AXQuery, role};

// ---------------------------------------------------------------------------
// Supported emoji list (from Lark reaction panel "Frequently used")
// ---------------------------------------------------------------------------

const SUPPORTED_EMOJIS: &[&str] = &[
    "Like", "Glance", "GotIt", "Done", "Smirk", "NewMoonFace", "Facepalm",
    "Yes", "Enough", "Lol", "Tears", "FistBump", "OK", "Embarrassed",
    "CryCoveringMouth", "Grin", "FingerHeart", "Praise",
];

fn emoji_list_str() -> String {
    SUPPORTED_EMOJIS.join(", ")
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "lark_autoreply", about = "Auto-reply in current Lark chat via Kimi API")]
struct Args {
    /// Polling interval in seconds
    #[arg(short, long, default_value_t = 5)]
    interval: u64,

    /// Custom system prompt for Kimi
    #[arg(short, long)]
    system: Option<String>,

    /// Dry run: print reply but don't send
    #[arg(short, long)]
    dry_run: bool,

    /// Verbose debug logging
    #[arg(short, long)]
    verbose: bool,
}

fn build_system_prompt(user_extra: Option<&str>) -> String {
    let base = format!(
        r#"你是用户本人，正在和朋友聊天。请根据聊天上下文用简短自然的中文回复，像真人一样。不要提及你是AI。

你必须严格按以下JSON格式回复，不要输出任何其他内容：
{{"emoji": "<表情名>", "reply": "<回复文本>"}}

可选表情列表（emoji字段必须从中选一个）：
{emojis}

规则：
- emoji: 根据对方消息语气选择最合适的表情，表示你的即时反应
- reply: 简短自然的文字回复，一句话即可，不要换行
- 只输出JSON，不要markdown代码块，不要解释"#,
        emojis = emoji_list_str()
    );
    match user_extra {
        Some(extra) => format!("{base}\n\n额外要求：{extra}"),
        None => base,
    }
}

// ---------------------------------------------------------------------------
// Copied utility functions (from lark_normal_group.rs / lark_send.rs)
// ---------------------------------------------------------------------------

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
    thread::sleep(Duration::from_millis(50));
    if let Some(ref ev) = up {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

fn move_mouse(x: f64, y: f64) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let point = CGPoint { x, y };
    let ev = CGEvent::new_mouse_event(
        source.as_deref(),
        CGEventType::MouseMoved,
        point,
        CGMouseButton::Left,
    );
    if let Some(ref ev) = ev {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

fn scroll_at(x: f64, y: f64, delta: i32) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let event = CGEvent::new_scroll_wheel_event2(
        source.as_deref(),
        CGScrollEventUnit::Pixel,
        1,
        delta, // negative = scroll down
        0,
        0,
    );
    if let Some(ref ev) = event {
        CGEvent::set_location(Some(ev), CGPoint { x, y });
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

fn type_text(text: &str) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let utf16: Vec<u16> = text.encode_utf16().collect();

    for chunk in utf16.chunks(20) {
        let down = CGEvent::new_keyboard_event(source.as_deref(), 0, true);
        if let Some(ref ev) = down {
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    Some(ev),
                    chunk.len() as _,
                    chunk.as_ptr(),
                );
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        thread::sleep(Duration::from_millis(5));

        let up = CGEvent::new_keyboard_event(source.as_deref(), 0, false);
        if let Some(ref ev) = up {
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    Some(ev),
                    chunk.len() as _,
                    chunk.as_ptr(),
                );
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn press_return() {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let down = CGEvent::new_keyboard_event(source.as_deref(), 36, true);
    if let Some(ref ev) = down {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
    thread::sleep(Duration::from_millis(20));
    let up = CGEvent::new_keyboard_event(source.as_deref(), 36, false);
    if let Some(ref ev) = up {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }
}

// ---------------------------------------------------------------------------
// Message parsing (from lark_normal_group.rs)
// ---------------------------------------------------------------------------

struct ChatMessage {
    sender: String,
    body: String,
    is_self: bool,
}

fn parse_sender(node: &AXNode) -> String {
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

fn extract_message_body(content: &AXNode, item_classes: &[String]) -> String {
    let is_post = item_classes.iter().any(|c| c == "post-message");
    if is_post {
        extract_post_body(content)
    } else {
        extract_body(content)
    }
}

fn extract_post_body(content: &AXNode) -> String {
    let post = content.find(AXQuery::new().dom_class("message-post"));
    let post = match post {
        Some(p) => p,
        None => return extract_body(content),
    };

    let mut parts = Vec::new();
    for child in post.children() {
        let cls = child.dom_classes();
        if cls.iter().any(|c| c == "richTextDocs-codeBlockV2-wrapper") {
            extract_code_block(&child, &mut parts);
        } else {
            parts.push(extract_body(&child));
        }
    }
    parts.join("\n")
}

fn extract_code_block(wrapper: &AXNode, result: &mut Vec<String>) {
    let lines = wrapper.find_all(AXQuery::new().dom_class("richTextDocs-code-line"));
    for line in &lines {
        let children = line.children();
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
}

fn parse_card(item: &AXNode) -> Option<ChatMessage> {
    let classes = item.dom_classes();
    let is_self = classes.contains(&"message-self".to_string());
    let is_first = classes.contains(&"message-item-first".to_string());

    let sender = if is_first {
        item.find(AXQuery::new().dom_class("message-info"))
            .map(|n| parse_sender(&n))
            .unwrap_or_default()
    } else {
        String::new()
    };

    let body = item
        .find(AXQuery::new().dom_class("message-content"))
        .map(|c| extract_message_body(&c, &classes))
        .unwrap_or_default();

    let body = body.replace('\u{200b}', "").trim().to_string();

    if body.is_empty() && sender.is_empty() {
        return None;
    }

    Some(ChatMessage {
        sender,
        body,
        is_self,
    })
}

// ---------------------------------------------------------------------------
// Emoji reaction
// ---------------------------------------------------------------------------

/// Scroll chat to bottom, then find the last !is_self message-item AXNode.
fn find_last_other_item(chat_wa: &AXNode) -> Option<AXNode> {
    // Scroll to bottom
    if let (Some((x, y)), Some((w, h))) = (chat_wa.position(), chat_wa.size()) {
        let cx = x + w / 2.0;
        let cy = y + h / 2.0;
        for _ in 0..5 {
            scroll_at(cx, cy, -300);
            thread::sleep(Duration::from_millis(100));
        }
        thread::sleep(Duration::from_millis(300));
    }

    let q = &AXQuery::new().dom_class("message-item");
    let items: Vec<AXNode> = accessibility::find_all(&chat_wa.0, q, 30)
        .into_iter()
        .map(AXNode::new)
        .collect();
    items.into_iter().rev().find(|item| {
        let cls = item.dom_classes();
        !cls.contains(&"message-self".to_string())
    })
}

/// React to a message-item with the given emoji name.
///
/// Flow: hover card → wait toolbar → hover praise → wait popover → click emoji.
fn react_emoji(win: &AXNode, item: &AXNode, emoji_name: &str) -> bool {
    // Step 1: hover message card to trigger toolbar
    let (x, y) = match item.position() {
        Some(p) => p,
        None => return false,
    };
    let (w, h) = match item.size() {
        Some(s) => s,
        None => return false,
    };
    move_mouse(x + w / 2.0, y + h / 2.0);
    thread::sleep(Duration::from_millis(1500));

    // Step 2: find toolbar and praise button
    let toolbar = match item.find(AXQuery::new().dom_class("messageAction__toolbar")) {
        Some(t) => t,
        None => {
            eprintln!("[react] toolbar not found after hover");
            return false;
        }
    };
    let praise = match toolbar.find(AXQuery::new().dom_class("praise")) {
        Some(p) => p,
        None => {
            eprintln!("[react] praise button not found");
            return false;
        }
    };

    // Step 3: hover praise to open emoji picker
    let (px, py) = match praise.position() {
        Some(p) => p,
        None => return false,
    };
    let (pw, ph) = match praise.size() {
        Some(s) => s,
        None => return false,
    };
    move_mouse(px + pw / 2.0, py + ph / 2.0);
    thread::sleep(Duration::from_millis(1500));
    // Re-hover to ensure mouse stays
    move_mouse(px + pw / 2.0, py + ph / 2.0);
    thread::sleep(Duration::from_millis(500));

    // Step 4: find emoji picker popover (in window, not in chat_wa)
    let popover = match win.find(AXQuery::new().dom_class("ud__popover")) {
        Some(p) => p,
        None => {
            eprintln!("[react] emoji popover not found");
            return false;
        }
    };

    // Step 5: find target emoji by searching all AXImage nodes in popover
    let images = popover.find_all(role("AXImage"));
    let target = images.into_iter().find(|n| {
        n.description().as_deref() == Some(emoji_name)
    });
    let target = match target {
        Some(t) => t,
        None => {
            eprintln!("[react] emoji {:?} not found in popover", emoji_name);
            return false;
        }
    };

    let (ex, ey) = match target.position() {
        Some(p) => p,
        None => return false,
    };
    let (ew, eh) = match target.size() {
        Some(s) => s,
        None => return false,
    };
    click_at(ex + ew / 2.0, ey + eh / 2.0);
    thread::sleep(Duration::from_millis(300));
    true
}

/// Reply to a message: hover card → click reply button → type text → send.
///
/// This creates a quoted reply in Lark, maintaining conversation context.
fn reply_to_message(item: &AXNode, text: &str) -> bool {
    // Step 1: hover message card to trigger toolbar
    let (x, y) = match item.position() {
        Some(p) => p,
        None => return false,
    };
    let (w, h) = match item.size() {
        Some(s) => s,
        None => return false,
    };
    move_mouse(x + w / 2.0, y + h / 2.0);
    thread::sleep(Duration::from_millis(1500));

    // Step 2: find toolbar and first reply button
    let toolbar = match item.find(AXQuery::new().dom_class("messageAction__toolbar")) {
        Some(t) => t,
        None => {
            eprintln!("[reply] toolbar not found after hover");
            return false;
        }
    };
    let reply_buttons = toolbar.find_all(AXQuery::new().dom_class("reply"));
    let reply_btn = match reply_buttons.first() {
        Some(r) => r,
        None => {
            eprintln!("[reply] reply button not found");
            return false;
        }
    };

    // Step 3: click reply button
    let (rx, ry) = match reply_btn.position() {
        Some(p) => p,
        None => return false,
    };
    let (rw, rh) = match reply_btn.size() {
        Some(s) => s,
        None => return false,
    };
    click_at(rx + rw / 2.0, ry + rh / 2.0);
    thread::sleep(Duration::from_millis(500));

    // Step 4: type text and send
    type_text(text);
    thread::sleep(Duration::from_millis(100));
    press_return();
    thread::sleep(Duration::from_millis(100));
    true
}

#[allow(dead_code)]
/// Reply in thread: open thread sidebar → find thread input → type text → send.
///
/// Two cases:
/// 1. Card already has a thread (`message-thread-container`): click the "Reply" area
///    in the `thread-reply-exposure-view` to open the thread sidebar.
/// 2. Card has no thread: hover to show toolbar → click 4th button (reply-in-thread).
///
/// After sidebar opens, find the second AXTextArea (placeholder "Reply to thread"),
/// focus it, type text, and press Return.
fn reply_in_thread(chat_wa: &AXNode, item: &AXNode, text: &str) -> bool {
    let has_thread = item.has_dom_class("message-thread-container");

    if has_thread {
        // Case 1: existing thread — click "Reply" area in thread-reply-exposure-view
        eprintln!("[thread] card has existing thread, clicking Reply area");
        let reply_area = item.find(AXQuery::new().dom_class("thread-reply-exposure-view"));
        match reply_area {
            Some(area) => {
                let (ax, ay) = match area.position() {
                    Some(p) => p,
                    None => return false,
                };
                let (aw, ah) = match area.size() {
                    Some(s) => s,
                    None => return false,
                };
                click_at(ax + aw / 2.0, ay + ah / 2.0);
                thread::sleep(Duration::from_millis(1500));
            }
            None => {
                eprintln!("[thread] thread-reply-exposure-view not found");
                return false;
            }
        }
    } else {
        // Case 2: no thread — hover to show toolbar, click 4th button
        eprintln!("[thread] no existing thread, using toolbar button");
        let (x, y) = match item.position() {
            Some(p) => p,
            None => return false,
        };
        let (w, h) = match item.size() {
            Some(s) => s,
            None => return false,
        };
        move_mouse(x + w / 2.0, y + h / 2.0);
        thread::sleep(Duration::from_millis(1500));

        let toolbar = match item.find(AXQuery::new().dom_class("messageAction__toolbar")) {
            Some(t) => t,
            None => {
                eprintln!("[thread] toolbar not found after hover");
                return false;
            }
        };
        let children = toolbar.children();
        // 4th button (index 3) is "reply in thread"
        if children.len() < 4 {
            eprintln!("[thread] toolbar has only {} buttons, expected >= 4", children.len());
            return false;
        }
        let btn = &children[3];
        let (bx, by) = match btn.position() {
            Some(p) => p,
            None => return false,
        };
        let (bw, bh) = match btn.size() {
            Some(s) => s,
            None => return false,
        };
        click_at(bx + bw / 2.0, by + bh / 2.0);
        thread::sleep(Duration::from_millis(2000));
    }

    // Wait for thread sidebar to open — poll for second AXTextArea
    let thread_input = wait_for_thread_input(chat_wa, 5);
    match thread_input {
        Some(input) => {
            // Focus and type
            input.set_focused(true);
            thread::sleep(Duration::from_millis(200));
            if let (Some((ix, iy)), Some((iw, ih))) = (input.position(), input.size()) {
                click_at(ix + iw / 2.0, iy + ih / 2.0);
                thread::sleep(Duration::from_millis(200));
            }
            type_text(text);
            thread::sleep(Duration::from_millis(100));
            press_return();
            thread::sleep(Duration::from_millis(100));
            true
        }
        None => {
            eprintln!("[thread] thread input not found after waiting");
            false
        }
    }
}

#[allow(dead_code)]
/// Poll for the second AXTextArea in messenger-chat (the thread input).
/// Returns Some if found within `max_retries` attempts.
fn wait_for_thread_input(chat_wa: &AXNode, max_retries: usize) -> Option<AXNode> {
    for attempt in 0..max_retries {
        let text_areas: Vec<AXNode> = accessibility::find_all(
            &chat_wa.0,
            &AXQuery::new().filter(|n| n.role().as_deref() == Some("AXTextArea")),
            40,
        )
        .into_iter()
        .map(AXNode::new)
        .collect();

        if text_areas.len() >= 2 {
            // The thread input is the one with "Reply to thread" placeholder
            let thread_ta = text_areas.into_iter().find(|ta| {
                ta.value()
                    .map(|v| v.contains("Reply to thread"))
                    .unwrap_or(false)
            });
            if thread_ta.is_some() {
                return thread_ta;
            }
        }

        if attempt < max_retries - 1 {
            eprintln!("[thread] waiting for thread input... (attempt {}/{})", attempt + 1, max_retries);
            thread::sleep(Duration::from_millis(1000));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Kimi API
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct KimiRequest {
    model: String,
    messages: Vec<KimiMessage>,
    temperature: f64,
}

#[derive(Serialize, Clone)]
struct KimiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct KimiResponse {
    choices: Vec<KimiChoice>,
}

#[derive(Deserialize)]
struct KimiChoice {
    message: KimiResponseMessage,
}

#[derive(Deserialize)]
struct KimiResponseMessage {
    content: String,
}

/// Kimi reply parsed into emoji + text.
#[derive(Deserialize, Debug)]
struct AutoReply {
    emoji: String,
    reply: String,
}

fn messages_to_kimi(msgs: &[ChatMessage], system_prompt: &str) -> Vec<KimiMessage> {
    let mut result = vec![KimiMessage {
        role: "system".to_string(),
        content: system_prompt.to_string(),
    }];

    for msg in msgs {
        if msg.body.is_empty() {
            continue;
        }
        let role = if msg.is_self { "assistant" } else { "user" };
        let content = if msg.sender.is_empty() {
            msg.body.clone()
        } else {
            format!("{}: {}", msg.sender, msg.body)
        };
        result.push(KimiMessage {
            role: role.to_string(),
            content,
        });
    }

    result
}

fn call_kimi(messages: Vec<KimiMessage>, verbose: bool) -> Result<String, String> {
    let api_key = std::env::var("KIMI_API_KEY")
        .map_err(|_| "KIMI_API_KEY environment variable not set".to_string())?;

    let request = KimiRequest {
        model: "kimi-k2-0905-preview".to_string(),
        messages,
        temperature: 0.7,
    };

    if verbose {
        eprintln!(
            "[kimi] request: {}",
            serde_json::to_string_pretty(&request).unwrap_or_default()
        );
    }

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.moonshot.cn/v1/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&request)
        .send()
        .map_err(|e| format!("HTTP error: {e}"))?;

    let status = resp.status();
    let body = resp.text().map_err(|e| format!("Read body error: {e}"))?;

    if !status.is_success() {
        return Err(format!("API error {status}: {body}"));
    }

    if verbose {
        eprintln!("[kimi] response: {}", trunc(&body, 500));
    }

    let parsed: KimiResponse =
        serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

    parsed
        .choices
        .first()
        .map(|c| c.message.content.trim().to_string())
        .ok_or_else(|| "No choices in response".to_string())
}

/// Parse Kimi's JSON response into AutoReply. Handles optional markdown fences.
fn parse_reply(raw: &str) -> Result<AutoReply, String> {
    // Strip markdown code fences if present
    let json_str = raw
        .trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw.trim());
    let json_str = json_str
        .strip_suffix("```")
        .unwrap_or(json_str)
        .trim();

    serde_json::from_str::<AutoReply>(json_str)
        .map_err(|e| format!("Failed to parse reply JSON: {e}\nRaw: {raw}"))
}

// ---------------------------------------------------------------------------
// Send reply
// ---------------------------------------------------------------------------

fn send_text(
    chat_wa: &AXNode,
    lark_app: &NSRunningApplication,
    text: &str,
) {
    // Activate Lark
    #[allow(deprecated)]
    lark_app.activateWithOptions(
        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    );
    thread::sleep(Duration::from_millis(300));

    // Focus input
    if let Some(input) = find_input(chat_wa) {
        let focused = input.set_focused(true);
        if !focused {
            if let (Some(pos), Some(sz)) = (input.position(), input.size()) {
                click_at(pos.0 + sz.0 / 2.0, pos.1 + sz.1 / 2.0);
            }
        }
        thread::sleep(Duration::from_millis(200));

        type_text(text);
        thread::sleep(Duration::from_millis(100));

        press_return();
        thread::sleep(Duration::from_millis(100));
    } else {
        eprintln!("[error] Input field not found, cannot send");
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn read_messages(chat_wa: &AXNode) -> Vec<ChatMessage> {
    let q = &AXQuery::new().dom_class("message-item");
    let items: Vec<AXNode> = accessibility::find_all(&chat_wa.0, q, 30)
        .into_iter()
        .map(AXNode::new)
        .collect();

    let mut messages = Vec::new();
    let mut last_sender = String::new();
    for item in &items {
        if let Some(mut msg) = parse_card(item) {
            if msg.sender.is_empty() {
                msg.sender = last_sender.clone();
            } else {
                last_sender = msg.sender.clone();
            }
            messages.push(msg);
        }
    }
    messages
}

fn main() {
    let args = Args::parse();

    // Init tracing
    if args.verbose {
        tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_target(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_target(false)
            .init();
    }

    if !accessibility::is_trusted() {
        eprintln!("Accessibility not granted.");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("main thread");
    let (pid, lark_app) = match find_lark_app(mtm) {
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
            std::process::exit(1);
        }
    };
    eprintln!("Window: {:?}", win.title());

    let chat_wa = win.select(&[AXQuery::new()
        .role("AXWebArea")
        .title_contains("messenger-chat")]);

    let chat_wa = match chat_wa {
        Some(wa) => {
            eprintln!("Found messenger-chat");
            wa
        }
        None => {
            eprintln!("messenger-chat not found.");
            std::process::exit(1);
        }
    };

    // Extract chat target from input placeholder
    if let Some(input) = find_input(&chat_wa) {
        if let Some(ph) = accessibility::attr_string(&input.0, "AXPlaceholderValue") {
            eprintln!("Chat: {}", ph);
        }
    }

    let system_prompt = build_system_prompt(args.system.as_deref());

    eprintln!(
        "Starting auto-reply loop (interval={}s, dry_run={}, emojis={})",
        args.interval,
        args.dry_run,
        SUPPORTED_EMOJIS.len()
    );

    let mut last_replied_body = String::new();

    loop {
        let messages = read_messages(&chat_wa);

        if let Some(last) = messages.last() {
            if args.verbose {
                eprintln!(
                    "[poll] last msg: is_self={}, sender={:?}, body={:?}",
                    last.is_self,
                    trunc(&last.sender, 20),
                    trunc(&last.body, 50)
                );
            }

            if !last.is_self && !last.body.is_empty() && last.body != last_replied_body {
                eprintln!("[trigger] New message from {}: {:?}", last.sender, trunc(&last.body, 50));

                let kimi_msgs = messages_to_kimi(&messages, &system_prompt);

                match call_kimi(kimi_msgs, args.verbose) {
                    Ok(raw) => {
                        eprintln!("[kimi] raw: {:?}", trunc(&raw, 200));

                        match parse_reply(&raw) {
                            Ok(reply) => {
                                eprintln!("[kimi] emoji={:?}, reply={:?}", reply.emoji, trunc(&reply.reply, 80));

                                if args.dry_run {
                                    println!("[DRY RUN] emoji={}, reply={}", reply.emoji, reply.reply);
                                } else {
                                    // Activate Lark
                                    #[allow(deprecated)]
                                    lark_app.activateWithOptions(
                                        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
                                    );
                                    thread::sleep(Duration::from_millis(300));

                                    // Step 1: react with emoji
                                    if SUPPORTED_EMOJIS.contains(&reply.emoji.as_str()) {
                                        if let Some(target_item) = find_last_other_item(&chat_wa) {
                                            if react_emoji(&win, &target_item, &reply.emoji) {
                                                eprintln!("[react] {} done", reply.emoji);
                                            } else {
                                                eprintln!("[react] {} failed", reply.emoji);
                                            }
                                        }
                                        thread::sleep(Duration::from_millis(500));
                                    } else {
                                        eprintln!("[react] unknown emoji {:?}, skipping", reply.emoji);
                                    }

                                    // Step 2: reply to the message (quoted reply)
                                    let text = reply.reply.replace('\n', " ");
                                    if let Some(target_item) = find_last_other_item(&chat_wa) {
                                        if reply_to_message(&target_item, &text) {
                                            eprintln!("[reply] {}", trunc(&text, 50));
                                        } else {
                                            eprintln!("[reply] failed, falling back to direct send");
                                            send_text(&chat_wa, &lark_app, &text);
                                        }
                                    } else {
                                        send_text(&chat_wa, &lark_app, &text);
                                        eprintln!("[sent] {}", trunc(&text, 50));
                                    }
                                }

                                last_replied_body = last.body.clone();
                            }
                            Err(e) => {
                                eprintln!("[error] {e}");
                                // Fallback: treat raw as plain text, reply to message
                                let text = raw.replace('\n', " ");
                                if args.dry_run {
                                    println!("[DRY RUN] (fallback) reply={}", text);
                                } else {
                                    #[allow(deprecated)]
                                    lark_app.activateWithOptions(
                                        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
                                    );
                                    thread::sleep(Duration::from_millis(300));
                                    if let Some(target_item) = find_last_other_item(&chat_wa) {
                                        if reply_to_message(&target_item, &text) {
                                            eprintln!("[reply] (fallback) {}", trunc(&text, 50));
                                        } else {
                                            send_text(&chat_wa, &lark_app, &text);
                                        }
                                    } else {
                                        send_text(&chat_wa, &lark_app, &text);
                                    }
                                }
                                last_replied_body = last.body.clone();
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[error] Kimi API: {e}");
                    }
                }
            }
        } else if args.verbose {
            eprintln!("[poll] no messages found");
        }

        thread::sleep(Duration::from_secs(args.interval));
    }
}
