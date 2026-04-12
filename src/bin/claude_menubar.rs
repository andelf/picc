//! Claude Code Menubar Status Hook
//!
//! 双模式运行：
//! - `claude_menubar`       — 运行 menubar UI（常驻进程）
//! - `claude_menubar --hook` — 作为 Claude hook，读取 stdin JSON，更新状态文件

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::Command;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSStatusItem};
use objc2_foundation::{NSString, NSTimer};
use picc_macos_app::{
    configure_accessory_app, new_menu_item as shared_new_menu_item, new_status_item,
    set_status_button_symbol,
};
use serde::{Deserialize, Serialize};

// -- 常量 --

const STATE_DIR: &str = ".claude/menubar_state";
const POLL_INTERVAL: f64 = 2.0;
const ANIM_INTERVAL: f64 = 0.1;
const SESSION_TIMEOUT_SECS: u64 = 5 * 60;
const IDLE_TIMEOUT_SECS: u64 = 60;

/// 工作中 braille spinner 帧（等宽，无视觉抖动）
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// -- Session 状态 --

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionState {
    tool_uses: u32,
    needs_input: bool,
    is_working: bool,
    last_event_ts: u64,
}

// -- Hook 输入 JSON --

#[derive(Debug, Deserialize)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    hook_event_name: String,
}

// -- 聚合状态 --

#[derive(Debug, Default)]
struct AggregatedState {
    sessions: Vec<(String, SessionState)>,
    total_tools: u32,
    any_needs_input: bool,
    any_working: bool,
}

static AGGREGATED: Mutex<AggregatedState> = Mutex::new(AggregatedState {
    sessions: Vec::new(),
    total_tools: 0,
    any_needs_input: false,
    any_working: false,
});

/// 动画帧索引
static ANIM_FRAME: Mutex<usize> = Mutex::new(0);

// -- thread_local --

thread_local! {
    static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
    static DELEGATE: RefCell<Option<Retained<MenuDelegate>>> = const { RefCell::new(None) };
    static POLL_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    static ANIM_TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

// -- MenuDelegate --

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ClaudeMenuDelegate"]
    #[derive(Debug, PartialEq)]
    pub struct MenuDelegate;

    #[allow(non_snake_case)]
    impl MenuDelegate {
        #[unsafe(method(resetAll:))]
        fn reset_all(&self, _sender: &AnyObject) {
            eprintln!("[claude_menubar] Reset all session state");
            let state_dir = state_dir();
            if let Ok(entries) = fs::read_dir(&state_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "json") {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
            poll_and_update();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            remove_pid_file();
            std::process::exit(0);
        }
    }
);

// -- 辅助函数 --

fn state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(STATE_DIR)
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn new_menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    shared_new_menu_item(mtm, title, action, key)
}

fn set_button_icon(button: &objc2_app_kit::NSStatusBarButton, name: &str) {
    set_status_button_symbol(button, name, "Claude Status");
}

unsafe fn set_button_monospaced_title(button: &NSObject, title: &str) {
    let font_cls = AnyClass::get(c"NSFont").unwrap();
    // 等宽字体，避免数字和字符变化时宽度抖动
    let font: *mut AnyObject =
        msg_send![font_cls, monospacedSystemFontOfSize: 0.0_f64, weight: 0.0_f64];

    let dict_cls = AnyClass::get(c"NSMutableDictionary").unwrap();
    let dict: *mut AnyObject = msg_send![dict_cls, new];
    let font_key = NSString::from_str("NSFont");
    let _: () = msg_send![dict, setObject: font, forKey: &*font_key];

    let attr_cls = AnyClass::get(c"NSAttributedString").unwrap();
    let ns_title = NSString::from_str(title);
    let attr_str: *mut AnyObject = msg_send![attr_cls, alloc];
    let attr_str: *mut AnyObject =
        msg_send![attr_str, initWithString: &*ns_title, attributes: dict];
    let _: () = msg_send![button, setAttributedTitle: attr_str];
}

fn invalidate_timer(cell: &RefCell<Option<Retained<NSTimer>>>) {
    if let Some(timer) = cell.borrow_mut().take() {
        timer.invalidate();
    }
}

// -- 状态文件操作 --

fn read_all_sessions() -> HashMap<String, SessionState> {
    let dir = state_dir();
    let mut sessions = HashMap::new();
    let now = now_ts();

    let Ok(entries) = fs::read_dir(&dir) else {
        return sessions;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(mut state) = serde_json::from_str::<SessionState>(&data) {
                    if now.saturating_sub(state.last_event_ts) > SESSION_TIMEOUT_SECS {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        // 60s 无事件自动判定为 idle
                        if state.is_working
                            && now.saturating_sub(state.last_event_ts) > IDLE_TIMEOUT_SECS
                        {
                            state.is_working = false;
                        }
                        sessions.insert(stem.to_string(), state);
                    }
                }
            }
        }
    }

    sessions
}

fn write_session_state(session_id: &str, state: &SessionState) {
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.json", session_id));
    if let Ok(data) = serde_json::to_string(state) {
        let _ = fs::write(path, data);
    }
}

fn remove_session(session_id: &str) {
    let path = state_dir().join(format!("{}.json", session_id));
    let _ = fs::remove_file(path);
}

// -- PID 文件管理 --

fn pid_file() -> PathBuf {
    state_dir().join("daemon.pid")
}

fn write_pid_file() {
    let _ = fs::create_dir_all(state_dir());
    let _ = fs::write(pid_file(), std::process::id().to_string());
}

fn remove_pid_file() {
    let _ = fs::remove_file(pid_file());
}

/// 检查 daemon 是否在运行（通过 PID 文件 + kill -0 验证进程存活）
fn is_daemon_running() -> bool {
    let Ok(pid_str) = fs::read_to_string(pid_file()) else {
        return false;
    };
    let pid_str = pid_str.trim();
    if pid_str.is_empty() {
        return false;
    }
    // kill -0 检查进程是否存在（不发送信号，只检查）
    Command::new("kill")
        .args(["-0", pid_str])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// 以 daemon 模式启动自身
fn spawn_daemon() {
    let exe = std::env::current_exe().expect("cannot get current exe path");
    match Command::new(&exe).arg("--daemon").spawn() {
        Ok(child) => {
            eprintln!("[claude_menubar] Daemon spawned (pid: {})", child.id());
        }
        Err(e) => {
            eprintln!("[claude_menubar] Failed to spawn daemon: {}", e);
        }
    }
}

/// hook 模式下确保 daemon 在运行
fn ensure_daemon() {
    if !is_daemon_running() {
        spawn_daemon();
    }
}

// -- Hook 模式 --

fn run_hook() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap_or(0);

    // 确保 daemon 在运行
    ensure_daemon();

    let hook: HookInput = match serde_json::from_str(&input) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[claude_menubar] Failed to parse hook input: {}", e);
            return;
        }
    };

    let session_id = &hook.session_id;
    let event = hook.hook_event_name.as_str();

    match event {
        "SessionEnd" => {
            remove_session(session_id);
        }
        _ => {
            let dir = state_dir();
            let path = dir.join(format!("{}.json", session_id));
            let mut state = fs::read_to_string(&path)
                .ok()
                .and_then(|d| serde_json::from_str::<SessionState>(&d).ok())
                .unwrap_or_default();

            state.last_event_ts = now_ts();

            match event {
                "UserPromptSubmit" => {
                    state.is_working = true;
                    state.needs_input = false;
                }
                "PreToolUse" => {
                    state.tool_uses += 1;
                    state.needs_input = false;
                    state.is_working = true;
                }
                "PostToolUse" => {
                    state.needs_input = false;
                    state.is_working = true;
                }
                "Stop" => {
                    state.needs_input = false;
                    state.is_working = false;
                }
                "Notification" => {
                    state.needs_input = true;
                    state.is_working = false;
                }
                "SessionStart" => {
                    state.is_working = true;
                }
                _ => {}
            }

            write_session_state(session_id, &state);
        }
    }
}

// -- 动画控制 --

fn start_anim_timer() {
    ANIM_TIMER.with(|t| {
        if t.borrow().is_some() {
            return; // 已在运行
        }
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            anim_tick();
        });
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(ANIM_INTERVAL, true, &block)
        };
        *t.borrow_mut() = Some(timer);
    });
}

fn stop_anim_timer() {
    ANIM_TIMER.with(|t| invalidate_timer(t));
    *ANIM_FRAME.lock().unwrap() = 0;
}

fn anim_tick() {
    let frame = {
        let mut f = ANIM_FRAME.lock().unwrap();
        let current = *f;
        *f = (current + 1) % SPINNER.len();
        current
    };

    let spinner_char = SPINNER[frame];

    let (session_count, total_tools) = {
        let agg = AGGREGATED.lock().unwrap();
        (agg.sessions.len(), agg.total_tools)
    };

    let title = format!(" {} {}S{}T", spinner_char, session_count, total_tools);

    STATUS_ITEM.with(|s| {
        let Some(item) = s.borrow().as_ref().cloned() else {
            return;
        };
        let mtm = MainThreadMarker::new().unwrap();
        if let Some(button) = item.button(mtm) {
            unsafe { set_button_monospaced_title(&button, &title) };
        }
    });
}

// -- 轮询并更新聚合状态 --

fn poll_and_update() {
    let sessions = read_all_sessions();
    let total_tools: u32 = sessions.values().map(|s| s.tool_uses).sum();
    let any_needs_input = sessions.values().any(|s| s.needs_input);
    let any_working = sessions.values().any(|s| s.is_working);

    let mut sorted: Vec<(String, SessionState)> = sessions.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    {
        let mut agg = AGGREGATED.lock().unwrap();
        agg.sessions = sorted;
        agg.total_tools = total_tools;
        agg.any_needs_input = any_needs_input;
        agg.any_working = any_working;
    }

    // 控制动画定时器
    if any_working {
        start_anim_timer();
    } else {
        stop_anim_timer();
    }

    update_menubar();
}

// -- Menubar 更新 --

fn update_menubar() {
    let (session_count, total_tools, any_needs_input, any_working, sessions) = {
        let agg = AGGREGATED.lock().unwrap();
        (
            agg.sessions.len(),
            agg.total_tools,
            agg.any_needs_input,
            agg.any_working,
            agg.sessions.clone(),
        )
    };

    STATUS_ITEM.with(|s| {
        let Some(item) = s.borrow().as_ref().cloned() else {
            return;
        };
        let mtm = MainThreadMarker::new().unwrap();
        if let Some(button) = item.button(mtm) {
            if session_count == 0 {
                // Idle — 无活跃 session
                set_button_icon(&button, "brain");
                unsafe { set_button_monospaced_title(&button, " --") };
            } else if any_needs_input {
                // Needs input — 等待用户操作
                set_button_icon(&button, "exclamationmark.bubble.fill");
                unsafe {
                    set_button_monospaced_title(
                        &button,
                        &format!(" {}S{}T", session_count, total_tools),
                    )
                };
            } else if any_working {
                // Working — 图标切 brain.fill，文字由动画定时器更新 spinner
                set_button_icon(&button, "brain.fill");
            } else {
                // Idle with sessions — 等待用户输入
                set_button_icon(&button, "brain");
                unsafe {
                    set_button_monospaced_title(
                        &button,
                        &format!(" {}S{}T", session_count, total_tools),
                    )
                };
            }
        }

        // 重建菜单
        if let Some(menu) = item.menu(mtm) {
            menu.removeAllItems();

            let summary = if session_count == 0 {
                "No active sessions".to_string()
            } else {
                format!(
                    "{} session{}, {} tool uses",
                    session_count,
                    if session_count == 1 { "" } else { "s" },
                    total_tools
                )
            };
            let summary_item = new_menu_item(mtm, &summary, None, "");
            summary_item.setEnabled(false);
            menu.addItem(&summary_item);

            if !sessions.is_empty() {
                menu.addItem(&NSMenuItem::separatorItem(mtm));

                for (id, state) in &sessions {
                    let short_id = if id.len() > 8 { &id[..8] } else { id };
                    let status_indicator = if state.needs_input {
                        " \u{23f3}" // ⏳
                    } else if state.is_working {
                        " \u{26a1}" // ⚡
                    } else {
                        " \u{1f4a4}" // 💤
                    };
                    let detail = format!(
                        "{}: {} tools{}",
                        short_id, state.tool_uses, status_indicator
                    );
                    let detail_item = new_menu_item(mtm, &detail, None, "");
                    detail_item.setEnabled(false);
                    menu.addItem(&detail_item);
                }
            }

            menu.addItem(&NSMenuItem::separatorItem(mtm));

            DELEGATE.with(|d| {
                let Some(delegate) = d.borrow().as_ref().cloned() else {
                    return;
                };

                let reset_item = new_menu_item(mtm, "Reset", Some(sel!(resetAll:)), "");
                unsafe { reset_item.setTarget(Some(&delegate)) };
                menu.addItem(&reset_item);

                let quit_item = new_menu_item(mtm, "Quit", Some(sel!(quit:)), "q");
                unsafe { quit_item.setTarget(Some(&delegate)) };
                menu.addItem(&quit_item);
            });
        }
    });
}

// -- Menubar 初始化 --

fn setup_menubar(mtm: MainThreadMarker) {
    let item: Retained<NSStatusItem> = new_status_item(-1.0);

    if let Some(button) = item.button(mtm) {
        set_button_icon(&button, "brain");
        unsafe { set_button_monospaced_title(&button, " --") };
    }

    let delegate: Retained<MenuDelegate> = unsafe { msg_send![MenuDelegate::alloc(mtm), init] };

    let menu = NSMenu::new(mtm);

    let summary_item = new_menu_item(mtm, "No active sessions", None, "");
    summary_item.setEnabled(false);
    menu.addItem(&summary_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let reset_item = new_menu_item(mtm, "Reset", Some(sel!(resetAll:)), "");
    unsafe { reset_item.setTarget(Some(&delegate)) };
    menu.addItem(&reset_item);

    let quit_item = new_menu_item(mtm, "Quit", Some(sel!(quit:)), "q");
    unsafe { quit_item.setTarget(Some(&delegate)) };
    menu.addItem(&quit_item);

    item.setMenu(Some(&menu));

    STATUS_ITEM.with(|s| {
        *s.borrow_mut() = Some(item);
    });
    DELEGATE.with(|d| {
        *d.borrow_mut() = Some(delegate);
    });
}

fn start_poll_timer() {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        poll_and_update();
    });
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(POLL_INTERVAL, true, &block)
    };
    POLL_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });
}

fn print_hook_config() {
    let bin_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "claude_menubar".into());

    eprintln!("=== Claude Code Hook Configuration ===");
    eprintln!("Add the following to ~/.claude/settings.json:\n");
    eprintln!(
        r#"{{
  "hooks": {{
    "UserPromptSubmit": [
      {{
        "matcher": "",
        "hooks": [{{ "type": "command", "command": "{bin_path} --hook" }}]
      }}
    ],
    "PreToolUse": [
      {{
        "matcher": "",
        "hooks": [{{ "type": "command", "command": "{bin_path} --hook" }}]
      }}
    ],
    "PostToolUse": [
      {{
        "matcher": "",
        "hooks": [{{ "type": "command", "command": "{bin_path} --hook" }}]
      }}
    ],
    "Notification": [
      {{
        "matcher": "permission_prompt",
        "hooks": [{{ "type": "command", "command": "{bin_path} --hook" }}]
      }},
      {{
        "matcher": "idle_prompt",
        "hooks": [{{ "type": "command", "command": "{bin_path} --hook" }}]
      }}
    ],
    "Stop": [
      {{
        "matcher": "",
        "hooks": [{{ "type": "command", "command": "{bin_path} --hook" }}]
      }}
    ]
  }}
}}"#
    );
    eprintln!();
}

// -- main --

fn run_menubar() {
    // 检查是否已有 daemon 在运行
    if is_daemon_running() {
        eprintln!("[claude_menubar] Daemon is already running.");
        return;
    }

    // 确保状态目录存在
    let _ = fs::create_dir_all(state_dir());

    // 写 PID 文件
    write_pid_file();

    let mtm = MainThreadMarker::new().expect("must be on the main thread");
    let app: Retained<NSApplication> = configure_accessory_app(mtm);

    setup_menubar(mtm);
    poll_and_update();
    start_poll_timer();

    eprintln!(
        "[claude_menubar] Running (pid: {}). Polling every {:.0}s.",
        std::process::id(),
        POLL_INTERVAL
    );

    app.run();

    // 清理（正常不会到这里，app.run() 是阻塞的）
    remove_pid_file();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--hook") {
        run_hook();
        return;
    }

    if args.iter().any(|a| a == "--daemon") {
        // daemon 模式：直接启动 menubar，不打印配置
        run_menubar();
        return;
    }

    // 默认：打印配置并启动 menubar
    print_hook_config();
    run_menubar();
}
