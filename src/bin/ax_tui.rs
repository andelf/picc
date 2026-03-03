//! ax_tui — Interactive TUI browser for macOS Accessibility tree.
//!
//! Usage:
//!   ax_tui --app Lark
//!   ax_tui --app Chrome
//!   ax_tui --pid 12345

use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use picc::accessibility::{self, AXNode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, Wrap,
};
use ratatui::{Frame, Terminal};
use tui_tree_widget::{Tree, TreeItem, TreeState};

use objc2::MainThreadMarker;

#[derive(Parser)]
#[command(
    name = "ax_tui",
    about = "Interactive TUI browser for macOS Accessibility tree"
)]
struct Args {
    /// Application name (partial match)
    #[arg(long)]
    app: Option<String>,

    /// Process ID
    #[arg(long)]
    pid: Option<i32>,

    /// Preload depth (default: 3)
    #[arg(long, default_value_t = 3)]
    preload: usize,
}

/// We use Vec<usize> as the tree identifier path from root.
type Id = Vec<usize>;

/// Metadata stored for each node in the tree.
struct NodeInfo {
    ax_node: AXNode,
    label: String,
    path: Vec<(String, usize)>,
    children_loaded: bool,
    child_count: usize,
}

/// Wrapper to send AXUIElement across threads.
/// Safety: AXUIElement is a CFType (Mach port-based IPC), thread-safe for AX API calls.
struct SendableAx(objc2_core_foundation::CFRetained<objc2_application_services::AXUIElement>);
unsafe impl Send for SendableAx {}

/// Input mode for the bottom bar.
#[derive(PartialEq, Eq)]
enum InputMode {
    Normal,
    Search,
    Locator,
    Action,
}

/// Request returned from action mode to be executed in the main loop.
enum ActionRequest {
    /// Activate target app, move mouse to element center.
    MoveTo { x: f64, y: f64 },
    /// Activate target app, click at element center.
    Click { x: f64, y: f64 },
}

struct App {
    /// The AX application root element.
    app_root: AXNode,
    /// Target application PID.
    pid: i32,
    /// Rebuilt from `nodes` each time the tree structure changes.
    tree_items: Vec<TreeItem<'static, usize>>,
    tree_state: TreeState<usize>,
    /// id path → node info. Root's children start with [0], [1], etc.
    nodes: HashMap<Id, NodeInfo>,
    attrs_state: ListState,
    input_mode: InputMode,
    search_query: String,
    locator_query: String,
    status_msg: String,
    /// Cached locator results: id → locator string.
    locator_cache: HashMap<Id, String>,
    /// Send locator computation requests to background thread.
    locator_tx: mpsc::Sender<(Id, SendableAx, SendableAx)>,
    /// Receive completed locator results.
    locator_rx: mpsc::Receiver<(Id, String)>,
    /// The id currently being computed (to avoid duplicate requests).
    locator_pending: Option<Id>,
    /// Preload depth for tree refresh.
    preload_depth: usize,
    /// Action mode: list of action names.
    action_items: Vec<String>,
    /// Action mode: currently selected index.
    action_selected: usize,
}

impl App {
    fn new(root: AXNode, pid: i32, preload_depth: usize) -> Self {
        let mut nodes = HashMap::new();
        populate_nodes(&root, &[], &[], 0, preload_depth, &mut nodes);

        let tree_items = rebuild_tree_items(&nodes, &[]);

        let mut state = TreeState::default();
        if !tree_items.is_empty() {
            state.select(vec![0]);
        }

        let (req_tx, req_rx) = mpsc::channel::<(Id, SendableAx, SendableAx)>();
        let (res_tx, res_rx) = mpsc::channel::<(Id, String)>();

        // Background thread for locator computation
        std::thread::spawn(move || {
            while let Ok((id, root, target)) = req_rx.recv() {
                let locator = accessibility::generate_locator(&root.0, &target.0);
                let _ = res_tx.send((id, locator));
            }
        });

        App {
            app_root: root,
            tree_items,
            tree_state: state,
            nodes,
            attrs_state: ListState::default(),
            input_mode: InputMode::Normal,
            search_query: String::new(),
            locator_query: String::new(),
            status_msg: String::new(),
            locator_cache: HashMap::new(),
            locator_tx: req_tx,
            locator_rx: res_rx,
            locator_pending: None,
            preload_depth,
            action_items: Vec::new(),
            action_selected: 0,
            pid,
        }
    }

    fn selected_id(&self) -> Vec<usize> {
        self.tree_state.selected().to_vec()
    }

    fn selected_node_info(&self) -> Option<&NodeInfo> {
        let id = self.selected_id();
        if id.is_empty() {
            return None;
        }
        self.nodes.get(&id)
    }

    fn selected_attrs(&self) -> Vec<String> {
        let Some(info) = self.selected_node_info() else {
            return vec![];
        };
        let node = &info.ax_node;
        let names = node.attr_names();
        let mut result = Vec::with_capacity(names.len() + 1);
        for name in &names {
            let val = format_attr_value(node, name);
            result.push(format!("{name}: {val}"));
        }
        let actions = node.actions();
        if !actions.is_empty() {
            result.push(format!("AXActions: {actions:?}"));
        }
        result
    }

    fn selected_path_string(&self) -> String {
        let Some(info) = self.selected_node_info() else {
            return String::new();
        };
        if info.path.is_empty() {
            return info.label.clone();
        }
        info.path
            .iter()
            .map(|(role, idx)| format!("{role}[{idx}]"))
            .collect::<Vec<_>>()
            .join(" > ")
    }

    /// Ensure children are loaded for the currently selected node.
    /// If the selected node is a "..." placeholder, load its parent's children instead.
    /// Returns true if tree was rebuilt.
    fn ensure_children_loaded(&mut self) -> bool {
        let mut id = self.selected_id();
        if id.is_empty() {
            return false;
        }

        // If selected node is the "..." placeholder (usize::MAX), load parent instead
        if id.last() == Some(&usize::MAX) {
            id.pop();
            if id.is_empty() {
                return false;
            }
        }

        let needs_load = self
            .nodes
            .get(&id)
            .map(|info| !info.children_loaded && info.child_count > 0)
            .unwrap_or(false);

        if needs_load {
            self.load_children_at(&id);
            self.tree_items = rebuild_tree_items(&self.nodes, &[]);
            return true;
        }
        false
    }

    /// Load children for the node at `id`.
    fn load_children_at(&mut self, id: &[usize]) {
        let Some(info) = self.nodes.get(id) else {
            return;
        };

        let children = info.ax_node.children();
        let parent_path = info.path.clone();

        for (i, child) in children.iter().enumerate() {
            let mut child_id = id.to_vec();
            child_id.push(i);

            // Skip if already loaded
            if self.nodes.contains_key(&child_id) {
                continue;
            }

            let label = node_label(child);
            let child_count = child.child_count();
            let role = child.role().unwrap_or_else(|| "?".to_string());
            let mut child_path = parent_path.clone();
            child_path.push((role, i));

            self.nodes.insert(
                child_id,
                NodeInfo {
                    ax_node: AXNode::new(child.0.clone()),
                    label,
                    path: child_path,
                    children_loaded: false,
                    child_count,
                },
            );
        }

        if let Some(info) = self.nodes.get_mut(id) {
            info.children_loaded = true;
        }
    }

    /// Refresh the entire tree and try to relocate to the previously selected node.
    fn refresh_tree(&mut self) {
        // Save the AXUIElement of the currently selected node for relocation
        let prev_element = self
            .selected_node_info()
            .map(|info| info.ax_node.0.clone());

        // Rebuild everything from scratch
        self.nodes.clear();
        self.locator_cache.clear();
        self.locator_pending = None;
        populate_nodes(&self.app_root, &[], &[], 0, self.preload_depth, &mut self.nodes);
        self.tree_items = rebuild_tree_items(&self.nodes, &[]);

        // Try to relocate to the previous node
        let relocated = prev_element
            .as_ref()
            .and_then(|el| self.ensure_element_in_tree(el));

        if let Some(id) = relocated {
            for depth in 1..id.len() {
                let ancestor = id[..depth].to_vec();
                self.tree_state.open(ancestor);
            }
            self.tree_state.select(id);
            self.tree_items = rebuild_tree_items(&self.nodes, &[]);
            self.status_msg = "Refreshed (relocated)".to_string();
        } else {
            self.tree_state = TreeState::default();
            if !self.tree_items.is_empty() {
                self.tree_state.select(vec![0]);
            }
            self.status_msg = "Refreshed (root)".to_string();
        }
    }

    /// Get the cached locator for the current selection, or "..." if still computing.
    fn selected_locator(&self) -> String {
        let id = self.selected_id();
        if id.is_empty() {
            return String::new();
        }
        self.locator_cache.get(&id).cloned().unwrap_or_else(|| "...".to_string())
    }

    /// Request locator computation for the current selection if not cached.
    fn request_locator(&mut self) {
        let id = self.selected_id();
        if id.is_empty() {
            return;
        }
        if self.locator_cache.contains_key(&id) {
            return;
        }
        if self.locator_pending.as_ref() == Some(&id) {
            return;
        }
        let Some(info) = self.nodes.get(&id) else {
            return;
        };
        // Walk up to find the app root
        let mut current = info.ax_node.0.clone();
        loop {
            match accessibility::attr_value(&current, "AXParent") {
                Some(parent_val) => {
                    let parent = unsafe {
                        objc2_core_foundation::CFRetained::retain(
                            std::ptr::NonNull::new_unchecked(
                                parent_val.as_ref() as *const objc2_core_foundation::CFType
                                    as *mut objc2_application_services::AXUIElement,
                            ),
                        )
                    };
                    current = parent;
                }
                None => break,
            }
        }
        let _ = self.locator_tx.send((
            id.clone(),
            SendableAx(current),
            SendableAx(info.ax_node.0.clone()),
        ));
        self.locator_pending = Some(id);
    }

    /// Drain completed locator results from the background thread.
    /// Returns true if any new results were received (needs redraw).
    fn poll_locator_results(&mut self) -> bool {
        let mut got_any = false;
        while let Ok((id, locator)) = self.locator_rx.try_recv() {
            if self.locator_pending.as_ref() == Some(&id) {
                self.locator_pending = None;
            }
            self.locator_cache.insert(id, locator);
            got_any = true;
        }
        got_any
    }

    /// Enter action mode for the currently selected node.
    fn enter_action_mode(&mut self) {
        let mut items = vec!["Move to".into(), "Click".into(), "Focus".into(), "Text content".into()];
        if let Some(info) = self.selected_node_info() {
            for action in info.ax_node.actions() {
                items.push(action);
            }
        }
        self.action_items = items;
        self.action_selected = 0;
        self.input_mode = InputMode::Action;
    }

    /// Execute the selected action. Returns an ActionRequest for actions
    /// that need app switching, or None for in-place actions.
    fn execute_action(&mut self) -> Option<ActionRequest> {
        let action_name = match self.action_items.get(self.action_selected) {
            Some(name) => name.clone(),
            None => return None,
        };
        self.input_mode = InputMode::Normal;

        let info = match self.selected_node_info() {
            Some(info) => info,
            None => {
                self.status_msg = "No node selected".into();
                return None;
            }
        };

        match action_name.as_str() {
            "Move to" => {
                if let (Some(pos), Some(sz)) = (info.ax_node.position(), info.ax_node.size()) {
                    let cx = pos.0 + sz.0 / 2.0;
                    let cy = pos.1 + sz.1 / 2.0;
                    return Some(ActionRequest::MoveTo { x: cx, y: cy });
                }
                self.status_msg = "No position/size available".into();
                None
            }
            "Click" => {
                if let (Some(pos), Some(sz)) = (info.ax_node.position(), info.ax_node.size()) {
                    let cx = pos.0 + sz.0 / 2.0;
                    let cy = pos.1 + sz.1 / 2.0;
                    return Some(ActionRequest::Click { x: cx, y: cy });
                }
                self.status_msg = "No position/size available".into();
                None
            }
            "Focus" => {
                let ok = info.ax_node.set_focused(true);
                self.status_msg = if ok {
                    "Focused".into()
                } else {
                    "Focus failed".into()
                };
                None
            }
            "Text content" => {
                let text = info.ax_node.text(15);
                if text.is_empty() {
                    self.status_msg = "(empty)".into();
                } else {
                    self.yank_to_clipboard(&text, "text content");
                    self.status_msg = format!("Copied: {}", trunc(&text, 80));
                }
                None
            }
            ax_action => {
                let ok = info.ax_node.perform_action(ax_action);
                self.status_msg = if ok {
                    format!("{ax_action} done")
                } else {
                    format!("{ax_action} failed")
                };
                None
            }
        }
    }

    /// Copy text to clipboard via pbcopy.
    fn yank_to_clipboard(&mut self, text: &str, label: &str) {
        if text.is_empty() {
            return;
        }
        let ok = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(text.as_bytes())?;
                }
                child.wait()
            })
            .is_ok();
        self.status_msg = if ok {
            format!("{label} copied!")
        } else {
            "Copy failed".to_string()
        };
    }

    fn bottom_height(&self, width: u16) -> u16 {
        if self.input_mode == InputMode::Action {
            // action items line + help line + 2 border
            return 2 + 2;
        }
        // 2 for border top+bottom, 1 for help line, 1 for status/empty line, 1 for locator
        let inner_w = (width as usize).saturating_sub(2).max(1);
        let path = self.selected_path_string();
        let path_lines = (path.len() / inner_w + 1) as u16;
        // path lines + locator line + status line + help line + 2 border
        path_lines + 1 + 1 + 1 + 2
    }

    fn draw(&mut self, frame: &mut Frame) {
        let bh = self.bottom_height(frame.area().width);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(bh)])
            .split(frame.area());

        let main_area = chunks[0];
        let bottom_area = chunks[1];

        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(main_area);

        self.draw_tree(frame, main_chunks[0]);
        self.draw_attrs(frame, main_chunks[1]);
        self.draw_bottom(frame, bottom_area);
    }

    fn draw_tree(&mut self, frame: &mut Frame, area: Rect) {
        let tree = Tree::new(&self.tree_items)
            .expect("unique identifiers")
            .block(Block::default().borders(Borders::ALL).title(" AX Tree "))
            .experimental_scrollbar(Some(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .track_symbol(None)
                    .end_symbol(None),
            ))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(tree, area, &mut self.tree_state);
    }

    fn draw_attrs(&mut self, frame: &mut Frame, area: Rect) {
        let attrs = self.selected_attrs();
        let items: Vec<ListItem> = attrs
            .iter()
            .map(|a| {
                let (name, val) = a.split_once(": ").unwrap_or((a, ""));
                ListItem::new(Line::from(vec![
                    Span::styled(name, Style::default().fg(Color::Cyan)),
                    Span::raw(": "),
                    Span::raw(val.to_string()),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Attributes "),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));

        frame.render_stateful_widget(list, area, &mut self.attrs_state);
    }

    /// Resolve a locator query: find the element via AX API, then navigate the tree to it.
    fn resolve_locator_query(&mut self) {
        let locator = self.locator_query.trim().to_string();
        if locator.is_empty() {
            return;
        }
        let found = accessibility::resolve_locator(&self.app_root.0, &locator);
        let Some(found_el) = found else {
            self.status_msg = format!("Locator not found: {locator}");
            return;
        };
        // Walk up from found_el to app_root, collecting the path of (parent, child_index) pairs.
        // Then ensure all ancestors are loaded in our tree and select the target.
        if let Some(id) = self.ensure_element_in_tree(&found_el) {
            for depth in 1..id.len() {
                let ancestor = id[..depth].to_vec();
                self.tree_state.open(ancestor);
            }
            self.tree_state.select(id);
            self.status_msg = format!("Found: {locator}");
        } else {
            self.status_msg = format!("Locator matched but node not in tree");
        }
        self.tree_items = rebuild_tree_items(&self.nodes, &[]);
    }

    /// Ensure an AXUIElement is present in the loaded tree.
    /// Walks up via AXParent to find the path from root, loading children as needed.
    /// Returns the tree Id if successful.
    fn ensure_element_in_tree(
        &mut self,
        target: &objc2_application_services::AXUIElement,
    ) -> Option<Id> {
        use objc2_core_foundation::CFEqual;

        // First check if it's already in our tree
        for (id, info) in &self.nodes {
            if CFEqual(Some(info.ax_node.0.as_ref()), Some(target.as_ref())) {
                return Some(id.clone());
            }
        }

        // Build ancestor chain from target up to app_root
        let mut chain: Vec<objc2_core_foundation::CFRetained<objc2_application_services::AXUIElement>> = Vec::new();
        chain.push(unsafe {
            objc2_core_foundation::CFRetained::retain(
                std::ptr::NonNull::new_unchecked(
                    target as *const objc2_application_services::AXUIElement
                        as *mut objc2_application_services::AXUIElement,
                ),
            )
        });
        loop {
            let current = chain.last().unwrap();
            if CFEqual(Some(current.as_ref()), Some(self.app_root.0.as_ref())) {
                break;
            }
            match accessibility::attr_value(current, "AXParent") {
                Some(parent_val) => {
                    let parent = unsafe {
                        objc2_core_foundation::CFRetained::retain(
                            std::ptr::NonNull::new_unchecked(
                                parent_val.as_ref()
                                    as *const objc2_core_foundation::CFType
                                    as *mut objc2_application_services::AXUIElement,
                            ),
                        )
                    };
                    chain.push(parent);
                }
                None => return None, // Can't reach root
            }
            if chain.len() > 100 {
                return None; // Safety limit
            }
        }

        // chain is [target, parent, grandparent, ..., app_root]
        // Walk from root down, ensuring children are loaded at each level
        chain.reverse(); // now [app_root, ..., parent, target]

        let mut current_id: Id = vec![];
        // Skip app_root (chain[0]), start from chain[1]
        for ancestor_el in &chain[1..] {
            // Ensure children are loaded at current_id level
            if current_id.is_empty() {
                // Root level — children should be loaded already, but ensure
                if self.nodes.keys().filter(|k| k.len() == 1).count() == 0 {
                    return None;
                }
            } else {
                let needs = self
                    .nodes
                    .get(&current_id)
                    .map(|info| !info.children_loaded && info.child_count > 0)
                    .unwrap_or(false);
                if needs {
                    self.load_children_at(&current_id);
                }
            }

            // Find which child index matches ancestor_el
            let child_ids: Vec<Id> = self
                .nodes
                .keys()
                .filter(|k| {
                    k.len() == current_id.len() + 1
                        && (current_id.is_empty() || k.starts_with(&current_id))
                })
                .cloned()
                .collect();

            let mut found = false;
            for child_id in &child_ids {
                if let Some(info) = self.nodes.get(child_id) {
                    if CFEqual(Some(info.ax_node.0.as_ref()), Some(ancestor_el.as_ref())) {
                        current_id = child_id.clone();
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                return None;
            }
        }

        Some(current_id)
    }

    fn draw_bottom(&self, frame: &mut Frame, area: Rect) {
        let path = self.selected_path_string();

        let text = match self.input_mode {
            InputMode::Search => vec![
                Line::from(vec![
                    Span::styled("Search: ", Style::default().fg(Color::Yellow)),
                    Span::raw(&self.search_query),
                    Span::styled("_", Style::default().fg(Color::Gray)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    " [Enter] confirm  [Esc] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
            InputMode::Locator => vec![
                Line::from(vec![
                    Span::styled("Locator: ", Style::default().fg(Color::Magenta)),
                    Span::raw(&self.locator_query),
                    Span::styled("_", Style::default().fg(Color::Gray)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    " [Enter] resolve  [Esc] cancel  e.g. #id, AXButton[title=\"Send\"], AXGroup:nth(2)",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
            InputMode::Action => {
                let mut spans: Vec<Span> = Vec::new();
                for (i, item) in self.action_items.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::raw("   "));
                    }
                    if i == self.action_selected {
                        spans.push(Span::styled(
                            format!("▸ {item}"),
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::LightGreen)
                                .add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        spans.push(Span::styled(item.as_str(), Style::default().fg(Color::White)));
                    }
                }
                vec![
                    Line::from(spans),
                    Line::from(Span::styled(
                        " [j/k ←→] select  [Enter] execute  [Esc] cancel",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]
            }
            InputMode::Normal => {
                let locator = self.selected_locator();
                let mut lines = vec![
                    Line::from(Span::styled(
                        path,
                        Style::default().fg(Color::White),
                    )),
                    Line::from(Span::styled(
                        locator,
                        Style::default().fg(Color::Cyan),
                    )),
                ];
                if !self.status_msg.is_empty() {
                    lines.push(Line::from(Span::styled(
                        &self.status_msg,
                        Style::default().fg(Color::Green),
                    )));
                } else {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    " [j/k] move  [h/l] fold  [Enter] action  [r]efresh  [y]ank  [Y] path  [/]search  [L]ocator  [q]uit",
                    Style::default().fg(Color::DarkGray),
                )));
                lines
            }
        };

        let title = if self.input_mode == InputMode::Action {
            " Actions "
        } else {
            " Path "
        };
        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }
}

// ---------------------------------------------------------------------------
// Tree building
// ---------------------------------------------------------------------------

fn node_label(node: &AXNode) -> String {
    let role = node.role().unwrap_or_else(|| "?".to_string());
    let mut label = role;

    if let Some(t) = node.title() {
        if !t.is_empty() {
            label.push_str(&format!(" {:?}", trunc(&t, 40)));
        }
    }

    if let Some(v) = node.value() {
        let v = v.replace('\u{200b}', "");
        if !v.is_empty() {
            label.push_str(&format!(" val={:?}", trunc(&v, 40)));
        }
    }

    if let Some(d) = node.description() {
        if !d.is_empty() {
            label.push_str(&format!(" desc={:?}", trunc(&d, 30)));
        }
    }

    if let Some(dom_id) = accessibility::attr_string(&node.0, "AXDOMIdentifier") {
        if !dom_id.is_empty() {
            label.push_str(&format!(" #{dom_id}"));
        }
    }

    if let Some((w, h)) = node.size() {
        match (w == 0.0, h == 0.0) {
            (true, true) => label.push_str(" [0x0]"),
            (true, false) => label.push_str(" [0xH]"),
            (false, true) => label.push_str(" [Wx0]"),
            _ => {}
        }
    }

    label
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}...")
    }
}

fn format_attr_value(node: &AXNode, name: &str) -> String {
    match name {
        "AXPosition" => node
            .position()
            .map(|(x, y)| format!("({x:.0}, {y:.0})"))
            .unwrap_or_else(|| "<none>".to_string()),
        "AXSize" => node
            .size()
            .map(|(w, h)| format!("({w:.0} x {h:.0})"))
            .unwrap_or_else(|| "<none>".to_string()),
        "AXChildren" => format!("{} children", node.child_count()),
        "AXDOMClassList" => {
            let classes = node.dom_classes();
            if classes.is_empty() {
                "<none>".to_string()
            } else {
                format!("{classes:?}")
            }
        }
        _ => {
            if let Some(s) = accessibility::attr_string(&node.0, name) {
                let s = s.replace('\u{200b}', "");
                if s.len() > 200 {
                    format!("{:?}", trunc(&s, 200))
                } else {
                    format!("{s:?}")
                }
            } else if accessibility::attr_value(&node.0, name).is_some() {
                "<non-string value>".to_string()
            } else {
                "<none>".to_string()
            }
        }
    }
}

/// Recursively populate the nodes HashMap by preloading `max_depth` levels.
fn populate_nodes(
    parent_ax: &AXNode,
    parent_id: &[usize],
    parent_path: &[(String, usize)],
    depth: usize,
    max_depth: usize,
    nodes: &mut HashMap<Id, NodeInfo>,
) {
    let children = parent_ax.children();

    for (i, child) in children.iter().enumerate() {
        let mut child_id = parent_id.to_vec();
        child_id.push(i);

        let label = node_label(child);
        let child_count = child.child_count();
        let role = child.role().unwrap_or_else(|| "?".to_string());
        let mut child_path = parent_path.to_vec();
        child_path.push((role, i));

        let will_load_children = depth < max_depth && child_count > 0;

        nodes.insert(
            child_id.clone(),
            NodeInfo {
                ax_node: AXNode::new(child.0.clone()),
                label,
                path: child_path.clone(),
                children_loaded: will_load_children || child_count == 0,
                child_count,
            },
        );

        if will_load_children {
            populate_nodes(child, &child_id, &child_path, depth + 1, max_depth, nodes);
        }
    }
}

/// Rebuild the Vec<TreeItem> from the nodes HashMap for a given parent id.
fn rebuild_tree_items(nodes: &HashMap<Id, NodeInfo>, parent_id: &[usize]) -> Vec<TreeItem<'static, usize>> {
    // Find direct children: keys that are parent_id ++ [i]
    let mut child_indices: Vec<usize> = nodes
        .keys()
        .filter_map(|k| {
            if k.len() == parent_id.len() + 1 && k.starts_with(parent_id) {
                Some(*k.last().unwrap())
            } else {
                None
            }
        })
        .collect();
    child_indices.sort();

    child_indices
        .into_iter()
        .map(|i| {
            let mut child_id = parent_id.to_vec();
            child_id.push(i);

            let info = nodes.get(&child_id).unwrap();
            let display = if info.child_count > 0 {
                format!("{} ({})", info.label, info.child_count)
            } else {
                info.label.clone()
            };

            if info.children_loaded && info.child_count > 0 {
                let sub_items = rebuild_tree_items(nodes, &child_id);
                TreeItem::new(i, display, sub_items)
                    .unwrap_or_else(|_| TreeItem::new_leaf(i, info.label.clone()))
            } else if info.child_count > 0 {
                // Not loaded yet — show placeholder
                TreeItem::new(
                    i,
                    display,
                    vec![TreeItem::new_leaf(usize::MAX, "...")],
                )
                .unwrap_or_else(|_| TreeItem::new_leaf(i, info.label.clone()))
            } else {
                TreeItem::new_leaf(i, display)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> io::Result<()> {
    let args = Args::parse();

    if args.app.is_none() && args.pid.is_none() {
        eprintln!("Error: specify --app <name> or --pid <pid>");
        std::process::exit(1);
    }

    if !accessibility::is_trusted() {
        eprintln!("Error: Accessibility not granted.");
        std::process::exit(1);
    }

    let mtm = MainThreadMarker::new().expect("main thread");

    let (pid, app_name) = if let Some(ref name) = args.app {
        match accessibility::find_app_by_name(mtm, name) {
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
    eprintln!("Loading AX tree (preload depth={})...", args.preload);

    let root = AXNode::app(pid);
    let mut app = App::new(root, pid, args.preload);

    // Setup terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal, &mut app);

    // Restore terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Error: {err}");
    }

    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    const DEBOUNCE: Duration = Duration::from_millis(20);

    app.request_locator();
    terminal.draw(|f| app.draw(f))?;

    let mut debounce: Option<Instant> = None;

    loop {
        let timeout = debounce.map_or(DEBOUNCE, |start| DEBOUNCE.saturating_sub(start.elapsed()));

        // Poll for completed locator results
        if app.poll_locator_results() {
            debounce.get_or_insert_with(Instant::now);
        }

        if event::poll(timeout)? {
            let result = match event::read()? {
                Event::Key(key) if !matches!(key.kind, KeyEventKind::Press) => Some(false),
                Event::Key(key) => match app.input_mode {
                    InputMode::Search => Some(handle_search_key(app, key.code)),
                    InputMode::Locator => Some(handle_locator_key(app, key.code)),
                    InputMode::Action => {
                        let (update, action_req) = handle_action_key(app, key.code);
                        if let Some(req) = action_req {
                            execute_external_action(terminal, app.pid, req)?;
                        }
                        Some(update)
                    }
                    InputMode::Normal => handle_normal_key(app, key.code, key.modifiers),
                },
                Event::Resize(_, _) => Some(true),
                _ => Some(false),
            };
            let Some(update) = result else {
                return Ok(());
            };

            if update {
                app.request_locator();
                debounce.get_or_insert_with(Instant::now);
            }
        }

        if debounce.is_some_and(|d| d.elapsed() > DEBOUNCE) {
            terminal.draw(|f| app.draw(f))?;
            debounce = None;
        }
    }
}

/// Returns None to signal quit, Some(bool) for whether to redraw.
fn handle_normal_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Option<bool> {
    // Clear status on any key
    app.status_msg.clear();

    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            return None;
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            return None;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let changed = app.tree_state.key_down();
            app.ensure_children_loaded();
            Some(changed)
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let changed = app.tree_state.key_up();
            app.ensure_children_loaded();
            Some(changed)
        }
        KeyCode::Char('l') | KeyCode::Right => {
            app.ensure_children_loaded();
            Some(app.tree_state.key_right())
        }
        KeyCode::Enter => {
            app.enter_action_mode();
            Some(true)
        }
        KeyCode::Char('h') | KeyCode::Left => {
            let prev = app.tree_state.selected().to_vec();
            let changed = app.tree_state.key_left();
            if app.tree_state.selected().is_empty() && !prev.is_empty() {
                app.tree_state.select(prev);
                return Some(false);
            }
            Some(changed)
        }
        KeyCode::Char(' ') => {
            app.ensure_children_loaded();
            Some(app.tree_state.toggle_selected())
        }
        KeyCode::Char('r') => {
            app.refresh_tree();
            Some(true)
        }
        KeyCode::Char('y') => {
            let locator = app.selected_locator();
            if locator == "..." {
                app.status_msg = "Locator computing...".to_string();
            } else {
                app.yank_to_clipboard(&locator, "Locator");
            }
            Some(true)
        }
        KeyCode::Char('Y') => {
            let path = app.selected_path_string();
            app.yank_to_clipboard(&path, "Path");
            Some(true)
        }
        KeyCode::Char('n') => {
            if !app.search_query.is_empty() {
                search_next(app);
            }
            Some(true)
        }
        KeyCode::Char('N') => {
            if !app.search_query.is_empty() {
                search_prev(app);
            }
            Some(true)
        }
        KeyCode::Char('/') => {
            app.input_mode = InputMode::Search;
            app.search_query.clear();
            Some(true)
        }
        KeyCode::Char('L') => {
            app.input_mode = InputMode::Locator;
            app.locator_query = app.selected_locator();
            if app.locator_query == "..." {
                app.locator_query.clear();
            }
            Some(true)
        }
        KeyCode::Home => Some(app.tree_state.select_first()),
        KeyCode::End => Some(app.tree_state.select_last()),
        KeyCode::PageDown => Some(app.tree_state.scroll_down(10)),
        KeyCode::PageUp => Some(app.tree_state.scroll_up(10)),
        _ => Some(false),
    }
}

fn handle_search_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            true
        }
        KeyCode::Enter => {
            app.input_mode = InputMode::Normal;
            if !app.search_query.is_empty() {
                search_next(app);
            }
            true
        }
        KeyCode::Backspace => {
            app.search_query.pop();
            true
        }
        KeyCode::Char(c) => {
            app.search_query.push(c);
            true
        }
        _ => false,
    }
}

fn handle_locator_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            true
        }
        KeyCode::Enter => {
            app.input_mode = InputMode::Normal;
            app.resolve_locator_query();
            true
        }
        KeyCode::Backspace => {
            app.locator_query.pop();
            true
        }
        KeyCode::Char(c) => {
            app.locator_query.push(c);
            true
        }
        _ => false,
    }
}

/// Handle key events in Action mode.
/// Returns (needs_redraw, optional ActionRequest for external execution).
fn handle_action_key(app: &mut App, code: KeyCode) -> (bool, Option<ActionRequest>) {
    match code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            (true, None)
        }
        KeyCode::Char('j') | KeyCode::Right | KeyCode::Down => {
            if !app.action_items.is_empty() {
                app.action_selected = (app.action_selected + 1) % app.action_items.len();
            }
            (true, None)
        }
        KeyCode::Char('k') | KeyCode::Left | KeyCode::Up => {
            if !app.action_items.is_empty() {
                app.action_selected = (app.action_selected + app.action_items.len() - 1)
                    % app.action_items.len();
            }
            (true, None)
        }
        KeyCode::Enter => {
            let req = app.execute_action();
            (true, req)
        }
        _ => (false, None),
    }
}

/// Execute an action that requires switching to the target app (Move to / Click).
fn execute_external_action(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    pid: i32,
    req: ActionRequest,
) -> io::Result<()> {
    // Leave alternate screen
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;

    // Activate target app
    picc::input::activate_app(pid);
    std::thread::sleep(Duration::from_millis(200));

    let (x, y) = match &req {
        ActionRequest::MoveTo { x, y } | ActionRequest::Click { x, y } => (*x, *y),
    };

    // Move mouse
    picc::input::mouse_move(x, y);

    // Click if requested
    if matches!(req, ActionRequest::Click { .. }) {
        std::thread::sleep(Duration::from_millis(50));
        picc::input::mouse_click(x, y);
    }

    // Let user see the effect
    std::thread::sleep(Duration::from_secs(1));

    // Restore alternate screen
    terminal::enable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;

    Ok(())
}

/// Build a search haystack from a node, querying multiple AX attributes live.
/// Searches: AXRole, AXTitle, AXDescription, AXValue, AXRoleDescription,
///           AXDOMClassList, AXDOMIdentifier, and the pre-built label.
fn build_search_haystack(node: &AXNode, label: &str) -> String {
    let mut haystack = label.to_lowercase();

    for attr in &[
        "AXTitle",
        "AXDescription",
        "AXValue",
        "AXRoleDescription",
        "AXDOMIdentifier",
    ] {
        if let Some(s) = accessibility::attr_string(&node.0, attr) {
            if !s.is_empty() {
                haystack.push('\n');
                haystack.push_str(&s.to_lowercase());
            }
        }
    }

    let classes = node.dom_classes();
    if !classes.is_empty() {
        haystack.push('\n');
        haystack.push_str(&classes.join(" ").to_lowercase());
    }

    haystack
}

/// Search for the next node matching the query across multiple AX attributes.
fn search_next(app: &mut App) {
    let query = app.search_query.to_lowercase();
    let current = app.selected_id();

    let mut all_ids: Vec<Id> = app.nodes.keys().cloned().collect();
    all_ids.sort();

    let start = all_ids
        .iter()
        .position(|id| *id == current)
        .map(|p| p + 1)
        .unwrap_or(0);

    let len = all_ids.len();
    for offset in 0..len {
        let idx = (start + offset) % len;
        let id = &all_ids[idx];
        if let Some(info) = app.nodes.get(id) {
            let haystack = build_search_haystack(&info.ax_node, &info.label);
            if haystack.contains(&query) {
                for depth in 1..id.len() {
                    let ancestor = id[..depth].to_vec();
                    app.tree_state.open(ancestor);
                }
                app.tree_state.select(id.clone());
                app.status_msg = format!("Found: {}", trunc(&info.label, 40));
                return;
            }
        }
    }
    app.status_msg = "Not found".to_string();
}

/// Search for the previous node matching the query.
fn search_prev(app: &mut App) {
    let query = app.search_query.to_lowercase();
    let current = app.selected_id();

    let mut all_ids: Vec<Id> = app.nodes.keys().cloned().collect();
    all_ids.sort();

    let cur_pos = all_ids
        .iter()
        .position(|id| *id == current)
        .unwrap_or(0);

    let len = all_ids.len();
    for offset in 1..=len {
        let idx = (cur_pos + len - offset) % len;
        let id = &all_ids[idx];
        if let Some(info) = app.nodes.get(id) {
            let haystack = build_search_haystack(&info.ax_node, &info.label);
            if haystack.contains(&query) {
                for depth in 1..id.len() {
                    let ancestor = id[..depth].to_vec();
                    app.tree_state.open(ancestor);
                }
                app.tree_state.select(id.clone());
                app.status_msg = format!("Found: {}", trunc(&info.label, 40));
                return;
            }
        }
    }
    app.status_msg = "Not found".to_string();
}
