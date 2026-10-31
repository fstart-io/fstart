// SPDX-License-Identifier: MIT OR Apache-2.0
//! Generation of SP3 APCB v3 payloads from board facts.
//!
//! This is the platform's side of the split documented in
//! `fstart_driver_amd::apcb`: it owns everything about the wire
//! (token catalogs, entry/group structure, the CBS and PPR payloads,
//! revision assignment) and nothing about any board. Adding an SP3
//! board changes nothing here.
//!
//! Construction goes through `amd_apcb`'s serde form because that is
//! the crate's complete public construction API; the tree built here
//! is made only of typed upstream values (token enums, wire structs),
//! never of authored JSON. Entries are emitted in the order this file
//! lists them; the builder may re-sort within a group. Donor APCBs
//! are already in that order except for one CBS quirk (Rome places
//! the medium-priority token entry before the low one), which cannot
//! matter to priority-based token resolution.
//!
//! Two deliberate simplifications against the donors, both to be
//! settled on hardware, not by copying donor bytes:
//! - Backup and active CBS payloads carry the same token values,
//!   derived from the backup catalog (Milan donors additionally
//!   differ in one unnamed token's presence/value and, on EthanolX,
//!   two low-priority timing tokens; Rome's are identical).
//! - `unique_apcb_instance` revisions are assigned by [`REVISION`],
//!   not transcribed from the donor's build counters.
//! - A board-instance token entry is emitted only for the token
//!   classes whose effective values actually differ from the board's
//!   global entry, where the vendor tool always writes full copies.

mod catalog;
mod convert;
mod milan;
mod rome;

use amd_apcb::{
    Apcb, ApcbIoOptions, BoardInstances, DfEntryId, EntryId, GroupId, MemoryEntryId, PriorityLevel,
    PriorityLevels, PspEntryId, TokenEntryId, V2_HEADER, V3_HEADER_EXT, df, memory,
};
use fstart_amdfw_format::directory::bios;
use fstart_driver_amd::apcb::tables::FreqTables;
use fstart_driver_amd::apcb::{ApcbFacts, BoardId};
use fstart_driver_amd::sp3::soc::Generation;
use num_traits::ToPrimitive;
use serde::Serialize;
use serde_json::{Value, json};

pub(crate) use catalog::{RawToken, TokenLists};

type Result<T> = std::result::Result<T, String>;

/// The revision (`unique_apcb_instance`) of every generated payload.
/// One value for all copies a build emits, so ABL never sees a stale
/// partner from us; bump when the encoding changes incompatibly.
pub const REVISION: u32 = 1;

/// Where a generated payload belongs in the BIOS directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selector {
    /// `bios::APCB` (0x60) or `bios::APCB_BACKUP` (0x68).
    pub kind: u8,
    pub instance: u8,
    pub subprogram: u8,
}

/// One generated APCB payload and its directory slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    pub selector: Selector,
    pub bytes: Vec<u8>,
}

struct Entry {
    id: EntryId,
    instance: u16,
    board_mask: BoardInstances,
    value: Value,
}

fn priority_levels(levels: &[PriorityLevel]) -> PriorityLevels {
    let mut out = PriorityLevels::new();
    for level in levels {
        match level {
            PriorityLevel::HardForce => out.set_hard_force(true),
            PriorityLevel::High => out.set_high(true),
            PriorityLevel::Medium => out.set_medium(true),
            PriorityLevel::EventLogging => out.set_event_logging(true),
            PriorityLevel::Low => out.set_low(true),
            PriorityLevel::Normal => out.set_normal(true),
        }
    }
    out
}

fn header(
    id: EntryId,
    instance: u16,
    board_mask: BoardInstances,
    priority: &[PriorityLevel],
) -> Value {
    let tokens = matches!(id, EntryId::Token(_));
    json!({
        "group_id": id.group_id().to_u16().unwrap(),
        "entry_id": id.type_id(),
        "instance_id": instance,
        "context_type": if tokens { "Tokens" } else { "Struct" },
        "context_format": if tokens { "SortAscending" } else { "Raw" },
        "unit_size": if tokens { 8 } else { 0 },
        "key_size": if tokens { 4 } else { 0 },
        "key_pos": 0,
        "priority_mask": serde_json::to_value(priority_levels(priority)).unwrap(),
        "board_instance_mask": board_mask.to_u16().unwrap(),
    })
}

fn struct_entry<T: Serialize>(
    id: EntryId,
    board_mask: BoardInstances,
    priority: &[PriorityLevel],
    key: &'static str,
    body: &T,
) -> Result<Entry> {
    Ok(Entry {
        id,
        instance: 0,
        board_mask,
        value: json!({
            "header": header(id, 0, board_mask, priority),
            (key): serde_json::to_value(body).map_err(|e| e.to_string())?,
        }),
    })
}

fn token_entry(
    class: TokenEntryId,
    instance: u16,
    board_mask: BoardInstances,
    priority: &[PriorityLevel],
    tokens: &[amd_apcb::TOKEN_ENTRY],
) -> Entry {
    let id = EntryId::Token(class);
    Entry {
        id,
        instance,
        board_mask,
        value: json!({
            "header": header(id, instance, board_mask, priority),
            "tokens": tokens.iter().map(|t| json!({
                "Unknown": {"entry_id": format!("{class:?}"), "tag": t.key.get(), "value": t.value.get()}
            })).collect::<Vec<_>>(),
        }),
    }
}

fn token_entries(
    classes: &catalog::EffectiveTokens,
    instance: u16,
    board_mask: BoardInstances,
    priority: &[PriorityLevel],
) -> Vec<Entry> {
    classes
        .iter()
        .map(|&(class, ref tokens)| token_entry(class, instance, board_mask, priority, tokens))
        .collect()
}

fn group_signature(group: GroupId) -> &'static str {
    match group {
        GroupId::Psp => "PSPG",
        GroupId::Df => "DFG ",
        GroupId::Memory => "MEMG",
        GroupId::Token => "TOKN",
        _ => unreachable!("the SP3 encoder emits only PSP, DF, memory and token groups"),
    }
}

fn build(groups: &[GroupId], mut entries: Vec<Entry>) -> Result<Vec<u8>> {
    // Donor order within a group: entry id, then instance, then the
    // specific board masks before the global one.
    let order = |g: GroupId| groups.iter().position(|&id| id == g).unwrap_or(usize::MAX);
    entries.sort_by_key(|e| {
        (
            order(e.id.group_id()),
            e.id.type_id(),
            e.instance,
            e.board_mask.to_u16().unwrap(),
        )
    });
    let mut head = V2_HEADER::default();
    head.header_size.set(128);
    head.unique_apcb_instance.set(REVISION);
    let mut ext = V3_HEADER_EXT::default();
    ext.signature_ending = *b"BCBA";
    let value = json!({
        "version": "0.5.5",
        "header": serde_json::to_value(head).map_err(|e| e.to_string())?,
        "v3_header_ext": serde_json::to_value(ext).map_err(|e| e.to_string())?,
        "groups": groups.iter().map(|&group| json!({
            "header": {"signature": group_signature(group), "group_id": group.to_u16().unwrap(), "header_size": 16, "version": 1, "_reserved_": 0}
        })).collect::<Vec<_>>(),
        "entries": entries.into_iter().map(|e| e.value).collect::<Vec<_>>(),
    });
    let apcb: Apcb<'static> =
        serde_json::from_value(value).map_err(|e| format!("APCB build: {e}"))?;
    let saved = apcb
        .save_no_inc()
        .map_err(|e| format!("APCB build: {e:?}"))?;
    let size = u32::from_le_bytes(saved[8..12].try_into().unwrap()) as usize;
    let bytes = saved[..size].to_vec();
    Apcb::load(bytes.clone().into(), &ApcbIoOptions::default())
        .map_err(|e| format!("generated APCB does not parse: {e:?}"))?;
    Ok(bytes)
}

fn console(facts: &ApcbFacts) -> memory::ConsoleOutControl {
    let c = &facts.console;
    let mut out = memory::AblConsoleOutControl::default();
    out.set_enable_console_logging(c.console_logging);
    out.set_enable_mem_flow_logging(c.mem_flow);
    out.set_enable_mem_setreg_logging(c.mem_setreg);
    out.set_enable_mem_getreg_logging(c.mem_getreg);
    out.set_enable_mem_status_logging(c.mem_status);
    out.set_enable_mem_pmu_logging(c.mem_pmu);
    out.set_enable_mem_pmu_sram_read_logging(c.mem_pmu_sram_read);
    out.set_enable_mem_pmu_sram_write_logging(c.mem_pmu_sram_write);
    out.set_enable_mem_test_verbose_logging(c.mem_test_verbose);
    out.set_enable_mem_basic_output_logging(c.mem_basic_output);
    out.set_abl_console_port(c.port);
    memory::ConsoleOutControl::new(
        out,
        memory::AblBreakpointControl::new(c.breakpoint, c.break_on_all_dies),
    )
}

/// The error-out and voltage structs are platform constants: upstream
/// defaults with the handful of fields every SP3 donor sets the same
/// way (ports 0x84/0x80, 32-bit, GPIO85 error reporting, no heartbeat).
/// No board has ever varied them.
macro_rules! error_out {
    ($ty:ty) => {{
        let mut out = <$ty>::default();
        out.set_clear_acknowledgement(false);
        out.set_enable_heart_beat(false);
        out.set_stop_on_first_fatal_error(false);
        out.set_input_port(132);
        out.set_input_port_size(memory::PortSize::_32Bit);
        let gpio = memory::Gpio::new(85, 1, 192);
        out.set_error_reporting_gpio(Some(gpio));
        // The setter also raises the enable flag; every donor carries
        // the pin assignment with reporting disabled.
        out.set_enable_error_reporting_gpio(false);
        out
    }};
}

fn slink() -> df::SlinkConfig {
    df::SlinkConfig {
        regions: [
            df::SlinkRegion::dummy(0),
            df::SlinkRegion::dummy(0),
            df::SlinkRegion::dummy(1),
            df::SlinkRegion::dummy(1),
        ],
    }
}

fn freq_entries(
    freq: &FreqTables,
    base: &FreqTables,
    board_mask: BoardInstances,
    unsupported: memory::DdrSpeed,
    only_changed: bool,
) -> Result<Vec<Entry>> {
    let mut out = vec![];
    let mut max = |id: MemoryEntryId,
                   rows: &'static [fstart_driver_amd::apcb::tables::FreqRow],
                   base_rows: &'static [fstart_driver_amd::apcb::tables::FreqRow]|
     -> Result<()> {
        if only_changed && rows == base_rows {
            return Ok(());
        }
        if id == MemoryEntryId::PsLrdimmDdr4MaxFreq {
            out.push(struct_entry(
                EntryId::Memory(id),
                board_mask,
                &[PriorityLevel::Normal],
                "LrMaxFreqElement",
                &rows
                    .iter()
                    .map(|r| convert::lr_max_freq(r, unsupported))
                    .collect::<Result<Vec<_>>>()?,
            )?);
        } else {
            out.push(struct_entry(
                EntryId::Memory(id),
                board_mask,
                &[PriorityLevel::Normal],
                "MaxFreqElement",
                &rows
                    .iter()
                    .map(|r| convert::max_freq(r, unsupported))
                    .collect::<Result<Vec<_>>>()?,
            )?);
        }
        Ok(())
    };
    max(
        MemoryEntryId::PsRdimmDdr4MaxFreq,
        freq.rdimm_max,
        base.rdimm_max,
    )?;
    max(
        MemoryEntryId::PsRdimmDdr4StretchFreq,
        freq.rdimm_stretch,
        base.rdimm_stretch,
    )?;
    max(
        MemoryEntryId::Ps3dsRdimmDdr4MaxFreq,
        freq.three_ds_max,
        base.three_ds_max,
    )?;
    max(
        MemoryEntryId::Ps3dsRdimmDdr4StretchFreq,
        freq.three_ds_stretch,
        base.three_ds_stretch,
    )?;
    max(
        MemoryEntryId::PsLrdimmDdr4MaxFreq,
        freq.lrdimm_max,
        base.lrdimm_max,
    )?;
    Ok(out)
}

/// Generate every APCB payload of one generation for a board: the
/// main payload, the CBS payload and the post-package-repair payload,
/// each at every directory selector the SP3 image layout uses.
pub fn apcbs(generation: Generation, facts: &ApcbFacts) -> Result<Vec<Generated>> {
    let catalog = catalog::catalog(generation)
        .ok_or_else(|| format!("{generation:?} has no APCB v3 catalog (Naples is v2)"))?;
    let subprogram = match generation {
        Generation::Rome => 0,
        Generation::Milan => 1,
        Generation::Naples => unreachable!(),
    };
    let sel = |kind: u8, instance: u8| Selector {
        kind,
        instance,
        subprogram,
    };

    let mut overrides = catalog::policy_tokens(&facts.policy, generation)?;
    overrides.extend(catalog::fact_tokens(facts));
    let effective = catalog::effective(catalog.setup, &overrides)?;

    // The main payload.
    let mut entries: Vec<Entry> = vec![];
    if let BoardId::Eeprom {
        controller,
        address,
        id_offset,
        revision_offset,
        mapping,
    } = facts.board_id
    {
        let mut head = amd_apcb::psp::BoardIdGettingMethodEeprom::default();
        head.set_i2c_controller_index(controller);
        head.set_device_address(address);
        head.set_board_id_offset(id_offset);
        head.set_board_rev_offset(revision_offset);
        let mapping: Vec<amd_apcb::psp::IdRevApcbMapping> = mapping
            .iter()
            .map(|m| {
                amd_apcb::psp::IdRevApcbMapping::new(
                    m.mask,
                    m.id,
                    match m.revision {
                        None => amd_apcb::psp::RevAndFeatureValue::NotApplicable,
                        Some(v) => amd_apcb::psp::RevAndFeatureValue::Value(v),
                    },
                    m.instance,
                )
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| format!("{e:?}"))?;
        entries.push(struct_entry(
            EntryId::Psp(PspEntryId::BoardIdGettingMethod),
            BoardInstances::all(),
            &[PriorityLevel::HardForce, PriorityLevel::Normal],
            "BoardIdGettingMethodEeprom",
            &(head, mapping),
        )?);
    }
    let e = &facts.electrical;
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::DimmInfoSmbus),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "DimmInfoSmbusElement",
        &facts
            .spd
            .iter()
            .map(convert::spd_slot)
            .collect::<Result<Vec<_>>>()?,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PlatformSpecificOverride),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "platform_specific_overrides",
        &convert::wiring(&facts.wiring)?,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PsRdimmDdr4OdtPat),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "Ddr4OdtPatElement",
        &e.rdimm_odt
            .iter()
            .map(convert::rdimm_odt)
            .collect::<Result<Vec<_>>>()?,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PsRdimmDdr4CadBus),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "RdimmDdr4CadBusElement",
        &e.rdimm_cad
            .iter()
            .map(convert::rdimm_cad)
            .collect::<Result<Vec<_>>>()?,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PsRdimmDdr4DataBus),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "Ddr4DataBusElement",
        &e.rdimm_data
            .iter()
            .map(convert::rdimm_data)
            .collect::<Result<Vec<_>>>()?,
    )?);
    entries.extend(freq_entries(
        &facts.freq,
        &facts.freq,
        BoardInstances::all(),
        catalog.unsupported_speed,
        false,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::Ps3dsRdimmDdr4DataBus),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "Ddr4DataBusElement",
        &e.three_ds_data
            .iter()
            .map(convert::rdimm_data)
            .collect::<Result<Vec<_>>>()?,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::ConsoleOutControl),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "ConsoleOutControl",
        &console(facts),
    )?);
    if catalog.error_out_116 {
        entries.push(struct_entry(
            EntryId::Memory(MemoryEntryId::ErrorOutControl),
            BoardInstances::all(),
            &[PriorityLevel::Normal],
            "ErrorOutControl116",
            &error_out!(memory::ErrorOutControl116),
        )?);
    } else {
        entries.push(struct_entry(
            EntryId::Memory(MemoryEntryId::ErrorOutControl),
            BoardInstances::all(),
            &[PriorityLevel::Normal],
            "ErrorOutControl112",
            &error_out!(memory::ErrorOutControl112),
        )?);
    }
    let mut voltage = memory::ExtVoltageControl::default();
    voltage.set_enabled(true);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::ExtVoltageControl),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "ExtVoltageControl",
        &voltage,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PsLrdimmDdr4OdtPat),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "LrdimmDdr4OdtPatElement",
        &e.lrdimm_odt
            .iter()
            .map(convert::lrdimm_odt)
            .collect::<Result<Vec<_>>>()?,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PsLrdimmDdr4CadBus),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "LrdimmDdr4CadBusElement",
        &e.lrdimm_cad
            .iter()
            .map(convert::lrdimm_cad)
            .collect::<Result<Vec<_>>>()?,
    )?);
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PsLrdimmDdr4DataBus),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "LrdimmDdr4DataBusElement",
        &e.lrdimm_data
            .iter()
            .map(convert::lrdimm_data)
            .collect::<Result<Vec<_>>>()?,
    )?);
    let tuning_end: memory::platform_tuning::Element =
        serde_json::from_value(json!({"Terminator": {}})).map_err(|e| e.to_string())?;
    entries.push(struct_entry(
        EntryId::Memory(MemoryEntryId::PlatformTuning),
        BoardInstances::all(),
        &[PriorityLevel::Normal],
        "platform_tuning",
        &vec![tuning_end],
    )?);
    // Board-instance variants: masked frequency and token entries for
    // whatever actually differs from the board's global entries.
    let as_pairs = |v: &[amd_apcb::TOKEN_ENTRY]| -> Vec<(u32, u32)> {
        v.iter().map(|t| (t.key.get(), t.value.get())).collect()
    };
    for instance in facts.instances {
        if instance.index >= 16 {
            return Err("board instance index must be below 16".into());
        }
        let mask = BoardInstances::from_instance(instance.index).map_err(|e| format!("{e:?}"))?;
        if let Some(freq) = &instance.freq {
            entries.extend(freq_entries(
                freq,
                &facts.freq,
                mask,
                catalog.unsupported_speed,
                true,
            )?);
        }
        let mut inst_overrides = catalog::policy_tokens(&instance.policy, generation)?;
        inst_overrides.extend(catalog::fact_tokens(facts));
        let inst_effective = catalog::effective(catalog.setup, &inst_overrides)?;
        for ((class, tokens), (_, global)) in inst_effective.iter().zip(&effective) {
            if as_pairs(global) != as_pairs(tokens) {
                entries.push(token_entry(
                    *class,
                    0,
                    mask,
                    &[PriorityLevel::Normal],
                    tokens,
                ));
            }
        }
    }
    entries.extend(token_entries(
        &effective,
        0,
        BoardInstances::all(),
        &[PriorityLevel::Normal],
    ));
    let mut groups: Vec<GroupId> = vec![];
    if matches!(facts.board_id, BoardId::Eeprom { .. }) {
        groups.push(GroupId::Psp);
    }
    groups.push(GroupId::Memory);
    groups.push(GroupId::Token);
    let main = build(&groups, entries)?;

    // The CBS payloads. Policy applies replacement-only (see
    // `catalog::effective_replace_only`): CBS is a subset of the
    // setup token universe whose values are partly its own (POST
    // package repair, MBIST), and none of those are policy fields.
    // The Slink struct rides along except in Milan's backup
    // payload, as in every Milan donor.
    let cbs_effective = catalog::effective_replace_only(catalog.cbs, &overrides)?;
    let cbs_payload = |slink_present: bool| -> Result<Vec<u8>> {
        let mut entries: Vec<Entry> = vec![];
        if slink_present {
            entries.push(struct_entry(
                EntryId::Df(DfEntryId::SlinkConfig),
                BoardInstances::all(),
                &[PriorityLevel::Low],
                "SlinkConfig",
                &slink(),
            )?);
        }
        for &(class, ref tokens) in &cbs_effective {
            entries.push(token_entry(
                class,
                0,
                BoardInstances::all(),
                &[PriorityLevel::Low],
                &[],
            ));
            entries.push(token_entry(
                class,
                1,
                BoardInstances::all(),
                &[PriorityLevel::Medium],
                tokens,
            ));
        }
        let groups: &[GroupId] = if slink_present {
            &[GroupId::Df, GroupId::Token]
        } else {
            &[GroupId::Token]
        };
        build(groups, entries)
    };
    let cbs_backup = cbs_payload(matches!(generation, Generation::Rome))?;
    let cbs_active = cbs_payload(true)?;
    // (per-role CBS payloads built above)

    // The post-package-repair payload: empty repair slots, both slots.
    let ppr = build(
        &[GroupId::Memory],
        vec![struct_entry(
            EntryId::Memory(MemoryEntryId::DdrPostPackageRepair),
            BoardInstances::all(),
            &[PriorityLevel::EventLogging],
            "DdrPostPackageRepairElement",
            &vec![memory::DdrPostPackageRepairElement::default(); 64],
        )?],
    )?;

    let mut out = vec![Generated {
        selector: sel(bios::APCB_BACKUP, 0),
        bytes: main,
    }];
    out.push(Generated {
        selector: sel(bios::APCB_BACKUP, 8),
        bytes: cbs_backup,
    });
    out.push(Generated {
        selector: sel(bios::APCB, 0),
        bytes: cbs_active,
    });
    for selector in [sel(bios::APCB_BACKUP, 9), sel(bios::APCB, 1)] {
        out.push(Generated {
            selector,
            bytes: ppr.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amd_apcb::{BoolToken, EntryItemBody, TOKEN_ENTRY};
    use fstart_driver_amd::apcb::tables::{electrical_reference, freq_reference};
    use fstart_driver_amd::apcb::{
        AblConsole, ChannelWiring, Instance, MemoryPolicy, Policy, SpdSlot,
    };

    static SPD: &[SpdSlot] = &[SpdSlot::muxed(0, 0, 0, 0xa0, 0xe0, 4, 0)];

    fn facts(generation: Generation) -> ApcbFacts {
        ApcbFacts {
            policy: Policy {
                mem: MemoryPolicy {
                    power_down: Some(true),
                    ..MemoryPolicy::DEFAULT
                },
                ..Policy::DEFAULT
            },
            console: AblConsole::DEFAULT,
            spd: SPD,
            board_id: BoardId::None,
            wiring: ChannelWiring {
                memclk: None,
                cke: None,
                odt: None,
                cs: None,
                dimms_per_channel: 1,
                channels_per_socket: 8,
            },
            electrical: electrical_reference(generation),
            freq: freq_reference(generation),
            instances: &[],
            rom3_base_high: 0,
            pcie_reset_pin_select: 0,
            sensor_3ds_upper: 75,
        }
    }

    #[test]
    fn generated_payloads_parse_validate_and_cover_the_slots() {
        for generation in [Generation::Rome, Generation::Milan] {
            let out = apcbs(generation, &facts(generation)).unwrap();
            assert_eq!(out.len(), 5);
            let sub = u8::from(generation == Generation::Milan);
            for generated in &out {
                assert_eq!(generated.selector.subprogram, sub);
                let apcb =
                    Apcb::load(generated.bytes.clone().into(), &ApcbIoOptions::default()).unwrap();
                apcb.validate(None).unwrap();
                assert_eq!(apcb.unique_apcb_instance().unwrap(), REVISION);
            }
            // The one policy delta reaches the wire; a token the board
            // has no opinion on keeps its catalog value.
            let main = &out[0].bytes;
            let apcb = Apcb::load(main.clone().into(), &ApcbIoOptions::default()).unwrap();
            let key = TOKEN_ENTRY::try_from(&BoolToken::MemEnablePowerDown(true))
                .unwrap()
                .key
                .get();
            let mut found = None;
            for group in apcb.groups().unwrap() {
                for entry in group.entries() {
                    if let EntryItemBody::Tokens(tokens) = &entry.body {
                        if let Some(t) = tokens.token(key) {
                            found = Some(t.value());
                        }
                    }
                }
            }
            assert_eq!(found, Some(1), "{generation:?} power-down token");
        }
        // Naples is APCB v2 silicon: no v3 catalog, a clear error.
        assert!(apcbs(Generation::Naples, &facts(Generation::Rome)).is_err());
    }

    #[test]
    fn identical_instances_emit_no_masked_entries() {
        static SAME: &[Instance] = &[Instance {
            index: 1,
            policy: Policy::DEFAULT,
            freq: None,
        }];
        let mut f = facts(Generation::Milan);
        // The base board and the instance agree except for the base
        // power-down choice, so exactly the bool class is masked.
        f.instances = SAME;
        let out = apcbs(Generation::Milan, &f).unwrap();
        let apcb = Apcb::load(out[0].bytes.clone().into(), &ApcbIoOptions::default()).unwrap();
        let mut masked = 0;
        for group in apcb.groups().unwrap() {
            for entry in group.entries() {
                if entry.board_instance_mask() == amd_apcb::BoardInstances::from(2u16) {
                    masked += 1;
                }
            }
        }
        assert_eq!(masked, 1, "only the differing class is masked");
    }
}

/// Semantic comparison of a donor APCB and a generated one, for board
/// tests: entries keyed by (group, entry, instance, board mask,
/// priority), token bodies compared as name-to-value maps, headers
/// (revision, sizes, checksum) ignored. Returns human-readable diffs;
/// empty means semantically identical.
#[doc(hidden)]
pub fn semantic_diff(donor: &[u8], generated: &[u8]) -> Vec<String> {
    fn decode(bytes: &[u8]) -> Value {
        let size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let apcb = Apcb::load(bytes[..size].to_vec().into(), &ApcbIoOptions::default())
            .expect("APCB parses");
        serde_json::to_value(&apcb).unwrap()
    }
    fn keyed(v: &Value) -> std::collections::BTreeMap<String, Value> {
        let mut out = std::collections::BTreeMap::new();
        for e in v["entries"].as_array().unwrap() {
            let h = &e["header"];
            let prio: Vec<&str> = [
                "hard_force",
                "high",
                "medium",
                "event_logging",
                "low",
                "normal",
            ]
            .into_iter()
            .filter(|p| h["priority_mask"][p] == true)
            .collect();
            let key = format!(
                "g={} id={} inst={} mask={} prio={prio:?}",
                h["group_id"], h["entry_id"], h["instance_id"], h["board_instance_mask"]
            );
            let mut body = e.clone();
            body.as_object_mut().unwrap().remove("header");
            if let Some(toks) = body.get_mut("tokens") {
                let mut map = serde_json::Map::new();
                for t in toks.as_array().unwrap() {
                    let (cls, b) = t.as_object().unwrap().iter().next().unwrap();
                    if cls == "Unknown" {
                        map.insert(format!("raw{}", b["tag"]), b["value"].clone());
                    } else {
                        let (n, val) = b.as_object().unwrap().iter().next().unwrap();
                        map.insert(n.clone(), val.clone());
                    }
                }
                *toks = Value::Object(map);
            }
            out.insert(key, body);
        }
        out
    }
    fn diff(a: &Value, b: &Value, path: String, out: &mut Vec<String>) {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                for k in x
                    .keys()
                    .chain(y.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    diff(
                        x.get(k).unwrap_or(&Value::Null),
                        y.get(k).unwrap_or(&Value::Null),
                        format!("{path}.{k}"),
                        out,
                    );
                }
            }
            (Value::Array(x), Value::Array(y)) => {
                if x.len() != y.len() {
                    out.push(format!("{path}: len donor={} gen={}", x.len(), y.len()));
                }
                for (i, (u, v)) in x.iter().zip(y.iter()).enumerate() {
                    diff(u, v, format!("{path}[{i}]"), out);
                }
            }
            _ if a != b => out.push(format!("{path}: donor={a} gen={b}")),
            _ => {}
        }
    }
    let (dk, gk) = (keyed(&decode(donor)), keyed(&decode(generated)));
    let mut out = vec![];
    for k in dk
        .keys()
        .chain(gk.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        match (dk.get(k), gk.get(k)) {
            (Some(a), Some(b)) => diff(a, b, k.clone(), &mut out),
            (Some(_), None) => out.push(format!("{k}: only in donor")),
            (None, Some(_)) => out.push(format!("{k}: only in generated")),
            _ => {}
        }
    }
    out
}
