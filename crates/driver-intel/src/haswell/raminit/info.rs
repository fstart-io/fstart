//! Host bridge capability fuses before dynamic fusing and the programmed
//! memory decoder after initialization (`info_dumps.c`, `raminit_shared.c`).
use super::*;

pub(super) fn capabilities(a: u32, b: u32) {
    fstart_log::debug!("CAPID0_A: {:#010x}", a);
    fstart_log::debug!(
        "DDR3L_EN={} DDR_WRTVREF={} OC_ENABLED_DSKU={}",
        PCI_CAPID0_A::DDR3L_EN.read(a),
        PCI_CAPID0_A::DDR_WRTVREF.read(a),
        PCI_CAPID0_A::OC_ENABLED_DSKU.read(a)
    );
    fstart_log::debug!(
        "DDR_OVERCLOCK={} CRID={} CDID={}",
        PCI_CAPID0_A::DDR_OVERCLOCK.read(a),
        PCI_CAPID0_A::CRID.read(a),
        PCI_CAPID0_A::CDID.read(a)
    );
    fstart_log::debug!(
        "DIDOE={} IGD={} PDCD={}",
        PCI_CAPID0_A::DIDOE.read(a),
        PCI_CAPID0_A::IGD.read(a),
        PCI_CAPID0_A::PDCD.read(a)
    );
    fstart_log::debug!(
        "X2APIC_EN={} DDPCD={} CDD={}",
        PCI_CAPID0_A::X2APIC_EN.read(a),
        PCI_CAPID0_A::DDPCD.read(a),
        PCI_CAPID0_A::CDD.read(a)
    );
    fstart_log::debug!(
        "FUFRD={} D1NM={} PCIE_RATIO_DIS={}",
        PCI_CAPID0_A::FUFRD.read(a),
        PCI_CAPID0_A::D1NM.read(a),
        PCI_CAPID0_A::PCIE_RATIO_DIS.read(a)
    );
    fstart_log::debug!(
        "DDRSZ={} PEGG2DIS={} DMIG2DIS={}",
        PCI_CAPID0_A::DDRSZ.read(a),
        PCI_CAPID0_A::PEGG2DIS.read(a),
        PCI_CAPID0_A::DMIG2DIS.read(a)
    );
    fstart_log::debug!(
        "VTDD={} FDEE={} ECCDIS={}",
        PCI_CAPID0_A::VTDD.read(a),
        PCI_CAPID0_A::FDEE.read(a),
        PCI_CAPID0_A::ECCDIS.read(a)
    );
    fstart_log::debug!(
        "DW={} PELWUD={} PEG10D={}",
        PCI_CAPID0_A::DW.read(a),
        PCI_CAPID0_A::PELWUD.read(a),
        PCI_CAPID0_A::PEG10D.read(a)
    );
    fstart_log::debug!(
        "PEG11D={} PEG12D={} DHDAD={}",
        PCI_CAPID0_A::PEG11D.read(a),
        PCI_CAPID0_A::PEG12D.read(a),
        PCI_CAPID0_A::DHDAD.read(a)
    );
    fstart_log::debug!("CAPID0_B: {:#010x}", b);
    fstart_log::debug!(
        "SPEGFX1={} DPEGFX1={} DMFC={}",
        PCI_CAPID0_B::SPEGFX1.read(b),
        PCI_CAPID0_B::DPEGFX1.read(b),
        PCI_CAPID0_B::DMFC.read(b)
    );
    fstart_log::debug!(
        "DDD={} HDCPD={} PEGX16D={}",
        PCI_CAPID0_B::DDD.read(b),
        PCI_CAPID0_B::HDCPD.read(b),
        PCI_CAPID0_B::PEGX16D.read(b)
    );
    fstart_log::debug!(
        "ADDGFXCAP={} ADDGFXEN={} PKGTYP={}",
        PCI_CAPID0_B::ADDGFXCAP.read(b),
        PCI_CAPID0_B::ADDGFXEN.read(b),
        PCI_CAPID0_B::PKGTYP.read(b)
    );
    fstart_log::debug!(
        "PEGG3_DIS={} PLL_REF100_CFG={} SOFTBIN={}",
        PCI_CAPID0_B::PEGG3_DIS.read(b),
        PCI_CAPID0_B::PLL_REF100_CFG.read(b),
        PCI_CAPID0_B::SOFTBIN.read(b)
    );
    fstart_log::debug!(
        "CACHESZ={} SMT={} OC_ENABLED_SSKU={}",
        PCI_CAPID0_B::CACHESZ.read(b),
        PCI_CAPID0_B::SMT.read(b),
        PCI_CAPID0_B::OC_ENABLED_SSKU.read(b)
    );
    fstart_log::debug!("OC_CTL_DSKU_DIS={}", PCI_CAPID0_B::OC_CTL_DSKU_DIS.read(b));
}
pub(super) fn report(m: MchBar) {
    let reference = if m.read32(0x5e00) & 16 != 0 {
        100000
    } else {
        133333
    };
    let clock = ((m.read32(0x5e04) & 15) * reference + 500) / 1000;
    let ch = m.read32(0x5000);
    fstart_log::info!(
        "DDR3 {} MHz; channel A/B/C = {}/{}/{}",
        clock,
        MAD_CHNL::CH_A.read(ch),
        MAD_CHNL::CH_B.read(ch),
        MAD_CHNL::CH_C.read(ch)
    );
    for c in 0..2 {
        let w = m.read32(0x5004 + 4 * c);
        fstart_log::debug!(
            "CH{} MAD_DIMM={:#010x}: A/B {} / {} MiB, DIMMA select {}, ECC I/O {} logic {}",
            c,
            w,
            256 * MAD_DIMM::DIMM_A_SIZE.read(w),
            256 * MAD_DIMM::DIMM_B_SIZE.read(w),
            MAD_DIMM::DIMM_A_SEL.read(w),
            MAD_DIMM::ECC_IO_ON.read(w),
            MAD_DIMM::ECC_LOGIC_ON.read(w)
        );
    }
}
