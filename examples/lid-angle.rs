//! Read MacBook lid angle from the built-in sensor via IOKit HID.
//!
//! Usage: cargo run --example lid-angle
//!
//! The lid angle sensor is an AppleSPUHIDDevice with UsagePage=0x20, Usage=138.
//! Report format: 3 bytes — [report_id(1), angle_lo, angle_hi]
//! Angle = little-endian u16 & 0x1FF (range 0–511 degrees)

#![allow(non_upper_case_globals, non_camel_case_types)]

use std::ffi::c_void;
use std::ptr;

// --- IOKit / CoreFoundation FFI ---

type CFIndex = isize;
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFSetRef = *const c_void;
type CFTypeRef = *const c_void;
type IOHIDManagerRef = *const c_void;
type IOHIDDeviceRef = *const c_void;
type IOReturn = i32;
type Boolean = u8;

const kCFAllocatorDefault: CFAllocatorRef = ptr::null();
const kCFNumberSInt32Type: CFIndex = 3;
const kIOHIDReportTypeInput: u32 = 1; // IOHIDReportType::Input
const kIOHIDOptionsTypeNone: u32 = 0;
const kIOReturnSuccess: IOReturn = 0;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(allocator: CFAllocatorRef, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: CFDictionaryRef);
    fn IOHIDManagerCopyDevices(manager: IOHIDManagerRef) -> CFSetRef;
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDManagerClose(manager: IOHIDManagerRef, options: u32) -> IOReturn;

    fn IOHIDDeviceGetReport(
        device: IOHIDDeviceRef,
        report_type: u32,
        report_id: CFIndex,
        report: *mut u8,
        report_length: *mut CFIndex,
    ) -> IOReturn;
    fn IOHIDDeviceSetProperty(
        device: IOHIDDeviceRef,
        key: CFStringRef,
        value: CFTypeRef,
    ) -> Boolean;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;

    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFMutableDictionaryRef;
    fn CFDictionarySetValue(dict: CFMutableDictionaryRef, key: *const c_void, value: *const c_void);
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: CFIndex,
        value_ptr: *const c_void,
    ) -> CFNumberRef;
    fn CFSetGetCount(set: CFSetRef) -> CFIndex;
    fn CFSetGetValues(set: CFSetRef, values: *mut *const c_void);
    fn CFRelease(cf: *const c_void);

    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

// CFSTR replacement — create CFString from bytes
extern "C" {
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const u8,
        encoding: u32,
    ) -> CFStringRef;
}
const kCFStringEncodingUTF8: u32 = 0x08000100;

fn cfstr(s: &str) -> CFStringRef {
    let c = std::ffi::CString::new(s).unwrap();
    unsafe {
        CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr() as _, kCFStringEncodingUTF8)
    }
}

fn cfnum(val: i32) -> CFNumberRef {
    unsafe {
        CFNumberCreate(
            kCFAllocatorDefault,
            kCFNumberSInt32Type,
            &val as *const _ as _,
        )
    }
}

// IOKit HID property keys
const DEVICE_USAGE_PAGE_KEY: &str = "DeviceUsagePage";
const DEVICE_USAGE_KEY: &str = "DeviceUsage";
const SENSOR_PROPERTY_REPORTING_STATE: &str = "ReportingState";

fn main() {
    unsafe {
        // Create HID manager
        let manager = IOHIDManagerCreate(kCFAllocatorDefault, kIOHIDOptionsTypeNone);
        if manager.is_null() {
            eprintln!("Failed to create IOHIDManager");
            std::process::exit(1);
        }

        // Build matching dictionary: UsagePage=0x20 (Sensor), Usage=138 (Lid)
        let matching = CFDictionaryCreateMutable(
            kCFAllocatorDefault,
            2,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        let usage_page_key = cfstr(DEVICE_USAGE_PAGE_KEY);
        let usage_key = cfstr(DEVICE_USAGE_KEY);
        let usage_page_val = cfnum(0x20); // Sensor page
        let usage_val = cfnum(138); // Lid usage

        CFDictionarySetValue(matching, usage_page_key as _, usage_page_val as _);
        CFDictionarySetValue(matching, usage_key as _, usage_val as _);

        IOHIDManagerSetDeviceMatching(manager, matching as _);

        let ret = IOHIDManagerOpen(manager, kIOHIDOptionsTypeNone);
        if ret != kIOReturnSuccess {
            eprintln!("Failed to open HID manager: 0x{:08x}", ret);
            std::process::exit(1);
        }

        // Get matched devices
        let device_set = IOHIDManagerCopyDevices(manager);
        if device_set.is_null() {
            eprintln!("No lid angle sensor found");
            IOHIDManagerClose(manager, kIOHIDOptionsTypeNone);
            std::process::exit(1);
        }

        let count = CFSetGetCount(device_set);
        if count == 0 {
            eprintln!("No lid angle sensor found");
            CFRelease(device_set);
            IOHIDManagerClose(manager, kIOHIDOptionsTypeNone);
            std::process::exit(1);
        }

        // Get first device
        let mut devices = vec![ptr::null(); count as usize];
        CFSetGetValues(device_set, devices.as_mut_ptr());
        let device: IOHIDDeviceRef = devices[0];

        // Wake up sensor: set ReportingState = true
        let reporting_key = cfstr(SENSOR_PROPERTY_REPORTING_STATE);
        IOHIDDeviceSetProperty(device, reporting_key, kCFBooleanTrue);

        // Small delay to let sensor wake up
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Read report
        let mut report = [0u8; 3];
        let mut report_len: CFIndex = 3;

        let ret = IOHIDDeviceGetReport(
            device,
            kIOHIDReportTypeInput,
            1, // report ID
            report.as_mut_ptr(),
            &mut report_len,
        );

        if ret != kIOReturnSuccess {
            eprintln!("Failed to get report: 0x{:08x}", ret);
            eprintln!("(May need to run with sudo or grant Input Monitoring permission)");
        } else {
            let raw = u16::from_le_bytes([report[1], report[2]]);
            let angle = raw & 0x1FF;
            println!("Lid angle: {}°", angle);
            println!(
                "Raw report: {:02x} {:02x} {:02x}",
                report[0], report[1], report[2]
            );
        }

        // Cleanup
        CFRelease(device_set);
        IOHIDManagerClose(manager, kIOHIDOptionsTypeNone);
        CFRelease(manager);
    }
}
