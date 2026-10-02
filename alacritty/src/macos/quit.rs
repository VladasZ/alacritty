//! Ask before the app quits while a tab still runs a process, and hand the
//! quit and the reopen over to the event loop, which parks the windows.

use std::ffi::CString;
use std::mem::{self, MaybeUninit};
use std::sync::OnceLock;
use std::{process, ptr};

use libc::{PROC_PIDTBSDINFO, c_int, pid_t, proc_bsdinfo, proc_listchildpids, proc_pidinfo};
use log::warn;
use objc2::ffi::class_addMethod;
use objc2::runtime::{AnyObject, Bool, Imp, Sel};
use objc2::{Encode, MainThreadMarker, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationTerminateReply,
};
use objc2_foundation::{NSString, ns_string};
use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType, TabAction};
use crate::macos::proc;

/// Upper bound for the children of one process, the terminal has one per tab.
const MAX_CHILDREN: usize = 1024;

type ShouldTerminate =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSApplicationTerminateReply;

type ShouldHandleReopen =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, Bool) -> Bool;

/// Where the delegate methods send the quit and the reopen.
static PROXY: OnceLock<EventLoopProxy<Event>> = OnceLock::new();

/// Make every quit path ask first when a tab is busy, then park the windows
/// instead of ending the process, and show them again on a reopen.
///
/// Cmd+Q, the menu item and the Dock all end in `terminate:`, and winit's delegate has no
/// `applicationShouldTerminate:`, so the method is added to its class here. The same goes
/// for `applicationShouldHandleReopen:hasVisibleWindows:`, sent on a Dock click or `open -a`.
pub fn hook_quit_and_reopen(proxy: EventLoopProxy<Event>) {
    if PROXY.set(proxy).is_err() {
        warn!("Quit is already hooked");
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() else {
        warn!("No application delegate, quit will not ask for confirmation");
        return;
    };

    let delegate: &AnyObject = delegate.as_ref();
    let class = ptr::from_ref(delegate.class()).cast_mut();
    let added = unsafe {
        let imp = mem::transmute::<ShouldTerminate, Imp>(should_terminate);
        class_addMethod(class, sel!(applicationShouldTerminate:), imp, c"Q@:@".as_ptr())
    };
    if !added.as_bool() {
        warn!("Could not hook application quit, quit will not ask for confirmation");
    }

    // BOOL is a char on Intel and a bool on Apple silicon, so the type string is built.
    let bool_type = Bool::ENCODING;
    let Ok(types) = CString::new(format!("{bool_type}@:@{bool_type}")) else {
        return;
    };
    let added = unsafe {
        let imp = mem::transmute::<ShouldHandleReopen, Imp>(should_handle_reopen);
        let sel = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
        class_addMethod(class, sel, imp, types.as_ptr())
    };
    if !added.as_bool() {
        warn!("Could not hook application reopen, closed windows will not come back");
    }
}

fn send(payload: EventType) -> bool {
    let Some(proxy) = PROXY.get() else {
        return false;
    };
    match proxy.send_event(Event::new(payload, None)) {
        Ok(()) => true,
        Err(err) => {
            warn!("Event loop is gone: {err}");
            false
        },
    }
}

unsafe extern "C-unwind" fn should_handle_reopen(
    _this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
    _has_visible_windows: Bool,
) -> Bool {
    send(EventType::Tab(TabAction::Unpark));
    Bool::YES
}

/// The event loop parks the windows and ends the process itself once nothing is left.
fn park_or_terminate() -> NSApplicationTerminateReply {
    if send(EventType::Quit) {
        NSApplicationTerminateReply::TerminateCancel
    } else {
        NSApplicationTerminateReply::TerminateNow
    }
}

unsafe extern "C-unwind" fn should_terminate(
    _this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) -> NSApplicationTerminateReply {
    let names = running_processes();
    let Some(mtm) = MainThreadMarker::new() else {
        return NSApplicationTerminateReply::TerminateNow;
    };
    if names.is_empty() {
        return park_or_terminate();
    }

    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Quit Alacritty?"));
    alert.setInformativeText(&NSString::from_str(&quit_message(&names)));
    alert.addButtonWithTitle(ns_string!("Quit"));
    alert.addButtonWithTitle(ns_string!("Cancel"));

    if alert.runModal() == NSAlertFirstButtonReturn {
        park_or_terminate()
    } else {
        NSApplicationTerminateReply::TerminateCancel
    }
}

fn quit_message(names: &[String]) -> String {
    match names {
        [name] => format!("{name} is still running in a tab. Quitting stops it."),
        _ => format!(
            "{} tabs still run a process: {}. Quitting stops them.",
            names.len(),
            names.join(", ")
        ),
    }
}

/// Foreground process names of the tabs where something other than the shell runs.
fn running_processes() -> Vec<String> {
    children(process::id() as pid_t).into_iter().filter_map(foreground_process).collect()
}

/// Name of the process a tab runs in the foreground, `None` while its shell is idle.
fn foreground_process(tab: pid_t) -> Option<String> {
    // Without a configured shell the tab's child is `login`. It runs as root, so its info
    // cannot be read, and the shell is its child.
    let shell = match bsd_info(tab) {
        Some(shell) => shell,
        None => children(tab).into_iter().find_map(bsd_info)?,
    };

    // Only a shell leads its own group, helpers like the updater's curl do not.
    if shell.pbi_pgid != shell.pbi_pid || shell.e_tpgid == 0 || shell.e_tpgid == shell.pbi_pgid {
        return None;
    }

    let name = proc::name(shell.e_tpgid as c_int).ok();
    Some(name.unwrap_or_else(|| String::from("a process")))
}

fn children(pid: pid_t) -> Vec<pid_t> {
    let mut pids: Vec<pid_t> = vec![0; MAX_CHILDREN];
    let size = (pids.len() * mem::size_of::<pid_t>()) as c_int;
    let count = unsafe { proc_listchildpids(pid, pids.as_mut_ptr().cast(), size) };
    pids.truncate(count.max(0) as usize);
    pids.retain(|pid| *pid > 0);
    pids
}

fn bsd_info(pid: pid_t) -> Option<proc_bsdinfo> {
    let mut info = MaybeUninit::<proc_bsdinfo>::uninit();
    let size = mem::size_of::<proc_bsdinfo>() as c_int;
    let read = unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    (read == size).then(|| unsafe { info.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_names_one_process() {
        let names = [String::from("vim")];
        assert_eq!(quit_message(&names), "vim is still running in a tab. Quitting stops it.");
    }

    #[test]
    fn message_counts_many_processes() {
        let names = [String::from("vim"), String::from("cargo")];
        assert_eq!(
            quit_message(&names),
            "2 tabs still run a process: vim, cargo. Quitting stops them."
        );
    }
}
