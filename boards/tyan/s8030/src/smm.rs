//! Native FCH dispatch plus opt-in SMM MCA observation. The `ras-observer`
//! feature (variant `tyan-s8030-ras`) enables the experimental #MC path;
//! ordinary builds retain OS-first behavior.

use fstart_platform_sp3::smm::ras::{Policy, SelTransport};

pub struct Ras;
impl Policy for Ras {
    const OBSERVE_MCE: bool = cfg!(feature = "ras-observer");
    // Saved S8030 BMC image 2b09b92ee677b5d9: DTB at 0x39a200 enables
    // KCS2 (CA8/CAC) and its kcsbridged service. KCS3 (CA2/CA3) remains
    // the OS interface. The existing CA0..CAF LPC window decodes both.
    const SEL: Option<SelTransport> = Some(SelTransport {
        port: 0xca8,
        spacing: 4,
    });
}

fstart_platform_sp3::smm_runtime::smm_bin!(
    crate::Board,
    fstart_platform_sp3::smm::Sp3SmmHandler<Ras>
);
