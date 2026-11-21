//! Named pipes live in one namespace for the whole machine: these checks keep
//! other local users from talking to an editor, or from posing as one.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The `TOKEN_USER` of a process; u64s keep the SID pointer in it aligned.
struct ProcessUser(Vec<u64>);

impl ProcessUser {
    fn of(process: HANDLE) -> io::Result<Self> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: a process handle the caller owns; the token is closed below.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        // SAFETY: a size query, then a buffer of that size.
        let ok = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
            let ok =
                GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            (ok != 0).then_some(buffer)
        };
        ok.map(Self).ok_or_else(io::Error::last_os_error)
    }

    fn current() -> io::Result<Self> {
        // SAFETY: the pseudo handle of this process needs no closing.
        Self::of(unsafe { GetCurrentProcess() })
    }

    fn sid(&self) -> PSID {
        // SAFETY: filled by GetTokenInformation(TokenUser).
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

/// Security attributes whose DACL admits this user only: the default one
/// also lets Everyone read.
pub(crate) struct OwnerOnly {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl OwnerOnly {
    pub(crate) fn new() -> io::Result<Self> {
        let user = ProcessUser::current()?;
        let mut sid_text: *mut u16 = std::ptr::null_mut();
        // SAFETY: a valid SID; the string is freed below.
        if unsafe { ConvertSidToStringSidW(user.sid(), &mut sid_text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
        let sid = unsafe {
            let len = (0..).take_while(|&i| *sid_text.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, len));
            LocalFree(sid_text as HLOCAL);
            sid
        };
        // Protected, so no inherited ACE widens it.
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SDDL string; the descriptor is freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }

    pub(crate) fn as_raw(&mut self) -> *mut c_void {
        (&mut self.attributes as *mut SECURITY_ATTRIBUTES).cast()
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.descriptor as HLOCAL) };
    }
}

/// Whether `pipe` is served by process `pid`, run by this user: the name alone
/// proves nothing, since anyone can create a pipe with it first.
pub(crate) fn check_server(pipe: &impl AsRawHandle, pid: u32) -> io::Result<()> {
    let refuse = |why: String| io::Error::new(io::ErrorKind::PermissionDenied, why);
    let mut server = 0;
    // SAFETY: a connected client end of a pipe.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut server) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if server != pid {
        return Err(refuse(format!(
            "the pipe of the Venturi window {pid} is served by process {server}"
        )));
    }
    // SAFETY: the handle is closed right after reading its user.
    let theirs = unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let user = ProcessUser::of(process);
        CloseHandle(process);
        user
    }
    .map_err(|e| refuse(format!("cannot tell who runs process {server}: {e}")))?;
    let ours = ProcessUser::current()?;
    // SAFETY: two valid SIDs.
    if unsafe { EqualSid(theirs.sid(), ours.sid()) } == 0 {
        return Err(refuse(format!("process {server} belongs to another user")));
    }
    Ok(())
}
