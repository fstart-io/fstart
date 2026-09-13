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
    /// Haswell platform.
    Haswell,
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

/// Logical display port requested by board policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Physical address newtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub u64);

/// PCI bus/device/function tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciBdf {
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub dev: u8,
    /// PCI function number.
    pub func: u8,
}

