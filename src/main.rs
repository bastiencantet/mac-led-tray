use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSFont, NSImage, NSMenu, NSMenuItem,
    NSSlider, NSStatusBar, NSStatusItem, NSSwitch, NSTextAlignment, NSTextField,
    NSVariableStatusItemLength, NSView,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

// SMC key that controls the Mac Mini front LED (found via Asahi Linux driver).
// 2 bytes: 00 00 = off, ff ff = max brightness.
const LED_KEY: &str = "LS0C";
const HELPER_NAME: &str = "led-helper";

// Whitelist of Mac models where LS0C is known to exist.
// Intel + M1 Mac Minis all use the "Macmini" prefix; newer Apple Silicon
// Mac Minis switched to the "MacXX,YY" scheme and are listed explicitly.
const SUPPORTED_EXACT: &[&str] = &[
    "Mac14,3",  // Mac Mini M2 (2023)
    "Mac14,12", // Mac Mini M2 Pro (2023)
    "Mac16,10", // Mac Mini M4 (2024)
    "Mac16,11", // Mac Mini M4 Pro (2024)
];

fn get_mac_model() -> String {
    Command::new("sysctl")
        .args(["-n", "hw.model"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn is_supported_mac(model: &str) -> bool {
    model.starts_with("Macmini") || SUPPORTED_EXACT.iter().any(|m| *m == model)
}

/// Shows a blocking alert. Returns true if the user chose to continue.
fn show_unsupported_alert(mtm: MainThreadMarker, model: &str) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    let alert = NSAlert::new(mtm);
    if let Some(icon) = NSImage::imageNamed(&NSString::from_str("NSCaution")) {
        unsafe { alert.setIcon(Some(&icon)) };
    }

    let model_display = if model.is_empty() { "unknown" } else { model };
    alert.setMessageText(&NSString::from_str("Unsupported Mac"));
    alert.setInformativeText(&NSString::from_str(&format!(
        "This model ({}) is not in the list of supported Macs.",
        model_display
    )));
    alert.addButtonWithTitle(&NSString::from_str("Quit"));
    alert.addButtonWithTitle(&NSString::from_str("Open Anyway"));

    let response = alert.runModal();
    // NSAlertFirstButtonReturn = 1000 (Quit), NSAlertSecondButtonReturn = 1001 (Continue)
    response == 1001
}

struct Helper {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

fn is_setuid_root(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.uid() == 0 && (m.mode() & 0o4000) != 0,
        Err(_) => false,
    }
}

fn elevate_helper(path: &Path) -> std::io::Result<()> {
    let p = path
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad path"))?;
    // Single-quote the path; ban quotes to prevent injection.
    if p.contains('\'') || p.contains('"') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains quote",
        ));
    }
    let script = format!(
        "do shell script \"chown root '{p}' && chmod u+s '{p}'\" with administrator privileges \
         with prompt \"LED needs admin access to talk to the SMC.\""
    );
    let status = Command::new("osascript").arg("-e").arg(&script).status()?;
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "admin prompt cancelled",
        ));
    }
    Ok(())
}

impl Helper {
    fn spawn() -> std::io::Result<Self> {
        let exe_dir = std::env::current_exe()?
            .parent()
            .map(|d| d.to_path_buf())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no exe dir"))?;
        let helper_path = exe_dir.join(HELPER_NAME);

        if !helper_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("helper missing: {}", helper_path.display()),
            ));
        }

        if !is_setuid_root(&helper_path) {
            eprintln!("helper is not setuid root, asking for admin...");
            elevate_helper(&helper_path)?;
        }

        let mut child = Command::new(&helper_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        let mut helper = Self {
            _child: child,
            stdin,
            stdout,
        };

        helper.send("PING")?;
        let resp = helper.recv()?;
        if !resp.starts_with("PONG") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("bad helper response: {}", resp),
            ));
        }
        Ok(helper)
    }

    fn send(&mut self, cmd: &str) -> std::io::Result<()> {
        writeln!(self.stdin, "{}", cmd)?;
        self.stdin.flush()
    }

    fn recv(&mut self) -> std::io::Result<String> {
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        Ok(line.trim().to_string())
    }

    fn write_led(&mut self, value: u8) -> std::io::Result<()> {
        let hex = format!("{:02x} {:02x}", value, value);
        self.send(&format!("WRITE {} {}", LED_KEY, hex))?;
        let resp = self.recv()?;
        if resp.starts_with("OK") {
            Ok(())
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::Other, resp))
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Effect {
    Blink,
    BlinkFast,
    Pulse,
    Sos,
    Strobe,
}

struct LedState {
    helper: Option<Helper>,
    is_on: bool,
    brightness: u8,
    last_on_brightness: u8,
    effect_stop: Option<Arc<AtomicBool>>,
}

impl LedState {
    fn new() -> Self {
        let helper = match Helper::spawn() {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("helper not available: {} (run `make setup`)", e);
                None
            }
        };
        Self {
            helper,
            is_on: true,
            brightness: 0xff,
            last_on_brightness: 0xff,
            effect_stop: None,
        }
    }

    fn write(&mut self, value: u8) {
        if let Some(ref mut h) = self.helper {
            if let Err(e) = h.write_led(value) {
                eprintln!("write LS0C failed: {}", e);
            }
        }
    }

    fn stop_effect(&mut self) {
        if let Some(flag) = self.effect_stop.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    fn start_effect(&mut self, effect: Effect) {
        self.stop_effect();
        let flag = Arc::new(AtomicBool::new(false));
        self.effect_stop = Some(flag.clone());
        std::thread::spawn(move || run_effect(effect, flag));
    }

    fn clear_effect(&mut self) {
        self.stop_effect();
        // Restore LED to user's last manual state
        let v = if self.is_on { self.brightness } else { 0 };
        self.write(v);
    }

    fn set_brightness(&mut self, value: u8) {
        self.stop_effect();
        self.brightness = value;
        if value > 0 {
            self.is_on = true;
            self.last_on_brightness = value;
        } else {
            self.is_on = false;
        }
        self.write(value);
    }

    fn set_on(&mut self, on: bool) {
        self.stop_effect();
        if on {
            self.is_on = true;
            let v = if self.last_on_brightness == 0 {
                0xff
            } else {
                self.last_on_brightness
            };
            self.brightness = v;
            self.write(v);
        } else {
            self.is_on = false;
            self.brightness = 0;
            self.write(0);
        }
    }
}

fn read_state() -> (bool, u8) {
    if let Ok(g) = STATE.lock() {
        if let Some(ref s) = *g {
            return (s.is_on, s.brightness);
        }
    }
    (false, 0)
}

fn write_raw(value: u8) {
    if let Ok(mut g) = STATE.lock() {
        if let Some(ref mut s) = *g {
            if let Some(ref mut h) = s.helper {
                let _ = h.write_led(value);
            }
        }
    }
}

fn sleep_cancellable(ms: u64, stop: &AtomicBool) -> bool {
    let step = 20;
    let mut remaining = ms;
    while remaining > 0 {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let s = remaining.min(step);
        std::thread::sleep(Duration::from_millis(s));
        remaining -= s;
    }
    !stop.load(Ordering::Relaxed)
}

fn run_effect(effect: Effect, stop: Arc<AtomicBool>) {
    match effect {
        Effect::Blink => run_blink(500, &stop),
        Effect::BlinkFast => run_blink(120, &stop),
        Effect::Strobe => run_blink(45, &stop),
        Effect::Pulse => run_pulse(&stop),
        Effect::Sos => run_sos(&stop),
    }
}

fn run_blink(period_ms: u64, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        write_raw(0xff);
        if !sleep_cancellable(period_ms, stop) {
            return;
        }
        write_raw(0x00);
        if !sleep_cancellable(period_ms, stop) {
            return;
        }
    }
}

fn run_pulse(stop: &AtomicBool) {
    // Sine wave, full cycle over 2 seconds
    let steps: u64 = 50;
    let step_ms: u64 = 40;
    let mut t: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        let phase = (t as f64 / steps as f64) * std::f64::consts::TAU;
        let val = ((phase.sin() * 0.5 + 0.5) * 255.0).round() as u8;
        write_raw(val);
        std::thread::sleep(Duration::from_millis(step_ms));
        t = (t + 1) % steps;
    }
}

fn run_sos(stop: &AtomicBool) {
    let dot: u64 = 180;
    let dash = dot * 3;
    let gap = dot;
    let letter_gap = dot * 3;
    let word_gap = dot * 7;

    let pulse = |on_ms: u64, off_ms: u64, stop: &AtomicBool| -> bool {
        write_raw(0xff);
        if !sleep_cancellable(on_ms, stop) {
            return false;
        }
        write_raw(0x00);
        sleep_cancellable(off_ms, stop)
    };

    while !stop.load(Ordering::Relaxed) {
        // S: ...
        for _ in 0..3 {
            if !pulse(dot, gap, stop) {
                return;
            }
        }
        if !sleep_cancellable(letter_gap.saturating_sub(gap), stop) {
            return;
        }
        // O: ---
        for _ in 0..3 {
            if !pulse(dash, gap, stop) {
                return;
            }
        }
        if !sleep_cancellable(letter_gap.saturating_sub(gap), stop) {
            return;
        }
        // S: ...
        for _ in 0..3 {
            if !pulse(dot, gap, stop) {
                return;
            }
        }
        if !sleep_cancellable(word_gap.saturating_sub(gap), stop) {
            return;
        }
    }
}

static STATE: Mutex<Option<LedState>> = Mutex::new(None);

fn with_state<F: FnOnce(&mut LedState)>(f: F) {
    if let Ok(mut g) = STATE.lock() {
        if let Some(ref mut s) = *g {
            f(s);
        }
    }
}

struct UiRefs {
    switch: Retained<NSSwitch>,
    slider: Retained<NSSlider>,
    status_label: Retained<NSTextField>,
    percent_label: Retained<NSTextField>,
}

struct HandlerIvars {
    ui: RefCell<Option<UiRefs>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LedTrayHandler"]
    #[ivars = HandlerIvars]
    struct Handler;

    impl Handler {
        #[unsafe(method(switchToggled:))]
        fn switch_toggled(&self, sender: &AnyObject) {
            let state: isize = unsafe { msg_send![sender, state] };
            let on = state != 0;
            with_state(|s| s.set_on(on));
            self.refresh_ui();
        }

        #[unsafe(method(sliderChanged:))]
        fn slider_changed(&self, sender: &AnyObject) {
            let value: f64 = unsafe { msg_send![sender, doubleValue] };
            let byte = value.clamp(0.0, 255.0) as u8;
            with_state(|s| s.set_brightness(byte));
            self.refresh_ui();
        }

        #[unsafe(method(effectNone:))]
        fn effect_none(&self, _sender: &AnyObject) {
            with_state(|s| s.clear_effect());
            self.refresh_ui();
        }

        #[unsafe(method(effectBlink:))]
        fn effect_blink(&self, _sender: &AnyObject) {
            with_state(|s| s.start_effect(Effect::Blink));
        }

        #[unsafe(method(effectBlinkFast:))]
        fn effect_blink_fast(&self, _sender: &AnyObject) {
            with_state(|s| s.start_effect(Effect::BlinkFast));
        }

        #[unsafe(method(effectPulse:))]
        fn effect_pulse(&self, _sender: &AnyObject) {
            with_state(|s| s.start_effect(Effect::Pulse));
        }

        #[unsafe(method(effectSos:))]
        fn effect_sos(&self, _sender: &AnyObject) {
            with_state(|s| s.start_effect(Effect::Sos));
        }

        #[unsafe(method(effectStrobe:))]
        fn effect_strobe(&self, _sender: &AnyObject) {
            with_state(|s| s.start_effect(Effect::Strobe));
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            if let Some(mtm) = MainThreadMarker::new() {
                let app = NSApplication::sharedApplication(mtm);
                app.terminate(None);
            }
        }
    }

    unsafe impl NSObjectProtocol for Handler {}
);

impl Handler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>();
        let this = this.set_ivars(HandlerIvars {
            ui: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn set_ui(&self, ui: UiRefs) {
        *self.ivars().ui.borrow_mut() = Some(ui);
    }

    fn refresh_ui(&self) {
        let (is_on, brightness) = read_state();
        let ui_ref = self.ivars().ui.borrow();
        let ui = match ui_ref.as_ref() {
            Some(u) => u,
            None => return,
        };
        let target_state = if is_on { 1 } else { 0 };
        unsafe {
            let _: () = msg_send![&ui.switch, setState: target_state as isize];
            ui.slider.setDoubleValue(brightness as f64);
        }
        let percent = ((brightness as f64 / 255.0) * 100.0).round() as u32;
        let status = if is_on { "ON" } else { "OFF" };
        ui.status_label
            .setStringValue(&NSString::from_str(status));
        ui.percent_label
            .setStringValue(&NSString::from_str(&format!("{}%", percent)));
    }
}

struct App {
    _status_item: Retained<NSStatusItem>,
    _handler: Retained<Handler>,
}

fn make_label(
    mtm: MainThreadMarker,
    text: &str,
    frame: NSRect,
    font_size: f64,
    align_right: bool,
    secondary: bool,
) -> Retained<NSTextField> {
    let alloc = NSTextField::alloc(mtm);
    let field: Retained<NSTextField> = unsafe { msg_send![alloc, initWithFrame: frame] };
    field.setStringValue(&NSString::from_str(text));
    field.setEditable(false);
    field.setBezeled(false);
    field.setDrawsBackground(false);
    field.setSelectable(false);
    let font: Retained<NSFont> = if secondary {
        NSFont::systemFontOfSize(font_size)
    } else {
        NSFont::boldSystemFontOfSize(font_size)
    };
    field.setFont(Some(&font));
    if align_right {
        field.setAlignment(NSTextAlignment::Right);
    }
    if secondary {
        let color = objc2_app_kit::NSColor::secondaryLabelColor();
        field.setTextColor(Some(&color));
    }
    field
}

fn build_control_view(
    mtm: MainThreadMarker,
    handler: &Retained<Handler>,
) -> (Retained<NSView>, UiRefs) {
    let pad: f64 = 14.0;
    let width: f64 = 260.0;
    let row_h: f64 = 22.0;
    let slider_h: f64 = 22.0;
    let gap: f64 = 6.0;
    let top_pad: f64 = 8.0;
    let bottom_pad: f64 = 10.0;
    let total_h = top_pad + row_h + gap + row_h + gap + slider_h + bottom_pad;

    let container: Retained<NSView> = unsafe {
        let alloc = NSView::alloc(mtm);
        msg_send![
            alloc,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, total_h))
        ]
    };

    // Row 1 (top): "LED" label on left, status + switch on right
    let row1_y = total_h - top_pad - row_h;
    let led_label = make_label(
        mtm,
        "LED",
        NSRect::new(NSPoint::new(pad, row1_y), NSSize::new(80.0, row_h)),
        13.0,
        false,
        false,
    );
    container.addSubview(&led_label);

    let switch_w: f64 = 40.0;
    let status_w: f64 = 50.0;
    let switch_x = width - pad - switch_w;
    let status_x = switch_x - 6.0 - status_w;

    let status_label = make_label(
        mtm,
        "ON",
        NSRect::new(NSPoint::new(status_x, row1_y), NSSize::new(status_w, row_h)),
        12.0,
        true,
        true,
    );
    container.addSubview(&status_label);

    let switch: Retained<NSSwitch> = unsafe {
        let alloc = NSSwitch::alloc(mtm);
        msg_send![
            alloc,
            initWithFrame: NSRect::new(
                NSPoint::new(switch_x, row1_y),
                NSSize::new(switch_w, row_h),
            )
        ]
    };
    unsafe {
        let _: () = msg_send![&switch, setState: 1isize];
        switch.setTarget(Some(handler));
        switch.setAction(Some(sel!(switchToggled:)));
    }
    container.addSubview(&switch);

    // Row 2: "Brightness" + percent
    let row2_y = row1_y - gap - row_h;
    let brightness_label = make_label(
        mtm,
        "Brightness",
        NSRect::new(NSPoint::new(pad, row2_y), NSSize::new(140.0, row_h)),
        12.0,
        false,
        true,
    );
    container.addSubview(&brightness_label);

    let percent_label = make_label(
        mtm,
        "100%",
        NSRect::new(
            NSPoint::new(width - pad - 60.0, row2_y),
            NSSize::new(60.0, row_h),
        ),
        12.0,
        true,
        true,
    );
    container.addSubview(&percent_label);

    // Row 3: slider
    let slider: Retained<NSSlider> = unsafe {
        let alloc = NSSlider::alloc(mtm);
        msg_send![
            alloc,
            initWithFrame: NSRect::new(
                NSPoint::new(pad, bottom_pad),
                NSSize::new(width - 2.0 * pad, slider_h),
            )
        ]
    };
    unsafe {
        slider.setMinValue(0.0);
        slider.setMaxValue(255.0);
        slider.setDoubleValue(255.0);
        slider.setTarget(Some(handler));
        slider.setAction(Some(sel!(sliderChanged:)));
        let _: () = msg_send![&slider, setContinuous: true];
    }
    container.addSubview(&slider);

    (
        container,
        UiRefs {
            switch,
            slider,
            status_label,
            percent_label,
        },
    )
}

fn make_header(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    // Try modern NSMenuItem.sectionHeaderWithTitle: (macOS 14+) — nicer styling.
    // Fall back to a disabled menu item on older systems.
    let title_ns = NSString::from_str(title);
    unsafe {
        let cls = <NSMenuItem as objc2::ClassType>::class();
        let responds: bool = msg_send![cls, respondsToSelector: sel!(sectionHeaderWithTitle:)];
        if responds {
            msg_send![cls, sectionHeaderWithTitle: &*title_ns]
        } else {
            let item = NSMenuItem::new(mtm);
            item.setTitle(&title_ns);
            item.setEnabled(false);
            item
        }
    }
}

fn make_action(
    mtm: MainThreadMarker,
    handler: &Retained<Handler>,
    title: &str,
    action: objc2::runtime::Sel,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    unsafe {
        item.setTarget(Some(handler));
        item.setAction(Some(action));
    }
    item
}

fn build_app(mtm: MainThreadMarker) -> App {
    let handler = Handler::new(mtm);

    let status_bar = NSStatusBar::systemStatusBar();
    let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

    if let Some(button) = status_item.button(mtm) {
        button.setTitle(&NSString::from_str("💡"));
    }

    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);

    // ── Power ───────────────────────────────
    menu.addItem(&make_header(mtm, "Power"));

    let (control_view, ui_refs) = build_control_view(mtm, &handler);
    handler.set_ui(ui_refs);
    let control_item = NSMenuItem::new(mtm);
    control_item.setView(Some(&control_view));
    menu.addItem(&control_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // ── Effects ─────────────────────────────
    menu.addItem(&make_header(mtm, "Effects"));
    let effects: &[(&str, objc2::runtime::Sel)] = &[
        ("Stop", sel!(effectNone:)),
        ("Blink", sel!(effectBlink:)),
        ("Blink Fast", sel!(effectBlinkFast:)),
        ("Pulse", sel!(effectPulse:)),
        ("SOS", sel!(effectSos:)),
        ("Strobe", sel!(effectStrobe:)),
    ];
    for (title, sel) in effects {
        menu.addItem(&make_action(mtm, &handler, title, *sel));
    }

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // ── App ─────────────────────────────────
    menu.addItem(&make_action(mtm, &handler, "Quit", sel!(quit:)));

    status_item.setMenu(Some(&menu));

    App {
        _status_item: status_item,
        _handler: handler,
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Compatibility gate — show a dialog for unknown Macs.
    let model = get_mac_model();
    eprintln!("detected Mac model: {}", model);
    if !is_supported_mac(&model) {
        if !show_unsupported_alert(mtm, &model) {
            return;
        }
    }

    *STATE.lock().unwrap() = Some(LedState::new());
    let _app = build_app(mtm);
    app.run();
}
