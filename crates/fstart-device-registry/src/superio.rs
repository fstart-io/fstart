use crate::DriverInstance;
use fstart_superio::SuperIoChip;
use heapless::Vec as HVec;

/// PnP logical-device descriptor derived from a concrete SuperIO driver config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperIoLdnDescriptor {
    /// Stable child-node suffix. Platform code prefixes this with the SuperIO node name.
    pub suffix: &'static str,
    /// Chip-specific logical-device number selected through config register `0x07`.
    pub ldn: u8,
    /// Whether this function is configured/enabled on this board.
    pub enabled: bool,
}

impl DriverInstance {
    /// Derive the SuperIO PnP logical devices for this driver instance.
    ///
    /// Boards select and configure the concrete SuperIO chip. The chip driver owns
    /// the mapping from function to LDN, so board crates never spell out what an
    /// LDN means.
    pub fn superio_ldns(&self) -> HVec<SuperIoLdnDescriptor, 16> {
        let mut ldns = HVec::new();
        match self {
            #[cfg(feature = "ite8721f")]
            Self::Ite8721f(cfg) => {
                push_superio_ldn(
                    &mut ldns,
                    "com1",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::COM1_LDN,
                    cfg.com1.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "com2",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::COM2_LDN,
                    cfg.com2.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "parallel",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::PARALLEL_LDN,
                    cfg.parallel.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "ec",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::EC_LDN,
                    cfg.env_controller.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "keyboard",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::KBC_LDN,
                    cfg.keyboard.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "mouse",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::MOUSE_LDN,
                    cfg.mouse.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "gpio",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::GPIO_LDN,
                    cfg.gpio.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "cir",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::CIR_LDN,
                    cfg.cir.is_some(),
                );
            }
            #[cfg(feature = "nsc-pc87382")]
            Self::NscPc87382(cfg) => {
                push_superio_ldn(
                    &mut ldns,
                    "com2",
                    <fstart_driver_nsc_pc87382::Pc87382Chip as SuperIoChip>::COM2_LDN,
                    cfg.com2.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "cir",
                    <fstart_driver_nsc_pc87382::Pc87382Chip as SuperIoChip>::CIR_LDN,
                    cfg.cir.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "gpio",
                    <fstart_driver_nsc_pc87382::Pc87382Chip as SuperIoChip>::GPIO_LDN,
                    cfg.gpio.is_some(),
                );
                ldns.push(SuperIoLdnDescriptor {
                    suffix: "dlpc",
                    ldn: fstart_driver_nsc_pc87382::PC87382_DLPC_LDN,
                    enabled: true,
                })
                .expect("SuperIO LDN descriptor capacity");
            }
            #[cfg(feature = "nsc-pc87392")]
            Self::NscPc87392(cfg) => {
                ldns.push(SuperIoLdnDescriptor {
                    suffix: "fdc",
                    ldn: fstart_driver_nsc_pc87392::PC87392_FDC_LDN,
                    enabled: false,
                })
                .expect("SuperIO LDN descriptor capacity");
                push_superio_ldn(
                    &mut ldns,
                    "parallel",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::PARALLEL_LDN,
                    cfg.parallel.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "com2",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::COM2_LDN,
                    cfg.com2.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "com1",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::COM1_LDN,
                    cfg.com1.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "gpio",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::GPIO_LDN,
                    cfg.gpio.is_some(),
                );
                ldns.push(SuperIoLdnDescriptor {
                    suffix: "wdt",
                    ldn: fstart_driver_nsc_pc87392::PC87392_WDT_LDN,
                    enabled: false,
                })
                .expect("SuperIO LDN descriptor capacity");
            }
            _ => {}
        }
        ldns
    }
}

fn push_superio_ldn(
    ldns: &mut HVec<SuperIoLdnDescriptor, 16>,
    suffix: &'static str,
    ldn: Option<u8>,
    enabled: bool,
) {
    if let Some(ldn) = ldn {
        ldns.push(SuperIoLdnDescriptor {
            suffix,
            ldn,
            enabled,
        })
        .expect("SuperIO LDN descriptor capacity");
    }
}
