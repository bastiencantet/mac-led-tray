// Sparkle auto-update integration.
//
// At app startup we instantiate SPUStandardUpdaterController from the bundled
// Sparkle.framework. It pulls the feed URL and the EdDSA public key from
// Info.plist (SUFeedURL / SUPublicEDKey) and handles everything — periodic
// checks, download, signature verification, install, relaunch.

use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};

// Keep the controller alive for the app's lifetime. Main-thread-only access
// in practice, but Mutex keeps Rust happy about the `Retained<AnyObject>`.
static UPDATER: Mutex<Option<Holder>> = Mutex::new(None);

struct Holder(#[allow(dead_code)] Retained<AnyObject>);
// We only instantiate and touch this from the main thread.
unsafe impl Send for Holder {}

pub fn init() {
    let cls = match AnyClass::get(c"SPUStandardUpdaterController") {
        Some(c) => c,
        None => {
            eprintln!("sparkle: framework class not found (dev mode without bundle?)");
            return;
        }
    };

    let controller: Option<Retained<AnyObject>> = unsafe {
        let alloc: *mut AnyObject = objc2::msg_send![cls, alloc];
        if alloc.is_null() {
            return;
        }
        let ctrl: *mut AnyObject = objc2::msg_send![
            alloc,
            initWithStartingUpdater: true,
            updaterDelegate: std::ptr::null_mut::<AnyObject>(),
            userDriverDelegate: std::ptr::null_mut::<AnyObject>(),
        ];
        Retained::from_raw(ctrl)
    };

    match controller {
        Some(c) => {
            if let Ok(mut g) = UPDATER.lock() {
                *g = Some(Holder(c));
            }
            eprintln!("sparkle: updater started");
        }
        None => eprintln!("sparkle: failed to init updater"),
    }
}
