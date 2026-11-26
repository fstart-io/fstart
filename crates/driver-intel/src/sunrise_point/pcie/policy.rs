// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit cold PCH-H root-port policy. Defaults follow native C zero policy;
//! equalization coefficients are source defaults, never measured calibration.
use super::Error;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Speed {
    Auto = 0,
    Gen1 = 1,
    Gen2 = 2,
    Gen3 = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Equalization {
    Software,
    Hardware,
    Static,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aspm {
    Default,
    Disabled,
    L0s,
    L1,
    L0sL1,
    Auto,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L1Substates {
    Default,
    Disabled,
    L11,
    L12,
}
#[derive(Debug, Clone, Copy)]
pub struct PortPolicy {
    pub enabled: bool,
    pub hotplug: bool,
    pub aer: bool,
    pub ltr: bool,
    pub clkreq: Option<u8>,
    /// Source clock inventory. Native hide-disabled does NOT mask ICC source
    /// clocks, since disabled ports do not have a validated source assignment.
    pub clock_source: u8,
    pub payload256: bool,
    pub speed: Speed,
    pub eq: Equalization,
    pub aspm: Aspm,
    pub l1ss: L1Substates,
    pub upstream_preset: u8,
    pub downstream_preset: u8,
}
impl PortPolicy {
    pub const DISABLED: Self = Self {
        enabled: false,
        hotplug: false,
        aer: false,
        ltr: false,
        clkreq: None,
        clock_source: 0,
        payload256: false,
        speed: Speed::Auto,
        eq: Equalization::Software,
        aspm: Aspm::Default,
        l1ss: L1Substates::Default,
        upstream_preset: 5,
        downstream_preset: 7,
    };
}
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub ports: [PortPolicy; 24],
    pub coefficients: [[u8; 2]; 5],
    pub static_coefficients: [[u8; 2]; 24],
    /// Same platform policy as native soc_vtd_enabled(), additionally fuse
    /// gated by the actual SA. This is NOT a claim of DMA/interrupt protection.
    pub vtd_policy: bool,
    pub lan_clkreq: Option<u8>,
}
impl Config {
    pub const DEFAULT: Self = Self {
        ports: [PortPolicy::DISABLED; 24],
        coefficients: [[6, 2], [4, 2], [8, 2], [2, 2], [10, 2]],
        static_coefficients: [[6, 2]; 24],
        vtd_policy: true,
        lan_clkreq: None,
    };
    pub(crate) fn validate_interrupt_devices(&self, devices: &[u8]) -> Result<(), Error> {
        for rp in 0..24u8 {
            let devfn = fstart_pci::devfn(super::fabric::slot(rp), rp % 8);
            if self.ports[usize::from(rp)].enabled != devices.contains(&devfn) {
                return Err(Error::Policy);
            }
        }
        Ok(())
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.lan_clkreq.is_some_and(|n| n >= 16) {
            return Err(Error::Policy);
        }
        for p in &self.ports {
            if p.clkreq.is_some_and(|n| n >= 16)
                || p.upstream_preset > 10
                || p.downstream_preset > 10
                || !p.enabled && (p.hotplug || p.aer || p.ltr)
            {
                return Err(Error::Policy);
            }
        }
        if self
            .coefficients
            .iter()
            .chain(self.static_coefficients.iter())
            .flatten()
            .any(|n| *n > 63)
        {
            return Err(Error::Policy);
        }
        Ok(())
    }
}
