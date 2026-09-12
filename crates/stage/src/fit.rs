//! FIT containers are authenticated in stable RAM before parsing. Every output
//! range is checked against trusted platform policy before any component write.
use crate::boot::{MemoryPolicy, MemoryWindow};
use fstart_core::services::BootMedia;
use fstart_crypto::digest::hash_sha256;

pub struct FitBootInfo {
    /// Verified kernel entry address (defaults to its load address).
    pub kernel_addr: u64,
}

#[derive(Debug)]
pub enum FitBootError {
    NotFound,
    ParseFailed,
    ConfigFailed,
    KernelDataFailed,
    NoKernelLoadAddr,
    InvalidLoadPlan,
    DigestMismatch,
}

pub fn error_str(err: &FitBootError) -> &'static str {
    match err {
        FitBootError::NotFound => "FIT image not found in FFS",
        FitBootError::ParseFailed => "failed to parse FIT image",
        FitBootError::ConfigFailed => "failed to resolve FIT configuration",
        FitBootError::KernelDataFailed => "failed to read FIT component",
        FitBootError::NoKernelLoadAddr => "FIT component has no load address",
        FitBootError::InvalidLoadPlan => "FIT destinations or compression rejected",
        FitBootError::DigestMismatch => "FIT destination digest mismatch",
    }
}

pub fn load_fit_components(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    fit_config: Option<&str>,
) -> Result<FitBootInfo, FitBootError> {
    load_fit_components_with_scratch(anchor_data, media, fit_config, None)
}

struct Component<'a> {
    source: &'a [u8],
    destination: MemoryWindow,
    digest: [u8; 32],
}

impl<'a> Component<'a> {
    fn plan(
        node: &fstart_boot::fit::FitImageNode<'a>,
        policy: &MemoryPolicy<'_>,
        source: MemoryWindow,
    ) -> Result<Self, FitBootError> {
        // FFS-level compression is already handled before parsing. Inner FIT
        // compression is not implemented by this consumer; never copy it as code.
        if node.compression() != fstart_boot::fit::FitCompression::None {
            return Err(FitBootError::InvalidLoadPlan);
        }
        let bytes = node.data().map_err(|_| FitBootError::KernelDataFailed)?;
        let start = node.load_addr().ok_or(FitBootError::NoKernelLoadAddr)?;
        let destination = MemoryWindow {
            start,
            size: bytes.len() as u64,
        };
        if !policy.permits(start, destination.size) || destination.overlaps(source) {
            return Err(FitBootError::InvalidLoadPlan);
        }
        Ok(Self {
            source: bytes,
            destination,
            digest: hash_sha256(bytes),
        })
    }

    /// Requires the installed policy's exclusive physical mapping contract.
    unsafe fn copy_verified(&self) -> Result<(), FitBootError> {
        // SAFETY: the whole plan was preflighted, including disjoint source,
        // other outputs and platform reservations, before constructing this slice.
        let output = unsafe {
            core::slice::from_raw_parts_mut(self.destination.start as *mut u8, self.source.len())
        };
        output.copy_from_slice(self.source);
        if hash_sha256(output) != self.digest {
            return Err(FitBootError::DigestMismatch);
        }
        Ok(())
    }
}

/// Load a supported FIT plan using authenticated stable container bytes. Missing
/// ramdisk addresses/data, inner compression, and overlap are hard failures.
pub fn load_fit_components_with_scratch(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    fit_config: Option<&str>,
    scratch: Option<&mut fstart_core::services::TempRamArena>,
) -> Result<FitBootInfo, FitBootError> {
    let fit_slice = crate::find_ffs_file_data_with_scratch(
        anchor_data,
        media,
        fstart_core::ffs::FileType::FitImage,
        scratch,
    )
    .ok_or(FitBootError::NotFound)?;
    let fit =
        fstart_boot::fit::FitImage::parse(fit_slice).map_err(|_| FitBootError::ParseFailed)?;
    let boot = fit
        .resolve_boot_images(fit_config)
        .map_err(|_| FitBootError::ConfigFailed)?;
    let policy = crate::directory::load_policy().ok_or(FitBootError::InvalidLoadPlan)?;
    let source = MemoryWindow {
        start: fit_slice.as_ptr() as u64,
        size: fit_slice.len() as u64,
    };
    let kernel = Component::plan(&boot.kernel, &policy, source)?;
    let ramdisk = boot
        .ramdisk
        .as_ref()
        .map(|node| Component::plan(node, &policy, source))
        .transpose()?;
    if ramdisk
        .as_ref()
        .is_some_and(|rd| kernel.destination.overlaps(rd.destination))
    {
        return Err(FitBootError::InvalidLoadPlan);
    }
    let entry = boot.kernel.entry_addr().unwrap_or(kernel.destination.start);
    if !policy.entry_alignment.is_power_of_two()
        || entry % policy.entry_alignment != 0
        || entry < kernel.destination.start
        || entry
            >= kernel
                .destination
                .start
                .checked_add(kernel.destination.size)
                .ok_or(FitBootError::InvalidLoadPlan)?
    {
        return Err(FitBootError::InvalidLoadPlan);
    }
    // Also reject any output alias with the original media window, even though
    // the container itself has already been copied into independent stable RAM.
    for component in core::iter::once(&kernel).chain(ramdisk.iter()) {
        crate::boot::validate_destination(
            media,
            &policy,
            component.destination.start,
            component.destination.size,
        )
        .map_err(|_| FitBootError::InvalidLoadPlan)?;
    }
    let ranges = [
        kernel.destination,
        ramdisk
            .as_ref()
            .map_or(kernel.destination, |rd| rd.destination),
    ];
    let ranges = &ranges[..1 + usize::from(ramdisk.is_some())];
    let pending =
        crate::loaded::begin(ranges, ranges).map_err(|_| FitBootError::InvalidLoadPlan)?;
    // SAFETY: set_load_policy establishes the mapping contract; all outputs and
    // sources were checked together before the first copy. No entry on failure.
    unsafe {
        if let Some(ramdisk) = ramdisk {
            ramdisk.copy_verified()?;
        }
        kernel.copy_verified()?;
    }
    pending.commit();
    Ok(FitBootInfo { kernel_addr: entry })
}
