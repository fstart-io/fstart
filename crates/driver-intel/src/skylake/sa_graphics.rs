// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native PEG VGA scanning and graphics policy (sa_pre_mem_native.c:1236–1348,
//! 1575–1622; SHA256 27e7566c9724ee95db1b69db040a64b5211a8460eb789b1758f7ebd2cf06a372).
//! Scans actual function-zero config space, as native does. No BAR probing,
//! heap or full OS allocator. Temporary buses are restored on every exit.
use super::{
    cpu::Cpu,
    sa_dmi,
    sa_peg::{Error, Resources},
    sa_registers::GraphicsControl,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u8, Aperture [ SIZE OFFSET(0) NUMBITS(5) [] ]];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryDisplay {
    Integrated,
    Peg,
    Pci,
    Auto,
}
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Resolved option AND actual devicetree IGD enable.
    pub igd_enabled: bool,
    pub primary: PrimaryDisplay,
    /// Source CFR options: 0/1/2/3/4 represent 0/32/64/96/128 MiB.
    pub stolen: u8,
    /// Source CFR options: 0/1/2 represent 128/256/512 MiB.
    pub aperture: u8,
    pub skip_external_scan: bool,
}
impl Policy {
    pub(crate) fn validate(self) -> Result<(), Error> {
        if self.stolen > 4 || !matches!(self.aperture, 0..=2) {
            return Err(Error::BadPolicy);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub igd_present: bool,
    pub igd_enabled: bool,
    pub vga_enabled: bool,
    pub stolen_code: u8,
    pub aperture_code: u8,
}
#[derive(Clone, Copy, Default)]
struct Bridge {
    bus: u8,
    device: u8,
    secondary: u8,
    next_device: u8,
    saved: u32,
}
/// Caller-owned bounded DFS records, not a global bus allocator. At most 32
/// downstream bridges; exhaustion is fatal with temporary buses restored.
pub struct ScanWorkspace {
    bridges: [Bridge; 32],
}
impl ScanWorkspace {
    pub const INITIAL: Self = Self {
        bridges: [Bridge {
            bus: 0,
            device: 0,
            secondary: 0,
            next_device: 0,
            saved: 0,
        }; 32],
    };
    /// # Safety
    /// Aligned/exclusive destination, no old drop state or ROM-image overlap.
    /// Initializes DFS storage only; no bus scan or VGA claim is performed.
    pub unsafe fn initialize_at(destination: *mut Self) {
        static IMAGE: ScanWorkspace = ScanWorkspace::INITIAL;
        unsafe { core::ptr::copy_nonoverlapping(&IMAGE, destination, 1) };
    }
}
impl Default for ScanWorkspace {
    fn default() -> Self {
        Self::INITIAL
    }
}
fn is_vga(class: u16) -> bool {
    matches!(class, 0x0001 | 0x0300)
}

impl Resources<'_> {
    fn bridge_vga(
        &self,
        root: fstart_pci::EcamDevice,
        work: &mut ScanWorkspace,
    ) -> Result<bool, Error> {
        let saved = root.read32(0x18);
        root.write8(0x19, 1);
        root.write8(0x1a, self.root.bus_end);
        let mut count = 0usize;
        let result = (|| {
            let mut bus = 1u8;
            let mut device = 0u8;
            let mut highest = 1u8;
            loop {
                if device == 32 {
                    if bus == 1 {
                        break;
                    }
                    let index = work.bridges[..count]
                        .iter()
                        .position(|b| b.secondary == bus)
                        .ok_or(Error::InvalidEndpoint)?;
                    let frame = &work.bridges[index];
                    self.dev(frame.bus, frame.device, 0)?.write8(0x1a, highest);
                    bus = frame.bus;
                    device = frame.next_device;
                    continue;
                }
                let d = self.dev(bus, device, 0)?;
                device += 1;
                if !d.is_present() || d.read16(0xa) != 0x0604 {
                    continue;
                }
                if count == work.bridges.len() || highest == self.root.bus_end {
                    return Err(Error::Capacity);
                }
                let old = d.read32(0x18);
                if old & 0x00ffffff != 0 {
                    return Err(Error::BusInUse);
                }
                highest += 1;
                work.bridges[count] = Bridge {
                    bus,
                    device: device - 1,
                    secondary: highest,
                    next_device: device,
                    saved: old,
                };
                count += 1;
                d.write8(0x18, bus);
                d.write8(0x19, highest);
                d.write8(0x1a, self.root.bus_end);
                self.dev(highest, 0, 0)?.write16(0, 0);
                bus = highest;
                device = 0;
            }
            let mut found = false;
            for bus in 1..=highest {
                for device in 0..32 {
                    let d = self.dev(bus, device, 0)?;
                    if d.is_present() && is_vga(d.read16(0xa)) {
                        found = true;
                    }
                }
            }
            Ok(found)
        })();
        // Reverse discovery keeps child config reachable until it is restored.
        for frame in work.bridges[..count].iter().rev() {
            self.dev(frame.bus, frame.device, 0)?
                .write32(0x18, frame.saved);
        }
        root.write32(0x18, saved);
        result
    }
    pub(crate) fn validate_scan_ownership(&self, policy: Policy) -> Result<(), Error> {
        if policy.skip_external_scan {
            return Ok(());
        }
        let deven = self.sa()?.read16(0x54);
        // Validate ownership of all candidate root buses before the first write.
        for f in 0..3 {
            let d = self.dev(0, 1, f)?;
            if deven & (8 >> f) != 0
                && d.is_present()
                && d.read8(0xba) & 0x40 != 0
                && d.read32(0x18) & 0x00ffff00 != 0
            {
                return Err(Error::BusInUse);
            }
        }
        Ok(())
    }
    /// Genuine optional PEG VGA scan. Source scans function zero only. Unlike
    /// native's bridge-only exception, explicit IGD-primary policy is honored
    /// for both direct and bridged VGA devices.
    pub fn scan_vga(
        &self,
        dmi: &sa_dmi::Dmi<'_>,
        cpu: Cpu,
        mode: MemoryBootMode,
        dmi_policy: sa_dmi::Policy,
        policy: Policy,
        work: &mut ScanWorkspace,
    ) -> Result<Option<u8>, Error> {
        policy.validate()?;
        self.validate(dmi, cpu, mode, dmi_policy)?;
        if policy.skip_external_scan {
            return Ok(None);
        }
        let sa = self.sa()?;
        let deven = sa.read16(0x54);
        let igd = self.dev(0, 2, 0)?.is_present();
        self.validate_scan_ownership(policy)?;
        let mut selected = None;
        for f in 0..3 {
            let d = self.dev(0, 1, f)?;
            if deven & (8 >> f) == 0 || !d.is_present() || d.read8(0xba) & 0x40 == 0 {
                continue;
            }
            // A present but untrained card is not a successful absence gate.
            if d.read16(0x11a) & 2 != 0 {
                return Err(Error::LinkTimeout(f));
            }
            let saved = d.read32(0x18);
            d.write32(0x18, (saved & 0xff0000ff) | 0x00010100);
            let ep = self.dev(1, 0, 0)?;
            ep.write16(0, 0);
            let result = (|| {
                if !ep.is_present() {
                    return Ok(false);
                }
                let class = ep.read16(0xa);
                if is_vga(class) {
                    Ok(true)
                } else if class == 0x0604 {
                    self.bridge_vga(d, work)
                } else {
                    Ok(false)
                }
            })();
            d.write32(0x18, saved);
            if result?
                && selected.is_none()
                && !(igd && policy.primary == PrimaryDisplay::Integrated)
            {
                let r = unsafe { d.register::<u16, ()>(0x3e) };
                r.set(r.get() | 0x18);
                selected = Some(f);
            }
        }
        if let Some(primary) = selected {
            for f in 0..3 {
                let d = self.dev(0, 1, f)?;
                if f != primary && d.is_present() {
                    let r = unsafe { d.register::<u16, ()>(0x3e) };
                    r.set(r.get() | 4);
                }
            }
        }
        Ok(selected)
    }
    /// Source graphics policy only; no stolen-memory mapping or RAM readiness.
    pub fn graphics(
        &self,
        dmi: &sa_dmi::Dmi<'_>,
        cpu: Cpu,
        mode: MemoryBootMode,
        dmi_policy: sa_dmi::Policy,
        policy: Policy,
        peg_primary: bool,
    ) -> Result<Outcome, Error> {
        policy.validate()?;
        self.validate(dmi, cpu, mode, dmi_policy)?;
        let sa = self.sa()?;
        let igd = self.dev(0, 2, 0)?;
        let present = igd.is_present();
        let enabled = present && policy.igd_enabled;
        let ggc = unsafe { sa.register::<u16, GraphicsControl::Register>(0x50) };
        let vga = enabled && (!peg_primary || policy.primary == PrimaryDisplay::Integrated);
        if enabled {
            ggc.modify(
                GraphicsControl::STOLEN.val(u16::from(policy.stolen)) + GraphicsControl::GTT.val(3),
            );
            ggc.modify(GraphicsControl::VGA_DISABLE.val(u16::from(!vga)));
        } else {
            ggc.modify(
                GraphicsControl::STOLEN.val(0)
                    + GraphicsControl::GTT.val(0)
                    + GraphicsControl::VGA_DISABLE::SET,
            );
            let deven = unsafe { sa.register::<u32, ()>(0x54) };
            deven.set(deven.get() & !(1 << 4));
        }
        let aperture = if enabled { policy.aperture } else { 1 };
        if present {
            unsafe { igd.register::<u8, Aperture::Register>(0x62) }
                .modify(Aperture::SIZE.val(aperture));
        }
        Ok(Outcome {
            igd_present: present,
            igd_enabled: enabled,
            vga_enabled: vga,
            stolen_code: if enabled { policy.stolen } else { 0 },
            aperture_code: aperture,
        })
    }
}

#[cfg(test)]
#[path = "sa_graphics_tests.rs"]
mod tests;
