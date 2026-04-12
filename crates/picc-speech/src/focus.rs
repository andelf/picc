use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2_application_services::{AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFRetained, CFString, CFType};

#[derive(Debug, Clone)]
pub struct FocusedText {
    pub element: CFRetained<AXUIElement>,
    pub text: String,
}

pub const SPACE_AFTER_PUNCT: &[char] = &[',', '.', ';', ':', '!', '?', ')', ']', '}', '"', '\''];

pub fn frontmost_bundle_id() -> Option<String> {
    use objc2_app_kit::NSRunningApplication;
    let workspace_cls = objc2::runtime::AnyClass::get(c"NSWorkspace")?;
    let workspace: Retained<NSObject> = unsafe { objc2::msg_send![workspace_cls, sharedWorkspace] };
    let app: Option<Retained<NSRunningApplication>> =
        unsafe { objc2::msg_send![&*workspace, frontmostApplication] };
    app.and_then(|a| a.bundleIdentifier().map(|b| b.to_string()))
}

pub fn read_focused_text() -> Option<FocusedText> {
    let focused = focused_ax_element()?;
    let text = attr_string(&focused, "AXValue")?;
    if text.is_empty() {
        return None;
    }
    if let Some(placeholder) = attr_string(&focused, "AXPlaceholderValue") {
        if text == placeholder {
            return None;
        }
    }
    Some(FocusedText {
        element: focused,
        text,
    })
}

pub fn char_before_cursor() -> Option<char> {
    let focused = focused_ax_element()?;
    let text = attr_string(&focused, "AXValue")?;
    if text.is_empty() {
        return None;
    }
    let range_cf = attr_value(&focused, "AXSelectedTextRange")?;
    let pos = if let Some(range) = ax_value_as_cfrange(&range_cf) {
        range.location as usize
    } else {
        text.encode_utf16().count()
    };
    if pos == 0 {
        return None;
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    if pos > utf16.len() {
        return text.chars().last();
    }
    let unit = utf16[pos - 1];
    if (0xDC00..=0xDFFF).contains(&unit) && pos >= 2 {
        char::decode_utf16([utf16[pos - 2], unit]).next()?.ok()
    } else {
        char::decode_utf16([unit]).next()?.ok()
    }
}

fn focused_ax_element() -> Option<CFRetained<AXUIElement>> {
    let system = unsafe { AXUIElement::new_system_wide() };
    let cf = attr_value(&system, "AXFocusedUIElement")?;
    Some(unsafe { CFRetained::cast_unchecked(cf) })
}

fn attr_value(element: &AXUIElement, attribute: &str) -> Option<CFRetained<CFType>> {
    let attr = CFString::from_str(attribute);
    let mut value_ptr: *const CFType = std::ptr::null();
    let err = unsafe {
        element.copy_attribute_value(
            &attr,
            NonNull::new(&mut value_ptr as *mut *const CFType as *mut _).unwrap(),
        )
    };
    if err.0 != 0 || value_ptr.is_null() {
        return None;
    }
    Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(value_ptr as *mut CFType)) })
}

fn attr_string(element: &AXUIElement, attribute: &str) -> Option<String> {
    let value = attr_value(element, attribute)?;
    value.downcast_ref::<CFString>().map(|s| s.to_string())
}

fn ax_value_as_cfrange(value: &CFRetained<CFType>) -> Option<objc2_core_foundation::CFRange> {
    let ax_value: &AXValue = unsafe { &*(value.as_ref() as *const CFType as *const AXValue) };
    let mut range = objc2_core_foundation::CFRange {
        location: 0,
        length: 0,
    };
    let ok = unsafe {
        ax_value.value(
            AXValueType::CFRange,
            NonNull::new_unchecked(&mut range as *mut _ as *mut std::ffi::c_void),
        )
    };
    ok.then_some(range)
}
