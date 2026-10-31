// SPDX-License-Identifier: MIT OR Apache-2.0
//! The per-generation APCB token catalogs and the mapping from board
//! policy (`fstart_driver_amd::apcb`) to catalog tokens.
//!
//! A catalog is the AGESA reference value of every token of a
//! generation, split by the role the token list plays (the main setup
//! entries and the CBS payload). Boards never name tokens: the encoder
//! starts from the catalog, replaces the values a board's policy
//! covers, and emits the rest untouched. Tokens amd-apcb has no name
//! for travel as [`RawToken`] with the value every donor agrees on.

use std::collections::BTreeMap;

use amd_apcb::{
    BoolToken, ByteToken, DfDramNumaPerSocket, DwordToken, FchConsoleOutMode, FchConsoleSerialPort,
    MemTsmeMode, TOKEN_ENTRY, TokenEntryId, WordToken,
};
use fstart_driver_amd::apcb::{AblSerialPort, NumaPerSocket, Policy, XgmiSpeed};
use fstart_driver_amd::sp3::soc::Generation;

use super::{milan, rome};

/// A token with no upstream name (or no simply constructible value):
/// its APCB class entry id, token key and reference value.
#[derive(Debug, Clone, Copy)]
pub struct RawToken {
    pub class: TokenEntryId,
    pub key: u32,
    pub value: u32,
}

/// One role's reference tokens (setup or CBS), by storage class.
pub struct TokenLists {
    pub bool_: &'static [BoolToken],
    pub byte: &'static [ByteToken],
    pub word: &'static [WordToken],
    pub dword: &'static [DwordToken],
    pub raw: &'static [RawToken],
}

/// Everything generation-specific the encoder needs beyond the tables
/// in `fstart_driver_amd::apcb::tables`.
pub struct Catalog {
    pub setup: &'static TokenLists,
    pub cbs: &'static TokenLists,
    /// The frequency-table "unsupported" sentinel of the generation
    /// (Rome and Milan genuinely differ).
    pub unsupported_speed: amd_apcb::memory::DdrSpeed,
    /// Milan's error-out struct is 116 bytes, Rome's 112.
    pub error_out_116: bool,
}

/// The catalog of a generation. Naples is Zeppelin silicon with an
/// APCB v2 ABI, not a v3 catalog; it has none here.
pub fn catalog(generation: Generation) -> Option<Catalog> {
    use amd_apcb::memory::DdrSpeed;
    match generation {
        Generation::Rome => Some(Catalog {
            setup: &rome::SETUP,
            cbs: &rome::CBS,
            unsupported_speed: DdrSpeed::UnsupportedRome,
            error_out_116: false,
        }),
        Generation::Milan => Some(Catalog {
            setup: &milan::SETUP,
            cbs: &milan::CBS,
            unsupported_speed: DdrSpeed::UnsupportedMilan,
            error_out_116: true,
        }),
        Generation::Naples => None,
    }
}

/// A typed token an override is made of.
pub enum Token {
    Bool(BoolToken),
    Byte(ByteToken),
    Word(WordToken),
    Dword(DwordToken),
}

impl Token {
    fn class_and_entry(&self) -> Result<(TokenEntryId, TOKEN_ENTRY), String> {
        Ok(match self {
            Self::Bool(t) => (
                TokenEntryId::Bool,
                TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
            ),
            Self::Byte(t) => (
                TokenEntryId::Byte,
                TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
            ),
            Self::Word(t) => (
                TokenEntryId::Word,
                TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
            ),
            Self::Dword(t) => (
                TokenEntryId::Dword,
                TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
            ),
        })
    }
}

/// Effective token records by storage class, in wire order.
pub type EffectiveTokens = [(TokenEntryId, Vec<TOKEN_ENTRY>); 4];

fn class_index(class: TokenEntryId) -> Result<usize, String> {
    Ok(match class {
        TokenEntryId::Bool => 0,
        TokenEntryId::Byte => 1,
        TokenEntryId::Word => 2,
        TokenEntryId::Dword => 3,
        TokenEntryId::Unknown(id) => {
            return Err(format!("unknown token entry class {id:#x}"));
        }
    })
}

/// The effective token records of a list after `overrides`, sorted
/// ascending by key within each class (the token entries' context
/// format). Overrides replace by token key and add keys the reference
/// list does not carry.
pub fn effective(lists: &TokenLists, overrides: &[Token]) -> Result<EffectiveTokens, String> {
    effective_inner(lists, overrides, false)
}

/// Like [`effective`], but overrides only change tokens the list
/// already carries. The CBS payloads are subsets of the setup token
/// universe by construction: a board policy value applies where CBS
/// has the token, and does not add it where it does not.
pub fn effective_replace_only(
    lists: &TokenLists,
    overrides: &[Token],
) -> Result<EffectiveTokens, String> {
    effective_inner(lists, overrides, true)
}

fn effective_inner(
    lists: &TokenLists,
    overrides: &[Token],
    replace_only: bool,
) -> Result<EffectiveTokens, String> {
    let mut by_class: [BTreeMap<u32, TOKEN_ENTRY>; 4] = Default::default();
    fn insert(
        by_class: &mut [BTreeMap<u32, TOKEN_ENTRY>; 4],
        class: TokenEntryId,
        entry: TOKEN_ENTRY,
    ) -> Result<(), String> {
        by_class[class_index(class)?].insert(entry.key.get(), entry);
        Ok(())
    }
    for t in lists.bool_ {
        insert(
            &mut by_class,
            TokenEntryId::Bool,
            TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
        )?;
    }
    for t in lists.byte {
        insert(
            &mut by_class,
            TokenEntryId::Byte,
            TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
        )?;
    }
    for t in lists.word {
        insert(
            &mut by_class,
            TokenEntryId::Word,
            TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
        )?;
    }
    for t in lists.dword {
        insert(
            &mut by_class,
            TokenEntryId::Dword,
            TOKEN_ENTRY::try_from(t).map_err(|e| format!("{e:?}"))?,
        )?;
    }
    for t in lists.raw {
        let entry = TOKEN_ENTRY {
            key: t.key.into(),
            value: t.value.into(),
        };
        insert(&mut by_class, t.class, entry)?;
    }
    let present: std::collections::BTreeSet<(usize, u32)> = by_class
        .iter()
        .enumerate()
        .flat_map(|(class, m)| m.keys().map(move |&k| (class, k)))
        .collect();
    for t in overrides {
        let (class, entry) = t.class_and_entry()?;
        if replace_only && !present.contains(&(class_index(class)?, entry.key.get())) {
            continue;
        }
        insert(&mut by_class, class, entry)?;
    }
    let [bool_, byte, word, dword] = by_class;
    Ok([
        (TokenEntryId::Bool, bool_.into_values().collect()),
        (TokenEntryId::Byte, byte.into_values().collect()),
        (TokenEntryId::Word, word.into_values().collect()),
        (TokenEntryId::Dword, dword.into_values().collect()),
    ])
}

/// The tokens a board policy becomes. Every field of
/// [`Policy`] names its token in its own docs; this is the one place
/// that knows the correspondence, including the generation split.
/// Fields left `None` produce no token: the catalog's reference
/// value for the generation stands.
pub fn policy_tokens(policy: &Policy, generation: Generation) -> Result<Vec<Token>, String> {
    let m = &policy.mem;
    let mut out: Vec<Token> = vec![];
    let mut bool_tok = |v: Option<bool>, f: fn(bool) -> BoolToken| {
        if let Some(v) = v {
            out.push(Token::Bool(f(v)));
        }
    };
    bool_tok(m.ecc, BoolToken::MemEnableEccFeature);
    bool_tok(m.parity, BoolToken::MemEnableParity);
    bool_tok(m.ecc_sync_flood, BoolToken::MemEccSyncFlood);
    bool_tok(m.ecc_redirection, BoolToken::MemEccRedirection);
    bool_tok(
        m.uncorrected_ecc_retry,
        BoolToken::MemUncorrectedEccRetryDdr4,
    );
    bool_tok(m.channel_interleaving, BoolToken::MemChannelInterleaving);
    bool_tok(
        m.chip_select_interleaving,
        BoolToken::MemEnableChipSelectInterleaving,
    );
    bool_tok(m.bank_swizzle, BoolToken::MemEnableBankSwizzle);
    bool_tok(m.bank_group_swap, BoolToken::MemEnableBankGroupSwap);
    bool_tok(m.bank_group_swap_alt, BoolToken::MemEnableBankGroupSwapAlt);
    bool_tok(m.mode_unganged, BoolToken::MemModeUnganged);
    bool_tok(m.hole_remapping, BoolToken::MemHoleRemapping);
    bool_tok(m.limit_below_1tib, BoolToken::MemLimitMemoryToBelow1TiB);
    bool_tok(m.power_down, BoolToken::MemEnablePowerDown);
    bool_tok(m.dqs_training_control, BoolToken::MemDqsTrainingControl);
    bool_tok(m.ignore_spd_checksum, BoolToken::MemIgnoreSpdChecksum);
    bool_tok(
        m.spd_read_optimization,
        BoolToken::MemSpdReadOptimizationDdr4,
    );
    bool_tok(m.clear, BoolToken::MemClear);
    bool_tok(m.restore_control, BoolToken::MemRestoreControl);
    bool_tok(m.pstate, BoolToken::MemPstate);
    bool_tok(m.on_die_thermal, BoolToken::MemOnDieThermalSensor);
    if let Some(d) = m.dimm_types {
        out.push(Token::Bool(BoolToken::MemUdimmCapable(d.udimm)));
        out.push(Token::Bool(BoolToken::MemSodimmCapable(d.sodimm)));
        out.push(Token::Bool(BoolToken::MemRdimmCapable(d.rdimm)));
        out.push(Token::Bool(BoolToken::MemLrdimmCapable(d.lrdimm)));
        out.push(Token::Bool(BoolToken::MemQuadRankCapable(d.quad_rank)));
    }
    match (generation, m.tsme) {
        (Generation::Rome, Some(tsme)) => {
            out.push(Token::Byte(ByteToken::MemTsmeModeRome(if tsme {
                MemTsmeMode::Enabled
            } else {
                MemTsmeMode::Disabled
            })))
        }
        (Generation::Milan, Some(tsme)) => out.push(Token::Bool(BoolToken::MemTsmeModeMilan(tsme))),
        (Generation::Naples, _) => return Err("Naples has no APCB v3 catalog".into()),
        _ => {}
    }
    if let Some(port) = policy.abl_serial_port {
        out.push(Token::Byte(ByteToken::FchConsoleOutSerialPort(
            match port {
                AblSerialPort::SuperIo => FchConsoleSerialPort::SuperIo,
                AblSerialPort::Uart0Mmio => FchConsoleSerialPort::Uart0Mmio,
                AblSerialPort::Uart1Mmio => FchConsoleSerialPort::Uart1Mmio,
            },
        )));
    }
    if let Some(on) = policy.abl_console_out {
        out.push(Token::Byte(ByteToken::FchConsoleOutMode(if on {
            FchConsoleOutMode::Enabled
        } else {
            FchConsoleOutMode::Disabled
        })));
    }
    if let Some(numa) = policy.numa_per_socket {
        out.push(Token::Byte(ByteToken::DfDramNumaPerSocket(match numa {
            NumaPerSocket::Auto => DfDramNumaPerSocket::Auto,
            NumaPerSocket::One => DfDramNumaPerSocket::One,
            NumaPerSocket::Two => DfDramNumaPerSocket::Two,
            NumaPerSocket::Four => DfDramNumaPerSocket::Four,
        })));
    }
    if let Some(v) = policy.bmc_socket {
        out.push(Token::Byte(ByteToken::BmcSocket(v)));
    }
    if let Some(v) = policy.bmc_start_lane {
        out.push(Token::Byte(ByteToken::BmcStartLane(v)));
    }
    if let Some(v) = policy.bmc_end_lane {
        out.push(Token::Byte(ByteToken::BmcEndLane(v)));
    }
    if let Some(speed) = policy.xgmi_4link_speed {
        out.push(Token::Byte(ByteToken::Df4LinkMaxXgmiSpeed(match speed {
            XgmiSpeed::Auto => amd_apcb::DfXgmi4LinkMaxSpeed::Auto,
            rate => {
                use num_traits::FromPrimitive;
                // The PHY's encoded steps (DfXgmiLinkMaxSpeed).
                let mbps = rate.mbps().unwrap();
                let code: u8 = match mbps {
                    6400 => 0,
                    7467 => 1,
                    8533 => 2,
                    9600 => 3,
                    10667 => 4,
                    11000..=32000 => (mbps / 1000 - 6) as u8,
                    _ => unreachable!("XgmiSpeed only has encodable variants"),
                };
                amd_apcb::DfXgmi4LinkMaxSpeed::from_u8(code)
                    .ok_or_else(|| format!("xGMI speed encoding {code}"))?
            }
        })));
    }
    Ok(out)
}

/// The token overrides of the board wiring facts that are not policy.
pub fn fact_tokens(facts: &fstart_driver_amd::apcb::ApcbFacts) -> Vec<Token> {
    vec![
        Token::Dword(DwordToken::FchRom3BaseHigh(facts.rom3_base_high)),
        Token::Byte(ByteToken::PcieResetPinSelect(facts.pcie_reset_pin_select)),
        Token::Word(WordToken::Dimm3DsSensorUpper(facts.sensor_3ds_upper)),
    ]
}
