//! TUI dashboard for MacBook ambient light sensor (ALS) via IOKit HID callback.
//!
//! Usage: cargo run --example ambient-light
//!
//! Features: real-time lux gauge, spectral channel bar chart, lux history sparkline.

#![allow(non_upper_case_globals, non_camel_case_types, static_mut_refs)]

use std::collections::VecDeque;
use std::ffi::{c_void, CString};
use std::io;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::symbols::Marker;
use ratatui::widgets::*;

// ── IOKit / CoreFoundation FFI ──────────────────────────────────────

type CFIndex = isize;
type CFAllocatorRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFTypeRef = *const c_void;
type CFRunLoopRef = *const c_void;
type CFRunLoopMode = CFStringRef;
type IOHIDDeviceRef = *const c_void;
type IOReturn = i32;
type Boolean = u8;
type MachPort = u32;

const kCFAllocatorDefault: CFAllocatorRef = ptr::null();
const kCFNumberSInt32Type: CFIndex = 3;
const kIOReturnSuccess: IOReturn = 0;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const u8) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        main_port: MachPort,
        matching: CFMutableDictionaryRef,
        existing: *mut u32,
    ) -> IOReturn;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> IOReturn;
    fn IORegistryEntrySetCFProperty(
        entry: u32,
        property_name: CFStringRef,
        property: CFTypeRef,
    ) -> IOReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;

    fn IOHIDDeviceCreate(allocator: CFAllocatorRef, service: u32) -> IOHIDDeviceRef;
    fn IOHIDDeviceOpen(device: IOHIDDeviceRef, options: u32) -> IOReturn;
    fn IOHIDDeviceClose(device: IOHIDDeviceRef, options: u32) -> IOReturn;
    fn IOHIDDeviceRegisterInputReportWithTimeStampCallback(
        device: IOHIDDeviceRef,
        report: *mut u8,
        report_length: CFIndex,
        callback: extern "C" fn(
            context: *mut c_void,
            result: IOReturn,
            sender: IOHIDDeviceRef,
            report_type: u32,
            report_id: u32,
            report: *const u8,
            report_length: CFIndex,
            timestamp: u64,
        ),
        context: *mut c_void,
    );
    fn IOHIDDeviceScheduleWithRunLoop(
        device: IOHIDDeviceRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFRunLoopMode,
    );
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: CFIndex,
        value_ptr: *const c_void,
    ) -> CFNumberRef;
    fn CFNumberGetValue(number: CFNumberRef, the_type: CFIndex, value_ptr: *mut c_void) -> Boolean;
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const u8,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: *const c_void);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRunInMode(
        mode: CFRunLoopMode,
        seconds: f64,
        return_after_source_handled: Boolean,
    ) -> i32;

    static kCFRunLoopDefaultMode: CFRunLoopMode;
}

const kCFStringEncodingUTF8: u32 = 0x08000100;

fn cfstr(s: &str) -> CFStringRef {
    let c = CString::new(s).unwrap();
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

fn prop_int(entry: u32, key: &str) -> Option<i32> {
    unsafe {
        let cf = IORegistryEntryCreateCFProperty(entry, cfstr(key), kCFAllocatorDefault, 0);
        if cf.is_null() {
            return None;
        }
        let mut val: i32 = 0;
        let ok = CFNumberGetValue(cf, kCFNumberSInt32Type, &mut val as *mut _ as _);
        CFRelease(cf);
        if ok != 0 {
            Some(val)
        } else {
            None
        }
    }
}

// ── Sensor data ─────────────────────────────────────────────────────

const ALS_REPORT_LEN: usize = 122;
const REPORT_INTERVAL_US: i32 = 10000;
const LUX_HISTORY_LEN: usize = 200;
const CHANNEL_HISTORY_LEN: usize = 200;

static GOT_REPORT: AtomicBool = AtomicBool::new(false);
static mut ALS_DATA: [u8; ALS_REPORT_LEN] = [0u8; ALS_REPORT_LEN];

extern "C" fn als_callback(
    _context: *mut c_void,
    _result: IOReturn,
    _sender: IOHIDDeviceRef,
    _report_type: u32,
    _report_id: u32,
    report: *const u8,
    report_length: CFIndex,
    _timestamp: u64,
) {
    if report_length as usize == ALS_REPORT_LEN {
        unsafe {
            ptr::copy_nonoverlapping(report, ALS_DATA.as_mut_ptr(), ALS_REPORT_LEN);
        }
        GOT_REPORT.store(true, Ordering::SeqCst);
    }
}

fn parse_report() -> (f32, [u32; 4]) {
    unsafe {
        let r = &ALS_DATA;
        let lux = f32::from_le_bytes([r[40], r[41], r[42], r[43]]);
        let ch = [20usize, 24, 28, 32]
            .map(|off| u32::from_le_bytes([r[off], r[off + 1], r[off + 2], r[off + 3]]));
        (lux, ch)
    }
}

// ── TUI state ───────────────────────────────────────────────────────

struct App {
    lux: f32,
    channels: [u32; 4],
    lux_history: VecDeque<f64>,
    channel_history: [VecDeque<f64>; 4],
    lux_max: f32,
    samples: u64,
}

impl App {
    fn new() -> Self {
        Self {
            lux: 0.0,
            channels: [0; 4],
            lux_history: VecDeque::with_capacity(LUX_HISTORY_LEN),
            channel_history: std::array::from_fn(|_| VecDeque::with_capacity(CHANNEL_HISTORY_LEN)),
            lux_max: 1.0,
            samples: 0,
        }
    }

    fn update(&mut self, lux: f32, channels: [u32; 4]) {
        self.lux = lux;
        self.channels = channels;
        self.samples += 1;

        if lux > self.lux_max {
            self.lux_max = lux * 1.2;
        }

        self.lux_history.push_back(lux as f64);
        if self.lux_history.len() > LUX_HISTORY_LEN {
            self.lux_history.pop_front();
        }

        for (i, &val) in channels.iter().enumerate() {
            self.channel_history[i].push_back(val as f64);
            if self.channel_history[i].len() > CHANNEL_HISTORY_LEN {
                self.channel_history[i].pop_front();
            }
        }
    }
}

// ── TUI rendering ───────────────────────────────────────────────────

fn ui(frame: &mut Frame, app: &App) {
    // Layout: top (lux info + channel bars) | bottom (history charts)
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Length(8), // lux gauge + channel bars
            Constraint::Min(8),    // lux history chart
            Constraint::Min(8),    // channel history chart
        ])
        .split(frame.area());

    // Title
    let title = Paragraph::new(format!(
        " Ambient Light Sensor   │   Lux: {:.4}   │   R: {}  G: {}  B: {}  Clear: {}   │   Samples: {}",
        app.lux, app.channels[0], app.channels[1], app.channels[2], app.channels[3], app.samples,
    ))
    .style(Style::default().fg(Color::White).bold())
    .block(Block::default().borders(Borders::ALL).title(" ALS Dashboard ").title_style(Style::default().fg(Color::Yellow).bold()));
    frame.render_widget(title, main_layout[0]);

    // Top row: lux gauge left, channel bar chart right
    let top_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(main_layout[1]);

    // Lux gauge
    let lux_ratio = (app.lux / app.lux_max).clamp(0.0, 1.0);
    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Lux: {:.4} (max: {:.1}) ", app.lux, app.lux_max)),
        )
        .gauge_style(Style::default().fg(Color::Yellow).bg(Color::DarkGray))
        .ratio(lux_ratio as f64)
        .label(format!("{:.2}", app.lux));
    frame.render_widget(gauge, top_layout[0]);

    // Channel bar chart
    let max_ch = app.channels.iter().copied().max().unwrap_or(1).max(1) as u64;
    let bar_data: Vec<Bar> = [
        ("Red", app.channels[0], Color::Red),
        ("Green", app.channels[1], Color::Green),
        ("Blue", app.channels[2], Color::Blue),
        ("Clear", app.channels[3], Color::White),
    ]
    .iter()
    .map(|&(label, val, color)| {
        Bar::default()
            .value(val as u64)
            .label(Line::from(label))
            .style(Style::default().fg(color))
            .value_style(Style::default().fg(Color::Black).bg(color).bold())
    })
    .collect();

    let barchart = BarChart::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Spectral Channels "),
        )
        .data(BarGroup::default().bars(&bar_data))
        .bar_width(7)
        .bar_gap(2)
        .max(max_ch);
    frame.render_widget(barchart, top_layout[1]);

    // Lux history chart
    let lux_data: Vec<(f64, f64)> = app
        .lux_history
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v))
        .collect();
    let lux_max_y = app.lux_history.iter().copied().fold(1.0f64, f64::max) * 1.2;

    let lux_dataset = Dataset::default()
        .name("Lux")
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Yellow))
        .data(&lux_data);

    let lux_chart = Chart::new(vec![lux_dataset])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Lux History "),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, LUX_HISTORY_LEN as f64])
                .labels(Vec::<Line>::new()),
        )
        .y_axis(Axis::default().bounds([0.0, lux_max_y]).labels(vec![
            Span::raw("0"),
            Span::raw(format!("{:.0}", lux_max_y / 2.0)),
            Span::raw(format!("{:.0}", lux_max_y)),
        ]));
    frame.render_widget(lux_chart, main_layout[2]);

    // Channel history chart
    let ch_colors = [Color::Red, Color::Green, Color::Blue, Color::White];
    let ch_names = ["Red", "Green", "Blue", "Clear"];
    let ch_data: Vec<Vec<(f64, f64)>> = (0..4)
        .map(|i| {
            app.channel_history[i]
                .iter()
                .enumerate()
                .map(|(j, &v)| (j as f64, v))
                .collect()
        })
        .collect();

    let ch_max_y = (0..4)
        .flat_map(|i| app.channel_history[i].iter().copied())
        .fold(1.0f64, f64::max)
        * 1.2;

    let ch_datasets: Vec<Dataset> = (0..4)
        .map(|i| {
            Dataset::default()
                .name(ch_names[i])
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(ch_colors[i]))
                .data(&ch_data[i])
        })
        .collect();

    let ch_chart = Chart::new(ch_datasets)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Channel History "),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, CHANNEL_HISTORY_LEN as f64])
                .labels(Vec::<Line>::new()),
        )
        .y_axis(Axis::default().bounds([0.0, ch_max_y]).labels(vec![
            Span::raw("0"),
            Span::raw(format!("{:.0}", ch_max_y / 2.0)),
            Span::raw(format!("{:.0}", ch_max_y)),
        ]))
        .legend_position(Some(LegendPosition::TopRight));
    frame.render_widget(ch_chart, main_layout[3]);
}

// ── Main ────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    unsafe {
        // Step 1: Wake up all SPU drivers
        let matching = IOServiceMatching(b"AppleSPUHIDDriver\0".as_ptr());
        let mut iter: u32 = 0;
        IOServiceGetMatchingServices(0, matching, &mut iter);
        loop {
            let svc = IOIteratorNext(iter);
            if svc == 0 {
                break;
            }
            for (key, val) in [
                ("SensorPropertyReportingState", 1),
                ("SensorPropertyPowerState", 1),
                ("ReportInterval", REPORT_INTERVAL_US),
            ] {
                IORegistryEntrySetCFProperty(svc, cfstr(key), cfnum(val) as _);
            }
            IOObjectRelease(svc);
        }
        IOObjectRelease(iter);

        // Step 2: Find ALS device
        let matching = IOServiceMatching(b"AppleSPUHIDDevice\0".as_ptr());
        let mut iter: u32 = 0;
        IOServiceGetMatchingServices(0, matching, &mut iter);

        let mut als_svc: u32 = 0;
        loop {
            let svc = IOIteratorNext(iter);
            if svc == 0 {
                break;
            }
            let up = prop_int(svc, "PrimaryUsagePage").unwrap_or(0);
            let u = prop_int(svc, "PrimaryUsage").unwrap_or(0);
            if up == 0xFF00u32 as i32 && u == 4 {
                als_svc = svc;
                break;
            }
            IOObjectRelease(svc);
        }
        IOObjectRelease(iter);

        if als_svc == 0 {
            eprintln!("No ambient light sensor found");
            std::process::exit(1);
        }

        // Step 3: Create HID device and register callback
        let device = IOHIDDeviceCreate(kCFAllocatorDefault, als_svc);
        IOObjectRelease(als_svc);
        if device.is_null() {
            eprintln!("Failed to create HID device");
            std::process::exit(1);
        }

        let ret = IOHIDDeviceOpen(device, 0);
        if ret != kIOReturnSuccess {
            eprintln!("Failed to open device: 0x{:08x}", ret);
            CFRelease(device);
            std::process::exit(1);
        }

        let mut report_buf = [0u8; 256];
        IOHIDDeviceRegisterInputReportWithTimeStampCallback(
            device,
            report_buf.as_mut_ptr(),
            report_buf.len() as CFIndex,
            als_callback,
            ptr::null_mut(),
        );

        let run_loop = CFRunLoopGetCurrent();
        IOHIDDeviceScheduleWithRunLoop(device, run_loop, kCFRunLoopDefaultMode);

        // Step 4: TUI loop
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

        let mut app = App::new();

        loop {
            // Pump IOKit run loop (non-blocking)
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 0);

            if GOT_REPORT.swap(false, Ordering::SeqCst) {
                let (lux, channels) = parse_report();
                app.update(lux, channels);
            }

            terminal.draw(|frame| ui(frame, &app))?;

            // Poll for keyboard input (short timeout to keep loop responsive)
            if event::poll(std::time::Duration::from_millis(30))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press
                        && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    {
                        break;
                    }
                }
            }
        }

        // Cleanup
        disable_raw_mode()?;
        io::stdout().execute(LeaveAlternateScreen)?;
        IOHIDDeviceClose(device, 0);
        CFRelease(device);
    }

    Ok(())
}
