/// One Rust-owned block-device candidate for firmware-image boot media.
///
/// Used for platforms where hardware boot-source registers select among block
/// devices (for example sunxi eGON). Board RON names only
/// `BootMedia(FirmwareImage(...))`; device candidates and offsets live here as
/// platform/provider metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformBootMediaCandidate {
    /// Board device name that provides `BlockDevice`.
    pub device: &'static str,
    /// Byte offset on the device where the FFS image starts.
    pub offset: u64,
    /// Size of the FFS image region in bytes.
    pub size: u64,
}

/// Rust-owned boot-source-selected block firmware-image candidates.
pub fn platform_boot_media_candidates(
    board_name: &str,
    platform: fstart_types::Platform,
) -> &'static [PlatformBootMediaCandidate] {
    use fstart_types::Platform;

    match (board_name, platform) {
        ("bananapi-m1", Platform::Armv7) => &[PlatformBootMediaCandidate {
            device: "mmc0",
            offset: 0x2000,
            size: 0x0080_0000,
        }],
        ("orangepi-pc2", Platform::Aarch64) | ("licheerv-dock", Platform::Riscv64) => {
            &[PlatformBootMediaCandidate {
                device: "mmc0",
                offset: 0x2000,
                size: 0x0100_0000,
            }]
        }
        ("orangepi-r1", Platform::Armv7) => &[
            PlatformBootMediaCandidate {
                device: "mmc0",
                offset: 0x2000,
                size: 0x0080_0000,
            },
            PlatformBootMediaCandidate {
                device: "spi0",
                offset: 0,
                size: 0x0100_0000,
            },
        ],
        _ => &[],
    }
}
