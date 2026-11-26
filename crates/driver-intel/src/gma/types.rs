//! Common Intel GMA data types.

/// Intel display generation family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generation {
    /// Gen3 i945 display block, also used by Pineview.
    I945,
    /// G45-family display block, also used for GM965/Crestline bring-up.
    G45,
    /// Ironlake/Sandybridge/Ivybridge display family.
    Ironlake,
    /// Haswell/Broadwell DDI display family.
    Haswell,
    /// Broxton display family.
    Broxton,
    /// Skylake/Kabylake display family.
    Skylake,
    /// Tigerlake/Alderlake display family.
    Tigerlake,
}

/// Intel CPU/platform identifier for display init dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cpu {
    /// 945G desktop chipset.
    I945G,
    /// 945GM mobile chipset.
    I945GM,
    /// GM965/Crestline mobile chipset.
    Gm965,
    /// G45 desktop chipset.
    G45,
    /// GM45 mobile chipset.
    Gm45,
    /// Pineview Atom integrated northbridge (desktop).
    Pineview,
    /// Pineview-M Atom integrated northbridge (mobile, LVDS).
    PineviewM,
    /// Ironlake platform.
    Ironlake,
    /// Sandybridge platform.
    Sandybridge,
    /// Ivybridge platform.
    Ivybridge,
    /// Haswell desktop/mobile (Traditional/H) platform.
    Haswell,
    /// Haswell-ULT (U-series low-power) platform.
    HaswellUlt,
    /// Haswell-ULX (Y-series ultra-low-power) platform.
    HaswellUlx,
    /// Broadwell platform.
    Broadwell,
    /// Broxton platform.
    Broxton,
    /// Skylake platform.
    Skylake,
    /// Kabylake platform.
    Kabylake,
    /// Tigerlake platform.
    Tigerlake,
    /// Alderlake platform.
    Alderlake,
}

impl Cpu {
    /// Classify an Intel IGD PCI device ID for the Haswell family
    /// (Traditional/Desktop/Mobile, ULT/U-series, and ULX/Y-series), matching
    /// libgfxinit `Is_Haswell` / `Is_Haswell_U` / `Is_Haswell_Y`.
    #[must_use]
    pub const fn from_haswell_pci_id(device_id: u16) -> Option<Self> {
        if (device_id & 0xffef) == 0x0a0e {
            Some(Self::HaswellUlx)
        } else if (device_id & 0xffc3) == 0x0a02 || (device_id & 0xffcf) == 0x0a0b {
            Some(Self::HaswellUlt)
        } else if (device_id & 0xffc3) == 0x0402
            || (device_id & 0xffcf) == 0x040b
            || (device_id & 0xffc3) == 0x0c02
            || (device_id & 0xffcf) == 0x0c0b
            || (device_id & 0xffc3) == 0x0d02
            || (device_id & 0xffcf) == 0x0d0b
        {
            Some(Self::Haswell)
        } else {
            None
        }
    }

    /// Return `true` for any Haswell SKU (Traditional, ULT, or ULX).
    #[must_use]
    pub const fn is_haswell(self) -> bool {
        matches!(self, Self::Haswell | Self::HaswellUlt | Self::HaswellUlx)
    }

    /// Return `true` for Haswell low-power SKUs (ULT or ULX, paired with Lynx Point-LP).
    #[must_use]
    pub const fn is_haswell_lp(self) -> bool {
        matches!(self, Self::HaswellUlt | Self::HaswellUlx)
    }
}

/// Logical display port requested by board policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Port {
    /// LVDS panel port.
    Lvds,
    /// Embedded DisplayPort panel port.
    Edp,
    /// VGA/CRT output.
    Vga,
    /// First HDMI port.
    HdmiA,
    /// Second HDMI port.
    HdmiB,
    /// Third HDMI port.
    HdmiC,
    /// HDMI connector on DDI D (Gen9).
    HdmiD,
    /// First DisplayPort.
    DpA,
    /// Second DisplayPort.
    DpB,
    /// Third DisplayPort.
    DpC,
    /// Fourth DisplayPort.
    DpD,
}

/// Display pipe identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipe {
    /// Pipe A.
    A,
    /// Pipe B.
    B,
    /// Pipe C.
    C,
}

/// Primary plane identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// Primary plane on pipe A.
    PrimaryA,
    /// Primary plane on pipe B.
    PrimaryB,
    /// Primary plane on pipe C.
    PrimaryC,
}

/// PCI address of the IGD function.
///
/// This is fstart's shared PCI address type (`pci_types::PciAddress`, re-exported
/// by `fstart-pci`), not a crate-local tuple, so callers can pass the same value
/// to PCI config access and to this crate.
pub use fstart_pci::PciAddress;
