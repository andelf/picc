const SKIP_AX_READ_BUNDLES: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "io.alacritty",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "co.zeit.hyper",
    "net.kovidgoyal.kitty",
];

const CLIPBOARD_ONLY_BUNDLES: &[&str] = &[
    "com.google.Chrome",
    "org.chromium.Chromium",
    "com.apple.Safari",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "com.electron.",
    "us.zoom.xos",
    "com.larksuite.Lark",
    "com.larksuite.larkApp",
    "com.bytedance.lark.Feishu",
];

pub fn should_skip_ax_read_for_bundle(bundle: Option<&str>) -> bool {
    bundle.is_some_and(|bundle| matches_bundle_list(bundle, SKIP_AX_READ_BUNDLES))
}

pub fn should_use_clipboard_for_bundle(bundle: Option<&str>) -> bool {
    bundle.is_some_and(|bundle| matches_bundle_list(bundle, CLIPBOARD_ONLY_BUNDLES))
}

pub fn clipboard_replace_is_safe(original_text: &str, current_text: Option<&str>) -> bool {
    current_text.is_some_and(|current| current.trim() == original_text.trim())
}

fn matches_bundle_list(bundle: &str, list: &[&str]) -> bool {
    list.iter().any(|entry| {
        if entry.ends_with('.') {
            bundle.starts_with(entry)
        } else {
            bundle == *entry
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        clipboard_replace_is_safe, should_skip_ax_read_for_bundle, should_use_clipboard_for_bundle,
    };

    #[test]
    fn terminal_bundles_skip_ax_read() {
        assert!(should_skip_ax_read_for_bundle(Some("com.apple.Terminal")));
        assert!(!should_skip_ax_read_for_bundle(Some("com.apple.Safari")));
    }

    #[test]
    fn browser_and_electron_bundles_use_clipboard() {
        assert!(should_use_clipboard_for_bundle(Some("com.google.Chrome")));
        assert!(should_use_clipboard_for_bundle(Some("com.electron.MyApp")));
        assert!(!should_use_clipboard_for_bundle(Some("com.apple.Terminal")));
    }

    #[test]
    fn clipboard_replace_requires_unchanged_text() {
        assert!(clipboard_replace_is_safe("hello", Some("hello")));
        assert!(clipboard_replace_is_safe(" hello ", Some("hello")));
        assert!(!clipboard_replace_is_safe("hello", Some("hello world")));
        assert!(!clipboard_replace_is_safe("hello", None));
    }
}
