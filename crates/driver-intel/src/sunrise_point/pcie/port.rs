// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native visible root-port programming and scoped endpoint bus ownership.
use super::{registers::*, *};
use tock_registers::{
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable, Writeable},
};
struct TemporaryBus {
    root: Option<EcamDevice>,
    saved: u32,
}
impl TemporaryBus {
    fn open(root: EcamDevice) -> Result<Self, Error> {
        let saved = root.read32(0x18);
        if saved & 0x00ffffff != 0 {
            return Err(Error::Owned);
        }
        let result = Self {
            root: Some(root),
            saved,
        };
        root.write32(0x18, saved | (2 << 8) | (2 << 16));
        if root.read32(0x18) & 0x00ffff00 != 0x00020200 {
            return Err(Error::Readback);
        }
        // Native endpoint bus-number capture, not an attempt to change VID.
        EcamDevice::new(2, 0, 0).write16(0, 0);
        Ok(result)
    }
    fn close(mut self) -> Result<(), Error> {
        let root = self.root.take().ok_or(Error::Owned)?;
        root.write32(0x18, self.saved);
        if root.read32(0x18) != self.saved {
            Err(Error::Readback)
        } else {
            Ok(())
        }
    }
}
impl Drop for TemporaryBus {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            root.write32(0x18, self.saved);
        }
    }
}
impl HardwarePort {
    pub(super) fn speed(&self) -> u8 {
        self.r.lsta.read(LinkSta::SPEED) as u8
    }
    pub(super) fn width(&self) -> u8 {
        self.r.lsta.read(LinkSta::WIDTH) as u8
    }
    pub(super) fn present(&self) -> bool {
        self.r.slotsta.is_set(SlotSta::PRESENT)
    }
    pub(super) fn train(
        &self,
        speed: u8,
        polls: usize,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.r.link2.modify(Link2::SPEED.val(u16::from(speed)));
        if !self.present() {
            return Err(Error::LinkTimeout);
        }
        for n in 0..=polls {
            if self.r.state.read(LinkState::LTSSM) == 0x33 {
                break;
            }
            if n == polls {
                return Err(Error::LinkTimeout);
            }
            delay.delay_us(10);
        }
        if speed > 2 {
            self.r.lctl3.set(self.r.lctl3.get() | 1);
        }
        self.r.lctl.modify(LinkCtl::RETRAIN::SET);
        self.r.clear_recovery.set(15 << 12);
        for n in 0..=polls {
            if !self.r.lctl.is_set(LinkCtl::RETRAIN)
                && !self.r.lsta.is_set(LinkSta::TRAINING)
                && self.r.lsta.is_set(LinkSta::ACTIVE)
                && self.r.state.read(LinkState::LTSSM) == 0x33
                && self.speed() == speed
                && self.width() > 0
                && self.width() <= self.max_width
                && (speed < 3 || self.r.linksta2.is_set(LinkSta2::EQ_COMPLETE))
            {
                return Ok(());
            }
            if n != polls {
                delay.delay_us(10);
            }
        }
        Err(Error::LinkTimeout)
    }
    fn force_gen1(&self, delay: &mut impl DelayNs) -> Result<(), Error> {
        match self.train(1, 5000, delay) {
            Ok(()) => return Ok(()),
            Err(Error::LinkTimeout) => {}
            Err(e) => return Err(e),
        }
        self.r.alc.modify(Alc::BLOCK::SET);
        self.r.lctl.modify(LinkCtl::DISABLE::SET);
        let result = (|| {
            let mut done = false;
            for n in 0..=25000 {
                if matches!(self.r.state.read(LinkState::LTSSM), 0x5e | 0x60) {
                    done = true;
                    break;
                }
                if n != 25000 {
                    delay.delay_us(2);
                }
            }
            if !done {
                return Err(Error::DisableTimeout);
            }
            self.r.link2.modify(Link2::SPEED.val(1));
            Ok(())
        })();
        self.r.lctl.modify(LinkCtl::DISABLE::CLEAR);
        self.r.alc.modify(Alc::BLOCK::CLEAR);
        result?;
        for n in 0..=5000 {
            if self.r.lsta.is_set(LinkSta::ACTIVE)
                && self.speed() == 1
                && self.width() > 0
                && self.width() <= self.max_width
                && self.r.state.read(LinkState::LTSSM) == 0x33
                && !self.r.lsta.is_set(LinkSta::TRAINING)
            {
                return Ok(());
            }
            if n != 5000 {
                delay.delay_us(10);
            }
        }
        Err(Error::LinkTimeout)
    }
    pub(super) fn common_clock(&self) -> Result<(), Error> {
        if !self.r.lsta.is_set(LinkSta::SLOT_CLOCK) {
            return Ok(());
        }
        let bus = TemporaryBus::open(self.device)?;
        let result = (|| {
            let endpoint = EcamDevice::new(2, 0, 0);
            let mut any = false;
            let count = if endpoint.read8(0xe) & 0x80 != 0 {
                8
            } else {
                1
            };
            for function in 0..count {
                let ep = EcamDevice::new(2, 0, function);
                if !ep.is_present() {
                    continue;
                }
                let Some(cap) = ep.find_capability(0x10) else {
                    return Err(Error::Capability);
                };
                if cap > 0xec {
                    return Err(Error::Capability);
                }
                let status = unsafe { ep.register::<u16, LinkSta::Register>(cap + 0x12) };
                if status.is_set(LinkSta::SLOT_CLOCK) {
                    self.r.lctl.modify(LinkCtl::COMMON::SET);
                    unsafe { ep.register::<u16, LinkCtl::Register>(cap + 0x10) }
                        .modify(LinkCtl::COMMON::SET);
                    any = true;
                }
            }
            if any {
                self.r.lctl.modify(LinkCtl::RETRAIN::SET);
            }
            Ok(())
        })();
        bus.close()?;
        result?;
        // Retrain completion is checked by the following EQ/final-speed train.
        Ok(())
    }
    fn endpoint_speed(&self) -> Result<Option<u8>, Error> {
        if !self.present() || !self.r.lsta.is_set(LinkSta::ACTIVE) {
            return Ok(None);
        }
        let bus = TemporaryBus::open(self.device)?;
        let result = (|| {
            let endpoint = EcamDevice::new(2, 0, 0);
            if !endpoint.is_present() {
                return Err(Error::Endpoint);
            }
            let cap = endpoint.find_capability(0x10).ok_or(Error::Capability)?;
            if cap > 0xec {
                return Err(Error::Capability);
            }
            let speed = unsafe { endpoint.register::<u32, Lcap::Register>(cap + 0xc) }
                .read(Lcap::SPEED) as u8;
            if speed == 0 {
                return Err(Error::Capability);
            }
            Ok(Some(speed))
        })();
        bus.close()?;
        result
    }
    pub(super) fn initialize(
        &self,
        config: &Config,
        fabric: &fabric::Fabric<'_>,
        delay: &mut impl DelayNs,
    ) -> Result<Link, Error> {
        let p = config.ports[usize::from(self.physical)];
        let previous = self.r.link2.read(Link2::SPEED);
        self.r.link2.modify(Link2::SPEED.val(1));
        if previous > 2 && self.present() && self.r.lsta.is_set(LinkSta::ACTIVE) {
            self.force_gen1(delay)?;
        }
        let endpoint = self.endpoint_speed()?;
        let gen3 = endpoint.is_some_and(|speed| speed >= 3);
        if !p.payload256 {
            self.r.dcap.modify(Dcap::PAYLOAD.val(0));
            self.r
                .ccfg
                .modify(Ccfg::UNSD::CLEAR + Ccfg::UNRS.val(0) + Ccfg::UNRD.val(1));
        } else {
            self.r.ccfg.modify(Ccfg::UNRD.val(1));
            self.r.dctl.modify(Dctl::PAYLOAD.val(1));
        }
        self.r
            .ltrcap
            .modify(LtrCap::OBFF.val(0) + LtrCap::ENABLE.val(u32::from(p.ltr)));
        self.r
            .dev2
            .modify(Dev2::OBFF.val(0) + Dev2::LTR.val(u16::from(p.ltr)));
        for (r, clear, set) in [
            (&self.r.rtp1, 0xffffff, 0xb75fa7),
            (&self.r.rtp2, 0xfff, 0xc97),
            (&self.r.l0sc, 0xffff0000, 0x14140000),
        ] {
            r.set((r.get() & !clear) | set);
        }
        self.r.pwr.modify(Pwr::RETRAIN::SET + Pwr::L1SQ::SET);
        let sa = EcamDevice::new(0, 0, 0);
        if sa.vendor_id() != 0x8086 || sa.read32(0xe4) == u32::MAX {
            return Err(Error::Device);
        }
        let vtd = config.vtd_policy
            && !unsafe { sa.register::<u32, crate::skylake::capabilities::CapidA::Register>(0xe4) }
                .is_set(crate::skylake::capabilities::CapidA::VTD_DISABLED);
        self.r
            .mpc_high
            .modify(MpcHigh::IRRCE.val(u8::from(!vtd)) + MpcHigh::MMBNCE::SET);
        self.r.mpc_low.modify(MpcLow::MCTP::SET);
        self.r.cfg2.set(
            (self.r.cfg2.get() & !(3 << 20))
                | (1 << 30)
                | (1 << 27)
                | (1 << 24)
                | (1 << 23)
                | (1 << 20),
        );
        self.r.debug.modify(
            Debug::EXIT.val(0)
                + Debug::SQOL0.val(u32::from(gen3))
                + Debug::CTONFAE::SET
                + Debug::SPCE::SET,
        );
        self.r.pme2.modify(Pme2::L23SC::SET + Pme2::L1SC::SET);
        self.r.nfts.set(0x7e703f38);
        self.r
            .g3l0
            .set((self.r.g3l0.get() & !0xff00ffff) | 0x28003d2c);
        self.r.mpc_mid.modify(
            MpcMid::SPEED.val(if matches!(p.speed, Speed::Gen1 | Speed::Gen2) {
                p.speed as u8
            } else {
                0
            }),
        );
        self.r.phy2.modify(Phy2::WAIT.val(0));
        self.r.rppgen.modify(Rppgen::PTOTOP::SET);
        for r in &self.r.presets {
            r.modify(
                FieldValue::<u16, ()>::new(15, 0, u16::from(p.downstream_preset))
                    + FieldValue::<u16, ()>::new(15, 8, u16::from(p.upstream_preset)),
            );
        }
        self.r.eq1.modify(Eq1::TS2::SET);
        let mut lcap = self.r.lcap.extract();
        lcap.modify(Lcap::EL1.val(4));
        match p.aspm {
            Aspm::Disabled => lcap.modify(Lcap::ASPM.val(0)),
            Aspm::L0s => lcap.modify(Lcap::ASPM.val(1)),
            Aspm::L1 => lcap.modify(Lcap::ASPM.val(2)),
            Aspm::L0sL1 => lcap.modify(Lcap::ASPM.val(3)),
            _ => {}
        }
        if gen3 {
            lcap.modify(Lcap::L0S::CLEAR);
        }
        self.r.lcap.set(lcap.get());
        let power = if self.max_width == 1 { 100 } else { 250 };
        self.r.slotcap.modify(
            SlotCap::HPS.val(u32::from(p.hotplug))
                + SlotCap::HPC.val(u32::from(p.hotplug))
                + SlotCap::POWER.val(power)
                + SlotCap::SCALE.val(1)
                + SlotCap::SLOT.val(u32::from(self.physical)),
        );
        let maximum = self.max_speed(p.speed);
        let sec = maximum >= 3;
        let l1 = p.clkreq.is_some() && p.l1ss != L1Substates::Disabled;
        self.r.secondary.set(if sec { header(0x19, 0) } else { 0 });
        self.r.l1.set(if l1 {
            header(0x1e, if sec { 0x220 } else { 0 })
        } else {
            0
        });
        let mut l1cap = self.r.l1cap.extract();
        l1cap.modify(L1Cap::SCALE.val(0) + L1Cap::VALUE.val(22));
        if !l1 {
            l1cap.modify(
                L1Cap::SUPPORTED::CLEAR
                    + L1Cap::ASPM11::CLEAR
                    + L1Cap::ASPM12::CLEAR
                    + L1Cap::PCIPM11::CLEAR
                    + L1Cap::PCIPM12::CLEAR,
            );
        } else if p.l1ss == L1Substates::L11 {
            l1cap.modify(L1Cap::ASPM12::CLEAR + L1Cap::PCIPM12::CLEAR);
        } else if p.l1ss == L1Substates::L12 {
            l1cap.modify(L1Cap::ASPM11::CLEAR + L1Cap::PCIPM11::CLEAR);
        }
        self.r.l1cap.set(l1cap.get());
        self.r.acs.set(header(
            0xd,
            if l1 {
                0x200
            } else if sec {
                0x220
            } else {
                0
            },
        ));
        self.r.acscap.set(self.r.acscap.get());
        self.r
            .aer
            .set(if p.aer { header(1, 0x140) } else { 0x140 << 20 });
        self.r.uem.set(self.r.uem.get() | (1 << 16));
        let mut fallback = false;
        if maximum >= 3 {
            match self.equalize(fabric, config, gen3, delay) {
                Ok(()) => {}
                Err(Error::NoMargin | Error::MarginLinkChanged | Error::LinkTimeout) => {
                    self.force_gen1(delay)?;
                    fallback = true;
                }
                Err(e) => return Err(e),
            }
        }
        self.r.mpc2.modify(
            Mpc2::PTNFAE::SET
                + Mpc2::EOI_DISABLE.val(u32::from(endpoint.is_none()))
                + Mpc2::LSTP::SET,
        );
        if p.hotplug {
            self.r.slotsta.write(SlotSta::CHANGE::SET);
            self.r
                .slotctl
                .modify(SlotCtl::HPE::CLEAR + SlotCtl::PDE::SET);
            self.r.mpc_low.modify(MpcLow::HPME::CLEAR);
            self.r.mpc_high.modify(MpcHigh::HOTPLUG::SET);
            self.r.uem.set(self.r.uem.get() | (1 << 14));
        }
        if let Some(endpoint_speed) = endpoint {
            let target = maximum.min(endpoint_speed);
            if !fallback {
                match self.train(target, 1500, delay) {
                    Ok(()) => {}
                    Err(Error::LinkTimeout) if target > 1 => {
                        self.force_gen1(delay)?;
                        fallback = true;
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(Link::Active {
                speed: self.speed(),
                width: self.width(),
                gen1_fallback: fallback,
            })
        } else {
            self.r.link2.modify(Link2::SPEED.val(u16::from(maximum)));
            Ok(Link::Empty)
        }
    }
    fn max_speed(&self, speed: Speed) -> u8 {
        let max = self.r.lcap.read(Lcap::SPEED) as u8;
        if speed == Speed::Auto {
            max
        } else {
            max.min(speed as u8)
        }
    }
    pub(super) fn lock(&self) -> Result<(), Error> {
        let dev = self.device;
        dev.write8(0x34, dev.read8(0x34));
        for cap in [
            Some(0x40),
            dev.find_capability(5),
            dev.find_capability(0xd),
            dev.find_capability(1),
        ]
        .into_iter()
        .flatten()
        {
            dev.write16(cap, dev.read16(cap));
        }
        if let Some(pm) = dev.find_capability(1) {
            if pm > 0xf8 {
                return Err(Error::Capability);
            }
            let value = dev.read16(pm + 4);
            dev.write16(pm + 4, value & !((1 << 3) | (1 << 15)));
        }
        self.r.dcap.set(self.r.dcap.get());
        self.r.lcap.set(self.r.lcap.get());
        self.r.slotcap.set(self.r.slotcap.get());
        let mpc = unsafe { dev.register::<u32, Mpc::Register>(0xd8) };
        mpc.modify(Mpc::UCEL.val(7) + Mpc::CCEL.val(4) + Mpc::LOCK::SET);
        if !mpc.is_set(Mpc::LOCK) {
            return Err(Error::Readback);
        }
        Ok(())
    }
}
const fn header(id: u16, next: u16) -> u32 {
    id as u32 | (1 << 16) | ((next as u32) << 20)
}
pub(super) fn tpower(value: u8, scale: u8) -> Result<u32, Error> {
    match scale {
        0 => Ok(u32::from(value) * 2),
        1 => Ok(u32::from(value) * 10),
        2 => Ok(u32::from(value) * 100),
        _ => Err(Error::Capability),
    }
}
pub(super) fn delay_index(micros: u32) -> Result<u32, Error> {
    const DELAYS: [u32; 16] = [
        0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 60, 70, 80, 90, 100,
    ];
    (9..16)
        .find(|i| DELAYS[*i] >= micros)
        .map(|i| i as u32)
        .ok_or(Error::ClockDelay)
}
pub(super) fn post_enumeration(
    config: &Config,
    ports: &Ports,
    pcr: &Pcr,
    pci: &fstart_pci::PciEcam,
) -> Result<(), Error> {
    for rp in 0..ports.count {
        let Some(function) = ports.routes[usize::from(rp)] else {
            continue;
        };
        let address = fstart_pci::PciAddress::new(0, 0, fabric::slot(rp), function);
        if !pci.devices().any(|a| a == address) {
            return Err(Error::Endpoint);
        }
        let dev = pci.device(address).ok_or(Error::Endpoint)?;
        if dev.read32(0x4c) >> 24 != u32::from(rp) + 1 {
            return Err(Error::Topology);
        }
        let p = config.ports[usize::from(rp)];
        if p.clkreq.is_none() || p.l1ss == L1Substates::Disabled {
            continue;
        }
        let rootcap = unsafe { dev.register::<u32, L1Cap::Register>(0x204) }.extract();
        let mut best = (
            rootcap.read(L1Cap::VALUE) as u8,
            rootcap.read(L1Cap::SCALE) as u8,
        );
        let mut micros = tpower(best.0, best.1)?;
        let secondary = dev.read8(0x19);
        let mut endpoint = None;
        if secondary != 0
            && let Some(address) = pci
                .devices()
                .find(|a| a.bus() == secondary && a.segment() == 0)
        {
            let ep = pci.device(address).ok_or(Error::Endpoint)?;
            if let Some(cap) = ep.find_extended_capability(0x1e) {
                if cap > 0xff0 || (ep.read32(cap) >> 16) & 15 != 1 {
                    return Err(Error::Capability);
                }
                let current = unsafe { ep.register::<u32, L1Cap::Register>(cap + 4) }.extract();
                let value = (
                    current.read(L1Cap::VALUE) as u8,
                    current.read(L1Cap::SCALE) as u8,
                );
                let required = tpower(value.0, value.1)?;
                if required > micros {
                    best = value;
                    micros = required;
                }
                endpoint = Some((ep, cap));
            }
        }
        let index = delay_index(micros)?; // reject unrepresentable delay BEFORE writes
        if let Some((ep, cap)) = endpoint {
            write_tpower(ep, cap, best);
        }
        write_tpower(dev, 0x200, best);
        fabric::clock_delay(pcr, p.clkreq.ok_or(Error::Policy)?, index)?;
    }
    Ok(())
}
fn write_tpower(dev: EcamDevice, cap: u16, value: (u8, u8)) {
    let control = unsafe { dev.register::<u32, L1Ctl::Register>(cap + 8) };
    let enabled = control.get() & 5;
    control.modify(L1Ctl::PCI12::CLEAR + L1Ctl::ASPM12::CLEAR);
    unsafe { dev.register::<u32, L1Ctl2::Register>(cap + 12) }
        .modify(L1Ctl2::VALUE.val(u32::from(value.0)) + L1Ctl2::SCALE.val(u32::from(value.1)));
    control.modify(L1Ctl::PCI12.val(enabled & 1) + L1Ctl::ASPM12.val((enabled >> 2) & 1));
}
