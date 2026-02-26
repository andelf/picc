//! macOS Accessibility API wrapper with selector-like query support.
//!
//! Wraps the AXUIElement C API via `objc2-application-services` for fast,
//! direct access to the accessibility tree.

use std::ptr::NonNull;

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFArray, CFIndex, CFRetained, CFString, CFType};

pub use objc2_application_services::AXIsProcessTrusted;

// ---------------------------------------------------------------------------
// Low-level convenience functions
// ---------------------------------------------------------------------------

/// Check if the current process is a trusted accessibility client.
pub fn is_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Get a raw attribute value from an element.
pub fn attr_value(element: &AXUIElement, attribute: &str) -> Option<CFRetained<CFType>> {
    let attr = CFString::from_str(attribute);
    let mut value_ptr: *const CFType = std::ptr::null();
    let err = unsafe {
        element.copy_attribute_value(
            &attr,
            NonNull::new(&mut value_ptr as *mut *const CFType as *mut _).unwrap(),
        )
    };
    if err != AXError(0) || value_ptr.is_null() {
        return None;
    }
    // copy_attribute_value follows the Create rule (+1 retained)
    Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(value_ptr as *mut CFType)) })
}

/// Get a string attribute value.
pub fn attr_string(element: &AXUIElement, attribute: &str) -> Option<String> {
    let value = attr_value(element, attribute)?;
    value.downcast_ref::<CFString>().map(|s| s.to_string())
}

/// List all attribute names on an element.
pub fn attr_names(element: &AXUIElement) -> Vec<String> {
    let mut names_ptr: *const CFArray = std::ptr::null();
    let err = unsafe {
        element.copy_attribute_names(
            NonNull::new(&mut names_ptr as *mut *const CFArray as *mut _).unwrap(),
        )
    };
    if err != AXError(0) || names_ptr.is_null() {
        return Vec::new();
    }
    let names: CFRetained<CFArray> =
        unsafe { CFRetained::from_raw(NonNull::new_unchecked(names_ptr as *mut CFArray)) };
    let count = names.len();
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let ptr = unsafe { names.as_opaque().value_at_index(i as CFIndex) };
        if !ptr.is_null() {
            let cf_str = unsafe { &*(ptr as *const CFString) };
            result.push(cf_str.to_string());
        }
    }
    result
}

/// List available action names on an element.
pub fn action_names(element: &AXUIElement) -> Vec<String> {
    let mut names_ptr: *const CFArray = std::ptr::null();
    let err = unsafe {
        element.copy_action_names(
            NonNull::new(&mut names_ptr as *mut *const CFArray as *mut _).unwrap(),
        )
    };
    if err != AXError(0) || names_ptr.is_null() {
        return Vec::new();
    }
    let names: CFRetained<CFArray> =
        unsafe { CFRetained::from_raw(NonNull::new_unchecked(names_ptr as *mut CFArray)) };
    let count = names.len();
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let ptr = unsafe { names.as_opaque().value_at_index(i as CFIndex) };
        if !ptr.is_null() {
            let cf_str = unsafe { &*(ptr as *const CFString) };
            result.push(cf_str.to_string());
        }
    }
    result
}

/// Perform an action on an element.
pub fn perform_action(element: &AXUIElement, action: &str) -> bool {
    let action_name = CFString::from_str(action);
    let err = unsafe { element.perform_action(&action_name) };
    err == AXError(0)
}

/// Set an attribute value on an element. Returns true on success.
pub fn set_attr_value(element: &AXUIElement, attribute: &str, value: &CFType) -> bool {
    let attr = CFString::from_str(attribute);
    let err = unsafe { element.set_attribute_value(&attr, value) };
    err == AXError(0)
}

/// Get child elements.
pub fn children(element: &AXUIElement) -> Vec<CFRetained<AXUIElement>> {
    let value = match attr_value(element, "AXChildren") {
        Some(v) => v,
        None => return Vec::new(),
    };
    cf_type_as_ax_elements(&value)
}

// ---------------------------------------------------------------------------
// CF type conversion helpers (private)
// ---------------------------------------------------------------------------

fn cf_type_as_ax_elements(value: &CFType) -> Vec<CFRetained<AXUIElement>> {
    let array = match value.downcast_ref::<CFArray>() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let count = array.len();
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let ptr = unsafe { array.as_opaque().value_at_index(i as CFIndex) };
        if !ptr.is_null() {
            let el = unsafe {
                CFRetained::retain(NonNull::new_unchecked(ptr as *mut AXUIElement))
            };
            result.push(el);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// AXQuery — selector-like query builder
// ---------------------------------------------------------------------------

/// A query for matching AX elements. All set fields must match (AND logic).
#[derive(Debug, Default, Clone)]
pub struct AXQuery {
    role: Option<String>,
    title: Option<String>,
    title_contains: Option<String>,
    value_contains: Option<String>,
    subrole: Option<String>,
    min_children: Option<usize>,
    has_descendant_text: Option<String>,
    predicate: Option<fn(&AXNode) -> bool>,
}

impl AXQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn role(mut self, r: &str) -> Self {
        self.role = Some(r.to_string());
        self
    }

    pub fn title(mut self, t: &str) -> Self {
        self.title = Some(t.to_string());
        self
    }

    pub fn title_contains(mut self, t: &str) -> Self {
        self.title_contains = Some(t.to_string());
        self
    }

    pub fn value_contains(mut self, v: &str) -> Self {
        self.value_contains = Some(v.to_string());
        self
    }

    pub fn subrole(mut self, s: &str) -> Self {
        self.subrole = Some(s.to_string());
        self
    }

    /// Match only elements with at least `n` direct children.
    pub fn min_children(mut self, n: usize) -> Self {
        self.min_children = Some(n);
        self
    }

    /// Match only elements that contain the given text somewhere in their subtree.
    pub fn has_text(mut self, text: &str) -> Self {
        self.has_descendant_text = Some(text.to_string());
        self
    }

    /// Match with a custom predicate function.
    pub fn filter(mut self, f: fn(&AXNode) -> bool) -> Self {
        self.predicate = Some(f);
        self
    }

    fn matches(&self, element: &AXUIElement) -> bool {
        if let Some(ref r) = self.role {
            match attr_string(element, "AXRole") {
                Some(ref v) if v == r => {}
                _ => return false,
            }
        }
        if let Some(ref t) = self.title {
            match attr_string(element, "AXTitle") {
                Some(ref v) if v == t => {}
                _ => return false,
            }
        }
        if let Some(ref t) = self.title_contains {
            match attr_string(element, "AXTitle") {
                Some(ref v) if v.contains(t.as_str()) => {}
                _ => return false,
            }
        }
        if let Some(ref vc) = self.value_contains {
            match attr_string(element, "AXValue") {
                Some(ref v) if v.contains(vc.as_str()) => {}
                _ => return false,
            }
        }
        if let Some(ref s) = self.subrole {
            match attr_string(element, "AXSubrole") {
                Some(ref v) if v == s => {}
                _ => return false,
            }
        }
        if let Some(min) = self.min_children {
            if children(element).len() < min {
                return false;
            }
        }
        if let Some(ref text) = self.has_descendant_text {
            let node = AXNode(unsafe {
                CFRetained::retain(NonNull::new_unchecked(
                    element as *const AXUIElement as *mut AXUIElement,
                ))
            });
            let found = node.texts(15).iter().any(|t| t.contains(text.as_str()));
            if !found {
                return false;
            }
        }
        if let Some(pred) = self.predicate {
            let node = AXNode(unsafe {
                CFRetained::retain(NonNull::new_unchecked(
                    element as *const AXUIElement as *mut AXUIElement,
                ))
            });
            if !pred(&node) {
                return false;
            }
        }
        true
    }
}

/// Convenience: build a query matching a role.
pub fn role(r: &str) -> AXQuery {
    AXQuery::new().role(r)
}

/// Convenience: build a query matching a title.
pub fn title(t: &str) -> AXQuery {
    AXQuery::new().title(t)
}

// ---------------------------------------------------------------------------
// DFS search functions
// ---------------------------------------------------------------------------

/// Find the first element matching `query`, searching up to `max_depth` levels.
pub fn find_first(
    root: &AXUIElement,
    query: &AXQuery,
    max_depth: usize,
) -> Option<CFRetained<AXUIElement>> {
    if max_depth == 0 {
        return None;
    }
    for child in children(root) {
        if query.matches(&child) {
            return Some(child);
        }
        if let Some(found) = find_first(&child, query, max_depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Find all elements matching `query`, searching up to `max_depth` levels.
pub fn find_all(
    root: &AXUIElement,
    query: &AXQuery,
    max_depth: usize,
) -> Vec<CFRetained<AXUIElement>> {
    let mut results = Vec::new();
    find_all_inner(root, query, max_depth, &mut results);
    results
}

fn find_all_inner(
    root: &AXUIElement,
    query: &AXQuery,
    max_depth: usize,
    results: &mut Vec<CFRetained<AXUIElement>>,
) {
    if max_depth == 0 {
        return;
    }
    for child in children(root) {
        if query.matches(&child) {
            results.push(child.clone());
        }
        find_all_inner(&child, query, max_depth - 1, results);
    }
}

// ---------------------------------------------------------------------------
// AXNode — ergonomic wrapper
// ---------------------------------------------------------------------------

/// Lightweight wrapper around `AXUIElement` for convenient chained access.
pub struct AXNode(pub CFRetained<AXUIElement>);

impl AXNode {
    /// Create a node for an application by PID.
    pub fn app(pid: i32) -> Self {
        Self(unsafe { AXUIElement::new_application(pid) })
    }

    /// Create a node from a retained element.
    pub fn new(element: CFRetained<AXUIElement>) -> Self {
        Self(element)
    }

    pub fn role(&self) -> Option<String> {
        attr_string(&self.0, "AXRole")
    }

    pub fn title(&self) -> Option<String> {
        attr_string(&self.0, "AXTitle")
    }

    pub fn value(&self) -> Option<String> {
        attr_string(&self.0, "AXValue")
    }

    pub fn description(&self) -> Option<String> {
        attr_string(&self.0, "AXDescription")
    }

    pub fn subrole(&self) -> Option<String> {
        attr_string(&self.0, "AXSubrole")
    }

    pub fn attr_names(&self) -> Vec<String> {
        attr_names(&self.0)
    }

    pub fn children(&self) -> Vec<AXNode> {
        children(&self.0).into_iter().map(AXNode::new).collect()
    }

    /// Get the nth direct child (0-indexed).
    pub fn child(&self, index: usize) -> Option<AXNode> {
        children(&self.0).into_iter().nth(index).map(AXNode::new)
    }

    /// Number of direct children.
    pub fn child_count(&self) -> usize {
        children(&self.0).len()
    }

    /// Find the first direct child matching `query`.
    pub fn child_matching(&self, q: &AXQuery) -> Option<AXNode> {
        children(&self.0).into_iter().find(|c| q.matches(c)).map(AXNode::new)
    }

    /// Find all direct children matching `query`.
    pub fn children_matching(&self, q: &AXQuery) -> Vec<AXNode> {
        children(&self.0)
            .into_iter()
            .filter(|c| q.matches(c))
            .map(AXNode::new)
            .collect()
    }

    /// Find the first descendant matching `query` (max depth 20).
    pub fn find(&self, q: AXQuery) -> Option<AXNode> {
        find_first(&self.0, &q, 20).map(AXNode::new)
    }

    /// Find all descendants matching `query` (max depth 20).
    pub fn find_all(&self, q: AXQuery) -> Vec<AXNode> {
        find_all(&self.0, &q, 20)
            .into_iter()
            .map(AXNode::new)
            .collect()
    }

    /// Navigate a path of queries: at each step, find the first descendant
    /// matching the query (DFS), then continue from that node.
    ///
    /// Like XPath: `//step1//step2//step3`
    ///
    /// ```ignore
    /// node.select(&[
    ///     role("AXWebArea").title_contains("messenger-chat"),
    ///     AXQuery::new().min_children(5),
    /// ])
    /// ```
    pub fn select(&self, steps: &[AXQuery]) -> Option<AXNode> {
        let mut current = AXNode::new(self.0.clone());
        for step in steps {
            current = find_first(&current.0, step, 30).map(AXNode::new)?;
        }
        Some(current)
    }

    /// Navigate a path of queries using only direct children at each step.
    ///
    /// Like XPath: `/step1/step2/step3`
    pub fn select_direct(&self, steps: &[AXQuery]) -> Option<AXNode> {
        let mut current = AXNode::new(self.0.clone());
        for step in steps {
            current = current.child_matching(step)?;
        }
        Some(current)
    }

    /// Collect all AXStaticText values from descendants up to `max_depth`.
    pub fn texts(&self, max_depth: usize) -> Vec<String> {
        let elements = find_all(&self.0, &role("AXStaticText"), max_depth);
        elements
            .iter()
            .filter_map(|el| attr_string(el, "AXValue"))
            .collect()
    }

    /// Concatenate all descendant text into one string.
    pub fn text(&self, max_depth: usize) -> String {
        self.texts(max_depth).join("")
    }

    /// Check if subtree contains an element with the given role.
    pub fn has_role(&self, r: &str, max_depth: usize) -> bool {
        find_first(&self.0, &role(r), max_depth).is_some()
    }

    /// List available actions on this element.
    pub fn actions(&self) -> Vec<String> {
        action_names(&self.0)
    }

    /// Perform an action on this element (e.g. "AXPress", "AXScrollToVisible").
    pub fn perform_action(&self, action: &str) -> bool {
        perform_action(&self.0, action)
    }

    /// Get the position (x, y) of this element on screen.
    pub fn position(&self) -> Option<(f64, f64)> {
        use objc2_application_services::{AXValue, AXValueType};
        use objc2_core_foundation::CGPoint;
        let value = attr_value(&self.0, "AXPosition")?;
        let ax_val = unsafe { &*(value.as_ref() as *const CFType as *const AXValue) };
        let mut point = CGPoint { x: 0.0, y: 0.0 };
        let ok = unsafe {
            ax_val.value(
                AXValueType(1), // kAXValueCGPointType
                NonNull::new_unchecked(&mut point as *mut CGPoint as *mut _),
            )
        };
        if ok { Some((point.x, point.y)) } else { None }
    }

    /// Get the size (width, height) of this element.
    pub fn size(&self) -> Option<(f64, f64)> {
        use objc2_application_services::{AXValue, AXValueType};
        use objc2_core_foundation::CGSize;
        let value = attr_value(&self.0, "AXSize")?;
        let ax_val = unsafe { &*(value.as_ref() as *const CFType as *const AXValue) };
        let mut size = CGSize { width: 0.0, height: 0.0 };
        let ok = unsafe {
            ax_val.value(
                AXValueType(2), // kAXValueCGSizeType
                NonNull::new_unchecked(&mut size as *mut CGSize as *mut _),
            )
        };
        if ok { Some((size.width, size.height)) } else { None }
    }

    /// Set an attribute value on the underlying element.
    pub fn set_attr_value(&self, attribute: &str, value: &CFType) -> bool {
        set_attr_value(&self.0, attribute, value)
    }

    /// Set AXValue (text) on this element.
    pub fn set_value(&self, text: &str) -> bool {
        let cf_str = CFString::from_str(text);
        let cf_type: &CFType = cf_str.as_ref();
        set_attr_value(&self.0, "AXValue", cf_type)
    }

    /// Set AXFocused on this element.
    pub fn set_focused(&self, focused: bool) -> bool {
        let val: &CFType = if focused {
            unsafe { objc2_core_foundation::kCFBooleanTrue.unwrap() }.as_ref()
        } else {
            unsafe { objc2_core_foundation::kCFBooleanFalse.unwrap() }.as_ref()
        };
        set_attr_value(&self.0, "AXFocused", val)
    }
}

impl std::fmt::Debug for AXNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AXNode")
            .field("role", &self.role())
            .field("title", &self.title())
            .finish()
    }
}
