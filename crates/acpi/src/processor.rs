//! Processor objects: `ACPI0007` processor devices and their power objects
//! (`_PSS`, `_PCT`, `_PSD`, `_CST`; ACPI 6.5 §8.4).
//!
//! Processor devices replace the deprecated `Processor()` term. Their
//! `_UID` matches the ACPI processor UID of the MADT entry, which is what
//! lets an OS describe more than 255 processors (x2APIC).

use acpi_tables::Aml;
use acpi_tables::AmlSink;
use acpi_tables::aml::{Name, PackageBuilder, ResourceTemplate};
use acpi_tables::gas::GAS;
use alloc::vec::Vec;

use crate::AmlError;

/// Generic Register descriptor (ACPI 6.5 §6.4.3.7). `acpi_tables` 0.2's
/// `aml::Register` writes length 0x12 instead of 12, which OSes reject.
#[derive(Clone, Copy)]
pub struct RegisterDescriptor(pub GAS);

impl Aml for RegisterDescriptor {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(0x82);
        sink.word(12);
        self.0.to_aml_bytes(sink);
    }
}

/// A processor device NameSeg: `C` and three hex digits (4096 processors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessorName([u8; 4]);

impl ProcessorName {
    pub const fn new(index: u16) -> Self {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        assert!(
            index < 0x1000,
            "processor index exceeds the C000..CFFF names"
        );
        Self([
            b'C',
            HEX[(index >> 8) as usize & 0xf],
            HEX[(index >> 4) as usize & 0xf],
            HEX[index as usize & 0xf],
        ])
    }

    pub fn as_str(&self) -> &str {
        // Always ASCII.
        core::str::from_utf8(&self.0).unwrap_or("C000")
    }
}

/// `Device(name) { Name(_HID, "ACPI0007") Name(_UID, uid) <body> }`.
pub fn processor_device(name: ProcessorName, uid: u32, body: &[u8]) -> Result<Vec<u8>, AmlError> {
    let mut children = Vec::new();
    Name::new("_HID".into(), &"ACPI0007").to_aml_bytes(&mut children);
    Name::new("_UID".into(), &uid).to_aml_bytes(&mut children);
    children.extend_from_slice(body);
    crate::aml_linker::device_vec(name.as_str(), &children)
}

/// One `_PSS` performance state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerformanceState {
    pub mhz: u32,
    pub milliwatts: u32,
    pub latency_us: u32,
    pub bus_master_latency_us: u32,
    pub control: u32,
    pub status: u32,
}

/// `Name(_PSS, Package { Package { freq, power, lat, bm_lat, ctl, sts } ... })`.
pub fn pss_aml(states: &[PerformanceState]) -> Vec<u8> {
    let mut pss = PackageBuilder::new();
    states.iter().for_each(|s| {
        let mut p = PackageBuilder::new();
        [
            s.mhz,
            s.milliwatts,
            s.latency_us,
            s.bus_master_latency_us,
            s.control,
            s.status,
        ]
        .iter()
        .for_each(|v| p.add_element(v));
        pss.add_element(&p);
    });
    crate::serialize(&Name::new("_PSS".into(), &pss))
}

/// `Name(_PCT, Package { ResourceTemplate { Register(control) },
/// ResourceTemplate { Register(status) } })`.
pub fn pct_aml(control: GAS, status: GAS) -> Vec<u8> {
    let (control, status) = (RegisterDescriptor(control), RegisterDescriptor(status));
    let (control, status) = (
        ResourceTemplate::new(alloc::vec![&control]),
        ResourceTemplate::new(alloc::vec![&status]),
    );
    let mut pct = PackageBuilder::new();
    pct.add_element(&control);
    pct.add_element(&status);
    crate::serialize(&Name::new("_PCT".into(), &pct))
}

/// `_PSD` / `_CSD` coordination type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Coordination {
    SwAll = 0xfc,
    SwAny = 0xfd,
    HwAll = 0xfe,
}

/// `Name(_PSD, Package { Package { 5, 0, domain, coordination, processors } })`.
pub fn psd_aml(domain: u32, coordination: Coordination, processors: u32) -> Vec<u8> {
    let mut entry = PackageBuilder::new();
    [5u32, 0, domain, coordination as u32, processors]
        .iter()
        .for_each(|v| entry.add_element(v));
    let mut psd = PackageBuilder::new();
    psd.add_element(&entry);
    crate::serialize(&Name::new("_PSD".into(), &psd))
}

/// One `_CST` power state.
#[derive(Debug, Clone, Copy)]
pub struct PowerState {
    pub register: GAS,
    /// C-state type (1 = C1, 2 = C2, 3 = C3).
    pub kind: u8,
    pub latency_us: u16,
    pub milliwatts: u32,
}

/// `Name(_CST, Package { count, Package { Register, type, latency, power } ... })`.
pub fn cst_aml(states: &[PowerState]) -> Vec<u8> {
    let mut cst = PackageBuilder::new();
    cst.add_element(&(states.len() as u32));
    states.iter().for_each(|s| {
        let register = RegisterDescriptor(s.register);
        let buffer = ResourceTemplate::new(alloc::vec![&register]);
        let mut p = PackageBuilder::new();
        p.add_element(&buffer);
        p.add_element(&s.kind);
        p.add_element(&s.latency_us);
        p.add_element(&s.milliwatts);
        cst.add_element(&p);
    });
    crate::serialize(&Name::new("_CST".into(), &cst))
}

#[cfg(test)]
mod tests {
    use super::*;
    use acpi_tables::gas::{AccessSize, AddressSpace};

    #[test]
    fn names() {
        assert_eq!(ProcessorName::new(0).as_str(), "C000");
        assert_eq!(ProcessorName::new(0x1ff).as_str(), "C1FF");
    }

    #[test]
    fn device_has_hid_and_uid() {
        let dev = processor_device(ProcessorName::new(0x12), 0x12, &[]).unwrap();
        assert_eq!(&dev[..2], &[0x5b, 0x82]);
        assert!(dev.windows(4).any(|w| w == b"C012"));
        assert!(dev.windows(9).any(|w| w == b"ACPI0007\0"));
        // Name(_UID, 0x12)
        assert!(
            dev.windows(7)
                .any(|w| w == [0x08, b'_', b'U', b'I', b'D', 0x0a, 0x12])
        );
    }

    #[test]
    fn power_objects() {
        let pss = pss_aml(&[PerformanceState {
            mhz: 2000,
            milliwatts: 3000,
            latency_us: 0,
            bus_master_latency_us: 0,
            control: 0,
            status: 0,
        }]);
        assert_eq!(&pss[1..5], b"_PSS");
        let ffh = GAS::new(
            AddressSpace::FunctionalFixedHardware,
            2,
            2,
            AccessSize::ByteAccess,
            0,
        );
        let cst = cst_aml(&[PowerState {
            register: ffh,
            kind: 1,
            latency_us: 1,
            milliwatts: 0,
        }]);
        assert_eq!(&cst[1..5], b"_CST");
        // Register descriptor in FFH space.
        assert!(cst.windows(4).any(|w| w == [0x82, 0x0c, 0x00, 0x7f]));
        let psd = psd_aml(3, Coordination::HwAll, 2);
        assert!(psd.ends_with(&[0x0a, 0x05, 0x00, 0x0a, 0x03, 0x0a, 0xfe, 0x0a, 0x02]));
    }
}
