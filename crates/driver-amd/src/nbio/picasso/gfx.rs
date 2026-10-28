// SPDX-License-Identifier: MIT OR Apache-2.0
//! GPU memory routing and ATOM integrated system information for Raven/Picasso.
//! This prepares the device for a display driver; it does not set a video mode.
use crate::{
    apob::Apob,
    ccx::tsc::DelayNs,
    smn::{Smn, SmnReg, apply, copy, rmw, write},
    smu::v10,
};
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::Readable;
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U16, U32},
};

type Le16 = U16<LittleEndian>;
type Le32 = U32<LittleEndian>;
#[derive(Debug, Clone, Copy)]
pub enum Connector {
    Edp,
    Hdmi,
    DisplayPort,
}
#[derive(Debug, Clone, Copy)]
pub struct Display {
    pub connector: Connector,
    /// Zero-based ATOM AUX and HPD LUT indices.
    pub aux: u8,
    pub hotplug: u8,
}
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub displays: &'static [Display],
    pub hdmi2_disable: u8,
}
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct Header {
    size: Le16,
    format: u8,
    content: u8,
}
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct Path {
    tag: Le16,
    acpi: Le16,
    connector: Le16,
    aux: u8,
    hpd: u8,
    encoder: Le16,
    mapping: u8,
    invert: u8,
    caps: Le16,
    reserved: Le16,
}
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct External {
    header: Header,
    guid: [u8; 16],
    paths: [Path; 7],
    tail: [u8; 8],
}
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct DpTuning {
    selector: u8,
    version: u8,
    size: Le16,
    reserved: Le16,
    settings: [u8; 50],
}
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct SystemInfo {
    header: Header,
    misc: Le32,
    gpu_caps: Le32,
    system: Le32,
    cpu_caps: Le32,
    gpu_spread: Le16,
    gpu_spread_type: Le16,
    lvds_spread: [Le16; 2],
    hdmi_spread: [Le16; 2],
    dvi_spread: [Le16; 2],
    phy_override: Le16,
    lvds_misc: Le16,
    backlight_hz: Le16,
    memory_type: u8,
    channels: u8,
    timings: [u8; 12],
    external: External,
    hdmi_tuning: [u8; 36],
    dp: [DpTuning; 2],
    camera_retimers: [u8; 0xf8],
    dp_extra: [DpTuning; 3],
    reserved: [Le32; 66],
}
#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct FusionInfo {
    system: SystemInfo,
    powerplay: [Le32; 256],
}
const _: () = {
    assert!(size_of::<SystemInfo>() == 0x400);
    assert!(core::mem::offset_of!(SystemInfo, external) == 0x38);
    assert!(core::mem::offset_of!(SystemInfo, dp) == 0xe8);
    assert!(core::mem::offset_of!(SystemInfo, dp_extra) == 0x250);
    assert!(size_of::<FusionInfo>() == 0x800);
};

fn system_info(
    smn: &Smn<impl ConfigRegionAccess>,
    config: Config,
) -> Result<FusionInfo, &'static str> {
    if config.displays.len() > 4 || config.displays.iter().any(|d| d.aux >= 6 || d.hotplug >= 6) {
        return Err("GFX display configuration");
    }
    let mut table = FusionInfo::new_zeroed();
    let info = &mut table.system;
    info.header = Header {
        size: 0x400.into(),
        format: 1,
        content: 11,
    };
    info.gpu_spread = 375.into();
    info.backlight_hz = 512.into();
    info.timings[5] = 5;
    info.timings[6] = 5;
    info.external.header = Header {
        size: (size_of::<External>() as u16).into(),
        format: 1,
        content: 1,
    };
    for tuning in info.dp.iter_mut().chain(info.dp_extra.iter_mut()) {
        tuning.settings.fill(0xff);
    }
    let read = |address| smn.reg(SmnReg::<()>::new(address)).get();
    for channel in 0..2 {
        let base = 0x50000 + (channel << 20);
        if read(base + 0x20) != 0 || read(base + 0x24) != 0 {
            info.channels += 1;
            info.memory_type = match read(base + 0x100) & 7 {
                0 => 0x1a,
                1 => 0x18,
                4 => 0x1d,
                5 => 0x1e,
                _ => 0,
            };
        }
    }
    let skip = read(0x5d5c0) >> 30 == 2
        || (read(0x5d59c) >> 16 == 0xa6da
            && read(0x5d5a0) == 0x0d83066c
            && read(0x5d5a4) == 0xd3d95469
            && read(0x5d5a8) & 0x7ff == 0x428);
    for (index, display) in config.displays.iter().enumerate() {
        let ordinal = match display.connector {
            Connector::Edp => 1,
            Connector::DisplayPort => {
                1 + config.displays[..index]
                    .iter()
                    .filter(|d| matches!(d.connector, Connector::DisplayPort))
                    .count()
            }
            Connector::Hdmi => {
                1 + config
                    .displays
                    .iter()
                    .filter(|d| matches!(d.connector, Connector::DisplayPort))
                    .count()
                    + config.displays[..index]
                        .iter()
                        .filter(|d| matches!(d.connector, Connector::Hdmi))
                        .count()
            }
        };
        let path = &mut info.external.paths[index + usize::from(skip && index >= 2)];
        let (tag, acpi, connector) = match display.connector {
            Connector::Edp => (2, 0x110, 0x14),
            Connector::DisplayPort => (
                [8, 0x80, 0x200, 0x400][ordinal - 1],
                0x200 + ordinal as u16 * 0x10,
                0x13,
            ),
            Connector::Hdmi => (
                [8, 0x80, 0x200, 0x400][ordinal - 1],
                0x200 + ordinal as u16 * 0x10,
                0x0c,
            ),
        };
        path.tag = tag.into();
        path.acpi = acpi.into();
        path.connector = (0x3000 | connector | (ordinal as u16) << 8).into();
        path.aux = display.aux;
        path.hpd = display.hotplug;
        path.mapping = 0xe4;
        path.caps = (if index >= 2 {
            0x100
        } else if !matches!(display.connector, Connector::Edp)
            && config.hdmi2_disable & (1 << index) != 0
        {
            0x200
        } else {
            0
        })
        .into();
    }
    Ok(table)
}

/// Prepare UMA and publish the ATOM table at its end, then notify the SMU.
///
/// # Safety
/// BSP only, after all CPUs are initialized. The APOB UMA hole must remain
/// reserved through OS handoff and be identity-mapped writable DRAM. No
/// display driver or GPU DMA may be active yet.
pub unsafe fn initialize(
    smn: &Smn<impl ConfigRegionAccess>,
    apob: &Apob<'_>,
    config: Config,
    top_mem: u64,
    top_mem2: Option<u64>,
    delay: &mut impl DelayNs,
) -> Result<crate::apob::Hole, &'static str> {
    let map = crate::apob::sysmap::first_system_memory_map(apob)
        .ok()
        .and_then(|map| map.sorted().ok())
        .ok_or("GFX APOB")?;
    let uma = *map
        .holes()
        .iter()
        .find(|h| h.kind == 0 && h.size >= 0x800 && h.end() <= 1 << 32)
        .ok_or("GFX UMA")?;
    let table = system_info(smn, config)?;
    let address = (uma.end() - size_of::<FusionInfo>() as u64) as usize;
    // SAFETY: validated reserved UMA, exclusive BSP ownership as above.
    unsafe {
        core::ptr::copy_nonoverlapping(
            table.as_bytes().as_ptr(),
            address as *mut u8,
            size_of::<FusionInfo>(),
        )
    };
    // Publish the table before the SMU/GPU reads it.
    unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) };
    if config
        .displays
        .iter()
        .any(|d| matches!(d.connector, Connector::Edp))
    {
        apply(smn, [rmw(0x1750c, 0, 0x02000011)]);
    }
    let tom2 = top_mem2.unwrap_or(0);
    apply(
        smn,
        [
            write(0x1012378c, (uma.size >> 20) as u32),
            write(0x6a05c, (uma.base >> 24) as u32),
            write(0x6a0b4, ((uma.end() - 1) >> 24) as u32),
            rmw(0x6a07c, 2, 2),
            rmw(0x6a050, 0xff800000, top_mem as u32),
            rmw(0x6a054, 0xff800001, tom2 as u32 | 1),
            rmw(0x6a058, 0xfff, (tom2 >> 32) as u32),
            copy(0x6a040, 0x13b1005c),
            copy(0x6a044, 0x13b10060),
            copy(0x6a04c, 0x13b00084),
            rmw(
                0x6a048,
                1 << 23,
                smn.reg(SmnReg::<()>::new(0x13b0004c)).get(),
            ),
            rmw(0x13b0004c, 0, 1 << 4),
            rmw(0x3440, 6, 0),
            rmw(0xe370, 7, 3 | 16),
            rmw(0x10135000, 0, 1 << 28),
            rmw(0x49ec, 0xff000000, 0),
        ],
    );
    v10::request(smn, 0x0e, [0; 6], delay).map_err(|_| "GFX SMU")?;
    Ok(uma)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;
    #[test]
    fn atom_table_has_native_display_order_tuning_defaults_and_memory_geometry() {
        static DISPLAYS: &[Display] = &[
            Display {
                connector: Connector::Edp,
                aux: 0,
                hotplug: 0,
            },
            Display {
                connector: Connector::Hdmi,
                aux: 1,
                hotplug: 1,
            },
            Display {
                connector: Connector::DisplayPort,
                aux: 3,
                hotplug: 3,
            },
        ];
        let space = SmnSpace::default();
        space.set(SmnReg::<()>::new(0x50020), 1);
        let config = Config {
            displays: DISPLAYS,
            hdmi2_disable: 2,
        };
        let table = system_info(&space.smn(), config).unwrap();
        assert_eq!(table.system.memory_type, 0x1a);
        assert_eq!(table.system.channels, 1);
        let paths = &table.system.external.paths;
        assert_eq!((paths[0].tag.get(), paths[0].connector.get()), (2, 0x3114));
        assert_eq!(
            (paths[1].tag.get(), paths[1].connector.get()),
            (0x80, 0x320c)
        );
        assert_eq!((paths[2].tag.get(), paths[2].connector.get()), (8, 0x3113));
        assert_eq!((paths[1].caps.get(), paths[2].caps.get()), (0x200, 0x100));
        assert!(
            table
                .system
                .dp
                .iter()
                .chain(&table.system.dp_extra)
                .all(|t| t.settings == [0xff; 50])
        );
        space.set(SmnReg::<()>::new(0x5d5c0), 2 << 30);
        let table = system_info(&space.smn(), config).unwrap();
        assert_eq!(table.system.external.paths[2].connector.get(), 0);
        assert_eq!(table.system.external.paths[3].connector.get(), 0x3113);
    }
}
