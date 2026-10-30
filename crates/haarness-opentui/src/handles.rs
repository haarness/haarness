//! Process handle hygiene on Windows. It lives here because this crate is the
//! one place the workspace allows unsafe Rust for native calls.

/// Stop the standard handles this process was given from passing to the
/// processes it starts. Windows hands every inheritable handle to a child,
/// whatever its own stdio is, so a long-lived server started by `haarness run`
/// (or by Desktop) held the caller's output pipe: `haarness run … | tail`,
/// or `powershell` capturing the output, waited until that server exited.
/// Children given `Stdio::inherit()` still get them: the standard library
/// duplicates each one for the child.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn keep_standard_handles_from_children() {
    use std::os::windows::io::AsRawHandle as _;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut core::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    for handle in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        if !handle.is_null() {
            // SAFETY: the handle is this process's own standard handle (or an
            // invalid value, which the call rejects); only its inherit flag
            // changes, and the result is not needed.
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Nothing to do elsewhere: Unix children get only the descriptors they are
/// given, once the inherited ones are close-on-exec.
#[cfg(not(windows))]
pub fn keep_standard_handles_from_children() {}

/// Whether reading standard input would return at once: something waits in
/// it, it ends, or it is a file or device. A pipe a parent opened and never
/// writes to or closes (a spawner that leaves stdin open) is not.
#[cfg(windows)]
#[allow(unsafe_code)]
#[must_use]
pub fn stdin_ready_now() -> bool {
    use std::os::windows::io::AsRawHandle as _;
    const FILE_TYPE_PIPE: u32 = 3;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileType(handle: *mut core::ffi::c_void) -> u32;
        fn PeekNamedPipe(
            pipe: *mut core::ffi::c_void,
            buffer: *mut core::ffi::c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            message: *mut u32,
        ) -> i32;
    }
    let handle = std::io::stdin().as_raw_handle();
    if handle.is_null() {
        return true;
    }
    // SAFETY: the handle is this process's own standard input; the call only
    // reports its kind.
    if unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
        return true;
    }
    let mut available = 0;
    // SAFETY: as above; nothing is read, only the count of waiting bytes is
    // written, into a local that outlives the call.
    let peeked = unsafe {
        PeekNamedPipe(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &raw mut available,
            std::ptr::null_mut(),
        )
    };
    // A pipe that cannot be peeked has been closed: reading it ends at once.
    peeked == 0 || available > 0
}
