// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only discovery of a single-socket Rs SoC: the PSP's APOB (CPU
//! topology, memory map) and the live IOS-to-bus bindings. The SoC selects
//! which physical IOS exist (its fuse layout); everything else is shared.
use fstart_driver_amd::{
    apob::{
        Apob, ApobError,
        ccx::CoreMap,
        rs::{self, Rs},
        sysmap::{self, MAX_REGIONS, Region},
    },
    df::v4::{self, Ios, Root},
};
use fstart_pci::ecam::GlobalEcam;
use tock_registers::interfaces::Readable;

/// The PSP RAM/MMIO spans. NOT a final OS memory map: low memory, firmware,
/// APOB, TSEG and device reservations still need the platform's complete
/// resource/ownership plan before any payload handoff.
pub type Regions = heapless::Vec<Region, MAX_REGIONS>;

pub struct Discovered<'a> {
    pub apob: Apob<'a, Rs>,
    pub topology: CoreMap<12>,
    pub regions: Regions,
}

/// Parse the PSP's APOB in its reserved aperture: the CPU topology (at most
/// `max_threads`) and the memory map.
pub fn apob(bytes: &[u8], max_threads: usize) -> Result<Discovered<'_>, ()> {
    let apob = Apob::new(bytes, Rs).map_err(|error| match error {
        ApobError::Signature => fstart_log::error!(
            "rs: APOB signature bytes={:#x},{:#x},{:#x},{:#x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3]
        ),
        ApobError::Version(version) => {
            fstart_log::error!("rs: APOB unsupported version={:#x}", version)
        }
        ApobError::Size { size, first } => fstart_log::error!(
            "rs: APOB size={:#x} first={:#x} aperture={:#x}",
            size,
            first,
            bytes.len()
        ),
        ApobError::Entry { offset } => {
            fstart_log::error!("rs: APOB malformed entry at {:#x}", offset)
        }
        _ => fstart_log::error!("rs: invalid bounded Rs APOB"),
    })?;
    fstart_log::info!(
        "rs: APOB version={:#x} size={:#x} records={}",
        apob.version(),
        apob.size(),
        apob.entries().count()
    );
    for entry in apob.entries().take(128).flatten() {
        fstart_log::info!(
            "rs: APOB {}/{} instance={:#x} bytes={:#x}",
            entry.group(),
            entry.kind(),
            entry.instance(),
            entry.bytes().len()
        );
    }
    let memory = sysmap::system_memory_map(&apob).map_err(|_| {
        fstart_log::error!("rs: invalid Rs APOB memory9/9 record");
    })?;
    let regions = sysmap::memory_map(Rs, memory.top, memory.holes()).map_err(|_| {
        fstart_log::error!("rs: invalid PSP memory holes");
    })?;
    fstart_log::info!(
        "rs: PSP memory top={:#x} holes={} regions={} (not a final OS map)",
        memory.top,
        memory.holes().count(),
        regions.len()
    );
    for hole in memory.holes() {
        fstart_log::info!(
            "rs: PSP hole base={:#x} size={:#x} type={}",
            hole.base,
            hole.size,
            hole.kind
        );
    }
    let topology = rs::core_map(&apob, 0).map_err(|_| {
        fstart_log::error!("rs: invalid Rs APOB CPU3/3 record");
    })?;
    if topology.threads() == 0 || topology.threads() > max_threads {
        fstart_log::error!("rs: APOB topology exceeds the platform's CPU bound");
        return Err(());
    }
    fstart_log::info!(
        "rs: APOB CPU enabled-threads={} populated-CCDs={}",
        topology.threads(),
        topology.ccds.len()
    );
    for ccd in &topology.ccds {
        for complex in &ccd.ccxs {
            fstart_log::info!(
                "rs: CCD logical={} physical={} complex logical={} physical={} cores={}",
                ccd.logical,
                ccd.id,
                complex.logical,
                complex.id,
                complex.cores.len()
            );
        }
    }
    Ok(Discovered {
        apob,
        topology,
        regions,
    })
}

/// Bind the SoC's present physical IOS to the live DF4 bus routes, without
/// installing a map or using DF2/3's incompatible indirect pair. The FCH's
/// root (`PRIMARY_IOS`) must be among them.
pub fn fabric(ios: &[Ios]) -> Result<heapless::Vec<Root, 4>, ()> {
    // SAFETY: BSP owns config index/data; APs are not released yet. Only
    // broadcast DF reads.
    let df = unsafe { v4::Df::new(GlobalEcam) };
    for root in ios {
        fstart_log::info!(
            "rs: IOS physical={} FID={:#x} instance={:#x} primary={}",
            root.physical_index,
            root.fabric_id,
            root.instance,
            root.physical_index == v4::PRIMARY_IOS
        );
    }
    let mut routes = heapless::Vec::<v4::BusRoute, 8>::new();
    for pair in 0..v4::CONFIG_PAIRS {
        let base = df.reg(v4::CONFIG_BASE.at(pair, 8)).get();
        let limit = df.reg(v4::CONFIG_LIMIT.at(pair, 8)).get();
        fstart_log::info!(
            "rs: DF4 config pair={} base={:#x} limit={:#x}",
            pair,
            base,
            limit
        );
        match v4::BusRoute::decode(base, limit) {
            Ok(Some(route)) => routes.push(route).map_err(|_| ())?,
            Ok(None) => {}
            Err(_) => {
                fstart_log::error!("rs: invalid DF4 bus route");
                return Err(());
            }
        }
    }
    let roots = v4::bind_routes(0, ios, &routes).map_err(|_| {
        fstart_log::error!("rs: incomplete/ambiguous live IOS-to-bus binding");
    })?;
    if !roots
        .iter()
        .any(|r| r.ios.physical_index == v4::PRIMARY_IOS)
    {
        fstart_log::error!("rs: the FCH's IOS has no bus route");
        return Err(());
    }
    for root in &roots {
        fstart_log::info!(
            "rs: IOS={} FID={:#x} segment={} buses={:#x}..{:#x}",
            root.ios.physical_index,
            root.ios.fabric_id,
            root.route.segment,
            root.route.base,
            root.route.limit
        );
    }
    Ok(roots)
}
