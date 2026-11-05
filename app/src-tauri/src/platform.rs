use objc2_app_kit::NSWorkspace;
use pa_core::Row;
use std::{ffi::c_void, process::Command, sync::mpsc, thread, time::Duration};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event: u32) -> f64;
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        text: *const i8,
        encoding: u32,
    ) -> *const c_void;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(value: *const c_void) -> bool;
    fn CFRelease(value: *const c_void);
}

pub fn idle_secs() -> u64 {
    unsafe { CGEventSourceSecondsSinceLastEventType(1, u32::MAX) as u64 }
}

pub fn locked() -> bool {
    unsafe {
        let dictionary = CGSessionCopyCurrentDictionary();
        if dictionary.is_null() {
            return false;
        }
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            c"CGSSessionScreenIsLocked".as_ptr(),
            0x0800_0100,
        );
        let value = CFDictionaryGetValue(dictionary, key);
        let locked = !value.is_null() && CFBooleanGetValue(value);
        CFRelease(key);
        CFRelease(dictionary);
        locked
    }
}

fn terminal_frontmost() -> bool {
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .and_then(|app| app.bundleIdentifier())
            .is_some_and(|id| id.to_string() == "com.apple.Terminal")
    })
}

pub struct TerminalQuery(mpsc::SyncSender<mpsc::Sender<Option<String>>>);

impl TerminalQuery {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<mpsc::Sender<Option<String>>>(0);
        thread::spawn(move || {
            for reply in rx {
                let result = Command::new("osascript")
                    .args([
                        "-e",
                        "tell application \"Terminal\" to tty of selected tab of front window",
                    ])
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| String::from_utf8(output.stdout).ok())
                    .map(|tty| tty.trim().to_owned())
                    .filter(|tty| !tty.is_empty());
                let _ = reply.send(result);
            }
        });
        Self(tx)
    }

    pub fn selected_tty(&self, rows: &[Row]) -> Option<String> {
        if !rows
            .iter()
            .any(|row| row.alert_color().is_some() && row.tty.is_some())
            || !terminal_frontmost()
        {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        self.0.try_send(tx).ok()?;
        let tty = rx.recv_timeout(Duration::from_millis(500)).ok().flatten();
        terminal_frontmost().then_some(tty).flatten()
    }
}

pub fn focus(row: &Row) {
    let (program, args): (&str, Vec<String>) = match (row.surface.as_str(), &row.tty) {
        ("terminal", Some(tty)) => (
            "osascript",
            vec![
                "-e".into(),
                r#"on run argv
    set targetTTY to item 1 of argv
    tell application "Terminal"
        repeat with w in windows
            repeat with t in tabs of w
                if tty of t is targetTTY then
                    set selected of t to true
                    set index of w to 1
                    activate
                    return
                end if
            end repeat
        end repeat
    end tell
end run"#
                    .into(),
                "--".into(),
                tty.clone(),
            ],
        ),
        ("claude-desktop", _) => (
            "open",
            vec!["-b".into(), "com.anthropic.claudefordesktop".into()],
        ),
        _ => return,
    };
    thread::spawn(move || match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => eprintln!("focus: {}", String::from_utf8_lossy(&output.stderr).trim()),
        Err(error) => eprintln!("focus: {error}"),
    });
}
