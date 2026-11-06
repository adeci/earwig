//! Windows: the filter runs in its own job object, which every process it
//! starts joins. It starts suspended and runs only once it is in the job, so
//! nothing it starts can escape.

use std::io;
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command};
use std::ptr;

use windows_sys::Win32::Foundation::FALSE;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

use crate::win32::owned;

pub(super) struct Tree(OwnedHandle);

pub(super) fn spawn(mut command: Command) -> io::Result<(Child, Tree)> {
    // SAFETY: an unnamed job with default security.
    let job = owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
    let mut child = command.creation_flags(CREATE_SUSPENDED).spawn()?;
    // SAFETY: live job and process handles.
    let joined = if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) }
        == FALSE
    {
        Err(io::Error::last_os_error())
    } else {
        resume(child.id())
    };
    if let Err(e) = joined {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok((child, Tree(job)))
}

impl Tree {
    pub(super) fn kill(&self) {
        // SAFETY: a live job handle.
        unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
    }
}

/// Resume the threads of the suspended process `pid` (just its first one).
fn resume(pid: u32) -> io::Result<()> {
    // SAFETY: a snapshot of all threads; process id 0 is ignored for them.
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).expect("small struct"),
        cntUsage: 0,
        th32ThreadID: 0,
        th32OwnerProcessID: 0,
        tpBasePri: 0,
        tpDeltaPri: 0,
        dwFlags: 0,
    };
    // SAFETY: a live snapshot and an entry with its size set.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != FALSE;
    let mut resumed = false;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: a thread id from the snapshot.
            let thread =
                owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, FALSE, entry.th32ThreadID) })?;
            // SAFETY: a live thread handle with the right to resume.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            resumed = true;
        }
        // SAFETY: as for `Thread32First`.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != FALSE;
    }
    if resumed {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "no thread of process {pid} to resume"
        )))
    }
}
