//! Which machine this is, by the id its operating system keeps for it. It
//! lives here because this crate is the one place the workspace allows unsafe
//! Rust for native calls.

/// This machine's hardware id: the platform UUID on macOS, the machine id on
/// Linux, the `MachineGuid` on Windows. Copying a home folder, a profile or a
/// configuration folder to another machine never copies it. `None` when the
/// machine has none, or it cannot be read; read directly, without starting a
/// program.
#[must_use]
pub fn hardware_id() -> Option<String> {
    platform()
        .map(|id| id.trim().to_owned())
        .filter(|id| usable(id))
}

/// Whether `id` names a machine: not empty, not the placeholder a Linux
/// system writes before its first boot, not all zeros.
fn usable(id: &str) -> bool {
    !id.is_empty()
        && id != "uninitialized"
        && id.chars().any(|c| c.is_ascii_alphanumeric() && c != '0')
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn platform() -> Option<String> {
    use std::ffi::c_long;
    use std::fmt::Write as _;
    #[repr(C)]
    struct Timespec {
        seconds: c_long,
        nanoseconds: c_long,
    }
    unsafe extern "C" {
        fn gethostuuid(id: *mut u8, wait: *const Timespec) -> i32;
    }
    let mut id = [0_u8; 16];
    let wait = Timespec {
        seconds: 1,
        nanoseconds: 0,
    };
    // SAFETY: the call writes the 16 bytes of `id` and reads `wait`, both
    // locals that outlive it.
    if unsafe { gethostuuid(id.as_mut_ptr(), &raw const wait) } != 0 {
        return None;
    }
    // As `ioreg` shows IOPlatformUUID: upper case, in five groups.
    let mut text = String::with_capacity(36);
    for (index, byte) in id.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        let _ = write!(text, "{byte:02X}");
    }
    Some(text)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform() -> Option<String> {
    let read = |path| std::fs::read_to_string(path).ok();
    lasting_machine_id(
        // The second is where systems without systemd keep it.
        [read("/etc/machine-id"), read("/var/lib/dbus/machine-id")],
        read("/run/machine-id"),
    )
}

/// The first usable Linux machine id of `kept` that lasts. On a read-only
/// root systemd makes up one for each boot in `/run/machine-id` and lays it
/// over `/etc/machine-id`: an id equal to `transient` is that one, which
/// would make the machine a new one at every boot.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn lasting_machine_id(
    kept: impl IntoIterator<Item = Option<String>>,
    transient: Option<String>,
) -> Option<String> {
    let transient = transient.map(|id| id.trim().to_owned());
    kept.into_iter()
        .flatten()
        .map(|id| id.trim().to_owned())
        .find(|id| usable(id) && transient.as_ref() != Some(id))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn platform() -> Option<String> {
    // `HKEY_LOCAL_MACHINE`, sign-extended as the Windows headers define it.
    const LOCAL_MACHINE: isize = -2_147_483_646;
    const STRING_VALUE: u32 = 0x0000_0002;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let subkey = wide("SOFTWARE\\Microsoft\\Cryptography");
    let value = wide("MachineGuid");
    let mut data = [0_u16; 64];
    let mut size = u32::try_from(std::mem::size_of_val(&data)).ok()?;
    // SAFETY: the names are NUL-terminated and outlive the call, which writes
    // at most `size` bytes into `data` and the length it wrote into `size`.
    let status = unsafe {
        RegGetValueW(
            LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            STRING_VALUE,
            std::ptr::null_mut(),
            data.as_mut_ptr().cast(),
            &raw mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let length = (usize::try_from(size).ok()? / 2).min(data.len());
    let text = String::from_utf16(&data[..length]).ok()?;
    Some(text.trim_end_matches('\0').to_owned())
}

#[cfg(not(any(unix, windows)))]
fn platform() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_and_blanks_name_no_machine() {
        for id in ["", "uninitialized", "00000000-0000-0000-0000-000000000000"] {
            assert!(!usable(id), "{id:?}");
        }
        assert!(usable("0EC1973A-0000-4000-8000-000000000001"));
        assert!(usable("4c4c4544004a4b1080354ac04f4e3332"));
    }

    #[test]
    fn a_linux_machine_id_made_up_for_one_boot_names_no_machine() {
        let id = |text: &str| Some(text.to_owned());
        let kept = "4c4c4544004a4b1080354ac04f4e3332";
        assert_eq!(
            lasting_machine_id([id(&format!("{kept}\n")), None], None).as_deref(),
            Some(kept)
        );
        // Laid over /etc/machine-id from /run: the same id in both.
        let booted = "0d3c1e2f6a7b48c9a1b2c3d4e5f60718";
        assert_eq!(
            lasting_machine_id([id(booted), id(booted)], id(&format!("{booted}\n"))),
            None
        );
        // A lasting one beside it still counts.
        assert_eq!(
            lasting_machine_id([id(booted), id(kept)], id(booted)).as_deref(),
            Some(kept)
        );
        assert_eq!(lasting_machine_id([id("uninitialized"), None], None), None);
    }

    #[test]
    fn this_machine_has_one_id_that_stays_put() {
        let id = hardware_id();
        assert_eq!(id, hardware_id());
        // Every desktop system the tests run on keeps one.
        if cfg!(any(target_os = "macos", windows)) {
            let id = id.clone().expect("this machine's hardware id");
            assert_eq!(id.len(), 36, "{id}");
            assert_eq!(id.matches('-').count(), 4, "{id}");
        }
        // The very id `ioreg` names, read without starting it.
        if cfg!(target_os = "macos")
            && let Ok(output) = std::process::Command::new("/usr/sbin/ioreg")
                .args(["-rd1", "-c", "IOPlatformExpertDevice"])
                .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            let named = text
                .lines()
                .find_map(|line| line.split_once("\"IOPlatformUUID\" = \""))
                .and_then(|(_, rest)| rest.split('"').next())
                .map(str::to_owned);
            assert_eq!(id, named);
        }
    }
}
