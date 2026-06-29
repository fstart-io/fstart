// ---------------------------------------------------------------------------
// Re-export driver config types (conditionally based on features)
// ---------------------------------------------------------------------------

#[cfg(feature = "ns16550")]
pub mod ns16550 {
    pub use fstart_driver_ns16550::{AccessMode, Ns16550Config};
}

#[cfg(feature = "pl011")]
pub mod pl011 {
    pub use fstart_driver_pl011::Pl011Config;
}

#[cfg(feature = "designware-i2c")]
pub mod designware_i2c {
    pub use fstart_driver_designware_i2c::DesignwareI2cConfig;
}

#[cfg(feature = "sunxi-a20-ccu")]
pub mod sunxi_a20_ccu {
    pub use fstart_driver_sunxi_ccu::SunxiA20CcuConfig;
}

#[cfg(feature = "sunxi-h3-ccu")]
pub mod sunxi_h3_ccu {
    pub use fstart_driver_sunxi_h3_ccu::SunxiH3CcuConfig;
}

#[cfg(feature = "sunxi-a20-dramc")]
pub mod sunxi_a20_dramc {
    pub use fstart_driver_sunxi_a20_dramc::SunxiA20DramcConfig;
}

#[cfg(feature = "sunxi-h3-dramc")]
pub mod sunxi_h3_dramc {
    pub use fstart_driver_sunxi_h3_dramc::{SunxiDramcVariant, SunxiH3DramcConfig};
}

#[cfg(feature = "sunxi-mmc")]
pub mod sunxi_mmc {
    pub use fstart_driver_sunxi_mmc::SunxiMmcConfig;
}

#[cfg(feature = "sunxi-spi")]
pub mod sunxi_spi {
    pub use fstart_driver_sunxi_spi::SunxiSpiConfig;
}

#[cfg(feature = "sunxi-d1-ccu")]
pub mod sunxi_d1_ccu {
    pub use fstart_driver_sunxi_d1_ccu::SunxiD1CcuConfig;
}

#[cfg(feature = "sunxi-d1-dramc")]
pub mod sunxi_d1_dramc {
    pub use fstart_driver_sunxi_d1_dramc::SunxiD1DramcConfig;
}

#[cfg(feature = "sifive-uart")]
pub mod sifive_uart {
    pub use fstart_driver_sifive_uart::SifiveUartConfig;
}

#[cfg(feature = "fu740-prci")]
pub mod fu740_prci {
    pub use fstart_driver_fu740_prci::Fu740PrciConfig;
}

#[cfg(feature = "fu740-ddr")]
pub mod fu740_ddr {
    pub use fstart_driver_fu740_ddr::Fu740DdrConfig;
}

#[cfg(feature = "pci-ecam")]
pub mod pci_ecam {
    pub use fstart_driver_pci_ecam::PciEcamConfig;
}

#[cfg(feature = "bochs-display")]
pub mod bochs_display {
    pub use fstart_driver_bochs_display::BochsDisplayConfig;
}

#[cfg(feature = "qemu-fw-cfg")]
pub mod qemu_fw_cfg {
    pub use fstart_driver_qemu_fw_cfg::QemuFwCfgConfig;
}

#[cfg(feature = "q35-hostbridge")]
pub mod q35_hostbridge {
    pub use fstart_driver_q35_hostbridge::Q35HostBridgeConfig;
}

#[cfg(feature = "ite8721f")]
pub mod ite8721f {
    pub use fstart_driver_ite8721f::Ite8721fConfig;
}

#[cfg(feature = "nsc-pc87382")]
pub mod nsc_pc87382 {
    pub use fstart_driver_nsc_pc87382::Pc87382Config;
}

#[cfg(feature = "nsc-pc87392")]
pub mod nsc_pc87392 {
    pub use fstart_driver_nsc_pc87392::Pc87392Config;
}

#[cfg(feature = "intel-pineview")]
pub mod intel_pineview {
    pub use fstart_driver_intel_pineview::IntelPineviewConfig;
}

#[cfg(feature = "intel-ich7")]
pub mod intel_ich7 {
    pub use fstart_driver_intel_ich7::IntelIch7Config;
}

#[cfg(feature = "intel-gm965")]
pub mod intel_gm965 {
    pub use fstart_driver_intel_gm965::IntelGm965Config;
}

#[cfg(feature = "intel-ich8")]
pub mod intel_ich8 {
    pub use fstart_driver_intel_ich8::IntelIch8Config;
}

#[cfg(feature = "lenovo-x61-mainboard")]
pub mod lenovo_x61_mainboard {
    pub use fstart_mainboard_lenovo_x61::LenovoX61MainboardConfig;
}

#[cfg(feature = "i2c-ck505")]
pub mod i2c_ck505 {
    pub use fstart_driver_i2c_ck505::I2cCk505Config;
}
