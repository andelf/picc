use std::ffi::c_void;

use icrate::block2::ConcreteBlock;
use icrate::ns_string;
use icrate::objc2::declare::{Ivar, IvarDrop};
use icrate::objc2::rc::{Allocated, Id, Owned, Shared};
use icrate::objc2::{
    declare_class, extern_class, extern_methods, msg_send, msg_send_id, sel, ClassType,
};
use icrate::AppKit::{
    NSApplication, NSBackingStoreBuffered, NSBackingStoreType, NSBezierPath, NSColor, NSEvent,
    NSFullSizeContentViewWindowMask, NSNonactivatingPanelMask, NSPanel, NSResponder, NSScreen,
    NSView, NSWindow, NSWindowCollectionBehaviorCanJoinAllSpaces,
    NSWindowCollectionBehaviorFullScreenAuxiliary, NSWindowController, NSWindowLevel,
    NSWindowStyleMask, NSWindowCollectionBehaviorFullScreenPrimary, NSBorderlessWindowMask, NSWindowStyleMaskTitled,
};
use icrate::Foundation::{
    CGRect, NSArray, NSDate, NSDictionary, NSError, NSLocale, NSObject, NSPoint, NSRect, NSRunLoop,
    NSSize, NSString, CGPoint,
};
use icrate::Speech::{self, SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognizer};
use picc::core_graphics::CGImageRef;
use picc::vision;

declare_class!(
    #[derive(Debug, PartialEq)]
    pub struct SnapWindow {
        start_pos: IvarDrop<Box<NSPoint>, "_start_pos">,
        end_pos: IvarDrop<Box<NSPoint>, "_end_pos">,
    }
    mod ivars;

    unsafe impl ClassType for SnapWindow {
        #[inherits(NSWindow, NSResponder, NSObject)]
        type Super = NSPanel;
        const NAME: &'static str = "SnapWindow";
    }

    /// override NSResponder
    #[allow(non_snake_case)]
    unsafe impl SnapWindow {
        // must for mouseEvent
        #[method(canBecomeKeyWindow)]
        unsafe fn canBecomeKeyWindow(&self) -> bool {
            true
        }

        #[method(canBecomeMainWindow)]
        unsafe fn canBecomeMainWindow(&self) -> bool {
            true
        }

        #[method(mouseMoved:)]
        unsafe fn mouseMoved(&self, event: &NSEvent) {
            println!("mouseMoved: {:?}", event);
        }

        #[method(mouseDragged:)]
        unsafe fn mouseDragged(&self, _event: &NSEvent) {
            let loc = NSEvent::mouseLocation();
            let start_loc = &self.start_pos;

            let x = f64::min(start_loc.x, loc.x);
            let y = f64::min(start_loc.y, loc.y);
            let w = f64::abs(start_loc.x - loc.x);
            let h = f64::abs(start_loc.y - loc.y);
            let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

            let subviews = self.contentView().unwrap().subviews();
            let overlay_view = subviews.first().unwrap();
            overlay_view.setFrame(rect);
            overlay_view.setHidden(false);
        }

        #[method(mouseDown:)]
        unsafe fn mouseDown(&mut self, event: &NSEvent) {
            let loc = NSEvent::mouseLocation();
            if event.clickCount() == 2 {
                println!("double click {:?}", loc);
                // self.toggleFullScreen(None);
            } else {
                Ivar::write(&mut self.start_pos, Box::new(loc));
            }
            self.contentView()
                .unwrap()
                .subviews()
                .first()
                .unwrap()
                .setHidden(true);
        }

        #[method(mouseUp:)]
        unsafe fn mouseUp(&mut self, _event: &NSEvent) {
            let loc = NSEvent::mouseLocation();
            println!("mouseUp: {:?}", loc);
            Ivar::write(&mut self.end_pos, Box::new(loc));

            let start_loc = &self.start_pos;
            let end_loc = &self.end_pos;

            // transfer pos to global
            let x = f64::min(start_loc.x, end_loc.x);
            let y = f64::min(start_loc.y, end_loc.y);
            let w = f64::abs(start_loc.x - end_loc.x);
            let h = f64::abs(start_loc.y - end_loc.y);
            if w < 10.0 || h < 10.0 {
                return;
            }

            // TODO: fix pos
            let y = 982.0 - y + 40.0; // 40 = menu bar

            let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
            println!("Crop Rect: {:?}", rect);
            let crop_img = picc::screenshot(rect).unwrap();
            println!(
                "=> crop_img {:?} {}x{}",
                crop_img,
                (&*crop_img).width(),
                (&*crop_img).height()
            );

            ocr(crop_img);
        }

        #[method(keyDown:)]
        unsafe fn keyDown(&self, event: &NSEvent) {
            println!("FR => {:?}", self.frame());
            if event.keyCode() == 53 {
                println!("ESC");
                self.orderOut(None);
                self.close();
                NSApplication::sharedApplication().terminate(None);
            } else if event.keyCode() == 36 {
                println!("ENTER");
                self.toggleFullScreen(None);
            } else if event.keyCode() == 12 {
                println!("quit");
                self.orderOut(None);
                self.close();
                NSApplication::sharedApplication().terminate(None);
            } else if event.keyCode() == 49 {
                println!("SPACE");
                self.contentView()
                    .unwrap()
                    .subviews()
                    .first()
                    .unwrap()
                    .setHidden(true);
            }
            println!("keyDown: {:?}", event);
        }
    }
);

extern_methods!(
    /// Methods declared on superclass `NSWindow`
    unsafe impl SnapWindow {
        /*#[method_id(@__retain_semantics Init initWithContentRect:styleMask:backing:defer:)]
        pub unsafe fn initWithContentRect_styleMask_backing_defer(
            this: Option<Allocated<Self>>,
            content_rect: NSRect,
            style: NSWindowStyleMask,
            backing_store_type: NSBackingStoreType,
            flag: bool,
        ) -> Id<Self, Shared>;*/

        #[allow(non_snake_case)]
        #[method_id(@__retain_semantics Init initWithContentRect:styleMask:backing:defer:screen:)]
        pub unsafe fn initWithContentRect_styleMask_backing_defer_screen(
            this: Option<Allocated<Self>>,
            content_rect: NSRect,
            style: NSWindowStyleMask,
            backing_store_type: NSBackingStoreType,
            flag: bool,
            screen: Option<&NSScreen>,
        ) -> Id<Self, Shared>;
    }
);

declare_class!(
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct DrawPathView;

    unsafe impl ClassType for DrawPathView {
        #[inherits(NSResponder, NSObject)]
        type Super = NSView;
        const NAME: &'static str = "DrawPathView";
    }

    unsafe impl DrawPathView {
        #[method(drawRect:)]
        unsafe fn drawRect(&self, dirty_rect: NSRect) {
            // println!("drawRect: {:?}", dirty_rect);
            let path = unsafe { NSBezierPath::bezierPathWithRect(dirty_rect) };
            unsafe {
                path.addClip();
                path.fill();
                NSColor::redColor().setStroke();
                path.setLineWidth(2.);
                path.stroke()
            }
        }
    }
);

extern_methods!(
    unsafe impl DrawPathView {
        #[method_id(@__retain_semantics Init initWithFrame:)]
        pub unsafe fn initWithFrame(
            this: Option<Allocated<Self>>,
            frame_rect: NSRect,
        ) -> Id<Self, Shared>;
    }
);

fn ocr(img: CGImageRef) {
    let req = vision::VNRecognizeTextRequest::new();

    // NOTE: According to the docs: when using multiple languages, the order of the languages in the array is significant.
    // The more complex language should be placed first in the array.
    let lang = NSArray::from_vec(vec![
        NSString::from_str("zh-Hans"),
        NSString::from_str("en-US"),
    ]);
    req.set_languages(&lang);

    let handler = vision::VNImageRequestHandler::new_with_cgimage(img, &NSDictionary::new());

    let reqs = NSArray::from_slice(&[req.clone()]);
    handler.perform(&reqs).unwrap();

    for item in req.results().iter() {
        for candidate in item.top_candidates(1).iter() {
            println!("candidate.string(): {:?}", candidate.string());
            // println!("candidate.confidence(): {:?}", candidate.confidence());
        }
    }
}

fn main() {
    let app = unsafe { NSApplication::sharedApplication() };

    let window = {
        let this = SnapWindow::alloc();
        // let content_rect = NSRect::new(NSPoint::new(0., 0.), NSSize::new(1024., 768.));

        let screen = unsafe { NSScreen::mainScreen().unwrap() };
        unsafe { println!("Screen size {:?}", screen.frame()) };
        let win = unsafe {
            let frame = screen.frame();
            println!("Screen: {:?}", frame);
            SnapWindow::initWithContentRect_styleMask_backing_defer_screen(
                this,
                //NSRect { origin: NSPoint::new(0.0, 0.0), size: frame.size },
                frame,
                NSNonactivatingPanelMask,
                // NSBorderlessWindowMask, - not good
                NSBackingStoreBuffered,
                false,
                Some(&screen),
            )
        };
        unsafe {
            win.setAcceptsMouseMovedEvents(true);
            win.setFloatingPanel(true);
            win.setCollectionBehavior(
                NSWindowCollectionBehaviorCanJoinAllSpaces
                   | NSWindowCollectionBehaviorFullScreenAuxiliary,
            );
            win.setMovableByWindowBackground(false);
            win.setExcludedFromWindowsMenu(true);
            win.setAlphaValue(0.3);
            win.setOpaque(false);
            win.setHasShadow(false);
            win.setHidesOnDeactivate(false);

            #[allow(non_upper_case_globals)]
            const kCGMaximumWindowLevelKey: NSWindowLevel = 14;
            // win.setLevel(kCGMaximumWindowLevelKey);

            win.setRestorable(false);
            win.disableSnapshotRestoration();
            win.setLevel(kCGMaximumWindowLevelKey + 1);

   //         win.center();
     //       win.makeKeyAndOrderFront(None);

            win.setMovable(false);
        }
        win
    };

    unsafe {
        //   window.center();
        //  window.setTitle(ns_string!("Hello, world!"));
        window.makeKeyAndOrderFront(None);
    }

    unsafe {
        let frame = NSScreen::mainScreen().unwrap().frame();
        window.setFrame_display_animate(frame, true, false);
    }

    unsafe {
        let path_view = DrawPathView::initWithFrame(
            DrawPathView::alloc(),
            NSRect::new(NSPoint::new(0., 0.), NSSize::new(200., 200.)),
        );
        path_view.setHidden(true);
        window.contentView().unwrap().addSubview(&path_view);
    }

    println!("=> subview {:?}", unsafe { window.contentView() });

    unsafe { app.run() };
}
