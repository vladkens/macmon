use std::error::Error;
use std::io::{Read as _, Write as _};
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
  NSApplication, NSApplicationActivationPolicy, NSBackgroundColorAttributeName, NSBackingStoreType,
  NSBezierPath, NSColor, NSEvent, NSEventMask, NSFont, NSFontAttributeName,
  NSForegroundColorAttributeName, NSImage, NSMenu, NSMenuItem, NSPopUpMenuWindowLevel, NSScreen,
  NSStatusBar, NSStatusItem, NSStringDrawing, NSView, NSWindow, NSWindowButton, NSWindowDelegate,
  NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSDictionary, NSNotification, NSPoint, NSRect, NSSize, NSString};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::updater;

// MARK: Global state

struct PtyHandle {
  master: Box<dyn portable_pty::MasterPty + Send>,
  child: Box<dyn portable_pty::Child + Send + Sync>,
  writer: Box<dyn std::io::Write + Send>,
}

static TERM_STATE: OnceLock<Mutex<vt100::Parser>> = OnceLock::new();
static PTY_HANDLE: OnceLock<Mutex<Option<PtyHandle>>> = OnceLock::new();

// AppKit objects are not Send/Sync, so we store raw pointers.
// Safety: all fields are written once on the main thread (via mem::forget of Retained)
// and never invalidated. All AppKit access happens on the main thread or via
// performSelectorOnMainThread.
struct TrayState {
  window: *mut NSWindow,
  view: *mut AnyObject,
  button: *mut AnyObject,
  menu: *mut NSMenu,
  status_item: *mut NSStatusItem,
  update_item: *mut NSMenuItem,
}
unsafe impl Send for TrayState {}
unsafe impl Sync for TrayState {}

static TRAY: OnceLock<Mutex<TrayState>> = OnceLock::new();

// MARK: Font metrics

struct FontMetrics {
  font: Retained<NSFont>,
  cell_width: f64,
  cell_height: f64,
}

// Safety: NSFont is immutable and we only create it on the main thread.
unsafe impl Send for FontMetrics {}
unsafe impl Sync for FontMetrics {}

static FONT_METRICS: OnceLock<FontMetrics> = OnceLock::new();

fn font_metrics() -> &'static FontMetrics {
  FONT_METRICS.get_or_init(|| {
    let font = NSFont::monospacedSystemFontOfSize_weight(13.0, 0.0);
    let advance = font.maximumAdvancement();
    let cell_width = if advance.width > 0.0 { advance.width } else { 7.8 };
    let cell_height = font.ascender() - font.descender() + font.leading();
    FontMetrics { font, cell_width, cell_height }
  })
}

// MARK: Terminal size helpers

fn cols_rows_for_size(content_size: NSSize) -> (u16, u16) {
  let fm = font_metrics();
  let cols = (content_size.width / fm.cell_width).floor().max(1.0) as u16;
  let rows = (content_size.height / fm.cell_height).floor().max(1.0) as u16;
  (cols, rows)
}

// MARK: Color mapping

fn ansi_color_to_nscolor(color: vt100::Color, is_fg: bool) -> Retained<NSColor> {
  match color {
    vt100::Color::Default => {
      if is_fg {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.9, 0.9, 0.9, 1.0)
      } else {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0)
      }
    }
    vt100::Color::Idx(idx) => idx_to_nscolor(idx),
    vt100::Color::Rgb(r, g, b) => NSColor::colorWithSRGBRed_green_blue_alpha(
      r as f64 / 255.0,
      g as f64 / 255.0,
      b as f64 / 255.0,
      1.0,
    ),
  }
}

fn idx_to_nscolor(idx: u8) -> Retained<NSColor> {
  static ANSI16: [(f64, f64, f64); 16] = [
    (0.0, 0.0, 0.0),
    (0.8, 0.0, 0.0),
    (0.0, 0.8, 0.0),
    (0.8, 0.8, 0.0),
    (0.0, 0.0, 0.8),
    (0.8, 0.0, 0.8),
    (0.0, 0.8, 0.8),
    (0.75, 0.75, 0.75),
    (0.5, 0.5, 0.5),
    (1.0, 0.0, 0.0),
    (0.0, 1.0, 0.0),
    (1.0, 1.0, 0.0),
    (0.0, 0.0, 1.0),
    (1.0, 0.0, 1.0),
    (0.0, 1.0, 1.0),
    (1.0, 1.0, 1.0),
  ];

  let (r, g, b) = if idx < 16 {
    ANSI16[idx as usize]
  } else if idx < 232 {
    let idx = idx - 16;
    let ri = idx / 36;
    let gi = (idx % 36) / 6;
    let bi = idx % 6;
    let to_f = |v: u8| if v == 0 { 0.0 } else { (55.0 + 40.0 * v as f64) / 255.0 };
    (to_f(ri), to_f(gi), to_f(bi))
  } else {
    let v = (8.0 + 10.0 * (idx - 232) as f64) / 255.0;
    (v, v, v)
  };

  NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

// MARK: Attributed string helpers

fn make_attrs_dict(
  fg: &NSColor,
  bg: &NSColor,
  font: &NSFont,
) -> Retained<NSDictionary<NSString, AnyObject>> {
  let fg_key: &NSString = unsafe { NSForegroundColorAttributeName };
  let bg_key: &NSString = unsafe { NSBackgroundColorAttributeName };
  let font_key: &NSString = unsafe { NSFontAttributeName };

  unsafe {
    let keys: [NonNull<ProtocolObject<dyn objc2_foundation::NSCopying>>; 3] =
      [NonNull::from(fg_key).cast(), NonNull::from(bg_key).cast(), NonNull::from(font_key).cast()];
    let objects: [NonNull<AnyObject>; 3] =
      [NonNull::from(fg).cast(), NonNull::from(bg).cast(), NonNull::from(font).cast()];

    NSDictionary::initWithObjects_forKeys_count(
      NSDictionary::alloc(),
      objects.as_ptr() as *mut _,
      keys.as_ptr() as *mut _,
      3,
    )
  }
}

// MARK: TerminalView

define_class!(
    #[unsafe(super(NSView))]
    #[name = "TerminalView"]
    #[ivars = ()]
    struct TerminalView;

    impl TerminalView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            draw_terminal();
        }

        #[unsafe(method(triggerRedraw))]
        fn trigger_redraw(&self) {
            self.setNeedsDisplay(true);
        }
    }
);

impl TerminalView {
  fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
    let this = mtm.alloc::<Self>().set_ivars(());
    unsafe { msg_send![super(this), initWithFrame: frame] }
  }
}

// MARK: Terminal drawing (batched per-run)

fn draw_terminal() {
  let fm = font_metrics();
  let parser = TERM_STATE.get().unwrap().lock().unwrap();
  let screen = parser.screen();
  let (rows, cols) = screen.size();

  // Fill background with black
  let bg = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0);
  bg.set();
  let bounds = NSRect::new(
    NSPoint::new(0.0, 0.0),
    NSSize::new(cols as f64 * fm.cell_width, rows as f64 * fm.cell_height),
  );
  NSBezierPath::fillRect(bounds);

  if cols == 0 {
    return;
  }

  let mut run_text = String::with_capacity(cols as usize);

  for row in 0..rows {
    let first = screen.cell(row, 0).unwrap();
    let mut run_fg = first.fgcolor();
    let mut run_bg = first.bgcolor();
    let mut run_start: u16 = 0;
    run_text.clear();

    let ch = first.contents();
    if ch.is_empty() {
      run_text.push(' ');
    } else {
      run_text.push_str(&ch);
    }

    for col in 1..cols {
      let cell = screen.cell(row, col).unwrap();
      let fg = cell.fgcolor();
      let bg = cell.bgcolor();

      if fg != run_fg || bg != run_bg {
        draw_run(&run_text, run_start, row, run_fg, run_bg, fm);
        run_text.clear();
        run_start = col;
        run_fg = fg;
        run_bg = bg;
      }

      let ch = cell.contents();
      if ch.is_empty() {
        run_text.push(' ');
      } else {
        run_text.push_str(&ch);
      }
    }

    // Flush last run of the row
    if !run_text.is_empty() {
      draw_run(&run_text, run_start, row, run_fg, run_bg, fm);
    }
  }
}

fn draw_run(
  text: &str,
  start_col: u16,
  row: u16,
  fg: vt100::Color,
  bg: vt100::Color,
  fm: &FontMetrics,
) {
  let fg_color = ansi_color_to_nscolor(fg, true);
  let bg_color = ansi_color_to_nscolor(bg, false);
  let dict = make_attrs_dict(&fg_color, &bg_color, &fm.font);
  let ns_str = NSString::from_str(text);
  let point = NSPoint::new(start_col as f64 * fm.cell_width, row as f64 * fm.cell_height);
  unsafe {
    ns_str.drawAtPoint_withAttributes(point, Some(&dict));
  }
}

// MARK: WindowDelegate

define_class!(
  #[unsafe(super(objc2_foundation::NSObject))]
  #[thread_kind = MainThreadOnly]
  #[name = "TrayWindowDelegate"]
  #[ivars = ()]
  struct TrayWindowDelegate;

  unsafe impl NSObjectProtocol for TrayWindowDelegate {}

  unsafe impl NSWindowDelegate for TrayWindowDelegate {
    #[unsafe(method(windowDidResize:))]
    fn window_did_resize(&self, _notification: &NSNotification) {
      handle_resize();
    }
  }
);

impl TrayWindowDelegate {
  fn new(mtm: MainThreadMarker) -> Retained<Self> {
    let this = mtm.alloc::<Self>().set_ivars(());
    unsafe { msg_send![super(this), init] }
  }
}

fn handle_resize() {
  let fm = font_metrics();

  let (win_ptr, view_ptr) = {
    let tray = TRAY.get().unwrap().lock().unwrap();
    (tray.window, tray.view)
  };

  if win_ptr.is_null() {
    return;
  }

  let content_size: NSSize = unsafe {
    let content_rect: NSRect = msg_send![win_ptr, contentLayoutRect];
    content_rect.size
  };

  let (cols, rows) = cols_rows_for_size(content_size);

  // Resize PTY
  {
    let handle = PTY_HANDLE.get().unwrap().lock().unwrap();
    if let Some(ref h) = *handle {
      let _ = h.master.resize(PtySize {
        rows,
        cols,
        pixel_width: content_size.width as u16,
        pixel_height: content_size.height as u16,
      });
    }
  }

  // Resize vt100 parser
  {
    let mut parser = TERM_STATE.get().unwrap().lock().unwrap();
    parser.set_size(rows, cols);
  }

  // Resize the view to match content
  if !view_ptr.is_null() {
    let new_frame = NSRect::new(
      NSPoint::new(0.0, 0.0),
      NSSize::new(cols as f64 * fm.cell_width, rows as f64 * fm.cell_height),
    );
    unsafe {
      let _: () = msg_send![view_ptr, setFrame: new_frame];
      let _: () = msg_send![view_ptr, setNeedsDisplay: true];
    }
  }
}

// MARK: PTY management

fn spawn_pty_and_reader(rows: u16, cols: u16) {
  let pty_system = native_pty_system();
  let pty_pair = pty_system
    .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
    .expect("Failed to open PTY");

  let exe = std::env::current_exe().unwrap_or_default();
  let mut cmd = CommandBuilder::new(&exe);
  cmd.env("TERM", "xterm-256color");

  let child = pty_pair.slave.spawn_command(cmd).expect("Failed to spawn child");
  drop(pty_pair.slave);

  let mut reader = pty_pair.master.try_clone_reader().expect("Failed to clone PTY reader");
  let writer = pty_pair.master.take_writer().expect("Failed to take PTY writer");

  {
    let mut handle = PTY_HANDLE.get().unwrap().lock().unwrap();
    *handle = Some(PtyHandle { master: pty_pair.master, child, writer });
  }

  // Reset parser state for fresh session
  {
    let mut parser = TERM_STATE.get().unwrap().lock().unwrap();
    *parser = vt100::Parser::new(rows, cols, 0);
  }

  // Reader thread
  std::thread::spawn(move || {
    let mut buf = [0u8; 4096];
    loop {
      match reader.read(&mut buf) {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          {
            let mut parser = TERM_STATE.get().unwrap().lock().unwrap();
            parser.process(&buf[..n]);
          }
          signal_redraw();
        }
      }
    }

    // Child exited — reap it
    {
      let mut handle = PTY_HANDLE.get().unwrap().lock().unwrap();
      if let Some(mut h) = handle.take() {
        let _ = h.child.wait();
      }
    }

    dispatch_hide_window();
  });
}

fn kill_pty_child() {
  let mut handle = PTY_HANDLE.get().unwrap().lock().unwrap();
  if let Some(mut h) = handle.take() {
    let _ = h.child.kill();
    let _ = h.child.wait();
  }
}

fn signal_redraw() {
  // Safety: view pointer is write-once (set in ensure_window, never cleared)
  let ptr = {
    let tray = TRAY.get().unwrap().lock().unwrap();
    tray.view
  };
  if !ptr.is_null() {
    unsafe {
      let _: () = msg_send![ptr, performSelectorOnMainThread: sel!(triggerRedraw), withObject: std::ptr::null::<AnyObject>(), waitUntilDone: false];
    }
  }
}

fn dispatch_hide_window() {
  // Safety: window pointer is write-once (set in ensure_window, never cleared)
  let ptr = {
    let tray = TRAY.get().unwrap().lock().unwrap();
    tray.window
  };
  if !ptr.is_null() {
    unsafe {
      let _: () = msg_send![ptr, performSelectorOnMainThread: sel!(orderOut:), withObject: std::ptr::null::<AnyObject>(), waitUntilDone: false];
    }
  }
}

// MARK: PTY input

fn send_pty_key(key: u8) {
  let mut handle = PTY_HANDLE.get().unwrap().lock().unwrap();
  if let Some(ref mut h) = *handle {
    let _ = h.writer.write_all(&[key]);
  }
}

// MARK: Window positioning

// Caller must not hold TRAY lock — this function acquires it to read button.

fn position_window_below_icon(win_ptr: *mut NSWindow) {
  let button_ptr = {
    let tray = TRAY.get().unwrap().lock().unwrap();
    tray.button
  };
  if button_ptr.is_null() {
    return;
  }

  // Get the button's window (the menu bar window) and its frame in screen coordinates
  let button_window: *mut NSWindow = unsafe { msg_send![button_ptr, window] };
  if button_window.is_null() {
    return;
  }
  let button_frame: NSRect = unsafe { msg_send![button_ptr, frame] };
  let screen_frame: NSRect = unsafe { msg_send![button_window, convertRectToScreen: button_frame] };

  // Get our window's frame size
  let win_frame: NSRect = unsafe { msg_send![win_ptr, frame] };
  let win_width = win_frame.size.width;
  let win_height = win_frame.size.height;

  // Window top should be at the bottom of the menu bar icon
  let top_y = screen_frame.origin.y;

  // Horizontal: center on the button, then clamp to screen bounds
  let button_center_x = screen_frame.origin.x + screen_frame.size.width / 2.0;
  let mut x = button_center_x - win_width / 2.0;

  // Clamp to screen edges
  let mtm = MainThreadMarker::new().unwrap();
  if let Some(screen) = NSScreen::mainScreen(mtm) {
    let s = screen.visibleFrame();
    let screen_left = s.origin.x;
    let screen_right = s.origin.x + s.size.width;
    if x + win_width > screen_right {
      x = screen_right - win_width;
    }
    if x < screen_left {
      x = screen_left;
    }
  }

  let origin = NSPoint::new(x, top_y - win_height);
  unsafe {
    let _: () = msg_send![win_ptr, setFrameOrigin: origin];
  }
}

// MARK: Window management

fn ensure_window(mtm: MainThreadMarker) {
  {
    let tray = TRAY.get().unwrap().lock().unwrap();
    if !tray.window.is_null() {
      return;
    }
  }

  let fm = font_metrics();
  let width = 80.0 * fm.cell_width;
  let height = 24.0 * fm.cell_height;

  let frame = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(width, height));
  // Titled needed for resize handles; Resizable for user resizing
  let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable;

  let window = unsafe {
    NSWindow::initWithContentRect_styleMask_backing_defer(
      mtm.alloc(),
      frame,
      style,
      NSBackingStoreType::Buffered,
      false,
    )
  };

  // Hide title bar chrome: transparent titlebar, hidden title, no traffic lights
  window.setTitle(&NSString::from_str(""));
  window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
  window.setTitlebarAppearsTransparent(true);
  if let Some(btn) = window.standardWindowButton(NSWindowButton::CloseButton) {
    btn.setHidden(true);
  }
  if let Some(btn) = window.standardWindowButton(NSWindowButton::MiniaturizeButton) {
    btn.setHidden(true);
  }
  if let Some(btn) = window.standardWindowButton(NSWindowButton::ZoomButton) {
    btn.setHidden(true);
  }

  // Not movable — stays anchored below the icon
  window.setMovable(false);
  window.setMovableByWindowBackground(false);

  unsafe { window.setReleasedWhenClosed(false) };

  // Float above normal windows like a popover
  window.setLevel(NSPopUpMenuWindowLevel);

  let bg = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0);
  window.setBackgroundColor(Some(&bg));

  // Set minimum size to ~40x10 terminal cells
  window.setContentMinSize(NSSize::new(40.0 * fm.cell_width, 10.0 * fm.cell_height));

  // Window delegate
  let delegate = TrayWindowDelegate::new(mtm);
  let delegate_proto: Retained<ProtocolObject<dyn NSWindowDelegate>> =
    ProtocolObject::from_retained(delegate);
  window.setDelegate(Some(&delegate_proto));
  std::mem::forget(delegate_proto);

  // Terminal view
  let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height));
  let term_view = TerminalView::new(content_rect, mtm);

  let view_ref: &NSView = &term_view;
  window.setContentView(Some(view_ref));

  // Store raw pointers
  {
    let mut tray = TRAY.get().unwrap().lock().unwrap();
    tray.view = Retained::as_ptr(&term_view) as *mut AnyObject;
    tray.window = Retained::as_ptr(&window) as *mut NSWindow;
    std::mem::forget(term_view);
    std::mem::forget(window);
  }
}

fn toggle_window(mtm: MainThreadMarker) -> bool {
  ensure_window(mtm);

  // Safety: window pointer is write-once (set in ensure_window above, never cleared)
  let win_ptr = {
    let tray = TRAY.get().unwrap().lock().unwrap();
    tray.window
  };
  assert!(!win_ptr.is_null());

  let is_visible: bool = unsafe { msg_send![win_ptr, isVisible] };
  if is_visible {
    unsafe {
      let _: () = msg_send![win_ptr, orderOut: std::ptr::null::<AnyObject>()];
    }
    // Don't kill the child — it keeps running in the background
    false
  } else {
    // Position below the status bar icon before showing
    position_window_below_icon(win_ptr);

    // Only spawn a new PTY if one isn't already running
    let needs_spawn = {
      let handle = PTY_HANDLE.get().unwrap().lock().unwrap();
      handle.is_none()
    };

    if needs_spawn {
      let content_size: NSSize = unsafe {
        let content_rect: NSRect = msg_send![win_ptr, contentLayoutRect];
        content_rect.size
      };
      let (cols, rows) = cols_rows_for_size(content_size);
      spawn_pty_and_reader(rows, cols);
    } else {
      // Trigger a redraw to show the latest state
      signal_redraw();
    }

    unsafe {
      let _: () = msg_send![win_ptr, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
    }
    true
  }
}

// MARK: TrayDelegate

define_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[name = "TrayDelegate"]
    #[ivars = ()]
    struct TrayDelegate;

    impl TrayDelegate {
        #[unsafe(method(toggleTerminal:))]
        fn toggle_terminal(&self, _sender: *mut AnyObject) {
            let mtm = MainThreadMarker::new().expect("toggle must be on main thread");
            toggle_window(mtm);
        }

        #[unsafe(method(changeColor:))]
        fn change_color(&self, _sender: *mut AnyObject) {
            send_pty_key(b'c');
        }

        #[unsafe(method(changeView:))]
        fn change_view(&self, _sender: *mut AnyObject) {
            send_pty_key(b'v');
        }

        #[unsafe(method(incInterval:))]
        fn inc_interval(&self, _sender: *mut AnyObject) {
            send_pty_key(b'+');
        }

        #[unsafe(method(decInterval:))]
        fn dec_interval(&self, _sender: *mut AnyObject) {
            send_pty_key(b'-');
        }

        #[unsafe(method(applyUpdateMenu:))]
        fn apply_update_menu(&self, _sender: *mut AnyObject) {
            let pending = {
                let mut p = get_pending_menu_update().lock().unwrap();
                p.take()
            };
            if let Some((title, switch_action)) = pending {
                let tray = TRAY.get().unwrap().lock().unwrap();
                let item_ptr = tray.update_item;
                if item_ptr.is_null() {
                    return;
                }
                let ns_title = NSString::from_str(&title);
                unsafe {
                    let _: () = msg_send![item_ptr, setTitle: &*ns_title];
                    if switch_action {
                        let _: () = msg_send![item_ptr, setAction: sel!(installUpdate:)];
                    }
                }
            }
        }

        #[unsafe(method(restartApp:))]
        fn restart_app(&self, _sender: *mut AnyObject) {
            kill_pty_child();
            let exe = std::env::current_exe().expect("can't find current exe");
            let mut cmd = std::process::Command::new(&exe);
            cmd.arg("tray");
            cmd.env("_MACMON_TRAY_BG", "1");
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            let _ = cmd.spawn();
            let mtm = MainThreadMarker::new().expect("restart must be on main thread");
            let app = NSApplication::sharedApplication(mtm);
            app.terminate(None);
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: *mut AnyObject) {
            std::thread::spawn(|| {
                match updater::check_for_update() {
                    Some(info) => {
                        {
                            let mut state = updater::get_update_state().lock().unwrap();
                            *state = Some(info);
                        }
                        update_menu_for_update();
                    }
                    None => {
                        // No update — briefly show that in menu title
                        set_update_menu_title("Up to Date");
                    }
                }
            });
        }

        #[unsafe(method(installUpdate:))]
        fn install_update(&self, _sender: *mut AnyObject) {
            let info = {
                let state = updater::get_update_state().lock().unwrap();
                state.clone()
            };
            if let Some(info) = info {
                set_update_menu_title("Updating...");
                std::thread::spawn(move || {
                    match updater::perform_update(&info) {
                        Ok(()) => {
                            set_update_menu_title("Updated! Restart to apply");
                        }
                        Err(e) => {
                            set_update_menu_title(&format!("Update failed: {}", e));
                        }
                    }
                });
            }
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: *mut AnyObject) {
            kill_pty_child();
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

// MARK: Update menu helpers

/// Pending update menu state, applied on the main thread.
static PENDING_MENU_UPDATE: OnceLock<Mutex<Option<(String, bool)>>> = OnceLock::new();

fn get_pending_menu_update() -> &'static Mutex<Option<(String, bool)>> {
  PENDING_MENU_UPDATE.get_or_init(|| Mutex::new(None))
}

/// Store the delegate pointer (as usize) for main-thread callbacks.
static DELEGATE_PTR: OnceLock<Mutex<usize>> = OnceLock::new();

fn get_delegate_ptr() -> &'static Mutex<usize> {
  DELEGATE_PTR.get_or_init(|| Mutex::new(0))
}

/// Queue a title change for the update menu item, dispatched to main thread.
fn set_update_menu_title(title: &str) {
  {
    let mut pending = get_pending_menu_update().lock().unwrap();
    *pending = Some((title.to_string(), false));
  }
  dispatch_apply_update_menu();
}

fn update_menu_for_update() {
  let info = {
    let state = updater::get_update_state().lock().unwrap();
    state.clone()
  };
  if let Some(info) = info {
    {
      let mut pending = get_pending_menu_update().lock().unwrap();
      *pending = Some((format!("Update to {} ...", info.latest_version), true));
    }
    dispatch_apply_update_menu();
  }
}

fn dispatch_apply_update_menu() {
  let addr = *get_delegate_ptr().lock().unwrap();
  if addr == 0 {
    return;
  }
  let ptr = addr as *mut AnyObject;
  unsafe {
    let _: () = msg_send![ptr, performSelectorOnMainThread: sel!(applyUpdateMenu:), withObject: std::ptr::null::<AnyObject>(), waitUntilDone: false];
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

  TRAY.get_or_init(|| {
    Mutex::new(TrayState {
      window: std::ptr::null_mut(),
      view: std::ptr::null_mut(),
      button: std::ptr::null_mut(),
      menu: std::ptr::null_mut(),
      status_item: std::ptr::null_mut(),
      update_item: std::ptr::null_mut(),
    })
  });
  TERM_STATE.get_or_init(|| Mutex::new(vt100::Parser::new(24, 80, 0)));
  PTY_HANDLE.get_or_init(|| Mutex::new(None));

  let mtm = MainThreadMarker::new().expect("must be called on the main thread");

  let app = NSApplication::sharedApplication(mtm);
  app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

  let status_bar = NSStatusBar::systemStatusBar();
  let status_item: Retained<NSStatusItem> = status_bar.statusItemWithLength(-1.0); // NSVariableStatusItemLength

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

    // Store button pointer for window positioning
    {
      let mut tray = TRAY.get().unwrap().lock().unwrap();
      tray.button = Retained::as_ptr(&button) as *mut AnyObject;
    }
  }

  // Left-click: toggle window via button action
  let delegate = TrayDelegate::new();
  let target: &AnyObject =
    unsafe { &*(delegate.as_ref() as *const TrayDelegate as *const AnyObject) };

  // Store delegate pointer for main-thread dispatch from background threads
  {
    let mut dp = get_delegate_ptr().lock().unwrap();
    *dp = target as *const AnyObject as usize;
  }

  if let Some(button) = status_item.button(mtm) {
    unsafe {
      button.setTarget(Some(target));
      button.setAction(Some(sel!(toggleTerminal:)));
    }
  }

  // Build right-click menu with all controls
  let menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(""));

  let color_item = unsafe {
    NSMenuItem::initWithTitle_action_keyEquivalent(
      mtm.alloc(),
      &NSString::from_str("Change Color"),
      Some(sel!(changeColor:)),
      &NSString::from_str("c"),
    )
  };
  unsafe { color_item.setTarget(Some(target)) };
  menu.addItem(&color_item);

  let view_item = unsafe {
    NSMenuItem::initWithTitle_action_keyEquivalent(
      mtm.alloc(),
      &NSString::from_str("Change View"),
      Some(sel!(changeView:)),
      &NSString::from_str("v"),
    )
  };
  unsafe { view_item.setTarget(Some(target)) };
  menu.addItem(&view_item);

  menu.addItem(&NSMenuItem::separatorItem(mtm));

  let inc_item = unsafe {
    NSMenuItem::initWithTitle_action_keyEquivalent(
      mtm.alloc(),
      &NSString::from_str("Increase Interval"),
      Some(sel!(incInterval:)),
      &NSString::from_str("+"),
    )
  };
  unsafe { inc_item.setTarget(Some(target)) };
  menu.addItem(&inc_item);

  let dec_item = unsafe {
    NSMenuItem::initWithTitle_action_keyEquivalent(
      mtm.alloc(),
      &NSString::from_str("Decrease Interval"),
      Some(sel!(decInterval:)),
      &NSString::from_str("-"),
    )
  };
  unsafe { dec_item.setTarget(Some(target)) };
  menu.addItem(&dec_item);

  menu.addItem(&NSMenuItem::separatorItem(mtm));

  let update_item = unsafe {
    NSMenuItem::initWithTitle_action_keyEquivalent(
      mtm.alloc(),
      &NSString::from_str("Check for Updates"),
      Some(sel!(checkForUpdates:)),
      &NSString::from_str(""),
    )
  };
  unsafe { update_item.setTarget(Some(target)) };
  menu.addItem(&update_item);

  let restart_item = unsafe {
    NSMenuItem::initWithTitle_action_keyEquivalent(
      mtm.alloc(),
      &NSString::from_str("Restart"),
      Some(sel!(restartApp:)),
      &NSString::from_str("r"),
    )
  };
  unsafe { restart_item.setTarget(Some(target)) };
  menu.addItem(&restart_item);

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

  // Store pointers for right-click handler
  {
    let mut tray = TRAY.get().unwrap().lock().unwrap();
    tray.menu = Retained::as_ptr(&menu) as *mut NSMenu;
    tray.status_item = Retained::as_ptr(&status_item) as *mut NSStatusItem;
    tray.update_item = Retained::as_ptr(&update_item) as *mut NSMenuItem;
  }

  // Check for updates in background on launch
  updater::check_in_background();
  // After the check completes, the update menu item will be updated
  std::thread::spawn(|| {
    // Wait for the background check to finish, then update menu
    std::thread::sleep(std::time::Duration::from_secs(12));
    update_menu_for_update();
  });

  // Right-click event monitor: temporarily set the menu on the status item
  // so macOS shows it, then remove it so left-click fires the action again.
  let monitor = unsafe {
    let block = block2::RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
      let (menu_ptr, si_ptr) = {
        let t = TRAY.get().unwrap().lock().unwrap();
        (t.menu, t.status_item)
      };
      if menu_ptr.is_null() || si_ptr.is_null() {
        return event.as_ptr();
      }

      // Temporarily set the menu so macOS shows it on this click
      let _: () = msg_send![si_ptr, setMenu: menu_ptr];

      // After the menu closes, remove it so left-click works again.
      // performSelector:withObject:afterDelay: runs after the current
      // event cycle (menu tracking) completes.
      let _: () = msg_send![si_ptr, performSelector: sel!(setMenu:), withObject: std::ptr::null::<AnyObject>(), afterDelay: 0.0f64];

      event.as_ptr()
    });
    NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::RightMouseDown, &block)
  };

  std::mem::forget(delegate);
  std::mem::forget(status_item);
  std::mem::forget(menu);
  std::mem::forget(update_item);
  std::mem::forget(monitor);

  app.run();

  Ok(())
}
