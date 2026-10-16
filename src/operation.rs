//! Cancellation, process-held mutation ownership and protected recovery state.

use crate::diagnostics::{AppError, AppResult, ErrorCode, Stage};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
pub struct Cancellation {
    flag: Arc<AtomicBool>,
    observe_console: bool,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn global() -> Self {
        Self {
            observe_console: true,
            ..Self::new()
        }
    }
    #[cfg(test)]
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
            || (self.observe_console
                && (INTERRUPTED.load(Ordering::Acquire) || crate::report::closed()))
    }
    pub fn check(&self) -> AppResult<()> {
        if self.is_cancelled() {
            Err(AppError::new(ErrorCode::Cancelled, Stage::State))
        } else {
            Ok(())
        }
    }
    pub fn wait(&self, duration: Duration) -> AppResult<()> {
        let start = Instant::now();
        loop {
            self.check()?;
            let remaining = duration.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(25)));
        }
    }

    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

pub fn install_console_handler() -> AppResult<()> {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(event: u32) -> windows_sys::core::BOOL {
        if event == 0 || event == 1 {
            INTERRUPTED.store(true, Ordering::Release);
            1
        } else {
            0
        }
    }
    static INSTALLED: OnceLock<AppResult<()>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| {
            if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0
                && unsafe { windows_sys::Win32::Foundation::GetLastError() }
                    != windows_sys::Win32::Foundation::ERROR_INVALID_HANDLE
            {
                Err(state_error())
            } else {
                Ok(())
            }
        })
        .clone()
}

pub fn state_directory() -> AppResult<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|root| PathBuf::from(root).join("ProxPilot"))
        .ok_or_else(state_error)
}

fn state_error() -> AppError {
    AppError::new(ErrorCode::StateUnavailable, Stage::State)
}

/// The open handle owns the lock; an abandoned filename is never an owner.
#[derive(Debug)]
pub struct MutationLock {
    _file: File,
}

impl MutationLock {
    pub fn acquire() -> AppResult<Self> {
        Self::acquire_at(&state_directory()?)
    }
    pub fn acquire_at(directory: &Path) -> AppResult<Self> {
        fs::create_dir_all(directory).map_err(|_| state_error())?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(directory.join("mutation.lock"))
            .map_err(|error| {
                if matches!(error.raw_os_error(), Some(32 | 33)) {
                    AppError::new(ErrorCode::Busy, Stage::State)
                } else {
                    state_error()
                }
            })?;
        Ok(Self { _file: file })
    }
}

const JOURNAL_HEADER: &[u8] = b"PPJR\x01";
const MAX_JOURNAL: usize = 1024 * 1024;

#[derive(Debug)]
pub struct RecoveryJournal {
    path: PathBuf,
}

impl RecoveryJournal {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn current() -> AppResult<Self> {
        Ok(Self::new(state_directory()?.join("recovery.bin")))
    }
    pub fn load<T: DeserializeOwned>(&self) -> AppResult<Option<T>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(state_error()),
        };
        let mut bytes = Vec::new();
        file.take((MAX_JOURNAL + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| state_error())?;
        if bytes.len() > MAX_JOURNAL || !bytes.starts_with(JOURNAL_HEADER) {
            return Err(state_error());
        }
        let plain = protect(&bytes[JOURNAL_HEADER.len()..], false)?;
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|_| state_error())
    }
    pub fn store<T: Serialize>(&self, value: &T) -> AppResult<()> {
        let plain = serde_json::to_vec(value).map_err(|_| state_error())?;
        if plain.len() > MAX_JOURNAL / 2 {
            return Err(state_error());
        }
        let cipher = protect(&plain, true)?;
        let parent = self.path.parent().ok_or_else(state_error)?;
        fs::create_dir_all(parent).map_err(|_| state_error())?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = self.path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // Only delete a temporary file this call actually created.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| state_error())?;
        let result = (|| {
            file.write_all(JOURNAL_HEADER)
                .and_then(|_| file.write_all(&cipher))
                .and_then(|_| file.sync_all())
                .map_err(|_| state_error())?;
            drop(file);
            atomic_replace(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    pub fn remove(&self) -> AppResult<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(state_error()),
        }
    }
}

fn protect(bytes: &[u8], encrypt: bool) -> AppResult<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    if bytes.is_empty() || bytes.len() > MAX_JOURNAL {
        return Err(state_error());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let entropy_bytes = b"ProxPilot.recovery.v1";
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len() as u32,
        pbData: entropy_bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if success == 0 {
        return Err(state_error());
    }
    if output.pbData.is_null() || output.cbData == 0 || output.cbData as usize > MAX_JOURNAL {
        if !output.pbData.is_null() {
            unsafe {
                LocalFree(output.pbData.cast());
            }
        }
        return Err(state_error());
    }
    let data =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(data)
}

fn atomic_replace(source: &Path, destination: &Path) -> AppResult<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(state_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::ErrorCode;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    fn fixture() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "proxpilot-operation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn cancelled_wait_stops_promptly_without_cancelling_another_operation() {
        let cancellation = Cancellation::new();
        let other = Cancellation::new();
        let trigger = cancellation.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            trigger.cancel();
        });
        let start = Instant::now();
        let error = cancellation.wait(Duration::from_secs(5)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Cancelled);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!other.is_cancelled());
        thread.join().unwrap();
    }

    #[test]
    fn modification_lock_is_owned_by_handle_not_stale_filename() {
        let root = fixture();
        let lock = MutationLock::acquire_at(&root).unwrap();
        assert_eq!(
            MutationLock::acquire_at(&root).unwrap_err().code,
            ErrorCode::Busy
        );
        drop(lock);
        let second = MutationLock::acquire_at(&root).unwrap();
        drop(second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn journal_is_encrypted_atomic_and_rejects_tampering() {
        let root = fixture();
        let path = root.join("recovery.bin");
        let journal = RecoveryJournal::new(path.clone());
        assert!(journal.load::<serde_json::Value>().unwrap().is_none());
        let first = serde_json::json!({"original":"secret-original-node", "pending":"node-b"});
        journal.store(&first).unwrap();
        let encrypted = std::fs::read(&path).unwrap();
        assert!(!encrypted
            .windows(b"secret-original-node".len())
            .any(|w| w == b"secret-original-node"));
        assert_eq!(journal.load::<serde_json::Value>().unwrap(), Some(first));
        let next = serde_json::json!({"original":"secret-original-node", "pending":"node-c"});
        journal.store(&next).unwrap();
        assert_eq!(journal.load::<serde_json::Value>().unwrap(), Some(next));
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "replacement must not leave temp files"
        );
        let mut tampered = std::fs::read(&path).unwrap();
        *tampered.last_mut().unwrap() ^= 0x55;
        std::fs::write(&path, &tampered).unwrap();
        assert_eq!(
            journal.load::<serde_json::Value>().unwrap_err().code,
            ErrorCode::StateUnavailable
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            tampered,
            "failed load must preserve recovery evidence"
        );
        journal.remove().unwrap();
        journal.remove().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_journal_is_an_error_not_silently_missing_state() {
        let root = fixture();
        let path = root.join("recovery.bin");
        std::fs::write(&path, b"broken").unwrap();
        assert!(RecoveryJournal::new(path.clone())
            .load::<serde_json::Value>()
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn journal_serialization_failure_preserves_previous_state() {
        struct Bad;
        impl serde::Serialize for Bad {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("private-serialization-error"))
            }
        }
        let root = fixture();
        let path = root.join("recovery.bin");
        let journal = RecoveryJournal::new(path.clone());
        journal.store(&42).unwrap();
        let original = std::fs::read(&path).unwrap();
        let error = journal.store(&Bad).unwrap_err();
        assert!(!error.to_string().contains("private-serialization-error"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(journal.load::<i32>().unwrap(), Some(42));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lock_child() {
        let Some(root) = std::env::var_os("PROXPILOT_TEST_LOCK_DIR") else {
            return;
        };
        let root = PathBuf::from(root);
        let _lock = MutationLock::acquire_at(&root).unwrap();
        std::fs::write(root.join("ready"), b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn modification_lock_is_released_when_owner_process_dies() {
        use std::os::windows::process::CommandExt;
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = fixture();
        let mut child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "operation::tests::lock_child", "--nocapture"])
                .env("PROXPILOT_TEST_LOCK_DIR", &root)
                .creation_flags(0x08000000)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let start = Instant::now();
        while !root.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before acquiring lock"
            );
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "child lock did not become ready"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            MutationLock::acquire_at(&root).unwrap_err().code,
            ErrorCode::Busy
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let lock = MutationLock::acquire_at(&root).unwrap();
        drop(lock);
        std::fs::remove_dir_all(root).unwrap();
    }
}
