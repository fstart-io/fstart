// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH dual-IOD MPIO v2 (`MpioInitFlowWh`, `MpioPcieWh`, `SilVenicePei.pe`):
//!
//! - Uses `Encoding::Retained` (`(cmd & 0xfff) << 8`) with `DOORBELL_V2`
//!   (`SMN 0x0C91_0954`), preserving 12-bit opcodes `0x304` (`SETUP_LINK`),
//!   `0x3a` (`WAIT_COMPLETE`), and `0x322` (`UPDATE_LINK`).
//! - Implements the WH MPIO v2 session sequence (`0x02` GlobalConfig, optional
//!   `0x08` ExtAttributes, `0x07` SendAsk, `0x304` MAP + `0x03` GetAsk with
//!   `args[4] = ask_count`, host remap programming, `0x304` CONFIGURE + `0x03`,
//!   `0x304` PERST + `0x03`, board PERST deassert, `0x304` TRAIN + **`0x3a`
//!   WAIT_COMPLETE** + `0x03`, logical-bridge update via `0x07` + `0x322`).
//! - Keeps WH silicon `core`, `rb_on_iod`, `local_port`, `pcie_bridge_id`,
//!   `remap_reg`, `logical_bridge`, and `devfn` strictly distinct across WH
//!   SP7 A0 (`2` RBs/IOD), WH SP7 B0 (`3` RBs/IOD), and WH SP8 (`4` RBs/IOD).
//! - Matches returned ASKs to board ports by `(local_start_lane, kind)` rather
//!   than array position, and programs `NB_PROG_DEVICE_REMAP` (`0x13B1_00B8 +
//!   remap_reg * 4`) at the Map → Configure boundary.

use core::num::NonZeroU32;
use fstart_core::services::Timer;
use heapless::Vec;
use tock_registers::interfaces::ReadWriteable;
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::FromBytes;

use crate::dxio::rs::ancillary::Ancillary;
use crate::dxio::rs::ask::{self, Ask, Kind, Lane, Mapping, Port, Status};
use crate::dxio::rs::mailbox::{self, DOORBELL_V2, Encoding, Mailbox, Poll};
pub use crate::dxio::rs::session::{CacheSync, Config, DMA_SIZE, Dma, Error, MAX_PORTS};

/// WH has separate all-instance TRAIN and WAIT barriers.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum State {
    Fresh,
    Submitted,
    Mapped,
    Configured,
    Perst,
    Training,
    Complete,
    Trained,
    Updated,
    Poisoned,
}

const ANCILLARY_OFFSET: usize = 0x8000;
const ASK_BYTES: usize = core::mem::size_of::<Ask>();
use crate::nbio::iohc::NbProgDeviceRemap;
use crate::smn::{SmnAccess, SmnReg};

/// WH MPIO v2 `BIOS_MPIO_MSG_WAIT_COMPLETE` (`0x3a`, `SilVenicePei` `0xf5fa2d`):
/// issued after `SETUP_LINK(TRAIN)` and before the post-train `GET_ASK`.
pub const MSG_WAIT_COMPLETE: u16 = 0x3a;

/// Number of entries in WH's per-RB logical-bridge / default-devfn table
/// (`SilVenicePei` `0xf61240`: `0x14 = 20`).
pub const PORT_SLOTS: usize = 20;

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

register_bitfields![u32,
    Global [
        USE_PHY_SRAM OFFSET(6) NUMBITS(1) [],
        VALID_PHY_FW OFFSET(7) NUMBITS(1) [],
        SKIP_VETTING OFFSET(8) NUMBITS(1) []
    ],
    Transfer [
        SELECTED OFFSET(0) NUMBITS(1) [],
        TO_MPIO OFFSET(1) NUMBITS(1) []
    ]
];

/// WH IOD PCIe complex topology variant (`SilVenicePei` `0xf37dc0..0xf37f36`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// WH SP7 A0 (`0x00B5_0F00`): 2 root bridges per IOD (`rb 0..=1`), 20-slot
    /// remap bank (`remap_reg = slot`).
    Sp7A0,
    /// WH SP7 B0 (`0x00B5_0F01+`, `0x00B6_0Fxx`, `0x00B7_0Fxx`): 3 root bridges
    /// per IOD (`rb 0..=2`), 12-slot remap bank (`remap_reg = slot - 8`).
    Sp7B0,
    /// WH SP8 (`0x00B9_0Fxx`, `0x00BA_0Fxx`): 4 root bridges per IOD
    /// (`rb 0..=3`), 12-slot remap bank (`remap_reg = slot - 8`).
    Sp8,
}

impl Layout {
    /// Select the complex table for an admitted WH processor from CPUID
    /// 80000001h EAX and EBX. `0xf37dc0` uses the package nibble and tests
    /// stepping zero for each special case; remaining WH parts use table B.
    pub const fn from_cpuid(eax: u32, ebx: u32) -> Self {
        match (eax & 0x0fff_0fff, ebx >> 28) {
            (0x00b5_0f00, 2) => Self::Sp7A0,
            (0x00b9_0f00 | 0x00ba_0f00, 4) | (0x00ba_0fc0, 8) | (0x00b9_0fc0, 9) => Self::Sp8,
            _ => Self::Sp7B0,
        }
    }

    pub const fn rbs_per_iod(self) -> u8 {
        match self {
            Self::Sp7A0 => 2,
            Self::Sp7B0 => 3,
            Self::Sp8 => 4,
        }
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
        use tock_registers::interfaces::Readable;
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

    /// The `(rb_on_iod, core, first_slot..end_slot)` of the 4-lane bonus core
    /// (`lanes 128..=131` local) on one IOD.
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
    /// IOD index (`0..=1`) owning these lanes.
    pub iod: u8,
    /// Root bridge index within the IOD (`0..layout.rbs_per_iod()`).
    pub rb_on_iod: u8,
    /// WH PCIe core ID (`0, 1, 2, 3`, or `6` for the 4-lane bonus core).
    pub core: u8,
    /// First `pcie_bridge_id` for `local_port = 0` on this core (`0`, `8`, or `16`).
    pub slot_base: u8,
    /// Maximum local ports on this core (`8` for x16 cores, `4` for bonus core).
    pub max_ports: u8,
    /// Die-local start lane (`0..=63` or `128..=131`), always `<= local_end`.
    pub local_start: u8,
    /// Die-local end lane (`0..=63` or `128..=131`), always `>= local_start`.
    pub local_end: u8,
    /// True if board lanes were reversed (`start > end`).
    pub reversed: bool,
}

impl LaneMapping {
    fn as_ask_mapping(self) -> Mapping {
        Mapping {
            root_index: self.rb_on_iod,
            wrapper: self.core,
            start: self.local_start,
            end: self.local_end,
            reversed: self.reversed,
        }
    }
}

/// Translate a socket-wide WH lane span `(start, end)` under `layout`.
///
/// Validates both endpoints lie inside the same IOD and the same silicon PCIe
/// core window, preserving lane reversal (`start > end`).
pub fn translate_socket_lane(
    layout: Layout,
    start: u8,
    end: u8,
) -> Result<LaneMapping, ask::Error> {
    let map_one = |lane: u8| -> Option<(u8, u8)> {
        match lane {
            0..=63 => Some((0, lane)),
            64..=127 => Some((1, lane - 64)),
            128..=131 => Some((0, lane)),
            132..=135 => Some((1, lane - 4)),
            _ => None,
        }
    };
    let (iod_a, local_a) = map_one(start).ok_or(ask::Error::NoLaneWindow)?;
    let (iod_b, local_b) = map_one(end).ok_or(ask::Error::NoLaneWindow)?;
    if iod_a != iod_b {
        return Err(ask::Error::CrossesWindow);
    }

    // Core table per layout: (first_lane, last_lane, rb_on_iod, core_id, slot_base, max_ports)
    let windows: &[(u8, u8, u8, u8, u8, u8)] = match layout {
        Layout::Sp7A0 => &[
            (0, 15, 0, 1, 8, 8),
            (16, 31, 1, 0, 0, 8),
            (32, 47, 1, 2, 8, 8),
            (128, 131, 0, 6, 16, 4),
        ],
        Layout::Sp7B0 => &[
            (0, 15, 0, 1, 8, 8),
            (16, 31, 2, 0, 8, 8),
            (32, 47, 1, 2, 8, 8),
            (128, 131, 0, 6, 16, 4),
        ],
        Layout::Sp8 => &[
            (0, 15, 0, 1, 8, 8),
            (16, 31, 3, 0, 8, 8),
            (32, 47, 1, 2, 8, 8),
            (48, 63, 2, 3, 8, 8),
            (128, 131, 3, 6, 16, 4),
        ],
    };

    for &(first, last, rb_on_iod, core, slot_base, max_ports) in windows {
        if (first..=last).contains(&local_a) {
            if !(first..=last).contains(&local_b) {
                return Err(ask::Error::CrossesWindow);
            }
            return Ok(LaneMapping {
                iod: iod_a,
                rb_on_iod,
                core,
                slot_base,
                max_ports,
                local_start: local_a.min(local_b),
                local_end: local_a.max(local_b),
                reversed: start > end,
            });
        }
    }
    Err(ask::Error::NoLaneWindow)
}

/// Remap register SMN address relative to a WH [`crate::nbio::wh::RootBridge`].
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
    /// Two ports mapped to the same board port, slot, or PCI devfn on an RB.
    Duplicate,
    /// Requested PCI devfn is not a valid WH root-port devfn for this layout.
    Devfn(u8),
    /// Lane translation failed.
    Lanes(ask::Error),
}

/// Verified root-port identity for one WH MPIO-managed PCIe port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Index in the IOD's returned ASK array.
    pub ask: usize,
    /// Index in the board's full port list (`ports`).
    pub port: usize,
    /// IOD index (`0..=1`).
    pub iod: u8,
    /// Root bridge index within `iod` (`0..layout.rbs_per_iod()`).
    pub rb_on_iod: u8,
    /// Silicon PCIe core ID (`0, 1, 2, 3`, or `6` for the bonus core).
    pub core: u8,
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
}

/// Bind MPIO-returned `asks` for `iod` to the matching board `ports` (filtered
/// to `iod`) by `(local_start_lane, kind)` — never by array index.
pub fn bind(
    layout: Layout,
    iod: u8,
    ports: &[Port],
    asks: &[Ask],
) -> Result<Vec<Binding, MAX_PORTS>, BindError> {
    let mut expected_count = 0usize;
    for port in ports {
        let map = translate_socket_lane(layout, port.start, port.end).map_err(BindError::Lanes)?;
        if map.iod == iod {
            expected_count += 1;
        }
    }

    let defaults = layout.default_devfns();
    let mut bindings: Vec<Binding, MAX_PORTS> = Vec::new();
    let mut matched: Vec<usize, MAX_PORTS> = Vec::new();

    for (ask_idx, sent) in asks.iter().enumerate() {
        let start = sent.lane().read(Lane::START);
        let mut candidates = ports.iter().enumerate().filter_map(|(idx, port)| {
            let map = translate_socket_lane(layout, port.start, port.end).ok()?;
            (map.iod == iod
                && Some(port.kind) == sent.kind()
                && u32::from(map.local_start) == start)
                .then_some((idx, port, map))
        });
        let (port_idx, port, map) = candidates.next().ok_or(BindError::Unmatched)?;
        if candidates.next().is_some() || matched.contains(&port_idx) {
            return Err(BindError::Duplicate);
        }
        matched.push(port_idx).map_err(|_| BindError::Duplicate)?;

        let trained = sent.status().read(Status::STATE) != 0;
        if port.kind != Kind::Pcie || !(trained || port.present) {
            continue;
        }

        let local_port = sent.status().read(Status::LOCAL_PORT_ID) as u8;
        if local_port >= map.max_ports {
            return Err(BindError::Port {
                core: map.core,
                local_port,
            });
        }
        let pcie_bridge_id = map.slot_base + local_port;
        let remap_reg = layout
            .remap_reg_index(pcie_bridge_id)
            .ok_or(BindError::Port {
                core: map.core,
                local_port,
            })?;
        let default_devfn = defaults[usize::from(pcie_bridge_id)];
        if default_devfn == 0xff {
            return Err(BindError::Port {
                core: map.core,
                local_port,
            });
        }
        let devfn = match (port.device, port.function) {
            (0, 0) => default_devfn,
            (dev, func) => fstart_pci::devfn(dev, func),
        };
        let logical_bridge = defaults
            .iter()
            .position(|&d| d == devfn && d != 0xff)
            .ok_or(BindError::Devfn(devfn))? as u8;

        let binding = Binding {
            ask: ask_idx,
            port: port_idx,
            iod,
            rb_on_iod: map.rb_on_iod,
            core: map.core,
            local_port,
            pcie_bridge_id,
            remap_reg,
            devfn,
            logical_bridge,
        };
        if bindings.iter().any(|b| {
            b.rb_on_iod == binding.rb_on_iod
                && (b.pcie_bridge_id == binding.pcie_bridge_id || b.devfn == binding.devfn)
        }) {
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
/// `iod`, placing every bound port's requested devfn first and assigning
/// remaining active default devfns in order (`SilVenicePei` `0xf61930`).
pub fn remap(layout: Layout, iod: u8, rb_on_iod: u8, bindings: &[Binding]) -> [u8; PORT_SLOTS] {
    let defaults = layout.default_devfns();
    let bound = || {
        bindings
            .iter()
            .filter(move |b| b.iod == iod && b.rb_on_iod == rb_on_iod)
    };
    let mut free = defaults
        .iter()
        .copied()
        .filter(|&d| d != 0xff && !bound().any(|b| b.devfn == d));
    core::array::from_fn(|slot| {
        if defaults[slot] == 0xff {
            return 0xff;
        }
        match bound().find(|b| usize::from(b.pcie_bridge_id) == slot) {
            Some(b) => b.devfn,
            None => free.next().unwrap_or(defaults[slot]),
        }
    })
}

/// Program `NB_PROG_DEVICE_REMAP` (`0x13B1_00B8 + remap_reg * 4`) on `nbio` for
/// every active slot of `rb_on_iod` whose mapped devfn differs from default
/// (preserving upper bits), called after `MAP` and before `CONFIGURE`.
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

/// Pre-validated WH MPIO v2 request bundle for a single local IOD (`0..=1`).
pub struct Prepared {
    asks: Vec<Ask, MAX_PORTS>,
    ancillary: Ancillary,
    psp_vetting: bool,
}

impl Prepared {
    /// Prepare a WH MPIO v2 bundle for `iod` (`0..=1`) under `layout`.
    /// Returns `Ok(None)` if no ports in `ports` target `iod`.
    pub fn for_iod(
        layout: Layout,
        iod: u8,
        ports: &[Config<'_>],
    ) -> Result<Option<(Self, Vec<usize, MAX_PORTS>)>, Error> {
        if iod >= 2 {
            return Err(Error::Lanes(ask::Error::RootIndex(iod)));
        }
        let mut asks: Vec<Ask, MAX_PORTS> = Vec::new();
        let mut indices: Vec<usize, MAX_PORTS> = Vec::new();
        let mut ancillary = Ancillary::new(iod);
        let mut used = [false; 136];

        for (board_idx, cfg) in ports.iter().enumerate() {
            if cfg.port.hotplug != 0 {
                return Err(Error::Unsupported);
            }
            let mapping = translate_socket_lane(layout, cfg.port.start, cfg.port.end)
                .map_err(Error::Lanes)?;
            if mapping.iod != iod {
                continue;
            }
            for lane in mapping.local_start..=mapping.local_end {
                if core::mem::replace(&mut used[usize::from(lane)], true) {
                    return Err(Error::Overlap);
                }
            }
            let mut local_port = cfg.port;
            local_port.start = mapping.local_start;
            local_port.end = mapping.local_end;
            let anc_idx = ancillary
                .add(&local_port, cfg.phy, cfg.speed)
                .map_err(Error::Ancillary)?;
            let ask = ask::build_mapped(mapping.as_ask_mapping(), &local_port, anc_idx)
                .map_err(Error::Lanes)?;
            asks.push(ask).map_err(|_| Error::Capacity)?;
            indices.push(board_idx).map_err(|_| Error::Capacity)?;
        }

        if asks.is_empty() {
            return Ok(None);
        }
        Ok(Some((
            Self {
                asks,
                ancillary,
                // Common global config initializes SkipVetting clear (0xf5e7fd).
                psp_vetting: true,
            },
            indices,
        )))
    }

    /// Select whether MPIO firmware runs PSP PHY vetting (`SkipVetting` bit).
    pub fn with_psp_vetting(mut self, enabled: bool) -> Self {
        self.psp_vetting = enabled;
        self
    }

    pub fn ask_count(&self) -> usize {
        self.asks.len()
    }
}

/// Exclusive WH MPIO v2 session for one IOD (`SilVenicePei` `0xf5ec00` / `0xf5f7b0`).
pub struct Session<A> {
    smn: A,
    mailbox: Mailbox,
    dma: Dma,
    prepared: Prepared,
    sync: CacheSync,
    state: State,
    idle_reads: NonZeroU32,
}

impl<A: SmnAccess> Session<A> {
    /// Create a WH MPIO v2 session using [`Encoding::Retained`] and [`DOORBELL_V2`].
    ///
    /// # Safety
    /// BSP-exclusive ownership of this IOD's MPIO controller and all alias views.
    /// `dma` must remain reserved until reset, identity-mapped below 4 GiB, and
    /// `sync` must flush CPU writes and invalidate stale cache lines.
    pub unsafe fn new(
        smn: A,
        dma: Dma,
        prepared: Prepared,
        sync: CacheSync,
        poll: Poll,
        idle_reads: NonZeroU32,
    ) -> Result<Self, Error> {
        if dma.len() != DMA_SIZE
            || dma.phys(0) == 0
            || dma.phys(0) & 63 != 0
            || dma.as_ptr() as usize & 63 != 0
        {
            return Err(Error::Buffer);
        }
        Ok(Self {
            smn,
            // SAFETY: caller owns this IOD's MPIO mailbox.
            mailbox: unsafe { Mailbox::new_with_doorbell(poll, Encoding::Retained, DOORBELL_V2) },
            dma,
            prepared,
            sync,
            state: State::Fresh,
            idle_reads,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn asks(&self) -> Result<&[Ask], Error> {
        match self.state {
            State::Mapped | State::Configured | State::Perst | State::Trained | State::Updated => {
                Ok(&self.prepared.asks)
            }
            _ => Err(Error::State),
        }
    }

    fn require(&self, expected: State) -> Result<(), Error> {
        if self.state == expected {
            Ok(())
        } else {
            Err(Error::State)
        }
    }

    fn cache(&self) {
        // SAFETY: constructor's allocation and cache-coherency contract.
        unsafe { (self.sync)(self.dma.as_ptr(), DMA_SIZE) };
    }

    fn guards(&self) -> Result<(), Error> {
        let ask_end = self.prepared.asks.len() * ASK_BYTES;
        let anc_end = ANCILLARY_OFFSET + self.prepared.ancillary.bytes().len();
        let mut bytes = [0u8; 64];
        for offset in (0..DMA_SIZE).step_by(bytes.len()) {
            self.dma.read(offset, &mut bytes);
            for (i, &byte) in bytes.iter().enumerate() {
                let at = offset + i;
                if at >= ask_end && !(ANCILLARY_OFFSET..anc_end).contains(&at) && byte != 0xa5 {
                    return Err(Error::Guard);
                }
            }
        }
        Ok(())
    }

    fn exchange(
        &mut self,
        command: u16,
        args: [u32; 6],
        timer: &dyn Timer,
    ) -> Result<[u32; 6], Error> {
        self.state = State::Poisoned;
        self.cache();
        fstart_log::info!(
            "mpio-wh: cmd={:#x} args=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]",
            command,
            args[0],
            args[1],
            args[2],
            args[3],
            args[4],
            args[5]
        );
        // SAFETY: Session owns this IOD's MPIO mailbox and cache-synced DMA buffer.
        let reply = unsafe { self.mailbox.request(&self.smn, command, args, timer) }
            .map_err(Error::Mailbox)?;
        // The mailbox has acknowledged TRAIN. Do not wait for the training
        // engine to go idle here: every IOD must start before WAIT_COMPLETE.
        if command != mailbox::msg::SETUP_LINK || args[0] != mailbox::phase::TRAIN {
            self.mailbox
                .wait_idle(&self.smn, timer, self.idle_reads)
                .map_err(Error::Mailbox)?;
        }
        self.cache();
        self.guards()?;
        Ok(reply)
    }

    fn send_asks(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        for (i, ask) in self.prepared.asks.iter().enumerate() {
            self.dma.write(i * ASK_BYTES, ask.bytes());
        }
        let mut flags = LocalRegisterCopy::<u32, Transfer::Register>::new(0);
        flags.write(Transfer::SELECTED::SET + Transfer::TO_MPIO::SET);
        let reply = self.exchange(
            mailbox::msg::TRANSFER_ASK,
            [
                0,
                self.dma.phys(0),
                flags.get(),
                0,
                self.prepared.asks.len() as u32,
                0,
            ],
            timer,
        )?;
        if reply[0] != 1 {
            return Err(Error::Receipt(mailbox::msg::TRANSFER_ASK, reply[0]));
        }
        Ok(())
    }

    fn get_asks(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.exchange(
            mailbox::msg::GET_ASK,
            [
                0,
                self.dma.phys(0),
                0,
                0,
                self.prepared.asks.len() as u32,
                0,
            ],
            timer,
        )?;
        for (i, ask) in self.prepared.asks.iter_mut().enumerate() {
            let mut bytes = [0; ASK_BYTES];
            self.dma.read(i * ASK_BYTES, &mut bytes);
            *ask = Ask::read_from_bytes(&bytes).map_err(|_| Error::Buffer)?;
            if ask.kind().is_none() {
                return Err(Error::Buffer);
            }
        }
        Ok(())
    }

    /// Submit GlobalConfig (`0x02`), optional ExtAttributes (`0x08`), and
    /// initial ASK descriptors (`0x07`).
    pub fn submit(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.require(State::Fresh)?;
        self.state = State::Poisoned;
        self.mailbox
            .wait_idle(&self.smn, timer, self.idle_reads)
            .map_err(Error::Mailbox)?;
        for offset in (0..DMA_SIZE).step_by(64) {
            self.dma.write(offset, &[0xa5; 64]);
        }
        for (i, ask) in self.prepared.asks.iter().enumerate() {
            self.dma.write(i * ASK_BYTES, ask.bytes());
        }
        self.dma
            .write(ANCILLARY_OFFSET, self.prepared.ancillary.bytes());

        let mut global = LocalRegisterCopy::<u32, Global::Register>::new(0);
        global.write(
            Global::USE_PHY_SRAM::SET
                + Global::VALID_PHY_FW::SET
                + Global::SKIP_VETTING.val(u32::from(!self.prepared.psp_vetting)),
        );
        self.exchange(
            mailbox::msg::GLOBAL_CONFIG,
            [global.get(), 0, 0, 0, 0, 0],
            timer,
        )?;
        if self.prepared.ancillary.dwords() != 0 {
            self.exchange(
                mailbox::msg::EXT_ATTRIBUTES,
                [
                    0,
                    self.dma.phys(ANCILLARY_OFFSET),
                    self.prepared.ancillary.dwords(),
                    0,
                    0,
                    0,
                ],
                timer,
            )?;
        }
        self.send_asks(timer)?;
        self.state = State::Submitted;
        Ok(())
    }

    /// Advance one pre-training phase (MAP, CONFIGURE or PERST) and fetch its ASKs.
    pub fn advance(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        let (phase, next) = match self.state {
            State::Submitted => (mailbox::phase::MAP, State::Mapped),
            State::Mapped => (mailbox::phase::CONFIGURE, State::Configured),
            State::Configured => (mailbox::phase::PERST, State::Perst),
            _ => return Err(Error::State),
        };
        self.exchange(mailbox::msg::SETUP_LINK, [phase, 0, 0, 0, 0, 0], timer)?;
        self.get_asks(timer)?;
        self.state = next;
        Ok(())
    }

    pub fn start_train(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.require(State::Perst)?;
        self.exchange(
            mailbox::msg::SETUP_LINK,
            [mailbox::phase::TRAIN, 0, 0, 0, 0, 0],
            timer,
        )?;
        self.state = State::Training;
        Ok(())
    }

    pub fn wait_train(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.require(State::Training)?;
        self.exchange(MSG_WAIT_COMPLETE, [0; 6], timer)?;
        self.state = State::Complete;
        Ok(())
    }

    pub fn collect_train(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.require(State::Complete)?;
        self.get_asks(timer)?;
        self.state = State::Trained;
        Ok(())
    }

    /// Write back resolved `LogicalBridgeId` values (`0..20`) via `TRANSFER_ASK`
    /// (`0x07`); issue `UPDATE_LINK` (`0x322`) only when enabled by platform policy.
    pub fn update(
        &mut self,
        ids: &[Option<u8>],
        update_link: bool,
        timer: &dyn Timer,
    ) -> Result<(), Error> {
        self.require(State::Trained)?;
        if ids.len() != self.prepared.asks.len() {
            return Err(Error::Buffer);
        }
        let mut updated = self.prepared.asks.clone();
        for (ask, id) in updated.iter_mut().zip(ids) {
            if let Some(id) = id {
                if ask.kind() != Some(Kind::Pcie) {
                    return Err(Error::Buffer);
                }
                ask.set_logical_bridge(*id).map_err(Error::Lanes)?;
            }
        }
        self.prepared.asks = updated;
        self.send_asks(timer)?;
        if update_link {
            self.exchange(mailbox::msg::UPDATE_LINK, [0; 6], timer)?;
        }
        self.state = State::Updated;
        Ok(())
    }
}

/// Issue TRAIN to all participating IODs, then wait for all of them before
/// the caller fetches results. The tuple retains the physical IOD for sparse systems.
pub fn train_all<A: SmnAccess>(
    sessions: &mut [(u8, Session<A>)],
    timer: &dyn Timer,
) -> Result<(), Error> {
    for (_, session) in sessions.iter_mut() {
        session.start_train(timer)?;
    }
    for (_, session) in sessions {
        session.wait_train(timer)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::rs::ask::LinkSpeed;
    use crate::smn::testing::{SmnModel, SmnSpace, SmnTest};
    use std::vec::Vec as StdVec;

    #[test]
    fn package_and_stepping_select_donor_layout_and_remap_readback() {
        assert_eq!(Layout::from_cpuid(0xb50f00, 2 << 28), Layout::Sp7A0);
        assert_eq!(Layout::from_cpuid(0xb50f00, 1 << 28), Layout::Sp7B0);
        assert_eq!(Layout::from_cpuid(0xb50f0b, 2 << 28), Layout::Sp7B0);
        assert_eq!(Layout::from_cpuid(0xba0fc0, 8 << 28), Layout::Sp8);
        let space = SmnSpace::default();
        space.set(remap_register(8).address(), 0x29);
        space.set(remap_register(16).address(), 0x19);
        assert_eq!(Layout::Sp7B0.slot_devfn(&space.smn(), 16), Some((5, 1)));
        assert_eq!(Layout::Sp7A0.slot_devfn(&space.smn(), 16), Some((3, 1)));
        assert_eq!(Layout::Sp7B0.slot_devfn(&space.smn(), 20), None);
    }

    struct FakeTimer;
    impl Timer for FakeTimer {
        fn timestamp_us(&self) -> u64 {
            0
        }
        fn delay_us(&self, _: u64) {}
    }

    type PhaseTrace = std::rc::Rc<std::cell::RefCell<StdVec<(u64, u32)>>>;

    #[derive(Clone)]
    struct MockWhMpio {
        ctrl: u32,
        args: [u32; 6],
        doorbells: StdVec<(u32, u32, [u32; 6])>,
        dma_base: u64,
        dma_ptr: *mut u8,
        trained_status: u32,
        phase_trace: Option<PhaseTrace>,
        reverse_get_ask: bool,
        saved_asks: StdVec<[u8; ASK_BYTES]>,
    }

    impl SmnModel for MockWhMpio {
        fn read(&mut self, addr: u32) -> u32 {
            if addr == mailbox::CONTROL.address() {
                self.ctrl
            } else if addr >= mailbox::ARGUMENTS.address()
                && addr < mailbox::ARGUMENTS.address() + 24
            {
                self.args[((addr - mailbox::ARGUMENTS.address()) / 4) as usize]
            } else {
                0
            }
        }

        fn write(&mut self, addr: u32, value: u32) {
            if addr == mailbox::CONTROL.address() {
                self.ctrl = value;
            } else if addr >= mailbox::ARGUMENTS.address()
                && addr < mailbox::ARGUMENTS.address() + 24
            {
                self.args[((addr - mailbox::ARGUMENTS.address()) / 4) as usize] = value;
            } else if addr == DOORBELL_V2.address() {
                let cmd = (self.ctrl >> 8) & 0xfff;
                self.doorbells.push((self.ctrl, value, self.args));
                if let Some(trace) = &self.phase_trace
                    && cmd != 1
                {
                    trace.borrow_mut().push((self.dma_base, cmd));
                }
                match cmd {
                    0x01 => {
                        self.args[0] = 0; // idle
                    }
                    0x07 => {
                        // Save ASKs from DMA buffer and acknowledge receipt.
                        let count = self.args[4] as usize;
                        assert_eq!(self.args[0], 0);
                        assert_eq!(self.args[1], self.dma_base as u32);
                        assert_eq!(self.args[2], 3);
                        self.saved_asks.clear();
                        for i in 0..count {
                            let mut buf = [0u8; ASK_BYTES];
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    self.dma_ptr.add(i * ASK_BYTES),
                                    buf.as_mut_ptr(),
                                    ASK_BYTES,
                                );
                            }
                            self.saved_asks.push(buf);
                        }
                        self.args[0] = 1;
                    }
                    0x03 => {
                        // Populate DMA buffer with status + local_port_id, optionally reversed.
                        let count = self.args[4] as usize;
                        assert_eq!(self.args[0], 0);
                        assert_eq!(self.args[1], self.dma_base as u32);
                        assert_eq!(count, self.saved_asks.len());
                        let mut out = self.saved_asks.clone();
                        for (i, buf) in out.iter_mut().enumerate() {
                            let status = (self.trained_status & 0xf) | (((i as u32) & 0x7) << 16);
                            buf[0x20..0x24].copy_from_slice(&status.to_le_bytes());
                        }
                        if self.reverse_get_ask {
                            out.reverse();
                        }
                        for (i, buf) in out.iter().enumerate() {
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    buf.as_ptr(),
                                    self.dma_ptr.add(i * ASK_BYTES),
                                    ASK_BYTES,
                                );
                            }
                        }
                    }
                    _ => {}
                }
                self.ctrl = (1u32 << 31) | 1;
            }
        }
    }

    unsafe fn noop_sync(_: *const u8, _: usize) {}

    #[test]
    fn both_iods_start_before_either_waits_or_fetches_results() {
        #[repr(align(64))]
        struct Buffer([u8; DMA_SIZE]);
        let mut buffers = [Buffer([0; DMA_SIZE]), Buffer([0; DMA_SIZE])];
        let config = [0, 64].map(|lane| Config {
            port: Port::pcie(lane, lane + 15),
            phy: &[],
            speed: &[],
        });
        let trace = std::rc::Rc::new(std::cell::RefCell::new(StdVec::new()));
        let mut sessions = StdVec::new();
        // Models outlive all session handles; the DMA buffers stay distinct.
        let models = core::array::from_fn::<_, 2, _>(|i| {
            SmnTest::new(MockWhMpio {
                ctrl: 1 << 31,
                args: [0; 6],
                doorbells: StdVec::new(),
                dma_base: (i as u64 + 1) * 0x100_0000,
                dma_ptr: buffers[i].0.as_mut_ptr(),
                trained_status: 6,
                phase_trace: Some(trace.clone()),
                reverse_get_ask: false,
                saved_asks: StdVec::new(),
            })
        });
        for (i, model) in models.iter().enumerate() {
            let iod = i as u8;
            let (prepared, _) = Prepared::for_iod(Layout::Sp7B0, iod, &config)
                .unwrap()
                .unwrap();
            let dma =
                unsafe { Dma::new(buffers[i].0.as_mut_ptr(), DMA_SIZE, model.model().dma_base) }
                    .unwrap();
            let mut session = unsafe {
                Session::new(
                    model.smn(),
                    dma,
                    prepared,
                    noop_sync,
                    Poll::default(),
                    NonZeroU32::MIN,
                )
            }
            .unwrap();
            session.submit(&FakeTimer).unwrap();
            for _ in 0..3 {
                session.advance(&FakeTimer).unwrap();
            }
            sessions.push((iod, session));
        }
        trace.borrow_mut().clear();
        train_all(&mut sessions, &FakeTimer).unwrap();
        for (_, session) in &mut sessions {
            session.collect_train(&FakeTimer).unwrap();
            session.update(&[None], false, &FakeTimer).unwrap();
        }
        let (a, b) = (models[0].model().dma_base, models[1].model().dma_base);
        assert_eq!(
            trace.borrow().as_slice(),
            &[
                (a, 0x304),
                (b, 0x304),
                (a, 0x3a),
                (b, 0x3a),
                (a, 3),
                (a, 7),
                (b, 3),
                (b, 7)
            ]
        );
    }

    #[test]
    fn splits_md25_cg0_lanes_and_keeps_bonus_core_distinct() {
        let board = [
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(0, 15)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(16, 31)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(32, 47)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(64, 79)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(80, 95)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(96, 111)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen4,
                    ..Port::pcie(129, 129)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen4,
                    ..Port::pcie(130, 131)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen5,
                    ..Port::pcie(132, 133)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    max_speed: LinkSpeed::Gen5,
                    ..Port::pcie(134, 135)
                },
                phy: &[],
                speed: &[],
            },
        ];
        let (_, idx0) = Prepared::for_iod(Layout::Sp7B0, 0, &board)
            .unwrap()
            .unwrap();
        let (_, idx1) = Prepared::for_iod(Layout::Sp7B0, 1, &board)
            .unwrap()
            .unwrap();
        assert_eq!(idx0.as_slice(), &[0, 1, 2, 6, 7]);
        assert_eq!(idx1.as_slice(), &[3, 4, 5, 8, 9]);

        // Bonus core on SP7 B0/A0 lives on RB0 (core 6, slots 16..20), NEVER RB3!
        let m_sp7 = translate_socket_lane(Layout::Sp7B0, 132, 133).unwrap();
        assert_eq!(
            (
                m_sp7.iod,
                m_sp7.rb_on_iod,
                m_sp7.core,
                m_sp7.slot_base,
                m_sp7.local_start,
                m_sp7.local_end,
                m_sp7.reversed
            ),
            (1, 0, 6, 16, 128, 129, false)
        );
        // On SP8 (4 RBs/IOD), bonus core lives on RB3 (core 6, slots 16..20).
        let m_sp8 = translate_socket_lane(Layout::Sp8, 135, 134).unwrap();
        assert_eq!(
            (m_sp8.iod, m_sp8.rb_on_iod, m_sp8.core, m_sp8.reversed),
            (1, 3, 6, true)
        );

        // Cross-IOD and cross-core spans are rejected.
        assert_eq!(
            translate_socket_lane(Layout::Sp7B0, 60, 67),
            Err(ask::Error::CrossesWindow)
        );
        assert_eq!(
            translate_socket_lane(Layout::Sp7B0, 131, 132),
            Err(ask::Error::CrossesWindow)
        );
        assert_eq!(
            translate_socket_lane(Layout::Sp7B0, 12, 19),
            Err(ask::Error::CrossesWindow)
        );
        assert_eq!(
            translate_socket_lane(Layout::Sp7B0, 136, 137),
            Err(ask::Error::NoLaneWindow)
        );
    }

    #[test]
    fn full_wh_session_emits_retained_opcodes_wait_complete_and_binds_reordered_asks() {
        #[repr(C, align(64))]
        struct AlignedBuf([u8; DMA_SIZE]);
        let mut backing = AlignedBuf([0u8; DMA_SIZE]);
        let phys = 0x1000_0000u64;
        let dma = unsafe { Dma::new(backing.0.as_mut_ptr(), DMA_SIZE, phys) }.unwrap();

        let board = [
            Config {
                port: Port {
                    device: 3,
                    function: 2, // custom devfn 0x1a (default for slot 8 is 0x19)
                    present: true,
                    max_speed: LinkSpeed::Gen6,
                    ..Port::pcie(0, 15)
                },
                phy: &[],
                speed: &[],
            },
            Config {
                port: Port {
                    present: true,
                    max_speed: LinkSpeed::Gen4,
                    ..Port::pcie(130, 131)
                },
                phy: &[],
                speed: &[],
            },
        ];
        let ports: StdVec<Port> = board.iter().map(|c| c.port).collect();
        let (prepared, _) = Prepared::for_iod(Layout::Sp7B0, 0, &board)
            .unwrap()
            .unwrap();

        let model = SmnTest::new(MockWhMpio {
            ctrl: 1u32 << 31,
            args: [0; 6],
            doorbells: StdVec::new(),
            dma_base: phys,
            dma_ptr: backing.0.as_mut_ptr(),
            trained_status: 6,
            phase_trace: None,
            reverse_get_ask: true,
            saved_asks: StdVec::new(),
        });
        let poll = Poll {
            reads: NonZeroU32::new(16).unwrap(),
            interval_us: NonZeroU32::new(1).unwrap(),
        };
        let mut sess = unsafe {
            Session::new(
                model.smn(),
                dma,
                prepared,
                noop_sync,
                poll,
                NonZeroU32::new(16).unwrap(),
            )
        }
        .unwrap();

        sess.submit(&FakeTimer).unwrap();
        sess.advance(&FakeTimer).unwrap(); // MAP

        // Returned ASKs were reversed by MockWhMpio — bind must match by lane, NOT index.
        let mapped = bind(Layout::Sp7B0, 0, &ports, sess.asks().unwrap()).unwrap();
        assert_eq!(mapped.len(), 2);
        let b_main = mapped.iter().find(|b| b.port == 0).unwrap();
        let b_bonus = mapped.iter().find(|b| b.port == 1).unwrap();
        assert_eq!(
            (
                b_main.rb_on_iod,
                b_main.core,
                b_main.pcie_bridge_id,
                b_main.remap_reg,
                b_main.devfn,
                b_main.logical_bridge
            ),
            (0, 1, 8 + b_main.local_port, b_main.local_port, 0x1a, 9)
        );
        assert_eq!(
            (
                b_bonus.rb_on_iod,
                b_bonus.core,
                b_bonus.pcie_bridge_id,
                b_bonus.remap_reg,
                b_bonus.devfn,
                b_bonus.logical_bridge
            ),
            (
                0,
                6,
                16 + b_bonus.local_port,
                8 + b_bonus.local_port,
                0x29 + b_bonus.local_port,
                16 + b_bonus.local_port
            )
        );

        // Program remaps into SMN and verify NB_PROG_DEVICE_REMAP written on B0 (slot - 8).
        let rb0_smn = SmnSpace::default();
        program_remaps(&rb0_smn.smn(), Layout::Sp7B0, 0, 0, &mapped);
        assert_eq!(
            rb0_smn.get(remap_register(b_main.remap_reg).address()) & 0xff,
            0x1a
        );

        sess.advance(&FakeTimer).unwrap(); // CONFIGURE
        sess.advance(&FakeTimer).unwrap(); // PERST
        sess.start_train(&FakeTimer).unwrap();
        assert!(sess.collect_train(&FakeTimer).is_err());
        sess.wait_train(&FakeTimer).unwrap();
        sess.collect_train(&FakeTimer).unwrap();

        let trained = bind(Layout::Sp7B0, 0, &ports, sess.asks().unwrap()).unwrap();
        let mut lbids = [None; 2];
        for b in &trained {
            lbids[b.ask] = Some(b.logical_bridge);
        }
        sess.update(&lbids, true, &FakeTimer).unwrap();
        assert_eq!(sess.state(), State::Updated);

        // Verify the exact control-register opcodes written (Encoding::Retained, no truncation!).
        let ctrls: StdVec<u32> = model
            .model()
            .doorbells
            .iter()
            .map(|(ctrl, _, _)| *ctrl)
            .filter(|&c| c != 0x00100) // omit STATUS idle polls
            .collect();
        assert_eq!(
            ctrls,
            [
                0x00200, // GLOBAL_CONFIG (0x02)
                0x00700, // TRANSFER_ASK (0x07)
                0x30400, // SETUP_LINK MAP (0x304, NOT truncated 0x04!)
                0x00300, // GET_ASK (0x03)
                0x30400, // SETUP_LINK CONFIGURE
                0x00300, // GET_ASK
                0x30400, // SETUP_LINK PERST
                0x00300, // GET_ASK
                0x30400, // SETUP_LINK TRAIN
                0x03a00, // WAIT_COMPLETE (0x3a)
                0x00300, // GET_ASK
                0x00700, // TRANSFER_ASK (logical bridge update)
                0x32200, // UPDATE_LINK (0x322, NOT truncated 0x22!)
            ]
        );
    }
}
