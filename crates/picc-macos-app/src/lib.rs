//! Small shared helpers for macOS accessory apps and status bar items.

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::NSString;

pub fn configure_accessory_app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app
}

pub fn new_status_item(length: f64) -> Retained<NSStatusItem> {
    NSStatusBar::systemStatusBar().statusItemWithLength(length)
}

pub fn set_status_item_symbol(
    item: &NSStatusItem,
    mtm: MainThreadMarker,
    symbol_name: &str,
    accessibility_label: &str,
) {
    if let Some(button) = item.button(mtm) {
        set_status_button_symbol(&button, symbol_name, accessibility_label);
    }
}

pub fn set_status_button_symbol(
    button: &objc2_app_kit::NSStatusBarButton,
    symbol_name: &str,
    accessibility_label: &str,
) {
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol_name),
        Some(&NSString::from_str(accessibility_label)),
    ) {
        image.setTemplate(true);
        button.setImage(Some(&image));
    }
}

pub fn new_menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}
