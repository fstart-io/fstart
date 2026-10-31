// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board-facing table rows and facts (`fstart_driver_amd::apcb`) into
//! the `amd-apcb` wire structs. Pure transcription: each function sets
//! exactly the fields the row carries; everything else is the wire
//! type's own default.

use amd_apcb::memory;
use fstart_driver_amd::apcb::tables::{
    CadRow, DataRow, DdrRate, FreqRow, OdtRow, Ranks, Rates, Slots,
};
use fstart_driver_amd::apcb::{ChannelWiring, SpdSlot};
use num_traits::FromPrimitive;

type Result<T> = std::result::Result<T, String>;

fn rates(rates: Rates) -> memory::DdrRates {
    let mut out = memory::DdrRates::new();
    for &rate in DdrRate::ALL {
        if !rates.contains(rate) {
            continue;
        }
        match rate {
            DdrRate::Ddr400 => out.set_ddr400(true),
            DdrRate::Ddr533 => out.set_ddr533(true),
            DdrRate::Ddr667 => out.set_ddr667(true),
            DdrRate::Ddr800 => out.set_ddr800(true),
            DdrRate::Ddr1066 => out.set_ddr1066(true),
            DdrRate::Ddr1333 => out.set_ddr1333(true),
            DdrRate::Ddr1600 => out.set_ddr1600(true),
            DdrRate::Ddr1866 => out.set_ddr1866(true),
            DdrRate::Ddr2133 => out.set_ddr2133(true),
            DdrRate::Ddr2400 => out.set_ddr2400(true),
            DdrRate::Ddr2667 => out.set_ddr2667(true),
            DdrRate::Ddr2933 => out.set_ddr2933(true),
            DdrRate::Ddr3200 => out.set_ddr3200(true),
        }
    }
    out
}

fn ranks(ranks: Ranks) -> memory::Ddr4DimmRanks {
    let mut out = memory::Ddr4DimmRanks::new();
    out.set_unpopulated(ranks.0 & Ranks::EMPTY.0 != 0);
    out.set_single_rank(ranks.0 & Ranks::SINGLE.0 != 0);
    out.set_dual_rank(ranks.0 & Ranks::DUAL.0 != 0);
    out.set_quad_rank(ranks.0 & Ranks::QUAD.0 != 0);
    out
}

fn lr_ranks(ranks: Ranks) -> memory::LrdimmDdr4DimmRanks {
    let mut out = memory::LrdimmDdr4DimmRanks::new();
    out.set_unpopulated(ranks.0 & Ranks::EMPTY.0 != 0);
    out.set_lr(ranks.0 & Ranks::LR.0 != 0);
    out
}

fn slots(slots: Slots) -> memory::DimmsPerChannelSelector {
    let mut out = memory::DimmsPerChannelSelector::new();
    out.set_one_dimm(slots.0 & Slots::ONE.0 != 0);
    out.set_two_dimms(slots.0 & Slots::TWO.0 != 0);
    out
}

fn vdd_rdimm() -> memory::RdimmDdr4Voltages {
    let mut v = memory::RdimmDdr4Voltages::new();
    v.set__1_2V(true);
    v
}

fn vdd_lrdimm() -> memory::LrdimmDdr4Voltages {
    let mut v = memory::LrdimmDdr4Voltages::new();
    v.set__1_2V(true);
    v
}

fn cad_drive(
    drive: fstart_driver_amd::apcb::tables::CadDrive,
) -> Result<memory::CadBusClkDriveStrength> {
    memory::CadBusClkDriveStrength::from_u8(drive as u8)
        .ok_or_else(|| "CAD drive strength".to_string())
}

pub fn rdimm_cad(row: &CadRow) -> Result<memory::RdimmDdr4CadBusElement> {
    use num_traits::ToPrimitive;
    let mut out = memory::RdimmDdr4CadBusElement::default();
    out.set_dimm_slots_per_channel(
        slots(row.slots)
            .to_u32()
            .ok_or_else(|| "slot selector".to_string())?,
    );
    out.set_ddr_rates(rates(row.rates));
    out.set_vdd_io(vdd_rdimm());
    out.set_dimm0_ranks(ranks(row.dimm0));
    out.set_dimm1_ranks(ranks(row.dimm1));
    out.set_gear_down_mode(row.gear_down);
    out.set_slow_mode(row.slow);
    out.set_address_command_control(row.addr_cmd_control);
    out.set_cke_drive_strength(cad_drive(row.drive[0])?);
    out.set_cs_odt_drive_strength(cad_drive(row.drive[1])?);
    out.set_address_command_drive_strength(cad_drive(row.drive[2])?);
    out.set_clk_drive_strength(cad_drive(row.drive[3])?);
    Ok(out)
}

pub fn lrdimm_cad(row: &CadRow) -> Result<memory::LrdimmDdr4CadBusElement> {
    use num_traits::ToPrimitive;
    let mut out = memory::LrdimmDdr4CadBusElement::default();
    out.set_dimm_slots_per_channel(
        slots(row.slots)
            .to_u32()
            .ok_or_else(|| "slot selector".to_string())?,
    );
    out.set_ddr_rates(rates(row.rates));
    out.set_vdd_io(vdd_lrdimm());
    out.set_dimm0_ranks(lr_ranks(row.dimm0));
    out.set_dimm1_ranks(lr_ranks(row.dimm1));
    out.set_gear_down_mode(u16::from(row.gear_down));
    out.set_slow_mode(u16::from(row.slow));
    out.set_address_command_control(row.addr_cmd_control);
    out.set_cke_drive_strength(cad_drive(row.drive[0])?);
    out.set_cs_odt_drive_strength(cad_drive(row.drive[1])?);
    out.set_address_command_drive_strength(cad_drive(row.drive[2])?);
    out.set_clk_drive_strength(cad_drive(row.drive[3])?);
    Ok(out)
}

fn vref(v: fstart_driver_amd::apcb::tables::VrefDq) -> Result<memory::VrefDq> {
    // Range 1 covers codes 0..64 (60.00%..100.95%); the reference
    // tables only use range 1.
    Ok(memory::VrefDq::Range1(
        memory::VrefDqRange1::from_u32(v.0).ok_or_else(|| "VrefDQ code".to_string())?,
    ))
}

pub fn rdimm_data(row: &DataRow) -> Result<memory::Ddr4DataBusElement> {
    let mut out = memory::Ddr4DataBusElement::default();
    out.set_dimm_slots_per_channel(row.slots.0.into());
    out.set_ddr_rates(rates(row.rates));
    out.set_vdd_io(vdd_rdimm());
    out.set_dimm0_ranks(ranks(row.dimm0));
    out.set_dimm1_ranks(ranks(row.dimm1));
    out.set_rtt_nom(
        memory::RttNom::from_u8(row.rtt_nom as u8).ok_or_else(|| "RTT_NOM".to_string())?,
    );
    out.set_rtt_wr(memory::RttWr::from_u8(row.rtt_wr as u8).ok_or_else(|| "RTT_WR".to_string())?);
    out.set_rtt_park(
        memory::RttPark::from_u8(row.rtt_park as u8).ok_or_else(|| "RTT_PARK".to_string())?,
    );
    out.set_dq_drive_strength(row.drive[0]);
    out.set_dqs_drive_strength(row.drive[1]);
    out.set_odt_drive_strength(row.drive[2]);
    out.set_pmu_phy_vref(row.pmu_vref);
    out.set_vref_dq(vref(row.dram_vref)?);
    Ok(out)
}

pub fn lrdimm_data(row: &DataRow) -> Result<memory::LrdimmDdr4DataBusElement> {
    let mut out = memory::LrdimmDdr4DataBusElement::default();
    out.set_dimm_slots_per_channel(row.slots.0.into());
    out.set_ddr_rates(rates(row.rates));
    out.set_vdd_io(vdd_lrdimm());
    out.set_dimm0_ranks(lr_ranks(row.dimm0));
    out.set_dimm1_ranks(lr_ranks(row.dimm1));
    out.set_rtt_nom(
        memory::RttNom::from_u8(row.rtt_nom as u8).ok_or_else(|| "RTT_NOM".to_string())?,
    );
    out.set_rtt_wr(memory::RttWr::from_u8(row.rtt_wr as u8).ok_or_else(|| "RTT_WR".to_string())?);
    out.set_rtt_park(
        memory::RttPark::from_u8(row.rtt_park as u8).ok_or_else(|| "RTT_PARK".to_string())?,
    );
    out.set_dq_drive_strength(row.drive[0]);
    out.set_dqs_drive_strength(row.drive[1]);
    out.set_odt_drive_strength(row.drive[2]);
    out.set_pmu_phy_vref(row.pmu_vref);
    // LRDIMM rows carry the raw MR6 code, like the donors.
    out.set_vref_dq(row.dram_vref.0);
    Ok(out)
}

fn odt_patterns(patterns: [u8; 2]) -> memory::OdtPatPatterns {
    let mut out = memory::OdtPatPatterns::new();
    out.set_reading_pattern(patterns[0]);
    out.set_writing_pattern(patterns[1]);
    out
}

pub fn rdimm_odt(row: &OdtRow) -> Result<memory::Ddr4OdtPatElement> {
    let mut out = memory::Ddr4OdtPatElement::default();
    let mut bitmaps = memory::Ddr4OdtPatDimmRankBitmaps::new();
    bitmaps.set_dimm0(ranks(row.dimms[0]));
    bitmaps.set_dimm1(ranks(row.dimms[1]));
    bitmaps.set_dimm2(ranks(row.dimms[2]));
    out.set_dimm_rank_bitmaps(bitmaps);
    out.set_cs0_odt_patterns(odt_patterns(row.patterns[0]));
    out.set_cs1_odt_patterns(odt_patterns(row.patterns[1]));
    out.set_cs2_odt_patterns(odt_patterns(row.patterns[2]));
    out.set_cs3_odt_patterns(odt_patterns(row.patterns[3]));
    Ok(out)
}

pub fn lrdimm_odt(row: &OdtRow) -> Result<memory::LrdimmDdr4OdtPatElement> {
    let mut out = memory::LrdimmDdr4OdtPatElement::default();
    let mut bitmaps = memory::LrdimmDdr4OdtPatDimmRankBitmaps::new();
    bitmaps.set_dimm0(lr_ranks(row.dimms[0]));
    bitmaps.set_dimm1(lr_ranks(row.dimms[1]));
    bitmaps.set_dimm2(lr_ranks(row.dimms[2]));
    out.set_dimm_rank_bitmaps(bitmaps);
    out.set_cs0_odt_patterns(odt_patterns(row.patterns[0]));
    out.set_cs1_odt_patterns(odt_patterns(row.patterns[1]));
    out.set_cs2_odt_patterns(odt_patterns(row.patterns[2]));
    out.set_cs3_odt_patterns(odt_patterns(row.patterns[3]));
    Ok(out)
}

fn speed(rate: DdrRate) -> Result<memory::DdrSpeed> {
    memory::DdrSpeed::from_u16(rate.mhz()).ok_or_else(|| "DDR speed".to_string())
}

pub fn max_freq(row: &FreqRow, unsupported: memory::DdrSpeed) -> Result<memory::MaxFreqElement> {
    Ok(memory::MaxFreqElement::new(
        unsupported,
        memory::DimmsPerChannel::Specific(slots(row.slots)),
        row.dimms,
        row.single_rank,
        row.dual_rank,
        row.quad_rank,
        speed(row.max)?,
    ))
}

pub fn lr_max_freq(
    row: &FreqRow,
    unsupported: memory::DdrSpeed,
) -> Result<memory::LrMaxFreqElement> {
    Ok(memory::LrMaxFreqElement::new(
        unsupported,
        memory::DimmsPerChannel::Specific(slots(row.slots)),
        row.dimms,
        row.single_rank,
        row.dual_rank,
        row.quad_rank,
        speed(row.max)?,
    ))
}

pub fn spd_slot(slot: &SpdSlot) -> Result<memory::DimmInfoSmbusElement> {
    let out = memory::DimmInfoSmbusElement::new_slot(
        slot.socket,
        slot.channel,
        slot.dimm,
        slot.address,
        Some(slot.mux),
        Some(slot.mux_control),
        Some(slot.mux_channel),
    )
    .map_err(|e| format!("{e:?}"))?;
    if !slot.present {
        // No donor uses absent-slot rows; boards list populated slots.
        return Err("absent SPD slot rows are not encodable yet".into());
    }
    Ok(out)
}

/// The PSP platform-specific overrides of a board's channel wiring.
/// Built through the upstream element type's serde form: the element
/// enum has no complete external constructor, and the JSON stays an
/// encoder-internal detail.
pub fn wiring(wiring: &ChannelWiring) -> Result<Vec<memory::platform_specific_override::Element>> {
    use serde_json::json;
    if !(1..=4).contains(&wiring.dimms_per_channel)
        || !(1..=8).contains(&wiring.channels_per_socket)
    {
        return Err("invalid channel wiring geometry".into());
    }
    let mut ops = vec![];
    let mut push = |name: &str, field: &str, value: serde_json::Value| -> Result<()> {
        ops.push(
            serde_json::from_value(json!({(name): {
                "sockets": {"socket_0": true, "socket_1": true, "socket_2": true, "socket_3": true,
                            "socket_4": true, "socket_5": true, "socket_6": true, "socket_7": true},
                "channels": "Any", "dimms": "Any", (field): value,
            }}))
            .map_err(|e| e.to_string())?,
        );
        Ok(())
    };
    if let Some(c) = wiring.memclk {
        push("MemclkMap", "connections", json!(c))?;
    }
    if let Some(c) = wiring.cke {
        push("CkeTristateMap", "connections", json!(c))?;
    }
    if let Some(c) = wiring.odt {
        push("OdtTristateMap", "connections", json!(c))?;
    }
    if let Some(c) = wiring.cs {
        push("CsTristateMap", "connections", json!(c))?;
    }
    push(
        "MaxDimmsPerChannel",
        "value",
        json!(wiring.dimms_per_channel),
    )?;
    push(
        "MaxChannelsPerSocket",
        "value",
        json!(wiring.channels_per_socket),
    )?;
    Ok(ops)
}
