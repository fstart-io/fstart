// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native USB controller/PHY sequencing.
//! SMU performs PHY programming; no FSP or opaque x86 initialization call.
use crate::ccx::picasso::{Identity, Silicon};
use crate::ccx::tsc::DelayNs;
use crate::smn::{Smn, apply, rmw, write};
use crate::smu::v10 as smu;
use heapless::Vec;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::ReadWrite,
};
const CONTROLLER0: u32 = 0x01000001;
const PHY: u32 = 0x01000002;
const CONTROLLER1: u32 = 0x01000003;
const TAIL: u32 = 0x8200000f;

#[derive(Debug, Clone, Copy)]
pub struct Phy2 {
    pub com_pds: u8,
    pub sq_rx: u8,
    pub tx_fsls: u8,
    pub pre_emphasis: u8,
    pub pre_pulse: bool,
    pub rise: u8,
    pub vref: u8,
    pub hsxv: u8,
    pub resistance: u8,
}
impl Phy2 {
    pub const DEFAULT: Self = Self {
        com_pds: 3,
        sq_rx: 3,
        tx_fsls: 3,
        pre_emphasis: 3,
        pre_pulse: false,
        rise: 1,
        vref: 6,
        hsxv: 3,
        resistance: 1,
    };
    pub const DEFAULT_LAST: Self = Self {
        pre_emphasis: 2,
        vref: 5,
        ..Self::DEFAULT
    };
    fn valid(self) -> bool {
        self.com_pds < 8
            && self.sq_rx < 8
            && self.tx_fsls < 16
            && self.pre_emphasis < 4
            && self.rise < 4
            && self.vref < 16
            && self.hsxv < 4
            && self.resistance < 4
    }
    fn bits(self) -> u32 {
        u32::from(self.com_pds)
            | u32::from(self.sq_rx) << 12
            | u32::from(self.tx_fsls) << 16
            | u32::from(self.pre_emphasis) << 20
            | u32::from(self.pre_pulse) << 23
            | u32::from(self.rise) << 24
            | u32::from(self.vref) << 28
    }
    fn controller1(self) -> u32 {
        u32::from(self.com_pds) << 23
            | u32::from(self.sq_rx) << 20
            | u32::from(self.tx_fsls) << 16
            | u32::from(self.pre_emphasis) << 10
            | u32::from(self.pre_pulse) << 12
            | u32::from(self.rise) << 6
            | u32::from(self.vref)
            | u32::from(self.hsxv) << 14
            | u32::from(self.resistance) << 8
    }
    fn crossing(self) -> u32 {
        u32::from(self.resistance) << 2 | u32::from(self.hsxv)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Phy3 {
    pub equalization: [u8; 4],
    pub equalization_enabled: [bool; 4],
    pub vref: [u8; 2],
    pub vref_enabled: [bool; 2],
    pub boost: [u8; 2],
    pub boost_enabled: [bool; 2],
}
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Overcurrent {
    Pin0,
    Pin1,
    Pin2,
    Pin3,
    Pin4,
    Pin5,
    None = 15,
}
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub phy2: Option<[Phy2; 6]>,
    pub phy3_override: Option<Phy3>,
    pub overcurrent: [Overcurrent; 6],
    pub force_gen1: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Configuration,
    Capacity,
    Smu(smu::Error),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Update {
    pub address: u32,
    pub mask: u32,
    pub data: u32,
    pub group: u32,
}
pub struct Plan {
    updates: Vec<Update, 64>,
}
impl Plan {
    fn add(&mut self, address: u32, mask: u32, data: u32, group: u32) -> Result<(), Error> {
        self.updates
            .push(Update {
                address,
                mask,
                data,
                group,
            })
            .map_err(|_| Error::Capacity)
    }
    pub fn updates(&self) -> &[Update] {
        &self.updates
    }
}
fn port_mask(package3: bool, mask: u8) -> u8 {
    let mask = mask & 15;
    if package3 {
        (mask & 1) | (mask << 2 & 8) | (mask >> 1 & 6)
    } else {
        mask
    }
}

/// Preflight board tuning before any hardware programming. The family owns
/// operation order and package-specific encoding; this is not a board recipe.
pub fn plan(config: &Config, identity: Identity, silicon_id: u32) -> Result<Plan, Error> {
    let single = identity.silicon == Silicon::Raven2;
    let package3 = silicon_id >> 16 & 15 == 3; // SMU package, NOT CPUID package.
    let tunes = config.phy2.unwrap_or([
        Phy2::DEFAULT,
        Phy2::DEFAULT,
        Phy2::DEFAULT,
        Phy2::DEFAULT,
        Phy2::DEFAULT_LAST,
        Phy2::DEFAULT_LAST,
    ]);
    if config.force_gen1 & !15 != 0 || tunes.iter().any(|t| !t.valid()) {
        return Err(Error::Configuration);
    }
    if let Some(tune) = config.phy3_override
        && (tune.equalization.iter().any(|&v| v >= 16)
            || tune.vref.iter().any(|&v| v >= 32)
            || tune.boost.iter().any(|&v| v >= 8))
    {
        return Err(Error::Configuration);
    }
    let oc = config
        .overcurrent
        .iter()
        .enumerate()
        .fold(0u32, |bits, (i, pin)| bits | (*pin as u32) << (i * 4));
    let mut p = Plan {
        updates: Vec::new(),
    };
    if identity.am4 && !single {
        p.add(0x16f80130, 15, 1, CONTROLLER0)?;
    }
    if identity.am4 {
        p.add(0x16d80130, 0xf0000, 0xf0000, CONTROLLER0)?;
        for port in 0..4 {
            p.add(0x16c0d024 + port * 0x80, 0x400, 0x400, CONTROLLER1)?;
        }
    }
    let usb2 = if identity.am4 { 0x21 } else { 0 };
    let usb3 = u8::from(identity.am4 && !single);
    if single {
        // C arithmetic is unsigned wrapping; multiplication deliberately
        // repeats the low-byte OC fields in the top byte.
        let map = (oc & 0xffffff).wrapping_mul(0x1000001);
        let map0 = if package3 {
            (map | oc << 20) & 0xf0000000
        } else {
            map
        };
        let map1 = oc >> 8 & 0xff;
        let map1 = if package3 {
            map1 | (oc >> 12 & 15) | (oc & 0xf0)
        } else {
            map1
        };
        p.add(
            0x16d80128,
            0xf003f,
            u32::from(port_mask(package3, usb3)) << 16 | usb2 & 0x3f,
            CONTROLLER0,
        )?;
        p.add(0x16d80120, u32::MAX, map0, CONTROLLER0)?;
        p.add(0x16d80124, 0xff, map1, CONTROLLER0)?;
        p.add(0x16c0c12c, 0x20000, 0, CONTROLLER1)?;
    } else {
        p.add(
            0x16d80128,
            0xf000f,
            u32::from(usb3 & 15) << 16 | usb2 & 15,
            CONTROLLER0,
        )?;
        p.add(
            0x16f80128,
            0xf000f,
            u32::from(usb3 & 0x10) << 12 | (usb2 >> 4 & 3),
            CONTROLLER0,
        )?;
        p.add(0x16d80120, u32::MAX, (oc & 0xffff) * 0x10001, CONTROLLER0)?;
        p.add(
            0x16f80120,
            0xfff,
            (oc >> 16 & 0xff) | (oc >> 8 & 0xf00),
            CONTROLLER0,
        )?;
        p.add(0x16c0c12c, 0x20000, 0, CONTROLLER1)?;
        p.add(0x16e0c12c, 0x20000, 0, CONTROLLER1)?;
    }
    for (i, tune) in tunes.iter().enumerate().take(if single { 6 } else { 4 }) {
        p.add(
            0x16d1c000 + i as u32 * 0x400,
            0xf3bf7007,
            tune.bits(),
            CONTROLLER0,
        )?;
        p.add(
            0x16d1c004 + i as u32 * 0x400,
            15,
            tune.crossing(),
            CONTROLLER0,
        )?;
    }
    if single && let Some(tune) = config.phy3_override {
        for i in 0..4 {
            let port = if package3 && i != 0 {
                if i == 3 { 1 } else { i + 1 }
            } else {
                i
            };
            p.add(
                0x16d2c064 + i as u32 * 0x400,
                31,
                u32::from(tune.equalization_enabled[port]) << 4
                    | u32::from(tune.equalization[port]),
                PHY,
            )?;
        }
        for i in 0..2 {
            p.add(
                if i == 0 { 0x16d2003c } else { 0x16d4003c },
                0x3ff,
                u32::from(tune.vref[i])
                    | u32::from(tune.vref_enabled[i]) << 5
                    | u32::from(tune.boost[i]) << 6
                    | u32::from(tune.boost_enabled[i]) << 9,
                PHY,
            )?;
        }
    }
    if !single {
        p.add(0x16f0c000, 0x03ffdfcf, tunes[4].controller1(), CONTROLLER0)?;
        p.add(0x16f1c000, 0xf3bf7007, tunes[5].bits(), CONTROLLER0)?;
        p.add(0x16f1c004, 15, tunes[5].crossing(), CONTROLLER0)?;
        p.add(0x16d80114, u32::MAX, 0, CONTROLLER0)?;
    }
    p.add(
        if single { 0x16d80114 } else { 0x16f80114 },
        u32::MAX,
        0,
        CONTROLLER0,
    )?;
    let gen1 = port_mask(package3, config.force_gen1);
    if gen1 != 0 {
        p.add(0x16d80130, 0xf0000, u32::from(gen1) << 16, CONTROLLER0)?;
        for i in 0..4 {
            if gen1 & (1 << i) != 0 {
                p.add(0x16c0d024 + i * 0x80, 0x400, 0x400, CONTROLLER1)?;
            }
        }
    }
    p.add(0, 0, 0, TAIL)?;
    for i in 0..4 {
        p.add(0x16d2c19c + i * 0x400, 0xffff, 0x1ff, PHY)?;
        p.add(0x16d2c1ac + i * 0x400, 0xffff, 0x80, PHY)?;
    }
    for i in 0..4 {
        p.add(0x16d2415c + i * 0x400, 0x7000, 0x3000, PHY)?;
    }
    Ok(p)
}

pub trait PhyRegisters {
    fn read(&mut self, index: usize) -> u32;
    fn write(&mut self, index: usize, value: u32);
}
register_structs! { PhyLayout { (0 => values: [ReadWrite<u32>; 8]), (0x20 => @END), } }
pub struct NativePhy {
    base: *const PhyLayout,
}
impl NativePhy {
    /// # Safety
    /// USB PHY registers decoded UC, exclusive BSP access.
    pub unsafe fn new() -> Self {
        Self {
            base: 0xfedc6700 as *const PhyLayout,
        }
    }
    fn layout(&self) -> &PhyLayout {
        unsafe { &*self.base }
    }
}
impl PhyRegisters for NativePhy {
    fn read(&mut self, index: usize) -> u32 {
        self.layout().values[index].get()
    }
    fn write(&mut self, index: usize, value: u32) {
        self.layout().values[index].set(value);
    }
}
#[allow(clippy::too_many_arguments)]
pub fn initialize(
    smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
    phy: &mut impl PhyRegisters,
    config: &Config,
    identity: Identity,
    silicon_id: u32,
    resume: bool,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let plan = plan(config, identity, silicon_id)?;
    // The SATA controller off (vendor values).
    apply(
        smn,
        [
            write(0x0310_0004, 0),
            rmw(0x0310_0000, 0, 1),
            rmw(0x0310_0000, 0, 1 << 11),
        ],
    );
    smu::request(smn, 0x44, [0; 6], delay).map_err(Error::Smu)?;
    for (i, value) in [0x10100, 0x9393, 0x303, 0, 0x10100, 0x9393, 0x303, 0]
        .into_iter()
        .enumerate()
    {
        let keep = if i == 0 || i == 4 { 0xff } else { 0 };
        let old = phy.read(i);
        phy.write(i, old & keep | value);
    }
    // Both xHCI controllers (vendor values).
    for controller in [0x16d8_0000, 0x16f8_0000] {
        let start = (!resume).then_some(write(controller + 0x104, 0x33));
        apply(smn, start.into_iter().chain([rmw(controller + 0x10c, 1 << 6, 0)]));
    }
    for update in plan.updates() {
        smu::request(
            smn,
            0x38,
            [update.address, update.mask, update.data, update.group, 0, 0],
            delay,
        )
        .map_err(Error::Smu)?;
    }
    smu::request(smn, if resume { 0x2d } else { 0x2e }, [0; 6], delay).map_err(Error::Smu)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            phy2: None,
            phy3_override: None,
            overcurrent: [Overcurrent::None; 6],
            force_gen1: 0,
        }
    }
    #[test]
    fn package_permutation_unsigned_oc_repeat_and_phy_encodings() {
        assert_eq!([1, 2, 4, 8].map(|mask| port_mask(true, mask)), [1, 8, 2, 4]);
        let rv2 = Identity::from_cpuid(0x820f00, 0, 1 << 30).unwrap();
        let p = plan(&config(), rv2, 1 << 30).unwrap();
        assert_eq!(p.updates()[1].data, 0xffffffff);
        assert_eq!(Phy2::DEFAULT.bits(), 0x61333003);
        assert_eq!(Phy2::DEFAULT_LAST.controller1(), 0x01b3c945);
        let mut invalid = config();
        invalid.force_gen1 = 0x10;
        assert!(matches!(plan(&invalid, rv2, 0), Err(Error::Configuration)));
        let pco = Identity::from_cpuid(0x810f80, 0, 1 << 31).unwrap();
        assert!(pco.reduced_io);
        assert_eq!(pco.silicon, Silicon::Picasso);
        // Fused-down PCO is NOT the single-controller RV2 branch.
        let p = plan(&config(), pco, 1 << 31).unwrap();
        assert!(p.updates().iter().any(|u| u.address == 0x16f80128));
    }
}
