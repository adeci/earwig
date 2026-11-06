//! Named pipe transport (Windows).
//!
//! Message-type pipes, so that an empty message can stand in for the
//! half-close a pipe lacks: a read that returns it is end of input. Handles
//! are overlapped, which lets writes give up after [`WRITE_TIMEOUT`]. Only the
//! user who runs the daemon may open the pipe, and only locally.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE,
    LocalFree, TRUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken,
};

use super::WRITE_TIMEOUT;
use crate::win32::{is, owned, wide};

/// Room for a burst of events on their way to a slow client.
const BUFFER_SIZE: u32 = 64 * 1024;
/// How long a client waits for a busy pipe to offer a free instance.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// One end of a pipe connection. Reads and writes go through `&Conn`; each
/// direction has one operation in flight at a time.
pub(super) struct Conn {
    pipe: OwnedHandle,
    read_done: OwnedHandle,
    write_done: OwnedHandle,
    write_timeout: Option<Duration>,
}

impl Conn {
    fn new(pipe: OwnedHandle, write_timeout: Option<Duration>) -> io::Result<Self> {
        Ok(Self {
            pipe,
            read_done: event()?,
            write_done: event()?,
            write_timeout,
        })
    }

    fn handle(&self) -> HANDLE {
        self.pipe.as_raw_handle()
    }

    /// Run one overlapped operation that `start` begins and wait for it, at
    /// most `timeout`; returns the bytes it moved.
    fn overlapped(
        &self,
        done: &OwnedHandle,
        timeout: Option<Duration>,
        start: impl FnOnce(*mut OVERLAPPED) -> i32,
    ) -> io::Result<u32> {
        // SAFETY: OVERLAPPED is plain data; all zeroes is its initial state.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = done.as_raw_handle();
        if start(&raw mut overlapped) == FALSE {
            let err = io::Error::last_os_error();
            if !is(&err, ERROR_IO_PENDING) && !is(&err, ERROR_MORE_DATA) {
                return Err(err);
            }
        }
        let millis = timeout.map_or(INFINITE, |t| {
            u32::try_from(t.as_millis()).unwrap_or(INFINITE - 1)
        });
        let mut moved = 0;
        // SAFETY: `overlapped` lives until the operation completed, which
        // this call or the cancellation below waits for.
        let ok = unsafe {
            GetOverlappedResultEx(
                self.handle(),
                &raw const overlapped,
                &raw mut moved,
                millis,
                FALSE,
            )
        };
        if ok != FALSE {
            return Ok(moved);
        }
        let err = io::Error::last_os_error();
        if is(&err, ERROR_MORE_DATA) {
            // A message longer than the buffer: the rest comes with the next
            // read.
            return Ok(moved);
        }
        if is(&err, WAIT_TIMEOUT) {
            // SAFETY: as above; waiting makes sure the cancelled operation
            // no longer touches `overlapped` or the buffer.
            unsafe {
                CancelIoEx(self.handle(), &raw const overlapped);
                GetOverlappedResult(self.handle(), &raw const overlapped, &raw mut moved, TRUE);
            }
            return Err(io::ErrorKind::TimedOut.into());
        }
        Err(err)
    }

    /// Wait for a client to open this server end.
    fn wait_for_client(&self) -> io::Result<()> {
        let connected = self.overlapped(&self.read_done, None, |overlapped| {
            // SAFETY: a pipe handle and a live OVERLAPPED.
            unsafe { ConnectNamedPipe(self.handle(), overlapped) }
        });
        match connected {
            Err(e) if is(&e, ERROR_PIPE_CONNECTED) => Ok(()),
            other => other.map(drop),
        }
    }
}

impl Read for &Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let read = self.overlapped(&self.read_done, None, |overlapped| {
            // SAFETY: `buf` outlives the operation (see `overlapped`).
            unsafe {
                ReadFile(
                    self.handle(),
                    buf.as_mut_ptr(),
                    len,
                    ptr::null_mut(),
                    overlapped,
                )
            }
        });
        match read {
            Ok(n) => Ok(n as usize),
            Err(e) if is(&e, ERROR_BROKEN_PIPE) || is(&e, ERROR_PIPE_NOT_CONNECTED) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

impl Write for &Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let written = self.overlapped(&self.write_done, self.write_timeout, |overlapped| {
            // SAFETY: `buf` outlives the operation (see `overlapped`).
            unsafe {
                WriteFile(
                    self.handle(),
                    buf.as_ptr(),
                    len,
                    ptr::null_mut(),
                    overlapped,
                )
            }
        })?;
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

pub(super) struct Listener {
    name: Vec<u16>,
    security: SecurityDescriptor,
    /// The instance the next client connects to.
    pending: Conn,
}

impl Listener {
    pub(super) fn bind(path: &Path) -> anyhow::Result<Self> {
        let name = wide(path.as_os_str());
        let security = SecurityDescriptor::user_only()?;
        let pending = match instance(&name, &security, true) {
            Ok(pending) => pending,
            Err(e) if is(&e, ERROR_ACCESS_DENIED) => {
                anyhow::bail!("earwig is already running (pipe {} exists)", path.display())
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("creating {}", path.display())));
            }
        };
        Ok(Self {
            name,
            security,
            pending,
        })
    }

    pub(super) fn accept(&mut self) -> io::Result<Conn> {
        if let Err(e) = self.pending.wait_for_client() {
            // An instance whose client came and went cannot connect again.
            self.pending = instance(&self.name, &self.security, false)?;
            return Err(e);
        }
        // A connected instance serves only its client; the next one waits
        // for whoever comes after. Until it exists the connected one stays
        // pending, and the next call hands it out.
        let next = instance(&self.name, &self.security, false)?;
        Ok(std::mem::replace(&mut self.pending, next))
    }
}

/// A new server instance of the pipe `name`. The first one claims the name,
/// which fails if another daemon holds it.
fn instance(name: &[u16], security: &SecurityDescriptor, first: bool) -> io::Result<Conn> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).expect("small struct"),
        lpSecurityDescriptor: security.0,
        bInheritHandle: FALSE,
    };
    let first = if first {
        FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        0
    };
    // SAFETY: `name` is NUL-terminated and `attributes` points at a live
    // descriptor.
    let pipe = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            &raw const attributes,
        )
    };
    Conn::new(owned(pipe)?, Some(WRITE_TIMEOUT))
}

pub(super) fn connect(path: &Path) -> io::Result<Conn> {
    let name = wide(path.as_os_str());
    let deadline = Instant::now() + BUSY_WAIT;
    loop {
        // SAFETY: `name` is NUL-terminated. Identification level keeps the
        // server from acting as this user.
        let pipe = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        match owned(pipe) {
            Ok(pipe) => return Conn::new(pipe, None),
            Err(e) if is(&e, ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                // Every instance is taken until the daemon creates the next.
                // SAFETY: `name` is NUL-terminated.
                unsafe { WaitNamedPipeW(name.as_ptr(), 100) };
            }
            Err(e) => return Err(e),
        }
    }
}

/// End this side's input: an empty message.
pub(super) fn shutdown_write(conn: &Conn) -> io::Result<()> {
    conn.overlapped(&conn.write_done, conn.write_timeout, |overlapped| {
        // SAFETY: a zero-length write from a live OVERLAPPED.
        unsafe { WriteFile(conn.handle(), ptr::null(), 0, ptr::null_mut(), overlapped) }
    })
    .map(drop)
}

/// The SID of the user running this process, as `S-1-5-21-…`.
pub(crate) fn user_sid() -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: the pseudo handle of this process and an out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut len = 0;
    // SAFETY: asks for the size only.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &raw mut len,
        )
    };
    // u64s for the alignment of TOKEN_USER and the SID after it.
    let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buffer` holds `len` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            len,
            &raw mut len,
        )
    };
    if ok == FALSE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: filled in as a TOKEN_USER above.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut string = ptr::null_mut();
    // SAFETY: a valid SID; the string is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut string) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
    let sid = unsafe {
        let len = (0..).take_while(|&i| *string.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(string, len))
    };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(string.cast()) };
    Ok(sid)
}

/// A DACL that lets only the current user in.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is an immutable block owned by this value alone.
unsafe impl Send for SecurityDescriptor {}

impl SecurityDescriptor {
    fn user_only() -> io::Result<Self> {
        let sddl = wide(OsStr::new(&format!("D:P(A;;GA;;;{})", user_sid()?)));
        let mut descriptor = ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; the descriptor is freed on drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// A manual-reset event for overlapped completion.
fn event() -> io::Result<OwnedHandle> {
    // SAFETY: no attributes, no name.
    owned(unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) })
}
