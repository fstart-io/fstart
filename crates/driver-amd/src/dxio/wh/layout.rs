// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PCIe complex of a WH IOD: its wrappers (PCIe cores), the root bridge
//! each sits on, and how a board's socket lanes reach their MPIO (DXIO)
//! lanes; the PCI devfn of each root-port slot and its remap.
//!
//! Both donors rewrite every wrapper of the per-RB complex templates
//! (`WhGetComplexTemplate`, Dell `0xf37dc0`) at run time
//! (`PcieFixupPlatformConfig`, Dell `0xf36e60`, GIGABYTE `0x709a00a7`,
//! instruction-checked; MPIO.md §2.3): a wrapper's socket lanes start at
//! `T_user[core + 4 * die]` (Dell `0xfa2224`), its MPIO lanes at
//! `T_dxio[core]` (Dell `0xfa222c`), sixteen each and not reversed; the x4
//! bonus core 6 is hard-coded (Dell `0xf36fc2`): socket lanes 132..=135 on
//! die 0 and 128..=131 on die 1, MPIO lanes 64..=67, core reversed. A port
//! is translated within its wrapper (`MpioTranslateLanesCallback`, Dell
//! `0xf7a310`): `mpio = dxio_start + (lane - user_start)`, or `dxio_end -
//! (lane - user_start)` on a reversed core; the ASK is reversed when the
//! core is reversed XOR the board's lanes run downwards.
//!
//! The root bridge (IOS) a wrapper sits on is the template's. On SP7 B0 the
//! third root bridge's IOHC is numbered 3, not 2 (`NbioPcieFixupPlatformConfigWh`
//! Dell `0xf379f0` → `0xf37410` rewrites `RBIndex` 2 → 3 on every part
//! that is neither A0 nor SP8; decompiler-inferred): its devfn remap,
//! bridge control, ARI and the `0x2A` release message use IOHC 3
//! ([`Layout::iohc`]); the DF and the root bus know it as physical root
//! bridge 2.

use heapless::Vec;
use tock_registers::interfaces::{ReadWriteable, Readable};

use super::port::PortDesc;
use crate::dxio::rs::ask::{self, Ask, Lane, Status};
use crate::dxio::rs::session::MAX_PORTS;
use crate::nbio::iohc::NbProgDeviceRemap;
use crate::nbio::wh::pcie::PortId;
use crate::smn::{SmnAccess, SmnReg};

/// Number of entries in WH's per-RB logical-bridge / default-devfn table
/// (`SilVenicePei` `0xf61240`: `0x14 = 20`).
pub const PORT_SLOTS: usize = 20;

/// Most ASKs per MPIO instance (Dell `0xf7ac50`: assert
/// `AskInstanceCount <= 36`, `MAX_PORT_DEVICE_MAP_SIZE`).
pub const MAX_ASKS: usize = 36;

/// Default PCI `devfn` for each logical bridge `0..20` on WH SP7 A0
/// (`SilVenicePei` `0xfbe210`):
/// - `0..8`: `1.1..1.7, 2.1` (Core 0 on RB1)
/// - `8..16`: `3.1..3.7, 4.1` (Core 1 on RB0, Core 2 on RB1)
/// - `16..20`: `5.1..5.4` (Bonus Core 6 on RB0)
pub const DEFAULT_DEVFNS_A0: [u8; PORT_SLOTS] = [
    0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x11, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x21,
    0x29, 0x2a, 0x2b, 0x2c,
];

/// Default PCI `devfn` for each logical bridge `0..20` on WH SP7 B0 and SP8
/// (`SilVenicePei` `0xfbe224`):
/// - `0..8`: unused (`0xff`)
/// - `8..16`: `3.1..3.7, 4.1` (remap registers `0..8`)
/// - `16..20`: `5.1..5.4` (Bonus Core 6, remap registers `8..12`)
pub const DEFAULT_DEVFNS_B0: [u8; PORT_SLOTS] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x21,
    0x29, 0x2a, 0x2b, 0x2c,
];

/// One PCIe wrapper (core) of a WH IOD after `PcieFixupPlatformConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wrapper {
    /// Core ID (wrapper byte `+0x0A` low nibble): the `0x1A34/0x1A38`
    /// apertures and the strap core.
    pub core: u8,
    /// RC aperture (wrapper byte high nibble): `0x1C40/0x1C44/0x1C48`.
    pub rc: u8,
    /// Physical root bridge (IOS) of the IOD that carries it.
    pub rb: u8,
    /// First socket lane on die 0 and on die 1 (`T_user`).
    pub user_start: [u8; 2],
    /// First MPIO lane (`T_dxio`).
    pub dxio_start: u8,
    /// Lanes.
    pub lanes: u8,
    /// The core's lanes run downwards (bonus core 6).
    pub reversed: bool,
    /// `pcie_bridge_id` of its port 0 (index into the default devfn table).
    pub slot_base: u8,
}

impl Wrapper {
    /// Most ports: one per two lanes of an x16 core, four on the bonus core.
    pub const fn max_ports(&self) -> u8 {
        if self.lanes == 4 { 4 } else { 8 }
    }
}

const fn x16(core: u8, rc: u8, rb: u8, user: [u8; 2], dxio: u8, slot_base: u8) -> Wrapper {
    Wrapper {
        core,
        rc,
        rb,
        user_start: user,
        dxio_start: dxio,
        lanes: 16,
        reversed: false,
        slot_base,
    }
}

const fn bonus(rb: u8) -> Wrapper {
    Wrapper {
        core: 6,
        rc: 5,
        rb,
        user_start: [132, 128],
        dxio_start: 64,
        lanes: 4,
        reversed: true,
        slot_base: 16,
    }
}

const SP7_A0: [Wrapper; 4] = [
    x16(2, 3, 1, [0, 80], 32, 8),
    x16(1, 2, 0, [16, 64], 0, 8),
    x16(0, 1, 1, [32, 96], 16, 0),
    bonus(0),
];
const SP7_B0: [Wrapper; 4] = [
    x16(2, 3, 1, [0, 80], 32, 8),
    x16(1, 2, 0, [16, 64], 0, 8),
    x16(0, 1, 2, [32, 96], 16, 8),
    bonus(0),
];
const SP8: [Wrapper; 5] = [
    x16(2, 3, 1, [0, 80], 32, 8),
    x16(1, 2, 0, [16, 64], 0, 8),
    x16(0, 1, 3, [32, 96], 16, 8),
    x16(3, 0, 2, [48, 112], 48, 8),
    bonus(3),
];

/// WH IOD PCIe complex topology variant (`SilVenicePei` `0xf37dc0..0xf37f36`),
/// chosen by [`crate::cpu::wh::Part::layout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// WH SP7 A0 (`0x00B5_0F00`): 2 root bridges per IOD, 20-slot remap
    /// bank (`remap_reg = slot`).
    Sp7A0,
    /// WH SP7 B0 (`0x00B5_0F01+`, `0x00B6_0Fxx`, `0x00B7_0Fxx`): 3 root
    /// bridges per IOD, the third on IOHC 3; 12-slot remap bank
    /// (`remap_reg = slot - 8`).
    Sp7B0,
    /// WH SP8 (`0x00B9_0Fxx`, `0x00BA_0Fxx`): 4 root bridges per IOD,
    /// 12-slot remap bank (`remap_reg = slot - 8`).
    Sp8,
}

impl Layout {
    /// The wrappers of one IOD (the same on every IOD; die 1 differs in
    /// [`Wrapper::user_start`] only).
    pub const fn wrappers(self) -> &'static [Wrapper] {
        match self {
            Self::Sp7A0 => &SP7_A0,
            Self::Sp7B0 => &SP7_B0,
            Self::Sp8 => &SP8,
        }
    }

    /// The wrapper of core `core`.
    pub fn wrapper(self, core: u8) -> Option<&'static Wrapper> {
        self.wrappers().iter().find(|w| w.core == core)
    }

    /// The IOHC SMN index (`0x13Bx_xxxx + iohc << 20`, the `0x2A` root-bridge
    /// argument) of each physical root bridge of an IOD: on SP7 B0 the third
    /// root bridge is IOHC 3 (`OverrideRbIndex`, Dell `0xf37410`,
    /// decompiler-inferred; checked at boot against the bus numbers,
    /// [`crate::nbio::wh::RootBridge::numbering`]).
    pub const fn iohcs(self) -> &'static [u8] {
        match self {
            Self::Sp7A0 => &[0, 1],
            Self::Sp7B0 => &[0, 1, 3],
            Self::Sp8 => &[0, 1, 2, 3],
        }
    }

    /// The IOHC of physical root bridge `rb` ([`Self::iohcs`]; `rb` past
    /// the layout's root bridges is its own).
    pub const fn iohc(self, rb: u8) -> u8 {
        let iohcs = self.iohcs();
        if (rb as usize) < iohcs.len() {
            iohcs[rb as usize]
        } else {
            rb
        }
    }

    /// Whether IOHC `iohc` carries a PCIe wrapper.
    pub const fn has_pcie(self, iohc: u8) -> bool {
        let wrappers = self.wrappers();
        let mut i = 0;
        while i < wrappers.len() {
            if self.iohc(wrappers[i].rb) == iohc {
                return true;
            }
            i += 1;
        }
        false
    }

    /// The PCIe wrappers of IOHC `iohc`.
    pub fn wrappers_of(self, iohc: u8) -> impl Iterator<Item = &'static Wrapper> {
        self.wrappers()
            .iter()
            .filter(move |w| self.iohc(w.rb) == iohc)
    }

    /// The vendor's strap index table and stride for this layout
    /// (`WhStrapIndexBase`: A0 or not).
    pub const fn is_a0(self) -> bool {
        matches!(self, Self::Sp7A0)
    }

    /// Default PCI `devfn` table indexed by logical bridge ID (`0..20`).
    pub const fn default_devfns(self) -> &'static [u8; PORT_SLOTS] {
        match self {
            Self::Sp7A0 => &DEFAULT_DEVFNS_A0,
            Self::Sp7B0 | Self::Sp8 => &DEFAULT_DEVFNS_B0,
        }
    }

    /// First active logical-bridge slot index in [`Self::default_devfns`].
    pub const fn first_slot(self) -> u8 {
        match self {
            Self::Sp7A0 => 0,
            Self::Sp7B0 | Self::Sp8 => 8,
        }
    }

    /// Read a PCIe slot's current DevFn through the same layout-specific index used for writes.
    pub fn slot_devfn(self, nbio: &impl SmnAccess, slot: u8) -> Option<(u8, u8)> {
        let index = self.remap_reg_index(slot)?;
        let raw = nbio.reg(remap_register(index)).get();
        let devfn = NbProgDeviceRemap::DEVFN.read(raw) as u8;
        (devfn != 0 && devfn != 0xff).then_some((devfn >> 3, devfn & 7))
    }

    pub const fn remap_reg_index(self, slot: u8) -> Option<u8> {
        let first = self.first_slot();
        if slot < first || (slot as usize) >= PORT_SLOTS {
            return None;
        }
        Some(slot - first)
    }

    /// The `(rb_on_iod, core, first_slot, end_slot)` of the 4-lane bonus core.
    pub const fn bonus_core(self) -> (u8, u8, u8, u8) {
        match self {
            Self::Sp7A0 | Self::Sp7B0 => (0, 6, 16, 20),
            Self::Sp8 => (3, 6, 16, 20),
        }
    }
}

/// Silicon lane translation for one WH PCIe port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneMapping {
    /// IOD (die) of the socket owning these lanes (`0..=1`).
    pub iod: u8,
    /// Physical root bridge (IOS) within the IOD.
    pub rb_on_iod: u8,
    /// IOHC SMN index of that root bridge ([`Layout::iohc`]).
    pub iohc: u8,
    /// WH PCIe core ID (`0, 1, 2, 3`, or `6` for the 4-lane bonus core).
    pub core: u8,
    /// RC aperture of the core ([`Wrapper::rc`]).
    pub rc: u8,
    /// First `pcie_bridge_id` for `local_port = 0` on this core (`0`, `8`, or `16`).
    pub slot_base: u8,
    /// Maximum local ports on this core (`8` for x16 cores, `4` for bonus core).
    pub max_ports: u8,
    /// Lowest MPIO lane of the port (ASK dword 0 `[15:0]`).
    pub start: u8,
    /// Highest MPIO lane of the port.
    pub end: u8,
    /// ASK reversed: the core is reversed XOR the board's lanes run downwards.
    pub reversed: bool,
}

impl LaneMapping {
    pub const fn width(&self) -> u8 {
        self.end - self.start + 1
    }
}

/// Translate a socket-wide WH lane span `(start, end)` under `layout`
/// ([`translate_port`] without a forced core reversal).
pub fn translate_socket_lane(
    layout: Layout,
    start: u8,
    end: u8,
) -> Result<LaneMapping, ask::Error> {
    translate_port(layout, start, end, false)
}

/// Translate a port's socket lanes `(start, end)` (`start > end`: the board
/// reversed the link) to its wrapper and MPIO lanes. Both ends must lie in
/// one wrapper of one die; `force_core_reversed` takes the reversed-core
/// path on any wrapper (Dell port parameter `0x31`, descriptor `+0x3B` bit 7).
pub fn translate_port(
    layout: Layout,
    start: u8,
    end: u8,
    force_core_reversed: bool,
) -> Result<LaneMapping, ask::Error> {
    let (lo, hi) = (start.min(end), start.max(end));
    for die in 0..2u8 {
        for w in layout.wrappers() {
            let user = w.user_start[usize::from(die)];
            let last = user + (w.lanes - 1);
            if !(user..=last).contains(&lo) {
                continue;
            }
            if !(user..=last).contains(&hi) {
                return Err(ask::Error::CrossesWindow);
            }
            let core_reversed = w.reversed || force_core_reversed;
            let dxio_end = w.dxio_start + (w.lanes - 1);
            let mpio = |lane: u8| {
                if core_reversed {
                    dxio_end - (lane - user)
                } else {
                    w.dxio_start + (lane - user)
                }
            };
            let (a, b) = (mpio(lo), mpio(hi));
            return Ok(LaneMapping {
                iod: die,
                rb_on_iod: w.rb,
                iohc: layout.iohc(w.rb),
                core: w.core,
                rc: w.rc,
                slot_base: w.slot_base,
                max_ports: w.max_ports(),
                start: a.min(b),
                end: a.max(b),
                reversed: core_reversed ^ (start > end),
            });
        }
    }
    Err(ask::Error::NoLaneWindow)
}

/// Remap register SMN address relative to a WH IOHC
/// ([`crate::nbio::wh::RootBridge`] on [`LaneMapping::iohc`]).
pub const fn remap_register(remap_reg: u8) -> SmnReg<NbProgDeviceRemap::Register> {
    SmnReg::new(0x13b1_00b8 + (remap_reg as u32) * 4)
}

/// Errors while binding MPIO-returned ASKs to WH root-bridge slots and devfns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// MPIO reported a local port ID out of range for the core.
    Port { core: u8, local_port: u8 },
    /// Returned ASK did not uniquely match a sent board port, or a sent port was missing.
    Unmatched,
    /// Two ports mapped to the same board port or slot on an RB.
    Duplicate,
    /// Lane translation failed.
    Lanes(ask::Error),
}

/// Verified root-port identity for one WH MPIO-managed PCIe port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Index in the IOD's returned ASK array.
    pub ask: usize,
    /// Index in the board's port list of the socket.
    pub port: usize,
    /// IOD index (`0..=1`).
    pub iod: u8,
    /// Physical root bridge within `iod` (the DF's, its root bus).
    pub rb_on_iod: u8,
    /// IOHC SMN index of the root bridge ([`Layout::iohc`]).
    pub iohc: u8,
    /// Silicon PCIe core ID (`0, 1, 2, 3`, or `6` for the bonus core).
    pub core: u8,
    /// RC aperture of the core ([`Wrapper::rc`]).
    pub rc: u8,
    /// Core-local port ID reported by MPIO (`0..max_ports`).
    pub local_port: u8,
    /// IOHC PCIe bridge slot (`slot_base + local_port`, `0..20`).
    pub pcie_bridge_id: u8,
    /// `NB_PROG_DEVICE_REMAP` register index (`pcie_bridge_id` on A0, `- 8` on B0/SP8).
    pub remap_reg: u8,
    /// Final PCI `(device << 3) | function` on `rb_on_iod`.
    pub devfn: u8,
    /// Logical bridge ID (`0..20`), i.e. the index of `devfn` in `layout.default_devfns()`.
    pub logical_bridge: u8,
    /// The board asked for another devfn, which the vendor's rules refuse
    /// (not a root-port devfn of the layout, or taken): the default was kept
    /// (`PcieMapPortPciAddressSil`, Dell `0xf602b0`).
    pub refused: Option<u8>,
}

impl Binding {
    /// The port's core, RC aperture and port, for its registers
    /// ([`crate::nbio::wh::pcie`]).
    pub const fn port_id(&self) -> PortId {
        PortId {
            core: self.core,
            rc: self.rc,
            port: self.local_port,
        }
    }
}

/// Bind MPIO-returned `asks` for `iod` to the matching board `ports` (the
/// socket's, filtered to `iod`) by `(MPIO start lane, kind)`, never by
/// array index. A requested devfn that is not one of the layout's root-port
/// devfns, or that another port of the root bridge already has, falls back
/// to the slot's default as the vendor does ([`Binding::refused`]).
pub fn bind(
    layout: Layout,
    iod: u8,
    ports: &[PortDesc<'_>],
    asks: &[Ask],
) -> Result<Vec<Binding, MAX_PORTS>, BindError> {
    let mapped = |port: &PortDesc<'_>| -> Option<Result<LaneMapping, BindError>> {
        if !port.engine.has_ask() {
            return None;
        }
        Some(port.mapping(layout).map_err(BindError::Lanes))
    };
    let mut expected_count = 0usize;
    for port in ports {
        match mapped(port) {
            Some(Ok(map)) if map.iod == iod => expected_count += 1,
            Some(Err(e)) => return Err(e),
            _ => {}
        }
    }

    let defaults = layout.default_devfns();
    let mut bindings: Vec<Binding, MAX_PORTS> = Vec::new();
    let mut matched: Vec<usize, MAX_ASKS> = Vec::new();

    for (ask_idx, sent) in asks.iter().enumerate() {
        let start = sent.lane().read(Lane::START);
        let mut candidates = ports.iter().enumerate().filter_map(|(idx, port)| {
            let map = mapped(port)?.ok()?;
            (map.iod == iod
                && u32::from(port.engine.controller()) == sent.lane().read(Lane::CONTROLLER)
                && u32::from(map.start) == start)
                .then_some((idx, port, map))
        });
        let (port_idx, port, map) = candidates.next().ok_or(BindError::Unmatched)?;
        if candidates.next().is_some() || matched.contains(&port_idx) {
            return Err(BindError::Duplicate);
        }
        matched.push(port_idx).map_err(|_| BindError::Duplicate)?;

        let trained = sent.status().read(Status::STATE) != 0;
        if !port.engine.is_pcie() || !(trained || port.present) {
            continue;
        }

        let local_port = sent.status().read(Status::LOCAL_PORT_ID) as u8;
        let bad = BindError::Port {
            core: map.core,
            local_port,
        };
        if local_port >= map.max_ports {
            return Err(bad);
        }
        let pcie_bridge_id = map.slot_base + local_port;
        let remap_reg = layout.remap_reg_index(pcie_bridge_id).ok_or(bad)?;
        let default_devfn = defaults[usize::from(pcie_bridge_id)];
        if default_devfn == 0xff {
            return Err(bad);
        }
        let taken = |devfn: u8| {
            bindings
                .iter()
                .any(|b| b.rb_on_iod == map.rb_on_iod && b.devfn == devfn)
        };
        let (devfn, refused) = match port.devfn {
            None => (default_devfn, None),
            Some((dev, func)) => {
                let want = fstart_pci::devfn(dev, func);
                if defaults.contains(&want) && want != 0xff && !taken(want) {
                    (want, None)
                } else {
                    (default_devfn, Some(want))
                }
            }
        };
        let logical_bridge = defaults
            .iter()
            .position(|&d| d == devfn)
            .unwrap_or(usize::from(pcie_bridge_id)) as u8;

        let binding = Binding {
            ask: ask_idx,
            port: port_idx,
            iod,
            rb_on_iod: map.rb_on_iod,
            iohc: map.iohc,
            core: map.core,
            rc: map.rc,
            local_port,
            pcie_bridge_id,
            remap_reg,
            devfn,
            logical_bridge,
            refused,
        };
        if bindings
            .iter()
            .any(|b| b.rb_on_iod == binding.rb_on_iod && b.pcie_bridge_id == pcie_bridge_id)
            || taken(devfn)
        {
            return Err(BindError::Duplicate);
        }
        bindings.push(binding).map_err(|_| BindError::Duplicate)?;
    }

    if matched.len() != expected_count {
        return Err(BindError::Unmatched);
    }
    Ok(bindings)
}

/// Compute the full 20-slot devfn permutation for root bridge `rb_on_iod` on
/// `iod`: every bound port's devfn on its slot, swapped with the slot that
/// had it by default (`PcieSetPortPciAddressMapPrivate`, Dell `0xf61930`).
pub fn remap(layout: Layout, iod: u8, rb_on_iod: u8, bindings: &[Binding]) -> [u8; PORT_SLOTS] {
    let mut map = *layout.default_devfns();
    for b in bindings
        .iter()
        .filter(|b| b.iod == iod && b.rb_on_iod == rb_on_iod)
    {
        let slot = usize::from(b.pcie_bridge_id);
        if let Some(other) = map.iter().position(|&d| d == b.devfn) {
            map.swap(slot, other);
        }
    }
    map
}

/// Program `NB_PROG_DEVICE_REMAP` (`0x13B1_00B8 + remap_reg * 4`) on `nbio`
/// (the root bridge's IOHC, [`Binding::iohc`]) for every active slot of
/// `rb_on_iod` whose mapped devfn differs from default (preserving upper
/// bits), called after `MAP` and before `CONFIGURE`.
pub fn program_remaps(
    nbio: &impl SmnAccess,
    layout: Layout,
    iod: u8,
    rb_on_iod: u8,
    bindings: &[Binding],
) {
    let defaults = layout.default_devfns();
    let mapped = remap(layout, iod, rb_on_iod, bindings);
    for (slot, (&devfn, &default)) in mapped.iter().zip(defaults.iter()).enumerate() {
        if default == 0xff || devfn == default {
            continue;
        }
        if let Some(reg_idx) = layout.remap_reg_index(slot as u8) {
            nbio.reg(remap_register(reg_idx))
                .modify(NbProgDeviceRemap::DEVFN.val(u32::from(devfn)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;

    fn map(layout: Layout, start: u8, end: u8) -> (u8, u8, u8, u8, u8, u8, bool) {
        let m = translate_socket_lane(layout, start, end).unwrap();
        (
            m.iod,
            m.rb_on_iod,
            m.iohc,
            m.core,
            m.start,
            m.end,
            m.reversed,
        )
    }

    #[test]
    fn socket_lanes_reach_the_donors_cores_and_mpio_lanes() {
        let b0 = Layout::Sp7B0;
        // Die 0: 0-15 core 2 (MPIO 32-47), 16-31 core 1 (0-15), 32-47 core 0 (16-31).
        assert_eq!(map(b0, 0, 15), (0, 1, 1, 2, 32, 47, false));
        assert_eq!(map(b0, 16, 31), (0, 0, 0, 1, 0, 15, false));
        assert_eq!(map(b0, 32, 47), (0, 2, 3, 0, 16, 31, false));
        // Die 1: 64-79 core 1, 80-95 core 2, 96-111 core 0.
        assert_eq!(map(b0, 64, 79), (1, 0, 0, 1, 0, 15, false));
        assert_eq!(map(b0, 80, 95), (1, 1, 1, 2, 32, 47, false));
        assert_eq!(map(b0, 96, 111), (1, 2, 3, 0, 16, 31, false));
        // Partial and board-reversed spans keep their offset in the core.
        assert_eq!(map(b0, 4, 7), (0, 1, 1, 2, 36, 39, false));
        assert_eq!(map(b0, 7, 4), (0, 1, 1, 2, 36, 39, true));
        // A0 keeps core 0 on root bridge 1.
        assert_eq!(map(Layout::Sp7A0, 32, 47), (0, 1, 1, 0, 16, 31, false));
        // SP8 core 3 and core 0.
        assert_eq!(map(Layout::Sp8, 48, 63), (0, 2, 2, 3, 48, 63, false));
        assert_eq!(map(Layout::Sp8, 112, 127), (1, 2, 2, 3, 48, 63, false));
        assert_eq!(map(Layout::Sp8, 32, 47), (0, 3, 3, 0, 16, 31, false));
        // No core 3 on SP7.
        assert_eq!(
            translate_socket_lane(b0, 48, 63),
            Err(ask::Error::NoLaneWindow)
        );
    }

    #[test]
    fn bonus_core_is_reversed_and_die_1_has_lanes_128_to_131() {
        let b0 = Layout::Sp7B0;
        // Lane 129 x1: die 1 bonus core, MPIO 66, reversed.
        assert_eq!(map(b0, 129, 129), (1, 0, 0, 6, 66, 66, true));
        assert_eq!(map(b0, 130, 131), (1, 0, 0, 6, 64, 65, true));
        assert_eq!(map(b0, 132, 133), (0, 0, 0, 6, 66, 67, true));
        assert_eq!(map(b0, 134, 135), (0, 0, 0, 6, 64, 65, true));
        // A board-reversed span on the reversed core is not reversed.
        assert_eq!(map(b0, 135, 134), (0, 0, 0, 6, 64, 65, false));
        assert_eq!(map(Layout::Sp8, 135, 132).1, 3);
        // A forced core reversal mirrors an x16 core.
        let m = translate_port(b0, 0, 3, true).unwrap();
        assert_eq!((m.start, m.end, m.reversed), (44, 47, true));
        // Spans across wrappers or dies are refused.
        for (a, b) in [(60, 67), (131, 132), (12, 19), (127, 128)] {
            assert!(translate_socket_lane(b0, a, b).is_err(), "{a}-{b}");
        }
        assert_eq!(
            translate_socket_lane(b0, 136, 137),
            Err(ask::Error::NoLaneWindow)
        );
    }

    #[test]
    fn iohc_numbering_and_slot_devfns() {
        assert_eq!(Layout::Sp7B0.iohc(2), 3);
        assert_eq!(Layout::Sp7A0.iohc(1), 1);
        assert_eq!(Layout::Sp8.iohc(2), 2);
        let space = SmnSpace::default();
        space.set(remap_register(8).address(), 0x29);
        space.set(remap_register(16).address(), 0x19);
        assert_eq!(Layout::Sp7B0.slot_devfn(&space.smn(), 16), Some((5, 1)));
        assert_eq!(Layout::Sp7A0.slot_devfn(&space.smn(), 16), Some((3, 1)));
        assert_eq!(Layout::Sp7B0.slot_devfn(&space.smn(), 20), None);
    }

    #[test]
    fn remap_swaps_the_requested_devfn_with_its_default_slot() {
        let b = |slot: u8, devfn: u8| Binding {
            ask: 0,
            port: 0,
            iod: 0,
            rb_on_iod: 0,
            iohc: 0,
            core: 1,
            rc: 2,
            local_port: slot - 8,
            pcie_bridge_id: slot,
            remap_reg: slot - 8,
            devfn,
            logical_bridge: 0,
            refused: None,
        };
        let m = remap(Layout::Sp7B0, 0, 0, &[b(8, 0x1a)]);
        assert_eq!(&m[8..10], &[0x1a, 0x19]);
        assert_eq!(m[0], 0xff);
        let space = SmnSpace::default();
        program_remaps(&space.smn(), Layout::Sp7B0, 0, 0, &[b(8, 0x1a)]);
        assert_eq!(space.get(remap_register(0).address()), 0x1a);
        assert_eq!(space.get(remap_register(1).address()), 0x19);
    }
}
