use std::error::Error;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{
  NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
  NSStatusItem,
};
use objc2_foundation::NSString;

// MARK: TrayDelegate

define_class!(
  #[unsafe(super(objc2_foundation::NSObject))]
  #[name = "TrayDelegate"]
  #[ivars = ()]
  struct TrayDelegate;

  impl TrayDelegate {
    #[unsafe(method(quitApp:))]
    fn quit_app(&self, _sender: *mut AnyObject) {
      let mtm = MainThreadMarker::new().expect("quit must be on main thread");
      let app = NSApplication::sharedApplication(mtm);
      app.terminate(None);
    }
  }
);

impl TrayDelegate {
  fn new() -> Retained<Self> {
    let this = Self::alloc().set_ivars(());
    unsafe { msg_send![super(this), init] }
  }
}

// MARK: Entry point

pub fn run_tray() -> Result<(), Box<dyn Error>> {
  // Daemonize: re-exec in background so the launching terminal can close
  if std::env::var("_MACMON_TRAY_BG").is_err() {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("tray");
    cmd.env("_MACMON_TRAY_BG", "1");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.spawn()?;
    return Ok(());
  }

  let mtm = MainThreadMarker::new().expect("must be called on the main thread");

  let app = NSApplication::sharedApplication(mtm);
  app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

  let status_bar = NSStatusBar::systemStatusBar();
  let status_item: Retained<NSStatusItem> = status_bar.statusItemWithLength(-1.0);

  if let Some(button) = status_item.button(mtm) {
    let icon_name = NSString::from_str("chart.bar.xaxis");
    let image: Option<Retained<NSImage>> =
      NSImage::imageWithSystemSymbolName_accessibilityDescription(&icon_name, None);
    if let Some(img) = image {
      img.setTemplate(true);
      button.setImage(Some(&img));
    } else {
      button.setTitle(&NSString::from_str("M"));
    }
  }

  let delegate = TrayDelegate::new();
  let target: &AnyObject =
    unsafe { &*(delegate.as_ref() as *const TrayDelegate as *const AnyObject) };

  let menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(""));

  let quit_item = unsafe {
    NSMenuItem::initWithTitle_action_keyEquivalent(
      mtm.alloc(),
      &NSString::from_str("Quit"),
      Some(sel!(quitApp:)),
      &NSString::from_str("q"),
    )
  };
  unsafe { quit_item.setTarget(Some(target)) };
  menu.addItem(&quit_item);

  unsafe {
    let _: () = msg_send![&*status_item, setMenu: &*menu];
  }

  std::mem::forget(delegate);
  std::mem::forget(status_item);
  std::mem::forget(menu);

  app.run();

  Ok(())
}
