//! Starting with the computer — off unless the person asks for it.
//!
//! On Windows this is one value under the current user's `Run` key: no
//! installer, no service, nothing left behind that the person cannot see in
//! Task Manager's startup list.

/// The name the entry appears under.
pub const ENTRY: &str = "Nearscreen Receiver";

#[cfg(windows)]
mod windows_impl {
    use super::ENTRY;
    use anyhow::{bail, Context, Result};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };

    const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn open(access: u32) -> Result<HKEY> {
        let mut key = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                windows::Win32::System::Registry::REG_SAM_FLAGS(access),
                &mut key,
            )
        };
        if status != ERROR_SUCCESS {
            bail!("cannot open the startup list ({status:?})");
        }
        Ok(key)
    }

    /// The command that would be run at login: this very binary.
    fn command() -> Result<String> {
        let exe = std::env::current_exe().context("cannot find this program on disk")?;
        Ok(format!("\"{}\"", exe.display()))
    }

    pub fn is_enabled() -> bool {
        let Ok(key) = open(KEY_READ.0) else {
            return false;
        };
        let mut size = 0u32;
        let name = wide(ENTRY);
        let status = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut size),
            )
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        status == ERROR_SUCCESS
    }

    pub fn set(enabled: bool) -> Result<()> {
        let key = open(KEY_WRITE.0)?;
        let name = wide(ENTRY);
        let status = if enabled {
            let value = wide(&command()?);
            let bytes =
                unsafe { std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2) };
            unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes)) }
        } else {
            unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) }
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        // Removing something that was never there is not a failure.
        if status != ERROR_SUCCESS && enabled {
            bail!("cannot change the startup list ({status:?})");
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use windows_impl::{is_enabled, set};

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set(_enabled: bool) -> anyhow::Result<()> {
    anyhow::bail!("starting at login is not wired up on this platform yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turning it on and off again must leave the computer exactly as it was.
    /// Only run by hand: it writes to the real startup list.
    #[test]
    #[ignore = "changes this computer's startup list"]
    fn switching_it_on_and_off_leaves_no_trace() {
        let before = is_enabled();
        set(true).unwrap();
        assert!(is_enabled());
        set(false).unwrap();
        assert!(!is_enabled());
        if before {
            set(true).unwrap();
        }
    }
}
