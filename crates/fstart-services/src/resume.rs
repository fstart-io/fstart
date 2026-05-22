//! ACPI S3 resume service traits and global boot-path state.

use core::sync::atomic::{AtomicU8, Ordering};

use fstart_types::BootPath;

use crate::ServiceError;

const BOOT_PATH_NORMAL: u8 = 0;
const BOOT_PATH_S3_RESUME: u8 = 1;

static BOOT_PATH: AtomicU8 = AtomicU8::new(BOOT_PATH_NORMAL);

/// Detects chipset sleep/resume state early in boot.
pub trait ResumeDetector: Send + Sync {
    /// Return the current firmware boot path.
    fn detect_boot_path(&self) -> Result<BootPath, ServiceError>;
}

/// Publish the detected boot path for later stages/capabilities.
pub fn set_boot_path(path: BootPath) {
    let raw = match path {
        BootPath::Normal => BOOT_PATH_NORMAL,
        BootPath::S3Resume => BOOT_PATH_S3_RESUME,
    };
    BOOT_PATH.store(raw, Ordering::SeqCst);
}

/// Return the current globally published boot path.
pub fn boot_path() -> BootPath {
    match BOOT_PATH.load(Ordering::SeqCst) {
        BOOT_PATH_S3_RESUME => BootPath::S3Resume,
        _ => BootPath::Normal,
    }
}

/// Return true when the active boot path is ACPI S3 resume.
pub fn is_s3_resume() -> bool {
    boot_path().is_s3_resume()
}
