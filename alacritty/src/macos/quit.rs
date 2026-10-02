//! Ask before the app quits while a tab still runs a process.

use std::mem::{self, MaybeUninit};
use std::{process, ptr};

use libc::{PROC_PIDTBSDINFO, c_int, pid_t, proc_bsdinfo, proc_listchildpids, proc_pidinfo};
use log::warn;
use objc2::ffi::class_addMethod;
use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationTerminateReply,
};
use objc2_foundation::{NSString, ns_string};

use crate::macos::proc;

/// Upper bound for the children of one process, the terminal has one per tab.
const MAX_CHILDREN: usize = 1024;

type ShouldTerminate =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSApplicationTerminateReply;

/// Make every quit path ask first when a tab is busy.
///
/// Cmd+Q, the menu item and the Dock all end in `terminate:`, and winit's delegate has no
/// `applicationShouldTerminate:`, so the method is added to its class here.
pub fn confirm_quit() {
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
        return NSApplicationTerminateReply::TerminateNow;
    }

    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Quit Alacritty?"));
    alert.setInformativeText(&NSString::from_str(&quit_message(&names)));
    alert.addButtonWithTitle(ns_string!("Quit"));
    alert.addButtonWithTitle(ns_string!("Cancel"));

    if alert.runModal() == NSAlertFirstButtonReturn {
        NSApplicationTerminateReply::TerminateNow
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
