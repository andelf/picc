use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRunLoop};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use std::ptr::NonNull;

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        return event.as_ptr();
    }

    let ev = event.as_ref();
    let flags = CGEvent::flags(Some(ev)).0;
    let device_flags = flags & 0xFFFF;
    let keycode = CGEvent::integer_value_field(Some(ev), CGEventField::KeyboardEventKeycode);

    println!(
        "type={:<15} keycode={:<4} flags=0x{:016x} device=0x{:04x}",
        format!("{:?}", event_type),
        keycode,
        flags,
        device_flags,
    );

    event.as_ptr()
}

fn main() {
    println!("按任意键查看 keycode 和 flags，Ctrl+C 退出");
    println!("---");

    let event_mask: CGEventMask = (1 << CGEventType::FlagsChanged.0)
        | (1 << CGEventType::KeyDown.0)
        | (1 << CGEventType::KeyUp.0);

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            event_mask,
            Some(callback),
            std::ptr::null_mut(),
        )
    };

    let Some(tap) = tap else {
        eprintln!("ERROR: 无法创建 event tap，请确认已授权辅助功能权限");
        std::process::exit(1);
    };

    let src = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        .expect("failed to create run loop source");

    unsafe {
        let run_loop = CFRunLoop::current().expect("no current run loop");
        run_loop.add_source(Some(&src), kCFRunLoopCommonModes);
        CFRunLoop::run();
    }
}
