//! Windows descendants join their owner before application code can execute.
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::process::Child;
use std::time::Instant;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::JobObjects::*;

#[derive(Debug)]
pub(crate) struct NativeProcessJob {
    handle: File,
    assigned: bool,
}

impl NativeProcessJob {
    pub(crate) fn new() -> io::Result<Self> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { File::from_raw_handle(raw) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle, assigned: false })
    }
    pub(crate) fn assign_and_resume(
        &mut self,
        child: &Child,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
        use windows_sys::Win32::System::Threading::{
            OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
        };
        check_spawn_deadline(deadline)?;
        if unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), child.as_raw_handle()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        self.assigned = true;
        check_spawn_deadline(deadline)?;
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let _snapshot = unsafe { File::from_raw_handle(raw) };
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut present = unsafe { Thread32First(raw, &mut entry) };
        let mut primary = None;
        while present != 0 {
            if entry.th32OwnerProcessID == child.id()
                && primary.replace(entry.th32ThreadID).is_some()
            {
                return Err(io::Error::other(
                    "suspended native child unexpectedly has multiple threads",
                ));
            }
            present = unsafe { Thread32Next(raw, &mut entry) };
        }
        let primary = primary
            .ok_or_else(|| io::Error::other("suspended native primary thread is missing"))?;
        let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, primary) };
        if raw_thread.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _thread = unsafe { File::from_raw_handle(raw_thread) };
        check_spawn_deadline(deadline)?;
        if unsafe { ResumeThread(raw_thread) } != 1 {
            return Err(io::Error::other(
                "native primary thread suspend count was not exactly one",
            ));
        }
        check_spawn_deadline(deadline)
    }
    pub(crate) fn is_assigned(&self) -> bool {
        self.assigned
    }
    pub(crate) fn active(&self) -> io::Result<u32> {
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }
    pub(crate) fn terminate(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.handle.as_raw_handle(), 0xdead) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub(crate) fn check_spawn_deadline(deadline: Option<Instant>) -> io::Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "original native spawn deadline exceeded",
        ))
    } else {
        Ok(())
    }
}
