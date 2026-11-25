// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH MPIO (`MpioInitFlowWh`, `MpioPcieWh`, `SilVenicePei.pe`): one MPIO
//! instance per IOD, reached through that IOD's root bus.
//!
//! - Transport: `Encoding::Retained` (`(cmd & 0xfff) << 8`) with
//!   `DOORBELL_V2` (`SMN 0x0C91_0954`), keeping the 12-bit posted opcodes
//!   `0x304` (`SETUP_LINK`) and `0x322` (`UPDATE_LINK`); `0x3A` waits for
//!   training. A status poll (`0x01` until idle) precedes every `0x03`,
//!   `0x07` and `0x08` (Dell `0xf5df10`).
//! - Session (MPIO.md §2.2): `0x02` global configuration ([`global`]),
//!   optional `0x08` ancillary data, `0x07` ASKs; three `0x304` (all
//!   arguments zero), each followed by `0x03`: map (then the host binds
//!   ports and remaps devfns, [`layout`], and writes the pre-reconfiguration
//!   straps, [`straps`]), reconfigure (post-reconfiguration programming),
//!   pre-train; PERST release; the early train (`0x304` with argument 1 =
//!   1, `0x03`) when a port asks for it; `0x304` and `0x3A` on every
//!   instance before any result is read ([`train_all`]); `0x03`, the
//!   logical bridges by `0x07` and, with UBM hotplug, `0x322`.
//! - Port description: [`port`]; hotplug: [`hotplug`].

pub mod early;
pub mod global;
pub mod hotplug;
pub mod layout;
pub mod port;
pub mod straps;

pub use global::GlobalConfig;
pub use layout::*;
pub use port::PortDesc;

use core::num::NonZeroU32;
use fstart_core::services::Timer;
use heapless::Vec;
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::FromBytes;

use crate::dxio::rs::ancillary::{self, Ancillary};
use crate::dxio::rs::ask::{self, Ask, Kind};
use crate::dxio::rs::mailbox::{self, DOORBELL_V2, Encoding, Mailbox, Poll};
pub use crate::dxio::rs::session::{CacheSync, DMA_SIZE, Dma, MAX_PORTS};
use crate::smn::{SmnAccess, SmnReg};

/// Session phase. WH has separate all-instance TRAIN and WAIT barriers.
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

/// Errors of a WH MPIO session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Overlap,
    Buffer,
    State,
    Guard,
    Lanes(ask::Error),
    /// Port `.0` of the socket's list is malformed.
    Port(usize, port::Error),
    Ancillary(ancillary::Error),
    Mailbox(mailbox::Error),
    Receipt(u16, u32),
}

impl From<mailbox::Error> for Error {
    fn from(e: mailbox::Error) -> Self {
        Self::Mailbox(e)
    }
}

/// Where the session keeps what MPIO reads and writes in its DMA window.
const ASK_OFFSET: usize = 0;
const DELI_OFFSET: usize = 0x4000;
/// Size of the early-link information MPIO returns (request `0x0E`).
pub const DELI_BYTES: usize = 0x60;
const ANCILLARY_OFFSET: usize = 0x8000;
const HOTPLUG_OFFSET: usize = 0xc000;
const ASK_BYTES: usize = core::mem::size_of::<Ask>();

/// WH MPIO requests beyond the RS set ([`mailbox::msg`]).
pub mod msg {
    /// `BIOS_MPIO_MSG_WAIT_COMPLETE` (Dell `0xf5fa3a`).
    pub const WAIT_COMPLETE: u16 = 0x3a;
    /// Early-link (DELI) information of a lane (Dell `0xf470c0`).
    pub const GET_DELI_INFO: u16 = 0x0e;
    /// UBM: release a port's PERST (`MpioReleasePortWh`, Dell `0xf679a0`).
    pub const UBM_PERST_RELEASE: u16 = 0x2a;
    /// Hotplug (`AmdNbioDxe` `0xcf80`).
    pub const HOTPLUG_CONFIG_TABLE_SET: u16 = 0x13;
    pub const HOTPLUG_ENABLE: u16 = 0x14;
    pub const HOTPLUG_I2C_SWITCH_ADDR_SET: u16 = 0x16;
    pub const HOTPLUG_FLAGS_SET: u16 = 0x19;
}

/// The mailbox of one IOD's MPIO as WH's firmware has it: the retained
/// encoding and the V2 doorbell. Every WH MPIO request goes through one of
/// these ([`Session`], [`Proxy`], [`train_all`]).
///
/// # Safety
/// As [`Mailbox::new_with_doorbell`]: the caller owns the IOD's MPIO
/// mailbox, which is idle.
pub(crate) const unsafe fn mailbox(poll: Poll) -> Mailbox {
    // SAFETY: the caller's contract; WH's MPIO v2 interface.
    unsafe { Mailbox::new_with_doorbell(poll, Encoding::Retained, DOORBELL_V2) }
}

/// The arguments of a [`MSG_PCIE_PROXY`] read-modify-write: `(old &
/// !clear) | set` of `width` at SMN `address`.
const fn proxy_rmw(address: u32, clear: u32, set: u32, width: Width) -> [u32; 6] {
    [address, clear, set, width as u32, 0, 0]
}

/// The arguments of a [`MSG_PCIE_PROXY`] read (the value in reply word 0).
const fn proxy_read(address: u32, width: Width) -> [u32; 6] {
    proxy_rmw(address, 0, 0, width)
}

register_bitfields![u32,
    Transfer [
        SELECTED OFFSET(0) NUMBITS(1) [],
        TO_MPIO OFFSET(1) NUMBITS(1) []
    ],
    /// Request `0x2A` argument 0.
    Release [
        PORT OFFSET(0) NUMBITS(8) [],
        CORE OFFSET(8) NUMBITS(8) [],
        ROOT_BRIDGE OFFSET(16) NUMBITS(8) [],
        /// Always 2.
        KIND OFFSET(24) NUMBITS(8) []
    ]
];

/// Pre-validated WH MPIO request bundle for one IOD.
pub struct Prepared {
    asks: Vec<Ask, MAX_ASKS>,
    ancillary: Ancillary,
    global: [u32; 6],
    early: bool,
}

impl Prepared {
    /// The ASKs and ancillary data of the ports of `socket`'s list `ports`
    /// on IOD `iod` under `layout`, with the global configuration `global`;
    /// ancillary records only if `ancillary` (`MPIOAncDataSupport`).
    /// `Ok(None)` if no port is on `iod`. The second value maps each ASK to
    /// its index in `ports`.
    pub fn for_iod(
        layout: Layout,
        socket: u8,
        iod: u8,
        ports: &[PortDesc<'_>],
        global: &GlobalConfig,
        ancillary: bool,
    ) -> Result<Option<(Self, Vec<usize, MAX_ASKS>)>, Error> {
        if iod >= 2 {
            return Err(Error::Lanes(ask::Error::RootIndex(iod)));
        }
        let mut asks: Vec<Ask, MAX_ASKS> = Vec::new();
        let mut indices: Vec<usize, MAX_ASKS> = Vec::new();
        // The type-1 header carries the socket (`GnbHandle+0x0A`).
        let mut stream = Ancillary::new(socket);
        let mut used = [false; 68];
        let mut early = false;

        for (i, desc) in ports.iter().enumerate() {
            if desc.engine == port::Engine::Unused {
                continue;
            }
            desc.validate().map_err(|e| Error::Port(i, e))?;
            let map = desc.mapping(layout).map_err(Error::Lanes)?;
            if map.iod != iod {
                continue;
            }
            for lane in map.start..=map.end {
                if core::mem::replace(&mut used[usize::from(lane)], true) {
                    return Err(Error::Overlap);
                }
            }
            let index = if ancillary {
                let (phy, speed) = desc.ancillary_values::<32>();
                // Type 7's parameter is the socket start lane.
                let (lo, hi) = desc.lanes();
                let socket_port = ask::Port {
                    kind: if desc.engine == port::Engine::Sata {
                        Kind::Sata
                    } else {
                        Kind::Pcie
                    },
                    ..ask::Port::pcie(lo, hi)
                };
                stream
                    .add(&socket_port, &phy, &speed)
                    .map_err(Error::Ancillary)?
            } else {
                0
            };
            let ask = desc.ask(&map, index).map_err(|e| Error::Port(i, e))?;
            early |= desc.early_train;
            asks.push(ask).map_err(|_| Error::Capacity)?;
            indices.push(i).map_err(|_| Error::Capacity)?;
        }

        if asks.is_empty() {
            return Ok(None);
        }
        Ok(Some((
            Self {
                asks,
                ancillary: stream,
                global: global.dwords(layout.is_a0()),
                early,
            },
            indices,
        )))
    }

    pub fn ask_count(&self) -> usize {
        self.asks.len()
    }

    /// A port asked to train early (phase 3, `CfgEarlyLink`).
    pub fn early_train(&self) -> bool {
        self.early
    }

    /// Request `0x02`'s arguments.
    pub fn global(&self) -> [u32; 6] {
        self.global
    }
}

/// The early-link information of a lane (request `0x0E`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deli {
    /// Byte `0x52`: the information is valid.
    pub valid: bool,
    /// Byte `0x53`: the core-local port.
    pub port: u8,
    pub raw: [u8; DELI_BYTES],
}

/// Exclusive WH MPIO session for one IOD (`SilVenicePei` `0xf5ec00` / `0xf5f7b0`).
pub struct Session<A> {
    smn: A,
    mailbox: Mailbox,
    dma: Dma,
    prepared: Prepared,
    sync: CacheSync,
    state: State,
    idle_reads: NonZeroU32,
    hotplug_bytes: usize,
}

impl<A: SmnAccess> Session<A> {
    /// Create a WH MPIO session on the IOD's mailbox ([`mailbox`]).
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
            mailbox: unsafe { mailbox(poll) },
            dma,
            prepared,
            sync,
            state: State::Fresh,
            idle_reads,
            hotplug_bytes: 0,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The IOD's SMN.
    pub fn smn(&self) -> &A {
        &self.smn
    }

    pub fn prepared(&self) -> &Prepared {
        &self.prepared
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
        let live = [
            ASK_OFFSET..ASK_OFFSET + self.prepared.asks.len() * ASK_BYTES,
            DELI_OFFSET..DELI_OFFSET + DELI_BYTES,
            ANCILLARY_OFFSET..ANCILLARY_OFFSET + self.prepared.ancillary.bytes().len(),
            HOTPLUG_OFFSET..HOTPLUG_OFFSET + self.hotplug_bytes,
        ];
        const GUARD: u64 = u64::from_ne_bytes([0xa5; 8]);
        let mut bytes = [0u8; 64];
        for offset in (0..DMA_SIZE).step_by(bytes.len()) {
            self.dma.read(offset, &mut bytes);
            // Eight bytes at a time; a word that is not all guard is
            // checked byte by byte against the live ranges.
            for (w, word) in bytes.as_chunks::<8>().0.iter().enumerate() {
                if u64::from_ne_bytes(*word) == GUARD {
                    continue;
                }
                for (i, &byte) in word.iter().enumerate() {
                    let at = offset + w * 8 + i;
                    if byte != 0xa5 && !live.iter().any(|r| r.contains(&at)) {
                        return Err(Error::Guard);
                    }
                }
            }
        }
        Ok(())
    }

    /// One request; the session stays poisoned unless it completes.
    fn exchange(
        &mut self,
        command: u16,
        args: [u32; 6],
        timer: &dyn Timer,
    ) -> Result<[u32; 6], Error> {
        let state = self.state;
        self.state = State::Poisoned;
        if matches!(
            command,
            mailbox::msg::GET_ASK | mailbox::msg::TRANSFER_ASK | mailbox::msg::EXT_ATTRIBUTES
        ) {
            self.mailbox
                .wait_idle(&self.smn, timer, self.idle_reads)
                .map_err(Error::Mailbox)?;
        }
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
        self.cache();
        self.guards()?;
        self.state = state;
        Ok(reply)
    }

    fn send_asks(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        for (i, ask) in self.prepared.asks.iter().enumerate() {
            self.dma.write(ASK_OFFSET + i * ASK_BYTES, ask.bytes());
        }
        let mut flags = LocalRegisterCopy::<u32, Transfer::Register>::new(0);
        flags.write(Transfer::SELECTED::SET + Transfer::TO_MPIO::SET);
        let reply = self.exchange(
            mailbox::msg::TRANSFER_ASK,
            [
                0,
                self.dma.phys(ASK_OFFSET),
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
                self.dma.phys(ASK_OFFSET),
                0,
                0,
                self.prepared.asks.len() as u32,
                0,
            ],
            timer,
        )?;
        for (i, ask) in self.prepared.asks.iter_mut().enumerate() {
            let mut bytes = [0; ASK_BYTES];
            self.dma.read(ASK_OFFSET + i * ASK_BYTES, &mut bytes);
            *ask = Ask::read_from_bytes(&bytes).map_err(|_| Error::Buffer)?;
            if ask.kind().is_none() {
                return Err(Error::Buffer);
            }
        }
        Ok(())
    }

    /// Submit the global configuration (`0x02`), the ancillary data
    /// (`0x08`, if any) and the ASKs (`0x07`).
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
            self.dma.write(ASK_OFFSET + i * ASK_BYTES, ask.bytes());
        }
        self.dma
            .write(ANCILLARY_OFFSET, self.prepared.ancillary.bytes());
        self.state = State::Fresh;
        self.exchange(mailbox::msg::GLOBAL_CONFIG, self.prepared.global, timer)?;
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

    /// The next pre-training step (map, reconfigure, pre-train): `0x304`
    /// with all arguments zero, then `0x03`.
    pub fn advance(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        let next = match self.state {
            State::Submitted => State::Mapped,
            State::Mapped => State::Configured,
            State::Configured => State::Perst,
            _ => return Err(Error::State),
        };
        self.exchange(mailbox::msg::SETUP_LINK, [0; 6], timer)?;
        self.get_asks(timer)?;
        self.state = next;
        Ok(())
    }

    /// Phase 3 after PERST release: `0x304` with argument 1 = 1, then `0x03`
    /// (Dell `0xf5f5f0`).
    pub fn early_train(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.require(State::Perst)?;
        self.exchange(mailbox::msg::SETUP_LINK, [0, 1, 0, 0, 0, 0], timer)?;
        self.get_asks(timer)?;
        self.state = State::Perst;
        Ok(())
    }

    pub fn start_train(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.require(State::Perst)?;
        self.exchange(mailbox::msg::SETUP_LINK, [0; 6], timer)?;
        self.state = State::Training;
        Ok(())
    }

    pub fn wait_train(&mut self, timer: &dyn Timer) -> Result<(), Error> {
        self.require(State::Training)?;
        self.exchange(msg::WAIT_COMPLETE, [0; 6], timer)?;
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
    /// (`0x07`); `UPDATE_LINK` (`0x322`) follows with UBM hotplug only
    /// (`CfgHotplugUMBSupport`, Dell `0xf5fc3c`).
    pub fn update(
        &mut self,
        ids: &[Option<u8>],
        ubm_hotplug: bool,
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
        if ubm_hotplug {
            self.exchange(mailbox::msg::UPDATE_LINK, [0; 6], timer)?;
        }
        self.state = State::Updated;
        Ok(())
    }

    fn require_live(&self) -> Result<(), Error> {
        match self.state {
            State::Poisoned | State::Training => Err(Error::State),
            _ => Ok(()),
        }
    }

    /// Request `0x0C`: strap `index` of `core` in `space`.
    pub fn write_strap(
        &mut self,
        core: u8,
        index: u16,
        value: u32,
        space: straps::Space,
        timer: &dyn Timer,
    ) -> Result<(), Error> {
        self.require_live()?;
        self.exchange(
            mailbox::msg::WRITE_STRAP,
            [
                straps::strap_argument(core, index),
                value,
                space as u32,
                0,
                0,
                0,
            ],
            timer,
        )?;
        Ok(())
    }

    /// Request `0x26` on the session's mailbox: `(old & !clear) | set` at
    /// SMN `address` (32-bit), as [`Proxy::rmw`].
    pub fn proxy_rmw(
        &mut self,
        address: u32,
        clear: u32,
        set: u32,
        timer: &dyn Timer,
    ) -> Result<(), Error> {
        self.require_live()?;
        self.exchange(
            MSG_PCIE_PROXY,
            proxy_rmw(address, clear, set, Width::Dword),
            timer,
        )?;
        Ok(())
    }

    /// Request `0x26` read of SMN `address` on the session's mailbox, as
    /// [`Proxy::read`] (the hotplug setup's counter wait,
    /// [`hotplug::settle`]).
    pub fn proxy_read(
        &mut self,
        address: u32,
        width: Width,
        timer: &dyn Timer,
    ) -> Result<u32, Error> {
        self.require_live()?;
        Ok(self.exchange(MSG_PCIE_PROXY, proxy_read(address, width), timer)?[0])
    }

    /// Run `ops`: straps and proxy requests through MPIO, SMN directly on
    /// the IOD, root-port configuration through `config(offset, clear, set,
    /// word)`.
    pub fn run(
        &mut self,
        ops: &[straps::Op],
        config: &mut dyn FnMut(u16, u32, u32, bool),
        timer: &dyn Timer,
    ) -> Result<(), Error> {
        use straps::Op;
        use tock_registers::interfaces::{Readable, Writeable};
        for op in ops {
            match *op {
                Op::Strap {
                    core,
                    index,
                    value,
                    space,
                } => self.write_strap(core, index, value, space, timer)?,
                Op::Proxy {
                    address,
                    clear,
                    set,
                } => self.proxy_rmw(address, clear, set, timer)?,
                Op::Smn {
                    address,
                    clear,
                    set,
                } => {
                    let reg = self.smn.reg(crate::smn::SmnReg::<()>::new(address));
                    reg.set((reg.get() & !clear) | set);
                }
                Op::Config {
                    offset,
                    clear,
                    set,
                    word,
                } => config(offset, clear, set, word),
            }
        }
        Ok(())
    }

    /// Request `0x0E`: the early-link information of MPIO lane `lane`.
    pub fn deli_info(&mut self, lane: u8, timer: &dyn Timer) -> Result<Deli, Error> {
        self.require_live()?;
        self.dma.write(DELI_OFFSET, &[0; DELI_BYTES]);
        self.exchange(
            msg::GET_DELI_INFO,
            [0, 0, self.dma.phys(DELI_OFFSET), 0, lane.into(), 0],
            timer,
        )?;
        let mut raw = [0; DELI_BYTES];
        self.dma.read(DELI_OFFSET, &mut raw);
        Ok(Deli {
            valid: raw[0x52] != 0,
            port: raw[0x53],
            raw,
        })
    }

    /// Request `0x2A`: release PERST of `port` of `core` on IOHC `iohc`
    /// (UBM hotplug only, `MpioReleasePortWh`).
    pub fn release_port(
        &mut self,
        iohc: u8,
        core: u8,
        port: u8,
        timer: &dyn Timer,
    ) -> Result<(), Error> {
        self.require_live()?;
        let mut arg = LocalRegisterCopy::<u32, Release::Register>::new(0);
        arg.write(
            Release::KIND.val(2)
                + Release::ROOT_BRIDGE.val(iohc.into())
                + Release::CORE.val(core.into())
                + Release::PORT.val(port.into()),
        );
        self.exchange(msg::UBM_PERST_RELEASE, [arg.get(), 0, 0, 0, 0, 0], timer)?;
        Ok(())
    }

    /// Request `0x16`: one hotplug I²C expander address.
    pub fn hotplug_i2c_address(&mut self, address: u32, timer: &dyn Timer) -> Result<(), Error> {
        self.require_live()?;
        self.exchange(
            msg::HOTPLUG_I2C_SWITCH_ADDR_SET,
            [address, 0, 0, 0, 0, 0],
            timer,
        )?;
        Ok(())
    }

    /// Request `0x13`: the slot table.
    pub fn hotplug_table(
        &mut self,
        table: &hotplug::Table,
        timer: &dyn Timer,
    ) -> Result<(), Error> {
        self.require_live()?;
        let bytes = table.bytes();
        self.dma.write(HOTPLUG_OFFSET, &bytes);
        self.hotplug_bytes = bytes.len();
        self.exchange(
            msg::HOTPLUG_CONFIG_TABLE_SET,
            [
                self.dma.phys(HOTPLUG_OFFSET),
                0,
                hotplug::TABLE_BYTES as u32,
                0,
                0,
                0,
            ],
            timer,
        )?;
        Ok(())
    }

    /// Requests `0x19` (if `flags` is non-zero) and `0x14`.
    pub fn hotplug_enable(
        &mut self,
        flags: u32,
        enable: u32,
        timer: &dyn Timer,
    ) -> Result<(), Error> {
        self.require_live()?;
        if flags != 0 {
            self.exchange(msg::HOTPLUG_FLAGS_SET, [flags, 0, 0, 0, 0, 0], timer)?;
        }
        self.exchange(msg::HOTPLUG_ENABLE, [enable, 0, 0, 0, 0, 0], timer)?;
        Ok(())
    }
}

/// WH MPIO `PCIE_PROXY` (`0x26`, `MpioServiceRequest`; SilVenicePei Dell
/// `0xf64210/0xf64330/0xf64440`, GIGABYTE `0x709b7e37/0x709b7eb4`, AmdNbioDxe
/// `0x24c10`): a read-modify-write of a PCIe core (`0x1A38_xxxx`), port
/// (`0x1A34_xxxx`) or root-complex private (`0x1C40/0x1C44/0x1C48_xxxx`)
/// register by the MPIO firmware. Arguments `{address, clear, set, width, 0,
/// 0}`: `(old & !clear) | set`; a read is `{address, 0, 0, width}` with the
/// value in reply word 0, a write `{address, 0xFFFF_FFFF, value, width}`.
/// Both donors reach these registers only this way (NBIO.md §2.9, Q2).
pub const MSG_PCIE_PROXY: u16 = 0x26;

/// WH MPIO `MPIO_ENABLE_CG` (`0x05`, AmdNbioDxe
/// `SmuMpioClockGatingRTBCallBack` `0x4ee0`): sent at ready-to-boot.
pub const MSG_ENABLE_CG: u16 = 0x05;

/// Access width of a [`MSG_PCIE_PROXY`] request (argument 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    Dword = 0,
    Word = 2,
}

/// The requests on one IOD's MPIO mailbox that carry no DMA buffer and that
/// the vendor issues outside the training session: [`MSG_PCIE_PROXY`] and
/// [`MSG_ENABLE_CG`]. Registers read and written through it are tock
/// registers ([`Self::reg`]); their `modify` is one firmware RMW, not a read
/// and a write. A failed request reads as all-ones and is kept in
/// [`Self::error`]; the mailbox stays poisoned after a timeout.
pub struct Proxy<'t, A> {
    smn: A,
    mailbox: core::cell::RefCell<Mailbox>,
    timer: &'t dyn Timer,
    error: core::cell::Cell<Option<mailbox::Error>>,
}

impl<'t, A: SmnAccess> Proxy<'t, A> {
    /// # Safety
    /// `smn` reaches one IOD whose MPIO mailbox the caller owns exclusively
    /// (BSP, no [`Session`] on it: before the training session, as the
    /// early link's LTSSM read, or after it), and its firmware is idle.
    pub unsafe fn new(smn: A, poll: Poll, timer: &'t dyn Timer) -> Self {
        Self {
            smn,
            // SAFETY: caller's contract.
            mailbox: core::cell::RefCell::new(unsafe { mailbox(poll) }),
            timer,
            error: core::cell::Cell::new(None),
        }
    }

    /// Request `msg` with `args` on this IOD's mailbox; the first failure
    /// is kept ([`Self::error`]).
    ///
    /// # Safety
    /// An address in `args` the firmware dereferences is physically
    /// addressable, coherent and reserved until reset (a timeout does not
    /// release it; [`Mailbox::request`]).
    pub unsafe fn request(&self, msg: u16, args: [u32; 6]) -> Result<[u32; 6], mailbox::Error> {
        // SAFETY: the caller's contract.
        let reply = unsafe {
            self.mailbox
                .borrow_mut()
                .request(&self.smn, msg, args, self.timer)
        };
        if let Err(error) = reply
            && self.error.get().is_none()
        {
            self.error.set(Some(error));
        }
        reply
    }

    /// A request without addresses.
    fn send(&self, msg: u16, args: [u32; 6]) -> Result<[u32; 6], mailbox::Error> {
        // SAFETY: no argument is an address the firmware dereferences.
        unsafe { self.request(msg, args) }
    }

    /// `(old & !clear) | set` at `address` by the firmware.
    pub fn rmw(
        &self,
        address: u32,
        clear: u32,
        set: u32,
        width: Width,
    ) -> Result<(), mailbox::Error> {
        self.send(MSG_PCIE_PROXY, proxy_rmw(address, clear, set, width))
            .map(drop)
    }

    pub fn read(&self, address: u32, width: Width) -> Result<u32, mailbox::Error> {
        self.send(MSG_PCIE_PROXY, proxy_read(address, width))
            .map(|reply| reply[0])
    }

    pub fn write(&self, address: u32, value: u32, width: Width) -> Result<(), mailbox::Error> {
        self.rmw(address, u32::MAX, value, width)
    }

    /// The 32-bit register `reg` through [`MSG_PCIE_PROXY`].
    pub fn reg<R: tock_registers::RegisterLongName>(
        &self,
        reg: SmnReg<R>,
    ) -> ProxyRegister<'_, 't, A, R> {
        ProxyRegister {
            proxy: self,
            address: reg.address(),
            _reg: core::marker::PhantomData,
        }
    }

    /// `MPIO_ENABLE_CG` with its three arguments (AmdNbioDxe: bit 20 and bit
    /// 14 of the SMU feature PCD 0xFE, then the unused-tile clock gating PCD
    /// 0x367 unless a HOB vetoes it).
    pub fn enable_clock_gating(&self, args: [bool; 3]) -> Result<(), mailbox::Error> {
        let [a, b, c] = args.map(u32::from);
        self.send(MSG_ENABLE_CG, [a, b, c, 0, 0, 0]).map(drop)
    }

    /// The first failed request, if any.
    pub fn error(&self) -> Option<mailbox::Error> {
        self.error.get()
    }
}

/// A register behind [`Proxy`].
pub struct ProxyRegister<'a, 't, A, R: tock_registers::RegisterLongName> {
    proxy: &'a Proxy<'t, A>,
    address: u32,
    _reg: core::marker::PhantomData<R>,
}

impl<A: SmnAccess, R: tock_registers::RegisterLongName> ProxyRegister<'_, '_, A, R> {
    /// One firmware read-modify-write of the fields in `value` (the vendor's
    /// `{address, ~and, or}` request), unlike `ReadWriteable::modify`.
    pub fn modify(
        &self,
        value: tock_registers::fields::FieldValue<u32, R>,
    ) -> Result<(), mailbox::Error> {
        self.proxy
            .rmw(self.address, value.mask(), value.value, Width::Dword)
    }
}

impl<A: SmnAccess, R: tock_registers::RegisterLongName> tock_registers::interfaces::Readable
    for ProxyRegister<'_, '_, A, R>
{
    type T = u32;
    type R = R;
    fn get(&self) -> u32 {
        self.proxy
            .read(self.address, Width::Dword)
            .unwrap_or(u32::MAX)
    }
}

impl<A: SmnAccess, R: tock_registers::RegisterLongName> tock_registers::interfaces::Writeable
    for ProxyRegister<'_, '_, A, R>
{
    type T = u32;
    type R = R;
    fn set(&self, value: u32) {
        let _ = self.proxy.write(self.address, value, Width::Dword);
    }
}

/// Phase 4's barriers (Dell `MpioCommonInitPhase4` `0xf5f7b0`): `0x304` to
/// every instance with ASKs, then `0x3A` to every instance, those without
/// ASKs (`idle`, their IOD's SMN) included, before the caller fetches any
/// result. The tuple keeps the vendor's instance key.
pub fn train_all<A: SmnAccess>(
    sessions: &mut [(u8, Session<A>)],
    idle: &[A],
    poll: Poll,
    timer: &dyn Timer,
) -> Result<(), Error> {
    for (_, session) in sessions.iter_mut() {
        session.start_train(timer)?;
    }
    for (_, session) in sessions.iter_mut() {
        session.wait_train(timer)?;
    }
    for smn in idle {
        // SAFETY: the caller owns these IODs' mailboxes; no DMA is involved.
        let mut mailbox = unsafe { mailbox(poll) };
        // SAFETY: as above.
        unsafe { mailbox.request(smn, msg::WAIT_COMPLETE, [0; 6], timer) }?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::{SmnModel, SmnTest};
    use std::vec::Vec as StdVec;

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
        doorbells: StdVec<(u32, [u32; 6])>,
        dma_base: u64,
        dma_ptr: *mut u8,
        trained_status: u32,
        phase_trace: Option<PhaseTrace>,
        reverse_get_ask: bool,
        saved_asks: StdVec<[u8; ASK_BYTES]>,
    }

    impl MockWhMpio {
        fn new(dma_base: u64, dma_ptr: *mut u8, trace: Option<PhaseTrace>) -> Self {
            Self {
                ctrl: 1 << 31,
                args: [0; 6],
                doorbells: StdVec::new(),
                dma_base,
                dma_ptr,
                trained_status: 6,
                phase_trace: trace,
                reverse_get_ask: false,
                saved_asks: StdVec::new(),
            }
        }
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
                self.doorbells.push((cmd, self.args));
                if let Some(trace) = &self.phase_trace
                    && cmd != 1
                {
                    trace.borrow_mut().push((self.dma_base, cmd));
                }
                match cmd {
                    0x01 => self.args[0] = 0,
                    0x07 => {
                        let count = self.args[4] as usize;
                        assert_eq!(self.args[1], self.dma_base as u32);
                        assert_eq!(self.args[2], 3);
                        self.saved_asks.clear();
                        for i in 0..count {
                            let mut buf = [0u8; ASK_BYTES];
                            // SAFETY: test DMA buffer.
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
                        assert_eq!(self.args[4] as usize, self.saved_asks.len());
                        let mut out = self.saved_asks.clone();
                        for (i, buf) in out.iter_mut().enumerate() {
                            let status = (self.trained_status & 0xf) | (((i as u32) & 0x7) << 16);
                            buf[0x20..0x24].copy_from_slice(&status.to_le_bytes());
                        }
                        if self.reverse_get_ask {
                            out.reverse();
                        }
                        for (i, buf) in out.iter().enumerate() {
                            // SAFETY: test DMA buffer.
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    buf.as_ptr(),
                                    self.dma_ptr.add(i * ASK_BYTES),
                                    ASK_BYTES,
                                );
                            }
                        }
                    }
                    0x0e => {
                        let at = (self.args[2] as u64 - self.dma_base) as usize;
                        // SAFETY: test DMA buffer.
                        unsafe {
                            *self.dma_ptr.add(at + 0x52) = 1;
                            *self.dma_ptr.add(at + 0x53) = 2;
                        }
                    }
                    _ => {}
                }
                self.ctrl = (1u32 << 31) | 1;
            }
        }
    }

    unsafe fn noop_sync(_: *const u8, _: usize) {}

    #[repr(C, align(64))]
    struct Buffer([u8; DMA_SIZE]);

    fn session<'a>(
        model: &'a SmnTest<MockWhMpio>,
        buffer: &mut Buffer,
        prepared: Prepared,
    ) -> Session<crate::smn::Smn<&'a SmnTest<MockWhMpio>>> {
        let dma =
            unsafe { Dma::new(buffer.0.as_mut_ptr(), DMA_SIZE, model.model().dma_base) }.unwrap();
        unsafe {
            Session::new(
                model.smn(),
                dma,
                prepared,
                noop_sync,
                Poll::default(),
                NonZeroU32::MIN,
            )
        }
        .unwrap()
    }

    #[test]
    fn every_instance_starts_and_waits_before_any_result_is_read() {
        let mut buffers = [Buffer([0; DMA_SIZE]), Buffer([0; DMA_SIZE])];
        let ports = [PortDesc::pcie(0, 15), PortDesc::pcie(64, 79)];
        let trace = std::rc::Rc::new(std::cell::RefCell::new(StdVec::new()));
        let models = core::array::from_fn::<_, 3, _>(|i| {
            let ptr = buffers[i.min(1)].0.as_mut_ptr();
            SmnTest::new(MockWhMpio::new(
                (i as u64 + 1) * 0x100_0000,
                ptr,
                Some(trace.clone()),
            ))
        });
        let mut sessions = StdVec::new();
        for iod in 0..2u8 {
            let (prepared, _) = Prepared::for_iod(
                Layout::Sp7B0,
                0,
                iod,
                &ports,
                &GlobalConfig::default(),
                false,
            )
            .unwrap()
            .unwrap();
            let mut s = session(
                &models[usize::from(iod)],
                &mut buffers[usize::from(iod)],
                prepared,
            );
            s.submit(&FakeTimer).unwrap();
            for _ in 0..3 {
                s.advance(&FakeTimer).unwrap();
            }
            sessions.push((iod, s));
        }
        trace.borrow_mut().clear();
        // Socket 1's IOD 0 has no port: it gets WAIT only.
        train_all(
            &mut sessions,
            &[models[2].smn()],
            Poll::default(),
            &FakeTimer,
        )
        .unwrap();
        for (_, s) in &mut sessions {
            s.collect_train(&FakeTimer).unwrap();
            s.update(&[None], false, &FakeTimer).unwrap();
        }
        let [a, b, c] = models.each_ref().map(|m| m.model().dma_base);
        assert_eq!(
            trace.borrow().as_slice(),
            &[
                (a, 0x304),
                (b, 0x304),
                (a, 0x3a),
                (b, 0x3a),
                (c, 0x3a),
                (a, 3),
                (a, 7),
                (b, 3),
                (b, 7)
            ]
        );
    }

    #[test]
    fn md25_ports_split_by_iod_with_the_bmc_on_iod_1() {
        let ports = [
            PortDesc::pcie(0, 15),
            PortDesc::pcie(16, 31),
            PortDesc::pcie(32, 47),
            PortDesc::pcie(64, 79),
            PortDesc::pcie(80, 95),
            PortDesc::pcie(96, 111),
            PortDesc::pcie(129, 129),
            PortDesc::pcie(130, 131),
            PortDesc::pcie(132, 133),
            PortDesc::pcie(134, 135),
        ];
        let g = GlobalConfig::default();
        let (_, idx0) = Prepared::for_iod(Layout::Sp7B0, 0, 0, &ports, &g, false)
            .unwrap()
            .unwrap();
        let (_, idx1) = Prepared::for_iod(Layout::Sp7B0, 0, 1, &ports, &g, false)
            .unwrap()
            .unwrap();
        assert_eq!(idx0.as_slice(), &[0, 1, 2, 8, 9]);
        assert_eq!(idx1.as_slice(), &[3, 4, 5, 6, 7]);
        // An unused engine sends no ASK; overlapping lanes are refused.
        let mut lan_off = ports;
        lan_off[7].engine = port::Engine::Unused;
        let (_, idx1) = Prepared::for_iod(Layout::Sp7B0, 0, 1, &lan_off, &g, false)
            .unwrap()
            .unwrap();
        assert_eq!(idx1.as_slice(), &[3, 4, 5, 6]);
        let overlap = [PortDesc::pcie(0, 7), PortDesc::pcie(4, 7)];
        assert_eq!(
            Prepared::for_iod(Layout::Sp7B0, 0, 0, &overlap, &g, false).err(),
            Some(Error::Overlap)
        );
    }

    #[test]
    fn full_session_sends_zero_setup_args_binds_reordered_asks_and_gates_update_link() {
        let mut backing = Buffer([0u8; DMA_SIZE]);
        let model = SmnTest::new(MockWhMpio {
            reverse_get_ask: true,
            ..MockWhMpio::new(0x1000_0000, backing.0.as_mut_ptr(), None)
        });
        let anc = [port::AncOverride::Speed(0xabcd)];
        let ports = [
            PortDesc {
                early_train: true,
                ..PortDesc::pcie(16, 31).devfn(3, 2)
            },
            PortDesc::pcie(132, 133).with_ancillary(&anc),
        ];
        let (prepared, _) =
            Prepared::for_iod(Layout::Sp7B0, 1, 0, &ports, &GlobalConfig::default(), true)
                .unwrap()
                .unwrap();
        assert!(prepared.early_train());
        let mut sess = session(&model, &mut backing, prepared);
        sess.submit(&FakeTimer).unwrap();
        sess.advance(&FakeTimer).unwrap(); // map

        let mapped = bind(Layout::Sp7B0, 0, &ports, sess.asks().unwrap()).unwrap();
        let main = mapped.iter().find(|b| b.port == 0).unwrap();
        let bonus = mapped.iter().find(|b| b.port == 1).unwrap();
        assert_eq!((main.rb_on_iod, main.core, main.devfn), (0, 1, 0x1a));
        assert_eq!(main.logical_bridge, 9);
        assert_eq!(
            (bonus.core, bonus.pcie_bridge_id),
            (6, 16 + bonus.local_port)
        );
        sess.write_strap(1, 0x75, 7, straps::Space::Port, &FakeTimer)
            .unwrap();
        sess.advance(&FakeTimer).unwrap(); // reconfigure
        sess.proxy_rmw(0x1c44_0288, 1, 1, &FakeTimer).unwrap();
        sess.advance(&FakeTimer).unwrap(); // pre-train
        sess.early_train(&FakeTimer).unwrap();
        sess.start_train(&FakeTimer).unwrap();
        assert!(sess.collect_train(&FakeTimer).is_err());
        sess.wait_train(&FakeTimer).unwrap();
        sess.collect_train(&FakeTimer).unwrap();
        let trained = bind(Layout::Sp7B0, 0, &ports, sess.asks().unwrap()).unwrap();
        let mut ids = [None; 2];
        for b in &trained {
            ids[b.ask] = Some(b.logical_bridge);
        }
        sess.update(&ids, false, &FakeTimer).unwrap();
        let deli = sess.deli_info(66, &FakeTimer).unwrap();
        assert!(deli.valid && deli.port == 2);
        sess.release_port(3, 0, 1, &FakeTimer).unwrap();
        assert_eq!(sess.state(), State::Updated);

        let sent: StdVec<(u32, [u32; 6])> = model
            .model()
            .doorbells
            .iter()
            .copied()
            .filter(|&(cmd, _)| cmd != 1)
            .collect();
        let cmds: StdVec<u32> = sent.iter().map(|&(c, _)| c).collect();
        assert_eq!(
            cmds,
            [
                0x02, 0x08, 0x07, 0x304, 0x03, 0x0c, 0x304, 0x03, 0x26, 0x304, 0x03, 0x304, 0x03,
                0x304, 0x3a, 0x03, 0x07, 0x0e, 0x2a
            ]
        );
        assert_eq!(sent[0].1, [0xc0, 0, 0, 3 << 22, 0, 0]);
        for &(cmd, args) in &sent {
            if cmd == 0x304 {
                assert!(args == [0; 6] || args == [0, 1, 0, 0, 0, 0]);
            }
        }
        assert_eq!(sent[11].1, [0, 1, 0, 0, 0, 0]);
        assert_eq!(sent[5].1[..3], [0x0001_0075, 7, 0]);
        assert_eq!(sent[18].1[0], 0x0203_0001);
        // Ancillary stream: header ID = socket 1, type 7 at socket lane 132.
        let mut anc_bytes = [0u8; 12];
        backing_read(&model, &mut anc_bytes);
        assert_eq!(anc_bytes[1], 1);
        assert_eq!(&anc_bytes[4..8], &[7, 0, 2, 132]);
    }

    fn backing_read(model: &SmnTest<MockWhMpio>, out: &mut [u8]) {
        // SAFETY: the test buffer outlives the model.
        unsafe {
            core::ptr::copy_nonoverlapping(
                model.model().dma_ptr.add(ANCILLARY_OFFSET),
                out.as_mut_ptr(),
                out.len(),
            )
        };
    }

    #[test]
    fn update_link_follows_only_with_ubm_hotplug() {
        let mut backing = Buffer([0u8; DMA_SIZE]);
        let model = SmnTest::new(MockWhMpio::new(0x2000_0000, backing.0.as_mut_ptr(), None));
        let ports = [PortDesc::pcie(0, 15)];
        let (prepared, _) =
            Prepared::for_iod(Layout::Sp7B0, 0, 0, &ports, &GlobalConfig::default(), false)
                .unwrap()
                .unwrap();
        let mut s = session(&model, &mut backing, prepared);
        s.submit(&FakeTimer).unwrap();
        for _ in 0..3 {
            s.advance(&FakeTimer).unwrap();
        }
        s.start_train(&FakeTimer).unwrap();
        s.wait_train(&FakeTimer).unwrap();
        s.collect_train(&FakeTimer).unwrap();
        s.update(&[Some(8)], true, &FakeTimer).unwrap();
        let last = model.model().doorbells.last().unwrap().0;
        assert_eq!(last, 0x322);
    }
}

/// An MPIO firmware for [`Proxy`] tests on an [`crate::smn::testing::SmnSpace`]
/// (`on_write`): answers [`MSG_PCIE_PROXY`] on the same space and keeps the
/// arguments of every other request.
#[cfg(test)]
pub(crate) fn proxy_firmware(space: &crate::smn::testing::SmnSpace, address: u32, _: u32) {
    use crate::dxio::rs::mailbox::{ARGUMENTS, CONTROL};
    if address != DOORBELL_V2.address() {
        return;
    }
    let args: [u32; 6] = core::array::from_fn(|i| space.get(ARGUMENTS.offset(4 * i as u32)));
    if (space.get(CONTROL) >> 8) & 0xfff == u32::from(MSG_PCIE_PROXY) {
        let old = space.get(args[0]);
        if args[1] == 0 && args[2] == 0 {
            space.set(ARGUMENTS, old);
        } else {
            space.set(args[0], (old & !args[1]) | args[2]);
        }
    }
    space.set(CONTROL, 0x8000_0001);
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use crate::dxio::rs::mailbox::{ARGUMENTS, CONTROL};
    use crate::smn::testing::SmnSpace;
    use fstart_core::services::timer::ManualTimer;
    use tock_registers::interfaces::{Readable, Writeable};

    register_bitfields![u32, Strap [
        AER OFFSET(5) NUMBITS(1) [],
        OTHER OFFSET(0) NUMBITS(4) []
    ]];

    #[test]
    fn proxy_requests_carry_address_clear_set_width_and_rmw_is_one_request() {
        let mut space = SmnSpace::default();
        space.on_write = Some(proxy_firmware);
        space.set(CONTROL, 1 << 31);
        let timer = ManualTimer::new();
        let proxy = unsafe { Proxy::new(space.smn(), Poll::default(), &timer) };
        let reg = SmnReg::<Strap::Register>::new(0x1a38_02c0 + (2 << 20));
        space.set(reg, 0x0000_0003);
        proxy.reg(reg).modify(Strap::AER::SET).unwrap();
        assert_eq!(space.get(reg), 0x23);
        let args: [u32; 4] = core::array::from_fn(|i| space.get(ARGUMENTS.offset(4 * i as u32)));
        assert_eq!(args, [reg.address(), 1 << 5, 1 << 5, 0]);
        assert_eq!(space.get(CONTROL), 0x8000_0001);
        assert_eq!(proxy.reg(reg).get(), 0x23);
        proxy.reg(reg).set(0x10);
        assert_eq!(space.get(reg), 0x10);
        proxy.read(0x1c40_005a, Width::Word).unwrap();
        assert_eq!(space.get(ARGUMENTS.offset(12)), 2);
        proxy.enable_clock_gating([true, false, true]).unwrap();
        assert_eq!(space.get(CONTROL.address()), 0x8000_0001);
        let args: [u32; 3] = core::array::from_fn(|i| space.get(ARGUMENTS.offset(4 * i as u32)));
        assert_eq!(args, [1, 0, 1]);
        assert_eq!(proxy.error(), None);
    }

    #[test]
    fn a_dead_mailbox_reads_all_ones_and_keeps_the_error() {
        let space = SmnSpace::default();
        let timer = ManualTimer::new();
        let poll = Poll {
            reads: NonZeroU32::new(2).unwrap(),
            interval_us: NonZeroU32::new(1).unwrap(),
        };
        let proxy = unsafe { Proxy::new(space.smn(), poll, &timer) };
        assert_eq!(proxy.reg(SmnReg::<()>::new(0x1a34_0014)).get(), u32::MAX);
        assert_eq!(proxy.error(), Some(mailbox::Error::ReadyTimeout));
    }
}
