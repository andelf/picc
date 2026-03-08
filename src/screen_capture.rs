//! ScreenCaptureKit-based window capture (macOS 14+).
//!
//! Captures windows by PID without requiring the target app to be in the foreground.

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCRunningApplication, SCScreenshotManager, SCShareableContent,
    SCStreamConfiguration, SCWindow,
};

/// Get all shareable content (windows, displays, applications).
/// Bridges the async ObjC callback to a synchronous call.
fn get_shareable_content() -> Option<Retained<SCShareableContent>> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        if !error.is_null() {
            let err = unsafe { &*error };
            eprintln!("SCShareableContent error: {}", err.localizedDescription());
            let _ = tx.send(None);
            return;
        }
        if content.is_null() {
            let _ = tx.send(None);
            return;
        }
        let retained = unsafe { Retained::retain(content).unwrap() };
        let _ = tx.send(Some(retained));
    });
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true,
            false, // include off-screen windows too
            &block,
        );
    }
    rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
}

/// Find the largest window belonging to the given PID.
fn find_window_by_pid(content: &SCShareableContent, pid: i32) -> Option<Retained<SCWindow>> {
    let windows = unsafe { content.windows() };
    let mut best: Option<(Retained<SCWindow>, f64)> = None;
    for window in windows.iter() {
        let app: Option<Retained<SCRunningApplication>> = unsafe { window.owningApplication() };
        let Some(app) = app else { continue };
        let app_pid = unsafe { app.processID() };
        if app_pid != pid {
            continue;
        }
        // Skip windows with layer != 0 (e.g. menu bar items)
        let layer = unsafe { window.windowLayer() };
        if layer != 0 {
            continue;
        }
        let frame = unsafe { window.frame() };
        // Skip tiny windows (likely invisible helpers)
        if frame.size.width < 10.0 || frame.size.height < 10.0 {
            continue;
        }
        let area = frame.size.width * frame.size.height;
        if best.as_ref().map_or(true, |(_, best_area)| area > *best_area) {
            best = Some((window.clone(), area));
        }
    }
    best.map(|(w, _)| w)
}

/// Capture a window image using ScreenCaptureKit's `SCScreenshotManager`.
/// The window does NOT need to be in the foreground.
fn capture_window(window: &SCWindow) -> Option<CFRetained<CGImage>> {
    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
    };

    let config = unsafe { SCStreamConfiguration::new() };
    // Use the window's native size (in pixels, accounting for Retina)
    let frame = unsafe { window.frame() };
    // Scale factor: assume 2x Retina; SCStreamConfiguration width/height are in pixels
    let scale = 2usize;
    unsafe {
        config.setWidth(frame.size.width as usize * scale);
        config.setHeight(frame.size.height as usize * scale);
        config.setShowsCursor(false);
        config.setScalesToFit(false);
    }

    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        if !error.is_null() {
            let err = unsafe { &*error };
            eprintln!("SCScreenshotManager error: {}", err.localizedDescription());
            let _ = tx.send(None);
            return;
        }
        if image.is_null() {
            let _ = tx.send(None);
            return;
        }
        let retained = unsafe { CFRetained::retain(std::ptr::NonNull::new_unchecked(image)) };
        let _ = tx.send(Some(retained));
    });

    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            &filter, &config, Some(&block),
        );
    }
    rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
}

/// Capture the first visible window of the given PID using ScreenCaptureKit.
/// Returns `None` if no suitable window is found or capture fails.
pub fn capture_window_by_pid(pid: i32) -> Option<CFRetained<CGImage>> {
    let content = get_shareable_content()?;
    let window = find_window_by_pid(&content, pid)?;
    capture_window(window.as_ref())
}
