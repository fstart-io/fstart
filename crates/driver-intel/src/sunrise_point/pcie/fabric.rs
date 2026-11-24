// SPDX-License-Identifier: MIT OR Apache-2.0
//! Physical RP SBI routing, topology, PSF grants/visibility and CLKREQ mapping.
//! From skylake/pch.c; never enumerate a disabled controller through ECAM.
use super::{registers::*, *};
use crate::p2sb::{Message, Opcode, Sbi};
use crate::sunrise_point::gpio::{Gpio, Group};
use tock_registers::{
    LocalRegisterCopy, RegisterLongName,
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable, Writeable},
};
const PIDS: [pcr::Port; 6] = [
    pcr::Port::Spa,
    pcr::Port::Spb,
    pcr::Port::Spc,
    pcr::Port::Spd,
    pcr::Port::Spe,
    pcr::Port::Spf,
];
pub(super) const fn slot(rp: u8) -> u8 {
    match rp / 8 {
        0 => 0x1c,
        1 => 0x1d,
        _ => 0x1b,
    }
}
pub(super) const fn controller_count(kbl: bool) -> u8 {
    if kbl { 6 } else { 5 }
}
pub(super) const fn fia_lane(index: u8) -> Option<u8> {
    match index {
        0..=7 => Some(index + 6),
        8..=23 => Some(index + 10),
        _ => None,
    }
}
pub(super) const fn standalone(rpc: u8, index: u8) -> bool {
    match rpc {
        0 => true,
        1 => index % 4 != 1,
        2 => index.is_multiple_of(2),
        3 => index.is_multiple_of(4),
        _ => false,
    }
}
pub(super) fn function_map(enabled: u32, count: u8) -> [u8; 24] {
    let mut result = core::array::from_fn(|index| index as u8 % 8);
    for group in (0..count).step_by(8) {
        if let Some(first) = (group..(group + 8).min(count)).find(|rp| enabled & (1 << rp) != 0) {
            result[usize::from(group)] = first % 8;
            result[usize::from(first)] = 0;
        }
    }
    result
}
pub(crate) struct Fabric<'a> {
    pub pcr: &'a Pcr,
    pub silicon: hsio::Silicon,
    pub sbi: Sbi<'a>,
    pub controllers: u8,
    pub rpc: [u8; 6],
    pub reverse: [bool; 6],
}
impl<'a> Fabric<'a> {
    pub fn new(
        pcr: &'a Pcr,
        p2sb: &'a EcamDevice,
        silicon: hsio::Silicon,
        delay: &mut impl DelayNs,
    ) -> Result<Self, Error> {
        let mut result = Self {
            pcr,
            silicon,
            sbi: unsafe { Sbi::new(p2sb) }?,
            controllers: controller_count(silicon.is_kbl_h()),
            rpc: [0; 6],
            reverse: [false; 6],
        };
        for controller in 0..result.controllers {
            result.rpc[usize::from(controller)] = result
                .read::<Strap::Register>(controller * 4, 0xfc, delay)?
                .read(Strap::RPC) as u8;
            result.reverse[usize::from(controller)] = result
                .read::<Debug::Register>(controller * 4, 0x324, delay)?
                .is_set(Debug::REVERSE);
        }
        Ok(result)
    }
    fn message(&self, rp: u8, offset: u16, opcode: Opcode) -> Result<Message, Error> {
        if rp >= self.controllers * 4 || offset & 3 != 0 || offset > 0xffc {
            return Err(Error::Topology);
        }
        Ok(Message {
            port: PIDS[usize::from(rp / 4)] as u8,
            offset,
            opcode,
            posted: false,
            byte_enable: if offset == 0x50 && opcode == Opcode::PciConfigWrite {
                3
            } else {
                15
            },
            bar: 0,
            function: (slot(rp) << 3) | (rp % 4),
        })
    }
    pub fn read<R: RegisterLongName>(
        &mut self,
        rp: u8,
        offset: u16,
        delay: &mut impl DelayNs,
    ) -> Result<LocalRegisterCopy<u32, R>, Error> {
        let msg = self.message(rp, offset, Opcode::PciConfigRead)?;
        let mut data = 0;
        self.sbi.execute(msg, &mut data, delay)?;
        Ok(LocalRegisterCopy::new(data))
    }
    pub fn modify<R: RegisterLongName>(
        &mut self,
        rp: u8,
        offset: u16,
        fields: FieldValue<u32, R>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut value = self.read::<R>(rp, offset, delay)?;
        value.modify(fields);
        let msg = self.message(rp, offset, Opcode::PciConfigWrite)?;
        let mut data = value.get();
        self.sbi.execute(msg, &mut data, delay)?;
        Ok(())
    }
    pub fn lane_index(&self, rp: u8, lane: u8) -> Result<u8, Error> {
        let offset = rp % 4 + lane;
        if rp >= self.controllers * 4 || offset >= 4 {
            return Err(Error::Topology);
        }
        Ok(rp / 4 * 4
            + if self.reverse[usize::from(rp / 4)] {
                3 - offset
            } else {
                offset
            })
    }
    pub fn lane(&self, rp: u8, lane: u8) -> Result<hsio::Lane, Error> {
        Ok(self
            .silicon
            .lane(fia_lane(self.lane_index(rp, lane)?).ok_or(Error::Topology)?)?)
    }
    pub fn pcie_owner(&self, rp: u8, lane: u8) -> Result<bool, Error> {
        Ok(self.silicon.lane_owner(
            self.pcr,
            fia_lane(self.lane_index(rp, lane)?).ok_or(Error::Topology)?,
        )? == 0)
    }
    pub fn standalone(&self, rp: u8) -> bool {
        standalone(self.rpc[usize::from(rp / 4)], rp)
    }
    pub fn grants(&self) -> Result<(), Error> {
        const DGCR: [[[u16; 2]; 4]; 6] = [
            [[11, 12], [14, 15], [17, 18], [20, 21]],
            [[24, 25], [27, 28], [30, 31], [33, 34]],
            [[37, 38], [40, 41], [43, 44], [46, 47]],
            [[50, 51], [53, 54], [56, 57], [59, 60]],
            [[63, 64], [66, 67], [69, 70], [72, 73]],
            [[182, 183], [185, 186], [188, 189], [191, 192]],
        ];
        const TARGET: [[u16; 4]; 6] = [
            [0, 1, 2, 3],
            [4, 5, 6, 7],
            [12, 13, 14, 15],
            [20, 21, 22, 23],
            [28, 29, 30, 31],
            [32, 33, 34, 35],
        ];
        const GRANTS: [[u32; 4]; 4] = [[1, 1, 1, 1], [2, 4, 1, 1], [2, 4, 2, 4], [4, 4, 4, 4]];
        let (dgcr, tgt) = if self.silicon.is_kbl_h() {
            (0x42d8, 0x4834)
        } else {
            (0x4214, 0x46dc)
        };
        for ctrl in 0..usize::from(self.controllers) {
            for channel in 0..4 {
                let grant = GRANTS[usize::from(self.rpc[ctrl])][channel];
                for offset in [
                    dgcr + 4 * DGCR[ctrl][channel][0],
                    dgcr + 4 * DGCR[ctrl][channel][1],
                    tgt + 4 * TARGET[ctrl][channel],
                ] {
                    self.pcr
                        .register::<Grants::Register>(pcr::Port::Psf1, offset)?
                        .modify(Grants::COUNT.val(grant));
                }
            }
        }
        self.pcr
            .register::<DmiFlow::Register>(pcr::Port::Dmi, 0x2314)?
            .modify(DmiFlow::BIT7::SET + DmiFlow::BIT31::SET);
        for port in [
            pcr::Port::Psf1,
            pcr::Port::Psf2,
            pcr::Port::Psf3,
            pcr::Port::Psf4,
        ] {
            self.pcr
                .register::<Flow::Register>(port, 0x4018)?
                .modify(Flow::INGRESS::SET);
        }
        for (port, offset) in [
            (pcr::Port::Psf1, 0x403c),
            (pcr::Port::Psf1, 0x4040),
            (pcr::Port::Psf2, 0x401c),
            (pcr::Port::Psf2, 0x4020),
            (pcr::Port::Psf2, 0x4024),
            (pcr::Port::Psf2, 0x402c),
        ] {
            self.pcr
                .register::<Flow::Register>(port, offset)?
                .modify(Flow::EGRESS::SET);
        }
        Ok(())
    }
    pub fn power_management(&mut self, mask: u32, delay: &mut impl DelayNs) -> Result<(), Error> {
        for rp in 0..self.controllers * 4 {
            if rp % 4 == 0 {
                let fields = E0::STATIC::SET
                    + E0::LCLKREQ::SET
                    + E0::BBCLKREQ::SET
                    + E0::SRDBC::SET
                    + E0::SEOSC::SET;
                self.modify(
                    rp,
                    0xe0,
                    fields + E0::TRUNK.val(u32::from(mask & (15 << rp) == 15 << rp)),
                    delay,
                )?;
                self.modify(rp, 0xe8, Pwr::WPD::SET + Pwr::DBUPI::SET, delay)?;
                self.modify(
                    rp,
                    0xf4,
                    F4::SIID.val(0) + F4::PLL2::SET + F4::PLL3::SET,
                    delay,
                )?;
                self.modify(rp, 0x424, Pme2::PHY::SET, delay)?;
                self.modify(rp, 0x428, Pce::PMC::CLEAR + Pce::HAE::SET, delay)?;
            }
            self.modify(rp, 0xe0, E0::DLC::SET + E0::DBC::SET, delay)?;
            self.modify(rp, 0xf4, F4::SCPT::SET, delay)?;
            let gate = mask & (1 << rp) != 0;
            self.modify(
                rp,
                0x420,
                Pme::L1LE::CLEAR
                    + Pme::FDPPG::SET
                    + Pme::DLSUPG.val(u32::from(gate))
                    + Pme::DLSULSD.val(u32::from(gate))
                    + Pme::L1FSOE.val(u32::from(gate)),
                delay,
            )?;
            self.modify(
                rp,
                0x424,
                Pme2::FDCP::SET
                    + Pme2::DETSC::SET
                    + Pme2::DISSC::SET
                    + Pme2::L23SC::SET
                    + Pme2::L1SC::SET,
                delay,
            )?;
        }
        for controller in 0..self.controllers {
            self.modify(
                controller * 4,
                0x338,
                Alc::ITLR::SET + Alc::ILLR::SET,
                delay,
            )?;
        }
        Ok(())
    }
    fn quiesce(&mut self, rp: u8, delay: &mut impl DelayNs) -> Result<(), Error> {
        if self
            .read::<LinkWord::Register>(rp, 0x50, delay)?
            .is_set(LinkWord::ACTIVE)
        {
            // Narrow SBI byte enable prevents echoing upper RW1C LNKSTA.
            self.modify(rp, 0x50, LinkWord::DISABLE::SET, delay)?;
            let mut done = false;
            for n in 0..=5000 {
                if !self
                    .read::<LinkWord::Register>(rp, 0x50, delay)?
                    .is_set(LinkWord::ACTIVE)
                {
                    done = true;
                    break;
                }
                if n != 5000 {
                    delay.delay_us(10);
                }
            }
            if !done {
                return Err(Error::DisableTimeout);
            }
        } else {
            let target = if self.standalone(rp) && self.pcie_owner(rp, 0)? {
                1
            } else {
                0x0e
            };
            self.modify(rp, 0x338, Alc::BLOCK::SET, delay)?;
            let mut done = false;
            for n in 0..=5000 {
                if self
                    .read::<LinkState::Register>(rp, 0x328, delay)?
                    .read(LinkState::LTSSM)
                    == target
                {
                    done = true;
                    break;
                }
                if n != 5000 {
                    delay.delay_us(10);
                }
            }
            if !done {
                return Err(Error::DisableTimeout);
            }
        }
        self.modify(rp, 0x408, Phy4::SQDIS::SET, delay)
    }
    pub fn mapping(
        &mut self,
        enabled: u32,
        pins: &[u8; 24],
        pmc: &PmcRegisters,
        delay: &mut impl DelayNs,
    ) -> Result<[u8; 24], Error> {
        let count = self.controllers * 4;
        let functions = function_map(enabled, count);
        let mut values = [0u32; 6];
        for ctrl in 0..self.controllers {
            values[usize::from(ctrl)] = self
                .pcr
                .register::<Pcd::Register>(PIDS[usize::from(ctrl)], 0)?
                .get();
        }
        let base = if self.silicon.is_kbl_h() {
            0x4274
        } else {
            0x41c0
        };
        for rp in 0..count {
            let function = functions[usize::from(rp)];
            let shift = 4 * (rp % 4);
            values[usize::from(rp / 4)] =
                (values[usize::from(rp / 4)] & !(7 << shift)) | (u32::from(function) << shift);
            self.pcr
                .register::<Function::Register>(pcr::Port::Psf1, base + 4 * u16::from(rp))?
                .modify(Function::FUNCTION.val(u32::from(function)));
        }
        for rp in 0..count {
            if enabled & (1 << rp) != 0 {
                continue;
            }
            self.quiesce(rp, delay)?;
            values[usize::from(rp / 4)] |= 1 << (16 + rp % 4);
            let shadow = if self.silicon.is_kbl_h() {
                0x3700
            } else {
                0x3300
            };
            let register = self.pcr.register::<Shadow::Register>(
                pcr::Port::Psf1,
                shadow - 0x100 * u16::from(rp) + 0x3c,
            )?;
            register.modify(Shadow::DISABLE::SET);
            if !register.is_set(Shadow::DISABLE) {
                return Err(Error::Readback);
            }
            // NST_PG_FDIS1[22:24] are SATA/ADSP/xDCI, NOT RP21..23.
            // KBL SPF ports are disabled through PCD/PSF and PHY PM above;
            // do not copy native's overlapping (2+rp) PMC write for RP21+.
            if rp < 20 {
                let disabled = pmc.non_static_disable.read(pmc::NonStaticDisable::PCIE) | (1 << rp);
                pmc.non_static_disable
                    .modify(pmc::NonStaticDisable::PCIE.val(disabled));
                if pmc.non_static_disable.read(pmc::NonStaticDisable::PCIE) & (1 << rp) == 0 {
                    return Err(Error::Readback);
                }
            }
        }
        for ctrl in 0..self.controllers {
            let r = self
                .pcr
                .register::<Pcd::Register>(PIDS[usize::from(ctrl)], 0)?;
            let value = values[usize::from(ctrl)] | Pcd::LOCK::SET.value;
            r.set(value);
            if r.get() != value {
                return Err(Error::Readback);
            }
        }
        for rp in 0..count {
            if enabled & (1 << rp) != 0 {
                let device = EcamDevice::new(0, slot(rp), functions[usize::from(rp)]);
                if device.vendor_id() != 0x8086
                    || device.read32(0x4c) >> 24 != u32::from(rp) + 1
                    || device.read8(fstart_pci::PCI_INTERRUPT_PIN) != pins[usize::from(rp)]
                {
                    return Err(Error::Topology);
                }
            }
        }
        Ok(functions)
    }
    pub fn fix_lanes(&self, pmc: &PmcRegisters, delay: &mut impl DelayNs) -> Result<(), Error> {
        let mut mask = 0u32;
        for ctrl in 0..self.controllers {
            let mut owned = [false; 4];
            for i in 0..4 {
                owned[usize::from(i)] = self.pcie_owner(ctrl * 4, i)?;
            }
            let gate = match self.rpc[usize::from(ctrl)] {
                1 => {
                    if !owned[0] && owned[1] {
                        2
                    } else {
                        0
                    }
                }
                2 => u8::from(!owned[0] && owned[1]) * 2 + u8::from(!owned[2] && owned[3]) * 8,
                3 if !owned[0] => 0b1110,
                _ => 0,
            };
            for i in 0..4 {
                if gate & (1 << i) != 0 && owned[usize::from(i)] {
                    let lane = fia_lane(self.lane_index(ctrl * 4, i)?).ok_or(Error::Topology)?;
                    if lane < 32 {
                        mask |= 1 << lane;
                    }
                }
            }
        }
        for (command, lanes) in [(0xe, mask & 0xffff), (0xf, mask >> 16)] {
            if lanes != 0 {
                // FULL proves FIFO availability, not analog power-gating
                // completion or a per-lane acknowledgement.
                wait_pmc(pmc, delay)?;
                pmc.pcie_message.write(
                    pmc::PcieMessage::LANES.val(lanes) + pmc::PcieMessage::OPCODE.val(command),
                );
                wait_pmc(pmc, delay)?;
            }
        }
        Ok(())
    }
}
fn wait_pmc(pmc: &PmcRegisters, delay: &mut impl DelayNs) -> Result<(), Error> {
    for sample in 0..=10000 {
        if !pmc.pcie_message_status.is_set(pmc::PcieMessageStatus::FULL) {
            return Ok(());
        }
        if sample != 10000 {
            delay.delay_us(1);
        }
    }
    Err(Error::PmcTimeout)
}
pub(super) fn restore_defaults(
    config: &Config,
    pcr: &Pcr,
    pmc: &PmcRegisters,
) -> Result<hsio::Silicon, Error> {
    config.validate()?;
    let silicon = hsio::Silicon::read(&EcamDevice::new(0, 0x1f, 0))?;
    if silicon.kind() != super::super::dmi::Kind::H {
        return Err(Error::Device);
    }
    let count = controller_count(silicon.is_kbl_h()) * 4;
    if config.ports[usize::from(count)..]
        .iter()
        .any(|p| p.enabled || p.clkreq.is_some())
    {
        return Err(Error::Policy);
    }
    if pcr
        .register::<Fia::Register>(pcr::Port::Fia, 0)?
        .is_set(Fia::LOCK)
        || pmc.function_disable.is_set(pmc::FunctionDisable::LOCK)
        || pmc.power_config3.is_set(pmc::PowerConfig3::LOCK)
    {
        return Err(Error::Locked);
    }
    for ctrl in 0..count / 4 {
        let reg = pcr.register::<Pcd::Register>(PIDS[usize::from(ctrl)], 0)?;
        if reg.is_set(Pcd::LOCK) {
            return Err(Error::Locked);
        }
        for i in 0..4u8 {
            let rp = ctrl * 4 + i;
            if (reg.get() >> (4 * i)) & 7 != u32::from(rp % 8) {
                return Err(Error::Topology);
            }
            let shadow_base = if silicon.is_kbl_h() { 0x3700 } else { 0x3300 };
            let shadow = pcr.register::<Shadow::Register>(
                pcr::Port::Psf1,
                shadow_base - 0x100 * u16::from(rp) + 0x3c,
            )?;
            if config.ports[usize::from(rp)].enabled
                && (reg.read(Pcd::DISABLE) & (1 << i) != 0
                    || shadow.is_set(Shadow::DISABLE)
                    || rp < 20
                        && pmc.non_static_disable.read(pmc::NonStaticDisable::PCIE) & (1 << rp)
                            != 0)
            {
                return Err(Error::Locked);
            }
        }
    }
    let base = if silicon.is_kbl_h() { 0x4274 } else { 0x41c0 };
    for rp in 0..count {
        let r = pcr.register::<Function::Register>(pcr::Port::Psf1, base + 4 * u16::from(rp))?;
        let value =
            Function::DEVICE.val(u32::from(slot(rp))) + Function::FUNCTION.val(u32::from(rp % 8));
        r.write(value);
        if !r.matches_all(value) {
            return Err(Error::Readback);
        }
    }
    Ok(silicon)
}
pub(super) fn clocks(config: &Config, pcr: &Pcr, kbl: bool) -> Result<(), Error> {
    let count = controller_count(kbl) * 4;
    let mut maps = [0u32; 4];
    let gpio = Gpio {
        pcr,
        program_itss_polarity: false,
    };
    for rp in 0..count {
        let field = if rp >= 23 { rp + 1 } else { rp };
        if let Some(clkreq) = config.ports[usize::from(rp)].clkreq {
            clock_pad(&gpio, clkreq)?;
            clock_delay(pcr, clkreq, 9)?;
            maps[usize::from(field / 8)] |= u32::from(clkreq) << (4 * (field % 8));
        }
    }
    if let Some(n) = config.lan_clkreq {
        clock_pad(&gpio, n)?;
        maps[2] |= u32::from(n) << 28;
    }
    for (i, value) in maps[..if kbl { 4 } else { 3 }].iter().enumerate() {
        let register = pcr.register::<()>(pcr::Port::Fia, 0x100 + 4 * i as u16)?;
        register.set(*value);
        let mask = if i < 2 || kbl && i == 2 {
            u32::MAX
        } else if i == 2 {
            0xf000ffff
        } else {
            15
        };
        if (register.get() ^ *value) & mask != 0 {
            return Err(Error::Readback);
        }
    }
    let control = pcr.register::<Fia::Register>(pcr::Port::Fia, 0)?;
    let fields = Fia::CLOCK.val(7) + Fia::LOCK::SET;
    control.modify(fields);
    if !control.matches_all(fields) {
        return Err(Error::Readback);
    }
    Ok(())
}
fn clock_pad(gpio: &Gpio<'_>, n: u8) -> Result<(), Error> {
    let pad = if n < 6 {
        Group::B.pad(5 + n)?
    } else {
        Group::H.pad(n - 6)?
    };
    gpio.native_function_deep(pad, 1)?;
    Ok(())
}
pub(super) fn clock_delay(pcr: &Pcr, n: u8, index: u32) -> Result<(), Error> {
    if n >= 16 || index >= 16 {
        return Err(Error::Policy);
    }
    pcr.register::<()>(pcr::Port::Icc, if n < 8 { 0x1000 } else { 0x1004 })?
        .modify(FieldValue::<u32, ()>::new(15, 4 * (n % 8) as usize, index));
    Ok(())
}
