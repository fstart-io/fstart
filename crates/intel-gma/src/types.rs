//! Common Intel GMA data types.

use serde::{Deserialize, Serialize};

/// Intel display generation family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Port {
    /// LVDS panel port.
    #[serde(alias = "LVDS")]
    Lvds,
    /// Embedded DisplayPort panel port.
    #[serde(alias = "EDP")]
    Edp,
    /// VGA/CRT output.
    #[serde(alias = "VGA")]
    Vga,
    /// First HDMI port.
    #[serde(alias = "HDMI_A", alias = "HDMI1")]
    HdmiA,
    /// Second HDMI port.
    #[serde(alias = "HDMI_B", alias = "HDMI2")]
    HdmiB,
    /// Third HDMI port.
    #[serde(alias = "HDMI_C", alias = "HDMI3")]
    HdmiC,
    /// First DisplayPort.
    #[serde(alias = "DP_A", alias = "DP1")]
    DpA,
    /// Second DisplayPort.
    #[serde(alias = "DP_B", alias = "DP2")]
    DpB,
    /// Third DisplayPort.
    #[serde(alias = "DP_C", alias = "DP3")]
    DpC,
    /// Fourth DisplayPort.
    #[serde(alias = "DP_D", alias = "DP4")]
    DpD,
}

/// Display pipe identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pipe {
    /// Pipe A.
    A,
    /// Pipe B.
    B,
    /// Pipe C.
    C,
}

/// Primary plane identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plane {
    /// Primary plane on pipe A.
    PrimaryA,
    /// Primary plane on pipe B.
    PrimaryB,
    /// Primary plane on pipe C.
    PrimaryC,
}

/// Physical address newtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PhysAddr(pub u64);

/// PCI bus/device/function tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PciBdf {
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub dev: u8,
    /// PCI function number.
    pub func: u8,
}

/// Kilohertz clock value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KHz(pub u32);
