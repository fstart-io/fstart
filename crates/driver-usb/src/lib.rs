// SPDX-License-Identifier: MIT OR Apache-2.0
//! Firmware xHCI integration using CrabUSB, not another controller driver.
//! Polling, one cold snapshot, bounded waits and read-only BOT LUN0 access.
//! Controllers, DMA and timed-out futures are retained until platform reset:
//! timeout is NOT cancellation, and no failed controller may be reused.
#![no_std]
extern crate alloc;
mod bot;
#[cfg(test)]
mod poll_tests;
#[cfg(feature = "single-executor")]
mod single_executor;
use alloc::{boxed::Box, vec, vec::Vec};
#[cfg(feature = "single-executor")]
pub use ax_sync::SpinLock;
use core::{
    fmt::Write,
    future::Future,
    num::NonZeroUsize,
    ptr::NonNull,
    task::{Context, Poll, Waker},
};
pub use crab_usb::KernelOp;
use crab_usb::{
    Event, EventHandler, USBHost,
    usb_if::{descriptor::EndpointType, transfer::Direction},
};
pub use dma_api;
use dma_api::DmaCoherency;
use fstart_core::services::Timer;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Error {
    Mmio = 1,
    Unsupported,
    Transport,
    Timeout,
    Controller,
    Policy,
    Protocol,
    Scsi,
    Poisoned,
    Consumed,
}
pub(crate) fn transport_error(error: impl core::fmt::Debug) -> Error {
    let mut text = heapless::String::<256>::new();
    let _ = write!(&mut text, "{error:?}");
    fstart_log::error!("usb: transport error {}", text.as_str());
    Error::Transport
}
#[derive(Clone)]
struct BarMapping {
    base: usize,
    size: usize,
}
impl xhci::accessor::Mapper for BarMapping {
    unsafe fn map(&mut self, address: usize, size: usize) -> NonZeroUsize {
        assert!(
            address >= self.base
                && address
                    .checked_add(size)
                    .is_some_and(|end| end <= self.base + self.size)
        );
        NonZeroUsize::new(address).unwrap()
    }
    fn unmap(&mut self, _: usize, _: usize) {} // Platform owns the permanent UC mapping.
}
#[derive(Clone, Copy)]
pub struct Capabilities {
    pub version: u16,
    pub ports: u8,
    pub slots: u8,
    pub scratchpads: u32,
}
/// # Safety
/// Exclusive access to an identity-mapped, uncached, valid xHCI BAR. No
/// controller access may overlap this read-only capability validation.
unsafe fn capabilities(base: usize, size: usize) -> Result<Capabilities, Error> {
    if base == 0 || base & 0xfff != 0 || size < 0x1000 || base.checked_add(size).is_none() {
        return Err(Error::Mmio);
    }
    let mapper = BarMapping { base, size };
    // SAFETY: checked fixed capability register footprint; mapper rejects an
    // out-of-BAR access. Reuse the xHCI library's register/bitfield definitions.
    let r = unsafe { xhci::registers::Capability::new(base, &mapper) };
    let version = r.hciversion.read_volatile().get();
    let length = usize::from(r.caplength.read_volatile().get());
    let hcs = r.hcsparams1.read_volatile();
    let ports = hcs.number_of_ports();
    let slots = hcs.number_of_device_slots();
    let scratchpads = r.hcsparams2.read_volatile().max_scratchpad_buffers();
    if !(0x100..=0x120).contains(&version)
        || length < 0x20
        || length & 3 != 0
        || ports == 0
        || ports > 32
        || slots == 0
        || hcs.number_of_interrupts() == 0
        || scratchpads > 512
    {
        return Err(Error::Unsupported);
    }
    let operational_end = length + 0x400 + usize::from(ports) * 16;
    let doorbell = r.dboff.read_volatile().get() as usize;
    let runtime = r.rtsoff.read_volatile().get() as usize;
    if operational_end > size
        || doorbell < operational_end
        || doorbell
            .checked_add((usize::from(slots) + 1) * 4)
            .is_none_or(|e| e > size)
        || runtime < operational_end
        || runtime
            .checked_add(0x20 + usize::from(hcs.number_of_interrupts()) * 32)
            .is_none_or(|e| e > size)
    {
        return Err(Error::Mmio);
    }
    let mut extended = usize::from(
        r.hccparams1
            .read_volatile()
            .xhci_extended_capabilities_pointer(),
    ) * 4;
    for _ in 0..64 {
        if extended == 0 {
            return Ok(Capabilities {
                version,
                ports,
                slots,
                scratchpads,
            });
        }
        if extended < 0x20 || extended.checked_add(8).is_none_or(|end| end > size) {
            return Err(Error::Mmio);
        }
        // SAFETY: bounded extended-capability header. Next is a relative
        // DWORD offset, not a PCI capability byte offset or absolute pointer.
        let header = unsafe { fstart_core::mmio::read32((base + extended) as *const u32) };
        let next = ((header >> 8) & 0xff) as usize;
        extended = if next == 0 {
            0
        } else {
            extended.checked_add(next * 4).ok_or(Error::Mmio)?
        };
    }
    Err(Error::Mmio)
}

fn run<F: Future>(
    future: F,
    events: Option<&EventHandler>,
    timer: &dyn Timer,
) -> Result<F::Output, Error> {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    let start = timer.timestamp_us();
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return Ok(value);
        }
        if timer.timestamp_us().wrapping_sub(start) >= 20_000_000 {
            // In-flight DMA and references are NOT cancelled by dropping a
            // future. Its frame and all driver allocations stay quarantined.
            core::mem::forget(future);
            return Err(Error::Timeout);
        }
        if events.is_some_and(|events| matches!(events.handle_event(), Event::Stopped)) {
            core::mem::forget(future);
            return Err(Error::Controller);
        }
        timer.delay_us(50);
    }
}
#[derive(Clone, Copy)]
pub struct DeviceId {
    pub id: usize,
    pub vendor: u16,
    pub product: u16,
    pub hub: bool,
}
pub struct MediaRead {
    pub device: DeviceId,
    pub blocks: u64,
    pub block_size: u32,
    pub sha256: [u8; 32],
    reader: ReadOnlyReader,
}
/// Permanent CPU buffer identity for platform DMA mapping caches. These
/// addresses are never freed/reallocated, even when the session fails/drops.
#[derive(Clone, Copy, Debug)]
pub struct RetainedIoRegion {
    pub cpu: usize,
    pub size: usize,
}
/// Retained BOT reader. Normal I/O is sequential and every buffer reuse follows
/// positive transfer completion. Any error/timeout poisons it, never replaying.
struct ReadOnlyReader {
    scsi: cotton_scsi::ScsiDevice<bot::Bot>,
    events: &'static EventHandler,
    timer: &'static dyn Timer,
    blocks: u64,
    block_size: u32,
    remaining: usize,
    poisoned: bool,
    regions: [RetainedIoRegion; 3],
}
impl MediaRead {
    pub fn retained_io_regions(&self) -> &[RetainedIoRegion; 3] {
        &self.reader.regions
    }
    /// Permit image-size reads once the platform registered mapping caches for
    /// exactly these permanent buffers. No buffer is reused before a positive
    /// completion; failed/timed-out sessions never submit again.
    /// # Safety
    /// All three regions must have retained, exclusive reusable DMA mappings
    /// for this device, with no aliasing to another session or caller storage.
    pub unsafe fn enable_retained_io_reuse(&mut self) {
        self.reader.remaining = usize::MAX;
    }
    /// Read whole logical blocks, never beyond the original measured capacity.
    /// Transport/phase failures poison this retained session; no replay.
    pub fn read_blocks(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), Error> {
        let reader = &mut self.reader;
        if reader.poisoned {
            return Err(Error::Poisoned);
        }
        read_extent(
            reader.blocks,
            reader.block_size,
            lba,
            buffer.len(),
            reader.remaining,
        )?;
        reader.remaining -= buffer.len();
        let mut next = lba;
        for chunk in buffer.chunks_mut(4096) {
            let count = chunk.len() / reader.block_size as usize;
            let result = run(
                async {
                    let actual = if next
                        .checked_add(count as u64)
                        .is_some_and(|end| end <= u64::from(u32::MAX) + 1)
                    {
                        reader.scsi.read_10(next as u32, count as u16, chunk).await
                    } else {
                        reader.scsi.read_16(next, count as u32, chunk).await
                    }
                    .map_err(scsi_error)?;
                    if actual != chunk.len() {
                        return Err(Error::Protocol);
                    }
                    Ok(())
                },
                Some(reader.events),
                reader.timer,
            );
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) | Err(e) => {
                    reader.poisoned = true;
                    return Err(e);
                }
            }
            next += count as u64;
        }
        Ok(())
    }
}
fn read_extent(
    blocks: u64,
    block_size: u32,
    lba: u64,
    bytes: usize,
    remaining: usize,
) -> Result<(), Error> {
    if !(512..=4096).contains(&block_size)
        || !block_size.is_power_of_two()
        || bytes == 0
        || !bytes.is_multiple_of(block_size as usize)
        || bytes > remaining
        || lba
            .checked_add((bytes / block_size as usize) as u64)
            .is_none_or(|end| end > blocks)
    {
        return Err(Error::Policy);
    }
    Ok(())
}
#[derive(Default)]
pub struct Report {
    pub devices: Vec<DeviceId>,
    pub media: Vec<MediaRead>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halt {
    Clean,
    NeedsReset,
}

/// One retained controller. DMA storage remains reserved through reset,
/// including after bounded quiescence; halt is not permission to free buffers.
pub struct Controller {
    host: &'static mut USBHost,
    events: &'static EventHandler,
    timer: &'static dyn Timer,
    consumed: bool,
    halt_attempted: bool,
    mapping: BarMapping,
    pub capabilities: Capabilities,
}
impl Controller {
    /// # Safety
    /// Own the enabled UC MMIO BAR and live device-scoped DMA capability for
    /// the entire boot. Controller must not already be running. DMA mappings
    /// must remain reserved even after failure; streaming buffers must use
    /// retained bounce storage so timeout cannot DMA into an expired borrow.
    /// With `single-executor`, all USB/DMA calls must stay on one BSP without
    /// preemption, IRQ/NMI/AP users or a competing scheduler/runtime provider.
    pub unsafe fn new_xhci<R: KernelOp + Timer>(
        base: usize,
        size: usize,
        coherency: DmaCoherency,
        runtime: &'static R,
    ) -> Result<Self, Error> {
        let capabilities = unsafe { capabilities(base, size) }?;
        let mut host = USBHost::new_xhci(
            NonNull::new(base as *mut u8).ok_or(Error::Mmio)?,
            coherency,
            runtime,
        )
        .map_err(transport_error)?;
        let events = Box::leak(Box::new(host.create_event_handler()));
        let host = Box::leak(Box::new(host));
        Ok(Self {
            host,
            events,
            timer: runtime,
            consumed: false,
            halt_attempted: false,
            mapping: BarMapping { base, size },
            capabilities,
        })
    }
    /// Stop all controller DMA before payload ownership transfer. Device IRQ
    /// delivery is disabled first, then Run/Stop is cleared and HCHalted is
    /// positively observed. A failed attempt is poisoned, never retried.
    pub fn quiesce(&mut self) -> Result<Halt, Error> {
        if !self.consumed || self.halt_attempted {
            return Err(Error::Consumed);
        }
        self.halt_attempted = true;
        self.host.disable_irq().map_err(transport_error)?;
        // SAFETY: BSP-exclusive validated BAR. No USB future is polled after
        // snapshot completion; this takes over register access permanently.
        let cap = unsafe { xhci::registers::Capability::new(self.mapping.base, &self.mapping) };
        let mut operational = unsafe {
            xhci::registers::Operational::new(
                self.mapping.base,
                cap.caplength.read_volatile(),
                &self.mapping,
            )
        };
        let initial = operational.usbsts.read_volatile();
        fstart_log::info!(
            "usb: halt preflight base={:#x} halted={} CNR={} HSE={} HCE={} running={}",
            self.mapping.base,
            initial.hc_halted(),
            initial.controller_not_ready(),
            initial.host_system_error(),
            initial.host_controller_error(),
            operational.usbcmd.read_volatile().run_stop()
        );
        if initial.controller_not_ready()
            || initial.host_controller_error()
            || initial.host_system_error()
        {
            return Err(Error::Controller);
        }
        operational.usbcmd.update_volatile(|command| {
            command.clear_interrupter_enable();
            command.clear_run_stop();
        });
        for _ in 0..1000 {
            let status = operational.usbsts.read_volatile();
            // HCHalted is the architectural drain/completion indication
            // (also Linux xhci_halt's criterion). A NEW HCE on a completed
            // stop means reset-required, not DMA-active or reusable. Never
            // accept an error before stopping, CNR, HSE, or HCE while active.
            if status.host_system_error()
                || status.controller_not_ready()
                || (status.host_controller_error() && !status.hc_halted())
            {
                fstart_log::error!(
                    "usb: halt rejected base={:#x} halted={} CNR={} HSE={} HCE={}",
                    self.mapping.base,
                    status.hc_halted(),
                    status.controller_not_ready(),
                    status.host_system_error(),
                    status.host_controller_error()
                );
                return Err(Error::Controller);
            }
            if status.hc_halted() && !operational.usbcmd.read_volatile().run_stop() {
                let outcome = if status.host_controller_error() {
                    Halt::NeedsReset
                } else {
                    Halt::Clean
                };
                fstart_log::info!(
                    "usb: xHCI quiesced; HCHalted=1 Run/Stop=0 IRQ=off reset-required={}; buffers retained",
                    outcome == Halt::NeedsReset
                );
                return Ok(outcome);
            }
            self.timer.delay_us(100);
        }
        Err(Error::Timeout)
    }
    pub fn read_only_snapshot(&mut self) -> Result<Report, Error> {
        if self.consumed {
            return Err(Error::Consumed);
        }
        self.consumed = true;
        // No event-register accesses while reset/CNR is active. Initial root
        // hub setup needs no command/transfer completions. Disable device IRQ
        // delivery afterwards; this firmware drives completions by polling.
        fstart_log::info!("usb: initializing xHCI host");
        self.snapshot()
    }
    fn snapshot(&mut self) -> Result<Report, Error> {
        run(self.host.init(), None, self.timer)?.map_err(transport_error)?;
        self.host.disable_irq().map_err(transport_error)?;
        fstart_log::info!("usb: xHCI initialized; probing descriptors");
        let mut report = Report::default();
        // Root-hub init starts asynchronous port resets; an immediately
        // empty change set is NOT an empty connector. Allow power/reset and
        // USB attach debounce to settle, then reconcile a bounded snapshot.
        for _ in 0..20 {
            self.timer.delay_us(100_000);
            let changes = run(self.host.probe_devices(), Some(self.events), self.timer)?
                .map_err(transport_error)?;
            if !changes.disconnected.is_empty() {
                return Err(Error::Controller);
            }
            if changes.connected.is_empty() {
                continue;
            }
            for info in changes.connected {
                if report.devices.len() >= 32 {
                    return Err(Error::Unsupported);
                }
                let identity = DeviceId {
                    id: info.id(),
                    vendor: info.vendor_id(),
                    product: info.product_id(),
                    hub: info.as_device_info().is_none(),
                };
                fstart_log::info!(
                    "usb: device={} VID={:#x} PID={:#x} hub={}",
                    identity.id,
                    identity.vendor,
                    identity.product,
                    identity.hub
                );
                report.devices.push(identity);
                let mut selection = None;
                for config in info.configurations() {
                    for interface in &config.interfaces {
                        for alt in &interface.alt_settings {
                            fstart_log::info!(
                                "usb: interface={} alternate={} class={:#x} subclass={:#x} protocol={:#x}",
                                alt.interface_number,
                                alt.alternate_setting,
                                alt.class,
                                alt.subclass,
                                alt.protocol
                            );
                            if alt.class != 8 || alt.subclass != 6 || alt.protocol != 0x50 {
                                continue;
                            }
                            let input = alt
                                .endpoints
                                .iter()
                                .filter(|e| {
                                    e.transfer_type == EndpointType::Bulk
                                        && e.direction == Direction::In
                                        && e.address & 15 != 0
                                })
                                .collect::<Vec<_>>();
                            let output = alt
                                .endpoints
                                .iter()
                                .filter(|e| {
                                    e.transfer_type == EndpointType::Bulk
                                        && e.direction == Direction::Out
                                        && e.address & 15 != 0
                                })
                                .collect::<Vec<_>>();
                            if input.len() == 1
                                && output.len() == 1
                                && alt.endpoints.len() == 2
                                && selection.is_none()
                            {
                                selection = Some((
                                    config.configuration_value,
                                    alt.interface_number,
                                    alt.alternate_setting,
                                    input[0].address,
                                    output[0].address,
                                ));
                            }
                        }
                    }
                }
                let Some((configuration, interface, alternate, input, output)) = selection else {
                    continue;
                };
                let info = info.into_device_info().ok_or(Error::Unsupported)?;
                let device = run(self.host.open_device(&info), Some(self.events), self.timer)?
                    .map_err(transport_error)?;
                let device = Box::leak(Box::new(device));
                let sample = run(
                    async {
                        device
                            .set_configuration(configuration)
                            .await
                            .map_err(transport_error)?;
                        let session = device
                            .claim_interface(interface, alternate)
                            .await
                            .map_err(transport_error)?;
                        let transport = bot::Bot::new(
                            session.endpoint(input).map_err(transport_error)?,
                            session.endpoint(output).map_err(transport_error)?,
                        );
                        let regions = transport.retained_regions();
                        let mut scsi = cotton_scsi::ScsiDevice::new(transport);
                        let mut ready = false;
                        for _ in 0..3 {
                            match scsi.test_unit_ready().await {
                                Ok(()) => {
                                    ready = true;
                                    break;
                                }
                                Err(cotton_scsi::Error::Scsi(
                                    cotton_scsi::scsi_transport::ScsiError::UnitAttention
                                    | cotton_scsi::scsi_transport::ScsiError::NotReady
                                    | cotton_scsi::scsi_transport::ScsiError::BecomingReady,
                                )) => self.timer.delay_us(100_000),
                                Err(e) => return Err(scsi_error(e)),
                            }
                        }
                        if !ready {
                            return Err(Error::Scsi);
                        }
                        fstart_log::info!("usb: BOT LUN0 ready; reading capacity");
                        let (last, size) = scsi.read_capacity_10().await.map_err(scsi_error)?;
                        let (last, size) = if last == u32::MAX {
                            scsi.read_capacity_16().await.map_err(scsi_error)?
                        } else {
                            (u64::from(last), size)
                        };
                        let blocks = last.checked_add(1).ok_or(Error::Protocol)?;
                        if !(512..=4096).contains(&size)
                            || !size.is_power_of_two()
                            || blocks.checked_mul(u64::from(size)).is_none()
                        {
                            return Err(Error::Unsupported);
                        }
                        let mut data = vec![0u8; size as usize];
                        fstart_log::info!(
                            "usb: blocks={:#x} block-size={}; read LBA0 only",
                            blocks,
                            size
                        );
                        if scsi.read_10(0, 1, &mut data).await.map_err(scsi_error)? != data.len() {
                            return Err(Error::Protocol);
                        }
                        Ok(MediaRead {
                            device: identity,
                            blocks,
                            block_size: size,
                            sha256: Sha256::digest(&data).into(),
                            reader: ReadOnlyReader {
                                scsi,
                                events: self.events,
                                timer: self.timer,
                                blocks,
                                block_size: size,
                                remaining: 2 * 1024 * 1024,
                                poisoned: false,
                                regions,
                            },
                        })
                    },
                    Some(self.events),
                    self.timer,
                )??;
                report.media.push(sample);
            }
        }
        Ok(report)
    }
}
fn scsi_error(error: cotton_scsi::Error<Error>) -> Error {
    match error {
        cotton_scsi::Error::Transport(e) => e,
        cotton_scsi::Error::ProtocolError => Error::Protocol,
        _ => Error::Scsi,
    }
}
