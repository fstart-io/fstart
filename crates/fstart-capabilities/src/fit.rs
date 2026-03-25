//! FIT (Flattened Image Tree) runtime boot helpers.
//!
//! Extracts the FIT runtime parsing and component loading logic into
//! testable, debuggable library functions.  Codegen generates calls to
//! these functions rather than inlining the entire FIT parsing sequence.
//!
//! The FIT blob is located in FFS as [`FileType::FitImage`], parsed
//! in-place (zero-copy for memory-mapped flash), and each component
//! (kernel, ramdisk) is copied to its load address from FIT metadata.

use fstart_services::BootMedia;

/// Result of loading FIT image components from FFS.
pub struct FitBootInfo {
    /// Load address of the kernel (where it was copied to).
    pub kernel_addr: u64,
}

/// Errors from FIT runtime boot operations.
#[derive(Debug)]
pub enum FitBootError {
    /// FIT image not found in FFS.
    NotFound,
    /// Failed to parse FIT image.
    ParseFailed,
    /// Failed to resolve FIT configuration.
    ConfigFailed,
    /// Failed to read kernel data from FIT.
    KernelDataFailed,
    /// Kernel has no load address in FIT metadata.
    NoKernelLoadAddr,
}

/// Return a static string description for a [`FitBootError`].
///
/// Used by generated code for error logging without requiring
/// `Display` or `Debug` formatting (which pull in format machinery).
pub fn error_str(err: &FitBootError) -> &'static str {
    match err {
        FitBootError::NotFound => "FIT image not found in FFS",
        FitBootError::ParseFailed => "failed to parse FIT image",
        FitBootError::ConfigFailed => "failed to resolve FIT configuration",
        FitBootError::KernelDataFailed => "failed to read kernel data from FIT",
        FitBootError::NoKernelLoadAddr => "kernel has no load address in FIT",
    }
}

/// Load FIT components from FFS: parse FIT image, copy kernel and
/// ramdisk to their load addresses.
///
/// This is the core of the FIT runtime boot path.  The FIT blob is
/// located in FFS as `FileType::FitImage`, parsed in-place (zero-copy
/// for memory-mapped flash), and each component is copied to its load
/// address from FIT metadata.
///
/// Returns the kernel's load address for the platform-specific boot
/// jump, or an error describing the failure.
///
/// # Safety contract
///
/// The caller must ensure that the load addresses in the FIT metadata
/// point to writable RAM with sufficient space for the component data.
/// This is guaranteed by the board config and linker script.
pub fn load_fit_components(
    anchor_data: &[u8],
    media: &impl BootMedia,
    fit_config: Option<&str>,
) -> Result<FitBootInfo, FitBootError> {
    // Step 1: Load FIT blob from FFS (zero-copy for memory-mapped flash).
    fstart_log::info!("loading FIT image from FFS...");
    let fit_slice =
        crate::find_ffs_file_data(anchor_data, media, fstart_types::ffs::FileType::FitImage)
            .ok_or(FitBootError::NotFound)?;

    // Step 2: Parse FIT image.
    fstart_log::info!("parsing FIT image ({} bytes)...", fit_slice.len());
    let fit = fstart_fit::FitImage::parse(fit_slice).map_err(|_| FitBootError::ParseFailed)?;

    // Step 3: Resolve boot configuration (default or named).
    let boot = fit
        .resolve_boot_images(fit_config)
        .map_err(|_| FitBootError::ConfigFailed)?;

    // Step 4: Extract kernel data and copy to load address.
    let kernel_data = boot
        .kernel
        .data()
        .map_err(|_| FitBootError::KernelDataFailed)?;
    let kernel_load = boot
        .kernel
        .load_addr()
        .ok_or(FitBootError::NoKernelLoadAddr)?;

    fstart_log::info!(
        "FIT: loading kernel ({} bytes) to {}",
        kernel_data.len(),
        fstart_log::Hex(kernel_load)
    );
    // SAFETY: load address points to writable RAM per board config.
    // The FIT metadata specifies load addresses that the board config
    // guarantees are in DRAM with sufficient space.
    unsafe {
        core::ptr::copy_nonoverlapping(
            kernel_data.as_ptr(),
            kernel_load as *mut u8,
            kernel_data.len(),
        );
    }

    // Step 5: Extract ramdisk if present and copy to its load address.
    if let Some(ref rd) = boot.ramdisk {
        if let Ok(rd_data) = rd.data() {
            if let Some(rd_load) = rd.load_addr() {
                fstart_log::info!(
                    "FIT: loading ramdisk ({} bytes) to {}",
                    rd_data.len(),
                    fstart_log::Hex(rd_load)
                );
                // SAFETY: load address points to writable RAM per board config.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        rd_data.as_ptr(),
                        rd_load as *mut u8,
                        rd_data.len(),
                    );
                }
            }
        }
    }

    Ok(FitBootInfo {
        kernel_addr: kernel_load,
    })
}
