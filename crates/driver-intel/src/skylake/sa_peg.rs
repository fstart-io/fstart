// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native PEG pre-memory services, not PCI resource allocation or memory init.
//!
//! Port of coreboot soc/intel/skylake/romstage/sa_pre_mem_native.c:358–439,
//! 690–1348,1373–1573; SHA256
//! 27e7566c9724ee95db1b69db040a64b5211a8460eb789b1758f7ebd2cf06a372.
//! Opaque recipes are source values, never measurements. Native silent poll
//! exhaustion is fatal here after the source recovery attempts. No rollback of
//! training/power transitions is claimed. Bus numbers alone are restored.
use super::{
    cpu::{Cpu, Package},
    dmi_registers::SaMsrio,
    registers::MchRegisters,
    sa_dmi,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess};
use fstart_pci::{EcamDevice, PciAddress, ecam_host::PciEcam};
use tock_registers::interfaces::{Readable, Writeable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Dmi(sa_dmi::Error),
    BadPolicy,
    Capacity,
    BusInUse,
    InvalidPort,
    InvalidEndpoint,
    CapabilityChain,
    LinkTimeout(u8),
    EqualizationTimeout(u8),
    Degraded(u8),
    DisableTimeout(u8),
}

/// Source devicetree enables, supplied as immutable board facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Devices {
    pub peg: [bool; 3],
    pub igd: bool,
    pub thermal: bool,
    pub imaging: bool,
    pub chap: bool,
    pub gmm: bool,
}
impl Devices {
    fn deven(self) -> u32 {
        1 | (u32::from(self.igd) << 4)
            | (u32::from(self.thermal) << 7)
            | (u32::from(self.imaging) << 10)
            | (u32::from(self.chap) << 14)
            | (u32::from(self.gmm) << 15)
            | (u32::from(self.peg[0]) << 3)
            | (u32::from(self.peg[1]) << 2)
            | (u32::from(self.peg[2]) << 1)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub devices: Devices,
    /// Actual widths: 0=AUTO, 1,2,4,8. Source selectors are decoded by caller.
    pub max_width: [u8; 3],
    /// One-ms samples for each link/EQ phase (native:100).
    pub poll_ms: u16,
    /// 100-us samples for disabling a controller (native:1000).
    pub disable_polls: u16,
}
impl Policy {
    pub(crate) fn validate(self) -> Result<(), Error> {
        if self.poll_ms == 0
            || self.disable_polls == 0
            || self
                .max_width
                .iter()
                .any(|v| !matches!(v, 0 | 1 | 2 | 4 | 8))
        {
            return Err(Error::BadPolicy);
        }
        Ok(())
    }
}

/// Stage-owned UC resources; does not initialize the full PCI allocator.
pub struct Resources<'a> {
    pub(crate) root: fstart_pci::PciRootInfo,
    mapped_bytes: usize,
    pub(crate) mch: &'a MchRegisters,
}
impl<'a> Resources<'a> {
    /// # Safety
    /// Exclusively own the mapped bus-zero ECAM prefix and whole fixed MCHBAR.
    /// Both mappings must refer to this same SA, be UC, and remain live for
    /// `'a`. No other enumerator/DMA user may own temporary downstream buses.
    /// Actual PMC boot classification is required by each operation.
    pub unsafe fn new(pci: &'a PciEcam, mch: &'a MchRegisters) -> Self {
        Self {
            root: fstart_pci::PciRootInfo {
                segment: 0,
                ecam_base: pci.ecam_base(),
                bus_start: pci.bus_start(),
                bus_end: pci.bus_end(),
            },
            mapped_bytes: pci.ecam_size() as usize,
            mch,
        }
    }

    /// Bind the same configuration-space service without a full PCI allocator
    /// and its device/resource arrays in CAR.
    /// # Safety
    /// Same mapping, ownership and temporary-bus requirements as new().
    pub unsafe fn from_ecam(
        root: fstart_pci::PciRootInfo,
        mapped_bytes: usize,
        mch: &'a MchRegisters,
    ) -> Result<Self, Error> {
        if root.segment != 0
            || root.bus_start != 0
            || root.bus_end < 1
            || root.ecam_base == 0
            || root.ecam_base & 0xfffff != 0
            || mapped_bytes < (usize::from(root.bus_end) + 1) << 20
            || root.ecam_base.checked_add(mapped_bytes as u64).is_none()
        {
            return Err(Error::Capacity);
        }
        Ok(Self {
            root,
            mapped_bytes,
            mch,
        })
    }

    pub(crate) fn dev(&self, bus: u8, dev: u8, function: u8) -> Result<EcamDevice, Error> {
        EcamDevice::from_root(
            self.root,
            self.mapped_bytes,
            PciAddress::new(0, bus, dev, function),
        )
        .ok_or(Error::Capacity)
    }
    pub(crate) fn sa(&self) -> Result<EcamDevice, Error> {
        self.dev(0, 0, 0)
    }
    fn port(&self, function: u8) -> EcamDevice {
        self.dev(0, 1, function).expect("validated bus zero")
    }
    pub(crate) fn validate(
        &self,
        dmi: &sa_dmi::Dmi<'_>,
        cpu: Cpu,
        mode: MemoryBootMode,
        policy: sa_dmi::Policy,
    ) -> Result<(), Error> {
        // Bus 1 is mandatory for genuine endpoint config discovery. The
        // mapping may be a prefix of the physical PCIEXBAR resource.
        if self.root.bus_start != 0
            || self.root.bus_end < 1
            || u16::from(self.root.bus_end) >= policy.ecam.buses
            || (self.mapped_bytes as u64) < (u64::from(self.root.bus_end) + 1) << 20
        {
            return Err(Error::Capacity);
        }
        dmi.preflight(&self.sa()?, cpu, mode, policy)
            .map_err(Error::Dmi)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PortState {
    #[default]
    Absent,
    Disabled,
    Empty,
    Active,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Link {
    pub function: u8,
    pub state: PortState,
    pub speed: u8,
    pub width: u8,
    pub endpoint_speed: u8,
    pub endpoint_width: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub controller_present: bool,
    pub links: [Link; 3],
    pub disabled: u8,
}
#[derive(Clone, Copy, Default)]
struct Root {
    function: u8,
    width: u8,
    start: u8,
    reverse: bool,
    present: bool,
    ep_speed: u8,
    max_width: u8,
    ep_width: u8,
    max_speed: u8,
}
impl Root {
    fn bundles(self, first: u8, end: u8) -> u8 {
        let mut mask = 0;
        for i in first..end {
            let lane = if self.reverse {
                self.start - i
            } else {
                self.start + i
            };
            mask |= 1 << (lane / 2);
        }
        mask
    }
}

impl Resources<'_> {
    fn roots(&self) -> Result<([Root; 3], usize), Error> {
        let p0 = self.port(0);
        let strap = (p0.read32(0x504) >> 16) & 3;
        let widths = match strap {
            0 => [8, 4, 4],
            2 => [8, 8, 0],
            _ => [16, 0, 0],
        };
        let reverse = p0.read32(0xd0c) & (1 << 20) != 0;
        let mut start = if reverse { 15i16 } else { 0 };
        let mut roots = [Root::default(); 3];
        let mut count = 0;
        for f in 0..3u8 {
            let width = widths[f as usize];
            if width == 0 || !self.port(f).is_present() {
                continue;
            }
            if self.port(f).vendor_id() != 0x8086 || self.port(f).read16(0xa) != 0x0604 {
                return Err(Error::InvalidPort);
            }
            roots[count] = Root {
                function: f,
                width,
                start: start as u8,
                reverse,
                max_speed: (self.port(f).read32(0xac) & 15) as u8,
                ..Root::default()
            };
            start += if reverse {
                -(i16::from(width))
            } else {
                i16::from(width)
            };
            count += 1;
        }
        Ok((roots, count))
    }
    fn recipe(&self, cpu: Cpu, msrs: &impl MsrAccess) {
        use super::dmi_registers::{
            Bundle1, Bundle2, Bundle3, Bundle4, Lane0, Lane1, LaneIo, LaneTx,
        };
        use tock_registers::interfaces::ReadWriteable;
        let p = self.port(0);
        for b in 0..8u16 {
            unsafe { p.register::<u32, Bundle1::Register>(0x904 + b * 0x20) }
                .modify(Bundle1::IGACQ.val(0));
            unsafe { p.register::<u32, Bundle2::Register>(0x908 + b * 0x20) }.modify(
                Bundle2::RXVCMDSSEL.val(1)
                    + Bundle2::TXEQCUR.val(0x2a)
                    + Bundle2::TXEQPOSTCUR.val(0xc)
                    + Bundle2::TXEQPRECUR.val(8),
            );
            unsafe { p.register::<u32, Bundle3::Register>(0x90c + b * 0x20) }
                .modify(Bundle3::RXRTBIN.val(3) + Bundle3::TXRTBIN.val(4) + Bundle3::PGTRK.val(8));
            unsafe { p.register::<u32, Bundle4::Register>(0x910 + b * 0x20) }.modify(
                Bundle4::G2RXCTLEPEAK.val(2)
                    + Bundle4::SPARE.val(0x30)
                    + Bundle4::G3RXCTLEPEAK.val(0),
            );
        }
        for lane in 0..16u16 {
            unsafe { p.register::<u32, Lane0::Register>(0xa00 + lane * 0x10) }
                .modify(Lane0::G23RXVREF.val(0xa));
            unsafe { p.register::<u32, Lane1::Register>(0xa04 + lane * 0x10) }
                .modify(Lane1::CDRPDDATMODE::SET);
            unsafe { p.register::<u32, LaneTx::Register>(0x70c + lane * 0x10) }
                .modify(LaneTx::OVERRIDE::SET + LaneTx::PI_RESSEL.val(0x10) + LaneTx::SWING.val(0));
            unsafe { p.register::<u32, LaneIo::Register>(0x808 + lane * 0x10) }
                .modify(LaneIo::GEN3_GAIN.val(3));
        }
        for b in 0..8u16 {
            let r = unsafe { p.register::<u32, ()>(0x91c + b * 0x20) };
            r.set(
                (r.get()
                    & !((1 << 30)
                        | (7 << 27)
                        | (7 << 23)
                        | (3 << 17)
                        | (3 << 13)
                        | (3 << 9)
                        | (3 << 6)))
                    | (3 << 27)
                    | (2 << 23),
            );
        }
        for f in 0..3 {
            let d = self.port(f);
            if !d.is_present() {
                continue;
            }
            let r = unsafe { d.register::<u32, ()>(0xdd8) };
            r.set(
                (r.get() & !((1 << 0) | (0x3f << 20) | (0x3f << 26)))
                    | 1
                    | (0x14 << 20)
                    | (0x3e << 26),
            );
        }
        for (off, value) in [
            (0xdc0, 0x32012b00),
            (0xdc4, 0x003b000c),
            (0xdc8, 0x00f802b4),
            (0xdcc, 0x34280d88),
            (0xdd0, 0x09369200),
            (0xdd4, 0x00c8c26c),
            (0xddc, 0x00016a00),
        ] {
            p.write32(off, value);
        }
        if cpu.package == Package::DesktopHalo {
            let staged: Msr<SaMsrio::Register> = Msr::new(0x791);
            for (offset, value) in [(0, 0x02048000u64), (0x10, 0x01805576), (4, 0x0004000a)] {
                for b in 0..8u64 {
                    msrs.reg(staged)
                        .set(((0x200 + offset + b * 0x20) << 32) | value);
                }
            }
        }
    }
    fn active(&self, root: Root) -> bool {
        self.port(root.function).read16(0x11a) & 2 == 0
    }
    fn presence(&self, f: u8) -> bool {
        self.port(f).read8(0xba) & (1 << 6) != 0
    }
    fn width(&self, root: Root) -> u8 {
        ((self.port(root.function).read16(0xb2) >> 4) & 0x3f) as u8
    }
    fn target(&self, root: Root, speed: u8) {
        let d = self.port(root.function);
        let r = unsafe { d.register::<u16, super::dmi_registers::LinkControl2::Register>(0xd0) };
        use tock_registers::interfaces::ReadWriteable;
        r.modify(super::dmi_registers::LinkControl2::SPEED.val(u16::from(speed)));
    }
    fn retrain(&self, root: Root) {
        let d = self.port(root.function);
        let r = unsafe { d.register::<u16, super::dmi_registers::LinkControl::Register>(0xb0) };
        use tock_registers::interfaces::ReadWriteable;
        r.modify(super::dmi_registers::LinkControl::RETRAIN::SET);
    }
    fn bounce(&self, root: Root, delay: &mut impl DelayNs) {
        let d = self.port(root.function);
        let r = unsafe { d.register::<u16, ()>(0xb0) };
        r.set(r.get() | 0x10);
        delay.delay_us(1);
        r.set(r.get() & !0x10);
        delay.delay_us(1);
    }
    fn wait_active(
        &self,
        roots: &[Root],
        enabled: [bool; 3],
        force: bool,
        budget: u16,
        delay: &mut impl DelayNs,
    ) {
        for elapsed in 0..=budget {
            if roots.iter().all(|r| {
                (!force && !enabled[r.function as usize])
                    || (self.active(*r) && self.port(r.function).read16(0xb2) & (1 << 11) == 0)
            }) {
                return;
            }
            if elapsed < budget {
                delay.delay_ms(1);
            }
        }
        // Recovery must still run. Final mandatory active checks are fatal.
    }
    fn wait_l0(&self, root: Root, budget: u16, delay: &mut impl DelayNs) -> Result<(), Error> {
        let d = self.port(root.function);
        let mut eq = root.ep_speed < 3;
        for elapsed in 0..=budget {
            if !eq {
                eq = d.read16(0xd2) & 2 != 0;
            }
            // A stale L0/EC sample before acceptance of RETRAIN is not
            // completion. Observe the actual self-clearing request and LT.
            if eq
                && d.read16(0xb0) & 0x20 == 0
                && d.read16(0xb2) & (1 << 11) == 0
                && (d.read32(0x214) >> 16) & 0xf == 7
            {
                return Ok(());
            }
            if elapsed < budget {
                delay.delay_ms(1);
            }
        }
        Err(if eq {
            Error::LinkTimeout(root.function)
        } else {
            Error::EqualizationTimeout(root.function)
        })
    }
    fn failing(&self, roots: &[Root], policy: Policy, degraded: bool) -> u8 {
        let mut mask = 0;
        for r in roots {
            if if degraded {
                r.present && self.width(*r) < r.max_width
            } else {
                !self.active(*r)
                    && self.presence(r.function)
                    && policy.devices.peg[r.function as usize]
            } {
                mask |= 1 << r.function;
            }
        }
        mask
    }
    fn icomp(&self, root: Root, disable: bool) {
        let mask = root.bundles(0, root.width);
        let p = self.port(0);
        for b in 0..8u16 {
            if mask & (1 << b) != 0 {
                let r = unsafe { p.register::<u32, ()>(0xe04 + b * 0x20) };
                r.set((r.get() & !(1 << 17)) | (u32::from(disable) << 17));
            }
        }
    }
    fn icomp_check(
        &self,
        roots: &[Root],
        policy: Policy,
        degraded: bool,
        delay: &mut impl DelayNs,
    ) {
        let mask = self.failing(roots, policy, degraded);
        if mask == 0 {
            return;
        }
        for r in roots {
            if mask & (1 << r.function) != 0 {
                self.icomp(*r, false);
                self.bounce(*r, delay);
            }
        }
        self.wait_active(roots, policy.devices.peg, true, policy.poll_ms, delay);
        let mask = self.failing(roots, policy, degraded);
        for r in roots {
            if mask & (1 << r.function) != 0 {
                self.icomp(*r, true);
                self.bounce(*r, delay);
            }
        }
    }
    fn endpoint(&self, root: &mut Root, policy: Policy) -> Result<(), Error> {
        root.present = self.active(*root) && policy.devices.peg[root.function as usize];
        if !root.present {
            return Ok(());
        }
        let d = self.port(root.function);
        let saved = d.read32(0x18);
        if saved & 0x00ffff00 != 0 {
            return Err(Error::BusInUse);
        }
        d.write32(0x18, (saved & 0xff0000ff) | 0x00010100);
        let ep = self.dev(1, 0, 0)?;
        ep.write16(0, 0);
        let result = (|| {
            let cap = pcie_capability(&ep)?;
            let (speed, width) = if let Some(cap) = cap {
                let l = ep.read32(cap + 0xc);
                ((l & 0xf) as u8, ((l >> 4) & 0x3f) as u8)
            } else {
                let l = d.read16(0xb2);
                ((l & 0xf) as u8, ((l >> 4) & 0x3f) as u8)
            };
            if !(1..=3).contains(&speed) || width == 0 || width > 16 {
                return Err(Error::InvalidEndpoint);
            }
            root.ep_speed = speed;
            root.ep_width = width;
            root.max_width = root.width.min(width);
            Ok(())
        })();
        d.write32(0x18, saved);
        result
    }
    fn power_bundles(&self, mask: u8) {
        let p = self.port(0);
        for b in 0..8u16 {
            if mask & (1 << b) != 0 {
                let r = unsafe { p.register::<u32, ()>(0x91c + b * 0x20) };
                r.set(r.get() | (1 << 31));
            }
        }
    }
    fn power_all(&self, f: u8) {
        let p = self.port(0);
        let strap = (p.read32(0x504) >> 16) & 3;
        let rev = p.read32(0xd0c) & (1 << 20) != 0;
        let range = match f {
            0 if strap == 3 => Some((0, 7)),
            0 => Some(if rev { (4, 7) } else { (0, 3) }),
            1 if strap == 2 => Some(if rev { (0, 3) } else { (4, 7) }),
            1 if strap == 0 => Some(if rev { (2, 3) } else { (4, 5) }),
            2 if strap == 0 => Some(if rev { (0, 1) } else { (6, 7) }),
            _ => None,
        };
        if let Some((start, end)) = range {
            let mut mask = 0;
            for b in start..=end {
                mask |= 1 << b;
            }
            self.power_bundles(mask);
        }
    }
    fn disable(
        &self,
        mut mask: u8,
        cpu: Cpu,
        policy: Policy,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let keep0 = mask & 6 != 6 && mask & 1 != 0;
        if mask & 6 != 6 {
            mask &= !1;
        }
        for f in (0..3u8).rev() {
            let d = self.port(f);
            if !d.is_present() || (mask & (1 << f) == 0 && !(f == 0 && keep0)) {
                continue;
            }
            let r = unsafe { d.register::<u32, ()>(0xd20) };
            r.set(r.get() | (1 << 30));
            let r = unsafe { d.register::<u8, ()>(0xb0) };
            r.set(r.get() | 0x10);
            let mut done = false;
            for elapsed in 0..=policy.disable_polls {
                let state = (self.port(0).read32(0x464) >> (8 * f)) & 0x3f;
                if state == 0 || state == 2 {
                    done = true;
                    break;
                }
                if elapsed < policy.disable_polls {
                    delay.delay_us(100);
                }
            }
            if !done {
                return Err(Error::DisableTimeout(f));
            }
            let r = unsafe { d.register::<u8, ()>(0xc20) };
            r.set(r.get() | 0x30);
            self.power_all(f);
            if mask & (1 << f) != 0 {
                let sa = self.sa()?;
                let r = unsafe { sa.register::<u8, ()>(0x54) };
                r.set(r.get() & !(8 >> f));
            }
        }
        if mask == 7 && cpu.package == Package::DesktopHalo {
            let r = &self.mch.bios_reset_complete;
            r.set(r.get() | 8);
        }
        Ok(())
    }
    fn credits(&self, mask: u8) {
        let table = if mask & 4 == 0 {
            &[
                0x08241251, 0x08241251, 0x0220a412, 0x10, 0x6d6, 0x00339dc7, 0x00301080,
                0x00339dc7, 0x00601080, 0x041c7388, 0x200, 0x4100, 0x48a,
            ]
        } else if mask & 2 == 0 {
            &[
                0x08241051, 0x08241051, 0x02208412, 0x10, 0x758, 0x003039c8, 0x00301080,
                0x003039c7, 0x00601080, 0x0400e388, 0x200, 0x4100, 0x48a,
            ]
        } else {
            return;
        };
        for (r, v) in self
            .mch
            .peg_credit0
            .iter()
            .chain(self.mch.peg_credit1.iter())
            .chain(core::iter::once(&self.mch.peg_credit2))
            .zip(table)
        {
            r.set(*v);
        }
    }
    pub(crate) fn validate_visible_ports(&self, policy: Policy) -> Result<(), Error> {
        if !self.port(0).is_present() {
            return Ok(());
        }
        let (roots, count) = self.roots()?;
        for root in &roots[..count] {
            let p = self.port(root.function);
            let speed = p.read32(0xac) & 0xf;
            if !(1..=3).contains(&speed)
                || policy.max_width[root.function as usize] > root.width
                || p.read32(0x18) & 0x00ffff00 != 0
            {
                return Err(Error::BadPolicy);
            }
        }
        Ok(())
    }
    /// Actual native PEG flow. All mandatory present-card failures are fatal,
    /// even where native merely logged or ignored a timed-out wait.
    #[allow(clippy::too_many_arguments)]
    pub fn init(
        &self,
        dmi: &sa_dmi::Dmi<'_>,
        cpu: Cpu,
        mode: MemoryBootMode,
        dmi_policy: sa_dmi::Policy,
        policy: Policy,
        msrs: &impl MsrAccess,
        delay: &mut impl DelayNs,
    ) -> Result<Outcome, Error> {
        policy.validate()?;
        self.validate(dmi, cpu, mode, dmi_policy)?;
        if cpu.package != Package::DesktopHalo || self.sa()?.device_id() & 6 == 4 {
            return Err(Error::BadPolicy);
        }
        self.validate_visible_ports(policy)?;
        let sa = self.sa()?;
        use super::sa_registers::GraphicsControl;
        use tock_registers::interfaces::ReadWriteable;
        let ggc = unsafe { sa.register::<u16, GraphicsControl::Register>(0x50) };
        ggc.modify(GraphicsControl::VGA_DISABLE::SET);
        let deven = unsafe { sa.register::<u32, ()>(0x54) };
        deven.set(deven.get() | policy.devices.deven());
        let mut outcome = Outcome {
            controller_present: self.port(0).is_present(),
            links: core::array::from_fn(|f| Link {
                function: f as u8,
                ..Link::default()
            }),
            disabled: 7,
        };
        if !outcome.controller_present {
            return Ok(outcome);
        }
        // DEVEN can reveal previously hidden controllers. Discover again only
        // after the actual source enable, never turn hidden into an absent gate.
        let (mut roots, count) = self.roots()?;
        self.validate_visible_ports(policy)?;
        let p0 = self.port(0);
        let r = unsafe { p0.register::<u32, ()>(0x490) };
        r.set(r.get() & !(1 << 28));
        for b in 0..8u16 {
            let r = unsafe { p0.register::<u32, ()>(0x918 + b * 0x20) };
            r.set(r.get() & !12);
        }
        for root in &roots[..count] {
            let d = self.port(root.function);
            let r = unsafe { d.register::<u32, ()>(0x224) };
            r.set(r.get() | 0x1f);
            for off in [0x308, 0x314, 0x32c, 0x330] {
                d.write32(off, d.read32(off));
            }
            let r = unsafe { d.register::<u32, ()>(0xcd4) };
            r.set((r.get() & !(0x7f << 24)) | (0x40 << 24));
            let r = unsafe { d.register::<u32, ()>(0x208) };
            r.set((r.get() & !(3 << 30)) | (1 << 30));
        }
        self.recipe(cpu, msrs);
        let gen3 = sa.read32(0xe8) & (1 << 20) == 0;
        for root in &roots[..count] {
            let d = self.port(root.function);
            if gen3 {
                for b in 0..u16::from(root.width / 2) {
                    let r = unsafe { d.register::<u32, ()>(0xda0 + b * 4) };
                    r.set((r.get() & 0x80f080f0) | 0x27072707);
                }
            }
            let speed = (d.read32(0xac) & 0xf).min(if gen3 { 3 } else { 2 }) as u8;
            // Preserve native's unusual clear-mask semantics exactly: low ten
            // LCAP bits are ORed, high bits cleared in the write-once write.
            let r = unsafe { d.register::<u32, ()>(0xac) };
            r.set((r.get() & 0x3ff) | (u32::from(root.width) << 4) | u32::from(speed));
            self.target(*root, speed);
        }
        let icomp = self.dev(0, 2, 0)?.device_id() == 0x191b;
        if icomp {
            for root in &roots[..count] {
                self.icomp(*root, true);
            }
        }
        for f in 0..3 {
            let d = self.port(f);
            if policy.devices.peg[f as usize] && d.is_present() {
                let r = unsafe { d.register::<u32, ()>(0xc24) };
                r.set(r.get() & !(1 << 16));
            }
        }
        self.wait_active(
            &roots[..count],
            policy.devices.peg,
            false,
            policy.poll_ms,
            delay,
        );
        if icomp {
            self.icomp_check(&roots[..count], policy, false, delay);
        }
        let mask = self.failing(&roots[..count], policy, false);
        if mask != 0 {
            for root in &roots[..count] {
                if mask & (1 << root.function) != 0 {
                    let d = self.port(root.function);
                    let r = unsafe { d.register::<u16, ()>(0xb0) };
                    r.set(r.get() | 0x10);
                    delay.delay_us(1);
                    let cfg = unsafe { d.register::<u32, ()>(0x25c) };
                    cfg.set(cfg.get() | (1 << 9));
                    r.set(r.get() & !0x10);
                    delay.delay_us(1);
                }
            }
            self.wait_active(
                &roots[..count],
                policy.devices.peg,
                true,
                policy.poll_ms,
                delay,
            );
            for root in &roots[..count] {
                if mask & (1 << root.function) != 0 {
                    self.retrain(*root);
                }
            }
            self.wait_active(
                &roots[..count],
                policy.devices.peg,
                true,
                policy.poll_ms,
                delay,
            );
        }
        for root in &roots[..count] {
            if policy.devices.peg[root.function as usize]
                && self.presence(root.function)
                && !self.active(*root)
            {
                return Err(Error::LinkTimeout(root.function));
            }
        }
        for root in &mut roots[..count] {
            self.endpoint(root, policy)?;
            outcome.links[root.function as usize].state =
                if !policy.devices.peg[root.function as usize] {
                    PortState::Disabled
                } else if root.present {
                    PortState::Active
                } else {
                    PortState::Empty
                };
        }
        if icomp {
            self.icomp_check(&roots[..count], policy, true, delay);
        }
        for root in &roots[..count] {
            let d = self.port(root.function);
            let r = unsafe { d.register::<u32, ()>(0xdd8) };
            r.set(r.get() & !(1 << 15));
        }
        for root in &roots[..count] {
            if root.present {
                self.target(*root, 1);
                self.retrain(*root);
            }
        }
        for root in &roots[..count] {
            if root.present {
                self.wait_l0(*root, policy.poll_ms, delay)?;
            }
        }
        for root in &roots[..count] {
            if root.present {
                let d = self.port(root.function);
                let r = unsafe { d.register::<u32, ()>(0xd98) };
                r.set(r.get() | 1);
                self.target(*root, 3);
                self.retrain(*root);
            }
        }
        for root in &roots[..count] {
            if root.present {
                self.wait_l0(*root, policy.poll_ms, delay)?;
            }
        }
        let mut disabled = 7;
        for root in &mut roots[..count] {
            let d = self.port(root.function);
            let requested = policy.max_width[root.function as usize];
            if root.present && requested != 0 && self.width(*root) != requested {
                let value = match requested {
                    1 => 1,
                    2 => 3,
                    4 => 7,
                    8 => 15,
                    _ => 31,
                };
                let r = unsafe { d.register::<u32, ()>(0x224) };
                r.set((r.get() & !0x1f) | value);
                self.bounce(*root, delay);
                self.wait_l0(*root, policy.poll_ms, delay)?;
            }
            if root.present {
                disabled &= !(1 << root.function);
                let r = unsafe { d.register::<u32, ()>(0x22c) };
                r.set((r.get() & !0xffff) | 0x2020);
                let r = unsafe { d.register::<u32, ()>(0x258) };
                r.set(r.get() | (1 << 29));
                let r = unsafe { d.register::<u32, ()>(0xd2c) };
                r.set(r.get() | (1 << 31));
                if self.active(*root) {
                    self.retrain(*root);
                    let mut observed = false;
                    for elapsed in 0..=policy.poll_ms {
                        if d.read16(0xb2) & (1 << 11) != 0 {
                            observed = true;
                            break;
                        }
                        if elapsed < policy.poll_ms {
                            delay.delay_ms(1);
                        }
                    }
                    if !observed {
                        return Err(Error::LinkTimeout(root.function));
                    }
                }
            }
            if root.present && root.max_width != 0 {
                let width = (root.max_width + 1) & !1;
                if width < root.width {
                    self.power_bundles(root.bundles(width, root.width));
                }
            }
        }
        for (f, enabled) in policy.devices.peg.iter().enumerate() {
            if !enabled {
                disabled |= 1 << f;
            }
        }
        self.disable(disabled, cpu, policy, delay)?;
        for root in &roots[..count] {
            if disabled & (1 << root.function) != 0 {
                continue;
            }
            self.wait_active(
                core::slice::from_ref(root),
                policy.devices.peg,
                true,
                policy.poll_ms,
                delay,
            );
            if !self.active(*root) || self.port(root.function).read16(0xb2) & (1 << 11) != 0 {
                return Err(Error::LinkTimeout(root.function));
            }
            let status = self.port(root.function).read16(0xb2);
            let speed = (status & 15) as u8;
            let width = ((status >> 4) & 63) as u8;
            let wanted = if policy.max_width[root.function as usize] == 0 {
                root.max_width
            } else {
                policy.max_width[root.function as usize].min(root.max_width)
            };
            if width != wanted
                || width > root.width
                || !(1..=3).contains(&speed)
                || speed
                    != root
                        .ep_speed
                        .min(root.max_speed)
                        .min(if gen3 { 3 } else { 2 })
            {
                return Err(Error::Degraded(root.function));
            }
            outcome.links[root.function as usize] = Link {
                function: root.function,
                state: PortState::Active,
                speed,
                width,
                endpoint_speed: root.ep_speed,
                endpoint_width: root.ep_width,
            };
        }
        self.credits(disabled);
        if cpu.package == Package::DesktopHalo {
            for f in 0..3 {
                let d = self.port(f);
                if d.is_present() {
                    let r = unsafe { d.register::<u32, ()>(0xd2c) };
                    r.set(r.get() & !(7 << 27));
                }
            }
            dmi.gate_peg_spine_clock();
        }
        outcome.disabled = disabled;
        Ok(outcome)
    }
}

/// PCI capability walk is bounded and validates every pointer before reading.
fn pcie_capability(ep: &EcamDevice) -> Result<Option<u16>, Error> {
    if ep.read32(0) == u32::MAX {
        return Ok(None);
    }
    if ep.read16(6) & 0x10 == 0 {
        return Ok(None);
    }
    let mut next = ep.read8(0x34);
    let mut seen = 0u64;
    for _ in 0..48 {
        if next == 0 {
            return Ok(None);
        }
        if next < 0x40 || next & 3 != 0 || next > 0xf0 {
            return Err(Error::CapabilityChain);
        }
        let bit = 1u64 << (next / 4 - 16);
        if seen & bit != 0 {
            return Err(Error::CapabilityChain);
        }
        seen |= bit;
        if ep.read8(u16::from(next)) == 0x10 {
            return Ok(Some(u16::from(next)));
        }
        next = ep.read8(u16::from(next) + 1);
    }
    Err(Error::CapabilityChain)
}
