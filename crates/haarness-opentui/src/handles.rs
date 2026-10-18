//! Process handle hygiene on Windows. It lives here because this crate is the
//! one place the workspace allows unsafe Rust for native calls.

/// Stop the standard handles this process was given from passing to the
/// processes it starts. Windows hands every inheritable handle to a child,
/// whatever its own stdio is, so a long-lived server started by `haarness run`
/// (or by Desktop) held the caller's output pipe: `haarness run … | tail`,
/// or PowerShell capturing the output, waited until that server exited.
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
