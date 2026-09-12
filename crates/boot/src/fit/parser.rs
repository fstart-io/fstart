//! Core FIT image parser — works in both `std` and `no_std` environments.
//!
//! A FIT (Flattened Image Tree) is a standard FDT/DTB blob with specific
//! node conventions defined by U-Boot. The structure is:
//!
//! ```text
//! / {
//!     description = "...";
//!     #address-cells = <1|2>;
//!     images {
//!         kernel { data, type, arch, os, compression, load, entry, hash-* };
//!         ramdisk { ... };
//!         fdt-1 { ... };
//!     };
//!     configurations {
//!         default = "conf-1";
//!         conf-1 { kernel, ramdisk, fdt, loadables, firmware };
//!     };
//! };
//! ```
//!
//! This parser handles both embedded data (`data` property inside the FDT)
//! and external data (`data-offset`/`data-position` + `data-size` with
//! payloads appended after the FDT blob).

use dtoolkit::fdt::Fdt;
use dtoolkit::{Node, Property};

use heapless::String as HString;

// ============================================================================
// Error type
// ============================================================================

/// Errors from FIT parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitError {
    /// Input too short to contain an FDT header.
    TooShort,
    /// FDT magic number is invalid.
    BadMagic,
    /// FDT parsing failed (structure, version, etc.).
    InvalidFdt,
    /// No `/images` node found (not a valid FIT).
    NoImagesNode,
    /// No `/configurations` node found.
    NoConfigurationsNode,
    /// Requested configuration not found.
    ConfigNotFound,
    /// Required property missing on a node.
    MissingProperty,
    /// Property value could not be decoded.
    PropertyDecode,
    /// External data reference is out of bounds.
    DataOutOfBounds,
    /// Hash verification failed.
    HashMismatch,
    /// Hash algorithm not supported or not enabled.
    UnsupportedHashAlgo,
}

// ============================================================================
// Enums for FIT metadata
// ============================================================================

/// FIT image type — the `type` property on an image node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitImageType {
    Kernel,
    KernelNoload,
    Ramdisk,
    FlatDt,
    Firmware,
    Standalone,
    Script,
    Tee,
    Fpga,
    /// Unrecognized type string.
    Unknown,
}

impl FitImageType {
    fn from_str(s: &str) -> Self {
        match s {
            "kernel" => Self::Kernel,
            "kernel_noload" => Self::KernelNoload,
            "ramdisk" => Self::Ramdisk,
            "flat_dt" => Self::FlatDt,
            "firmware" => Self::Firmware,
            "standalone" => Self::Standalone,
            "script" => Self::Script,
            "tee" => Self::Tee,
            "fpga" => Self::Fpga,
            _ => Self::Unknown,
        }
    }
}

/// Compression method — the `compression` property on an image node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitCompression {
    None,
    Gzip,
    Bzip2,
    Lzma,
    Lzo,
    Lz4,
    Zstd,
    Unknown,
}

impl FitCompression {
    fn from_str(s: &str) -> Self {
        match s {
            "none" => Self::None,
            "gzip" => Self::Gzip,
            "bzip2" => Self::Bzip2,
            "lzma" => Self::Lzma,
            "lzo" => Self::Lzo,
            "lz4" => Self::Lz4,
            "zstd" => Self::Zstd,
            _ => Self::Unknown,
        }
    }
}

/// Architecture — the `arch` property on an image node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitArch {
    Arm,
    Arm64,
    Riscv,
    X86,
    X86_64,
    Unknown,
}

impl FitArch {
    fn from_str(s: &str) -> Self {
        match s {
            "arm" => Self::Arm,
            "arm64" => Self::Arm64,
            "riscv" => Self::Riscv,
            "x86" => Self::X86,
            "x86_64" => Self::X86_64,
            _ => Self::Unknown,
        }
    }
}

// ============================================================================
// FitImage — top-level parser
// ============================================================================

/// A parsed FIT image.
///
/// Zero-copy: borrows the input blob. The same struct works at buildtime
/// (`std`, reading from `Vec<u8>`) and runtime (`no_std`, reading from
/// memory-mapped flash).
pub struct FitImage<'a> {
    /// The parsed FDT (covers only the FDT portion of the blob).
    fdt: Fdt<'a>,
    /// The full blob including any external data appended after the FDT.
    blob: &'a [u8],
    /// The FDT total size (where external data offsets are relative to).
    fdt_size: usize,
}

impl<'a> FitImage<'a> {
    /// Parse a FIT image from a byte slice.
    ///
    /// The slice must contain the complete FIT blob: FDT header + structure +
    /// strings + any appended external data. The FDT `totalsize` field
    /// determines where the FDT ends and external data begins.
    pub fn parse(blob: &'a [u8]) -> Result<Self, FitError> {
        if blob.len() < 8 {
            return Err(FitError::TooShort);
        }

        // Validate FDT magic (0xd00dfeed, big-endian)
        let magic = u32::from_be_bytes(blob[0..4].try_into().map_err(|_| FitError::TooShort)?);
        if magic != 0xd00d_feed {
            return Err(FitError::BadMagic);
        }

        // Read totalsize to slice the FDT portion
        let totalsize =
            u32::from_be_bytes(blob[4..8].try_into().map_err(|_| FitError::TooShort)?) as usize;

        if totalsize > blob.len() {
            return Err(FitError::TooShort);
        }

        let fdt = Fdt::new(&blob[..totalsize]).map_err(|_| FitError::InvalidFdt)?;

        // Verify this is a FIT: must have /images node
        if fdt.find_node("/images").is_none() {
            return Err(FitError::NoImagesNode);
        }

        Ok(Self {
            fdt,
            blob,
            fdt_size: totalsize,
        })
    }

    /// The FIT description string from the root node.
    pub fn description(&self) -> Option<&'a str> {
        self.fdt
            .root()
            .property("description")
            .and_then(|p| p.as_str().ok())
    }

    /// The `#address-cells` value from the root (1 or 2). Defaults to 1.
    pub fn address_cells(&self) -> u32 {
        self.fdt
            .root()
            .property("#address-cells")
            .and_then(|p| p.as_u32().ok())
            .unwrap_or(1)
    }

    /// The total size of the FDT portion of the blob.
    pub fn fdt_size(&self) -> usize {
        self.fdt_size
    }

    /// Iterate over all image nodes under `/images`.
    pub fn images(&self) -> impl Iterator<Item = FitImageNode<'a>> + '_ {
        self.fdt
            .find_node("/images")
            .into_iter()
            .flat_map(|n| n.children())
            .map(move |node| FitImageNode {
                node,
                blob: self.blob,
                fdt_size: self.fdt_size,
            })
    }

    /// Look up a specific image by name (e.g., "kernel", "ramdisk-1").
    pub fn image(&self, name: &str) -> Option<FitImageNode<'a>> {
        self.images().find(|img| img.name() == name)
    }

    /// Iterate over all configuration nodes under `/configurations`.
    pub fn configurations(&self) -> impl Iterator<Item = FitConfig<'a>> {
        self.fdt
            .find_node("/configurations")
            .into_iter()
            .flat_map(|n| n.children())
            .map(|node| FitConfig { node })
    }

    /// Get the name of the default configuration.
    pub fn default_config_name(&self) -> Option<&'a str> {
        self.fdt
            .find_node("/configurations")
            .and_then(|n| n.property("default"))
            .and_then(|p| p.as_str().ok())
    }

    /// Get the default configuration node.
    pub fn default_config(&self) -> Option<FitConfig<'a>> {
        let name = self.default_config_name()?;
        self.config(name)
    }

    /// Get a configuration by name.
    pub fn config(&self, name: &str) -> Option<FitConfig<'a>> {
        self.configurations().find(|c| c.name() == name)
    }

    /// Resolve a configuration: if `name` is `Some`, look it up; otherwise
    /// use the default. Returns an error if not found.
    pub fn resolve_config(&self, name: Option<&str>) -> Result<FitConfig<'a>, FitError> {
        match name {
            Some(n) => self.config(n).ok_or(FitError::ConfigNotFound),
            None => self.default_config().ok_or(FitError::ConfigNotFound),
        }
    }

    /// Resolve a full boot configuration: given a config (or default),
    /// return the image nodes for kernel, ramdisk, and FDT.
    ///
    /// Returns `(kernel, ramdisk, fdt)` — ramdisk and fdt may be `None`.
    pub fn resolve_boot_images(
        &self,
        config_name: Option<&str>,
    ) -> Result<ResolvedBootImages<'a>, FitError> {
        let config = self.resolve_config(config_name)?;

        let kernel_name = config.kernel().ok_or(FitError::MissingProperty)?;
        let kernel = self.image(kernel_name).ok_or(FitError::ConfigNotFound)?;

        let ramdisk = config.ramdisk().and_then(|name| self.image(name));

        let fdt = config.fdt_name().and_then(|name| self.image(name));

        let firmware = config.firmware().and_then(|name| self.image(name));

        Ok(ResolvedBootImages {
            config,
            kernel,
            ramdisk,
            fdt,
            firmware,
        })
    }
}

/// A resolved set of boot images from a FIT configuration.
pub struct ResolvedBootImages<'a> {
    /// The selected configuration.
    pub config: FitConfig<'a>,
    /// The kernel image (always present).
    pub kernel: FitImageNode<'a>,
    /// The ramdisk image (optional).
    pub ramdisk: Option<FitImageNode<'a>>,
    /// The FDT/device-tree image (optional).
    pub fdt: Option<FitImageNode<'a>>,
    /// The firmware image (optional, e.g., ATF/OpenSBI in FIT).
    pub firmware: Option<FitImageNode<'a>>,
}

// ============================================================================
// FitImageNode — a single image under /images
// ============================================================================

/// A single image node from the FIT `/images` section.
///
/// Provides access to the image metadata and data. Data may be embedded
/// (the `data` property) or external (appended after the FDT, referenced
/// by `data-offset`/`data-position` + `data-size`).
pub struct FitImageNode<'a> {
    node: dtoolkit::fdt::FdtNode<'a>,
    blob: &'a [u8],
    fdt_size: usize,
}

impl<'a> FitImageNode<'a> {
    /// Image node name (e.g., "kernel", "ramdisk-1", "fdt-1").
    pub fn name(&self) -> &'a str {
        self.node.name()
    }

    /// Human-readable description.
    pub fn description(&self) -> Option<&'a str> {
        self.node
            .property("description")
            .and_then(|p| p.as_str().ok())
    }

    /// Image type (kernel, ramdisk, flat_dt, firmware, etc.).
    pub fn image_type(&self) -> FitImageType {
        self.node
            .property("type")
            .and_then(|p| p.as_str().ok())
            .map(FitImageType::from_str)
            .unwrap_or(FitImageType::Unknown)
    }

    /// Compression method.
    pub fn compression(&self) -> FitCompression {
        self.node
            .property("compression")
            .and_then(|p| p.as_str().ok())
            .map(FitCompression::from_str)
            .unwrap_or(FitCompression::None)
    }

    /// Architecture.
    pub fn arch(&self) -> FitArch {
        self.node
            .property("arch")
            .and_then(|p| p.as_str().ok())
            .map(FitArch::from_str)
            .unwrap_or(FitArch::Unknown)
    }

    /// OS type string (e.g., "linux", "u-boot").
    pub fn os_type(&self) -> Option<&'a str> {
        self.node.property("os").and_then(|p| p.as_str().ok())
    }

    /// Load address. Handles both 1-cell (u32) and 2-cell (u64) values.
    pub fn load_addr(&self) -> Option<u64> {
        let prop = self.node.property("load")?;
        // Try u32 first (1 cell), then u64 (2 cells)
        prop.as_u32()
            .map(u64::from)
            .ok()
            .or_else(|| prop.as_u64().ok())
    }

    /// Entry point address. Handles both 1-cell and 2-cell values.
    pub fn entry_addr(&self) -> Option<u64> {
        let prop = self.node.property("entry")?;
        prop.as_u32()
            .map(u64::from)
            .ok()
            .or_else(|| prop.as_u64().ok())
    }

    /// Get the image data.
    ///
    /// Handles three cases:
    /// 1. Embedded: `data` property contains the bytes directly.
    /// 2. External with `data-position` (absolute offset in blob).
    /// 3. External with `data-offset` (relative to `ALIGN(fdt_totalsize, 4)`).
    pub fn data(&self) -> Result<&'a [u8], FitError> {
        // Case 1: Embedded data property
        if let Some(prop) = self.node.property("data") {
            return Ok(prop.value());
        }

        // External data: need data-size
        let data_size = self
            .node
            .property("data-size")
            .and_then(|p| p.as_u32().ok())
            .ok_or(FitError::MissingProperty)? as usize;

        // Case 2: Absolute position
        if let Some(pos_prop) = self.node.property("data-position") {
            let offset = pos_prop.as_u32().map_err(|_| FitError::PropertyDecode)? as usize;
            let end = offset
                .checked_add(data_size)
                .ok_or(FitError::DataOutOfBounds)?;
            if end > self.blob.len() {
                return Err(FitError::DataOutOfBounds);
            }
            return Ok(&self.blob[offset..end]);
        }

        // Case 3: Relative offset (from ALIGN(fdt_totalsize, 4))
        if let Some(off_prop) = self.node.property("data-offset") {
            let rel_offset = off_prop.as_u32().map_err(|_| FitError::PropertyDecode)? as usize;
            let base = (self.fdt_size + 3) & !3; // align to 4 bytes
            let offset = base
                .checked_add(rel_offset)
                .ok_or(FitError::DataOutOfBounds)?;
            let end = offset
                .checked_add(data_size)
                .ok_or(FitError::DataOutOfBounds)?;
            if end > self.blob.len() {
                return Err(FitError::DataOutOfBounds);
            }
            return Ok(&self.blob[offset..end]);
        }

        Err(FitError::MissingProperty)
    }

    /// Iterate over hash sub-nodes (hash-1, hash-2, etc.).
    pub fn hashes(&self) -> impl Iterator<Item = FitHash<'a>> {
        self.node
            .children()
            .filter(|c| c.name().starts_with("hash"))
            .map(|node| FitHash { node })
    }

    /// Verify all hashes for this image.
    ///
    /// Returns `Ok(())` if all hashes pass (or no hashes are present).
    /// Returns `Err(FitError::HashMismatch)` if any hash fails.
    /// Returns `Err(FitError::UnsupportedHashAlgo)` if a required algorithm
    /// is not compiled in.
    pub fn verify_hashes(&self) -> Result<(), FitError> {
        let data = self.data()?;
        for hash in self.hashes() {
            hash.verify(data)?;
        }
        Ok(())
    }

    /// Get the data size without copying — useful for pre-allocating.
    pub fn data_size(&self) -> Option<usize> {
        // Embedded: property length
        if let Some(prop) = self.node.property("data") {
            return Some(prop.value().len());
        }
        // External: data-size property
        self.node
            .property("data-size")
            .and_then(|p| p.as_u32().ok())
            .map(|s| s as usize)
    }
}

// ============================================================================
// FitConfig — a configuration under /configurations
// ============================================================================

/// A configuration node from the FIT `/configurations` section.
///
/// Configurations bind together specific images into a bootable combination.
pub struct FitConfig<'a> {
    node: dtoolkit::fdt::FdtNode<'a>,
}

impl<'a> FitConfig<'a> {
    /// Configuration node name (e.g., "conf-1").
    pub fn name(&self) -> &'a str {
        self.node.name()
    }

    /// Human-readable description.
    pub fn description(&self) -> Option<&'a str> {
        self.node
            .property("description")
            .and_then(|p| p.as_str().ok())
    }

    /// Name of the kernel image node.
    pub fn kernel(&self) -> Option<&'a str> {
        self.node.property("kernel").and_then(|p| p.as_str().ok())
    }

    /// Name of the ramdisk image node.
    pub fn ramdisk(&self) -> Option<&'a str> {
        self.node.property("ramdisk").and_then(|p| p.as_str().ok())
    }

    /// Name of the FDT image node.
    pub fn fdt_name(&self) -> Option<&'a str> {
        self.node.property("fdt").and_then(|p| p.as_str().ok())
    }

    /// Name of the firmware image node.
    pub fn firmware(&self) -> Option<&'a str> {
        self.node.property("firmware").and_then(|p| p.as_str().ok())
    }

    /// Names of loadable images (comma-separated in FIT, stringlist in FDT).
    pub fn loadables(&self) -> impl Iterator<Item = &'a str> {
        self.node
            .property("loadables")
            .into_iter()
            .flat_map(|p| p.as_str_list())
    }

    /// Get the compatible string (for auto-matching).
    pub fn compatible(&self) -> Option<&'a str> {
        self.node
            .property("compatible")
            .and_then(|p| p.as_str().ok())
    }
}

// ============================================================================
// FitHash — a hash sub-node of an image
// ============================================================================

/// A hash node (e.g., `hash-1`) under an image node.
pub struct FitHash<'a> {
    node: dtoolkit::fdt::FdtNode<'a>,
}

/// Bounded string for hash algorithm names.
type AlgoName = HString<32>;

impl<'a> FitHash<'a> {
    /// Hash algorithm name (e.g., "sha256", "sha1", "crc32").
    pub fn algo(&self) -> Option<&'a str> {
        self.node.property("algo").and_then(|p| p.as_str().ok())
    }

    /// Expected hash value bytes (filled in by mkimage).
    pub fn value(&self) -> Option<&'a [u8]> {
        self.node.property("value").map(|p| p.value())
    }

    /// Verify this hash against the provided data.
    ///
    /// Computes the hash of `data` and compares it to the stored `value`.
    pub fn verify(&self, data: &[u8]) -> Result<(), FitError> {
        let algo = self.algo().ok_or(FitError::MissingProperty)?;
        let expected = self.value().ok_or(FitError::MissingProperty)?;

        // Parse algo into a bounded string for matching
        let mut algo_name = AlgoName::new();
        // Truncate if too long (shouldn't happen for real algo names)
        for c in algo.chars().take(32) {
            let _ = algo_name.push(c);
        }

        match algo_name.as_str() {
            "sha256" => verify_sha256(data, expected),
            // Future: "sha1", "sha512", "crc32"
            _ => Err(FitError::UnsupportedHashAlgo),
        }
    }
}

/// Verify SHA-256 hash using fstart-crypto (when sha2-digest feature is enabled).
#[cfg(feature = "sha2-digest")]
fn verify_sha256(data: &[u8], expected: &[u8]) -> Result<(), FitError> {
    if expected.len() != 32 {
        return Err(FitError::HashMismatch);
    }

    let computed = fstart_crypto::hash_sha256(data);
    if computed[..] == *expected {
        Ok(())
    } else {
        Err(FitError::HashMismatch)
    }
}

/// Stub when sha2-digest is not available.
#[cfg(not(feature = "sha2-digest"))]
fn verify_sha256(_data: &[u8], _expected: &[u8]) -> Result<(), FitError> {
    Err(FitError::UnsupportedHashAlgo)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Build a minimal FIT blob for testing using dtoolkit.
    ///
    /// Creates a FIT with one kernel image and one configuration,
    /// with embedded data.
    fn make_test_fit() -> Vec<u8> {
        use dtoolkit::model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty};

        let mut tree = DeviceTree::new();

        // Root properties
        tree.root.add_property(DeviceTreeProperty::new(
            "description",
            b"Test FIT image\0".to_vec(),
        ));
        tree.root.add_property(DeviceTreeProperty::new(
            "#address-cells",
            1u32.to_be_bytes().to_vec(),
        ));

        // /images node
        let mut images = DeviceTreeNode::new("images");

        // /images/kernel
        let mut kernel = DeviceTreeNode::new("kernel");
        kernel.add_property(DeviceTreeProperty::new(
            "description",
            b"Test kernel\0".to_vec(),
        ));
        kernel.add_property(DeviceTreeProperty::new("type", b"kernel\0".to_vec()));
        kernel.add_property(DeviceTreeProperty::new("arch", b"riscv\0".to_vec()));
        kernel.add_property(DeviceTreeProperty::new("os", b"linux\0".to_vec()));
        kernel.add_property(DeviceTreeProperty::new("compression", b"none\0".to_vec()));
        // Load address: 0x80200000 as 1-cell u32 BE
        kernel.add_property(DeviceTreeProperty::new(
            "load",
            0x8020_0000u32.to_be_bytes().to_vec(),
        ));
        kernel.add_property(DeviceTreeProperty::new(
            "entry",
            0x8020_0000u32.to_be_bytes().to_vec(),
        ));
        // Embedded data: 16 bytes of test pattern
        let test_data = vec![
            0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
            0x07, 0x08,
        ];
        kernel.add_property(DeviceTreeProperty::new("data", test_data.clone()));

        images.add_child(kernel);

        // /images/ramdisk
        let mut ramdisk = DeviceTreeNode::new("ramdisk");
        ramdisk.add_property(DeviceTreeProperty::new(
            "description",
            b"Test ramdisk\0".to_vec(),
        ));
        ramdisk.add_property(DeviceTreeProperty::new("type", b"ramdisk\0".to_vec()));
        ramdisk.add_property(DeviceTreeProperty::new("arch", b"riscv\0".to_vec()));
        ramdisk.add_property(DeviceTreeProperty::new("os", b"linux\0".to_vec()));
        ramdisk.add_property(DeviceTreeProperty::new("compression", b"none\0".to_vec()));
        let rd_data = vec![0xAA; 32];
        ramdisk.add_property(DeviceTreeProperty::new("data", rd_data));

        images.add_child(ramdisk);
        tree.root.add_child(images);

        // /configurations node
        let mut configs = DeviceTreeNode::new("configurations");
        configs.add_property(DeviceTreeProperty::new("default", b"conf-1\0".to_vec()));

        let mut conf1 = DeviceTreeNode::new("conf-1");
        conf1.add_property(DeviceTreeProperty::new(
            "description",
            b"Test boot config\0".to_vec(),
        ));
        conf1.add_property(DeviceTreeProperty::new("kernel", b"kernel\0".to_vec()));
        conf1.add_property(DeviceTreeProperty::new("ramdisk", b"ramdisk\0".to_vec()));

        configs.add_child(conf1);
        tree.root.add_child(configs);

        tree.to_dtb()
    }

    #[test]
    fn test_parse_fit() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).expect("should parse test FIT");

        assert_eq!(fit.description(), Some("Test FIT image"));
        assert_eq!(fit.address_cells(), 1);
    }

    #[test]
    fn test_image_enumeration() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).unwrap();

        let images: Vec<_> = fit.images().collect();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].name(), "kernel");
        assert_eq!(images[1].name(), "ramdisk");
    }

    #[test]
    fn test_image_metadata() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).unwrap();

        let kernel = fit.image("kernel").expect("kernel should exist");
        assert_eq!(kernel.image_type(), FitImageType::Kernel);
        assert_eq!(kernel.arch(), FitArch::Riscv);
        assert_eq!(kernel.os_type(), Some("linux"));
        assert_eq!(kernel.compression(), FitCompression::None);
        assert_eq!(kernel.load_addr(), Some(0x8020_0000));
        assert_eq!(kernel.entry_addr(), Some(0x8020_0000));
    }

    #[test]
    fn test_embedded_data() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).unwrap();

        let kernel = fit.image("kernel").unwrap();
        let data = kernel.data().expect("should have embedded data");
        assert_eq!(data.len(), 16);
        assert_eq!(&data[0..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn test_configuration_resolution() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).unwrap();

        assert_eq!(fit.default_config_name(), Some("conf-1"));

        let config = fit.default_config().expect("should have default config");
        assert_eq!(config.name(), "conf-1");
        assert_eq!(config.kernel(), Some("kernel"));
        assert_eq!(config.ramdisk(), Some("ramdisk"));
        assert_eq!(config.fdt_name(), None);
    }

    #[test]
    fn test_resolve_boot_images() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).unwrap();

        let boot = fit.resolve_boot_images(None).expect("should resolve");
        assert_eq!(boot.kernel.name(), "kernel");
        assert!(boot.ramdisk.is_some());
        assert_eq!(boot.ramdisk.unwrap().name(), "ramdisk");
        assert!(boot.fdt.is_none());
        assert!(boot.firmware.is_none());
    }

    #[test]
    fn test_named_config() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).unwrap();

        let config = fit
            .resolve_config(Some("conf-1"))
            .expect("should find conf-1");
        assert_eq!(config.kernel(), Some("kernel"));

        let err = fit.resolve_config(Some("nonexistent"));
        assert!(matches!(err, Err(FitError::ConfigNotFound)));
    }

    #[test]
    fn test_invalid_magic() {
        let blob = vec![0x00; 64];
        assert!(matches!(FitImage::parse(&blob), Err(FitError::BadMagic)));
    }

    #[test]
    fn test_too_short() {
        let blob = vec![0xd0, 0x0d, 0xfe, 0xed]; // magic only, no totalsize
        assert!(matches!(FitImage::parse(&blob), Err(FitError::TooShort)));
    }

    #[test]
    fn test_data_size() {
        let blob = make_test_fit();
        let fit = FitImage::parse(&blob).unwrap();

        let kernel = fit.image("kernel").unwrap();
        assert_eq!(kernel.data_size(), Some(16));

        let ramdisk = fit.image("ramdisk").unwrap();
        assert_eq!(ramdisk.data_size(), Some(32));
    }
}
