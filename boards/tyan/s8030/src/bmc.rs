//! The S8030 BMC: an ASPEED AST2500 on LPC (BOARD_S8030 §4).
//!
//! Early order (AST2500PeiInit `9029f23e`, Rome `ffe6a4c1`; Milan
//! AST25xxPeiInit writes the same values; also the Naples S8026):
//!
//! 1. FCH LPC decodes ([`open_lpc_decodes`]; the KCS window comes from
//!    PeiIpmiBmcInitialize);
//! 2. Super I/O at `0x2e`: SUART1 (LDN 2) at `0x3f8`, active ([`early_init`]);
//! 3. SCU writes through iLPC2AHB ([`SCU_UPDATES`]): unlock, UART1 pins.
//!
//! COM1 is the AST2500 SUART1 at `0x3f8`; the rear COM header reaches it
//! with J15 at 2-3 (default; 1-2 routes the header to FCH UART0 instead)
//! and J55/J56 at 1-2 (default, BMC COM port 1). The vendor leaves console
//! redirection off; fstart uses COM1 as its console ([`CONSOLE`]).
//!
//! The BMC is not waited for here: there is no BMC-ready GPIO on the
//! S8030 (unlike MJ11), and BmcSync (DXE, NetFn 32h cmd 3Dh, up to 120 ×
//! 2 s) is a vendor-tool handshake fstart does not need.

use alloc::vec::Vec;

use embedded_hal::delay::DelayNs;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_core::services::device::{BusDevice, DeviceError};
use fstart_driver_ipmi::Error as IpmiError;
use fstart_driver_ipmi::kcs::{Kcs, Status};
use fstart_driver_superio::aspeed_ast2400::{Ast2400, SCU_BASE, SCU_PROTECTION_KEY};
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_driver_zen::fch::FchHw;
use fstart_driver_zen::fch::lpc::{self, WideIo};
use fstart_platform_sp3::acpi::Board as AcpiBoard;
use tock_registers::interfaces::{Readable, Writeable};

/// Super I/O config port (unlock `a5 a5`, lock `aa`).
pub const SIO_PORT: u16 = 0x2e;
/// COM1 = SUART1.
pub const COM1_PORT: u16 = 0x3f8;
pub const COM1_IRQ: u8 = 4;
pub const BAUD: u32 = 115_200;
/// IPMI KCS data port; status/command at `+1` (AMI default, inferred from
/// the decode window, [`WIDE_IO`]).
pub const KCS_PORT: u16 = 0xca2;
pub const KCS_SPACING: u16 = 1;

/// One FCH wide I/O window: 512 bytes, or 16 with `small`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WideWindow {
    pub which: WideIo,
    pub base: u16,
    pub small: bool,
}

impl WideWindow {
    pub const fn size(&self) -> u16 {
        if self.small { 0x10 } else { 0x200 }
    }
}

/// The wide I/O windows the board opens for the BMC.
///
/// The vendor asks for SIO `0x2e`, COM1 `0x3f8` and the ranges `0xa00`
/// (`0x60` bytes), `0x3e0` (16) and `0x2e0` (32) (AST2500PeiInit table
/// `0xffe6aba4`, `{u16 port, u8 len, .., u32 kind}` × 5, observed bytes),
/// then PeiIpmiBmcInitialize (`ffe738c6`) puts the KCS window in wide I/O 2:
/// D14F3 `0x90` = `0xca0`, `0x74` |= 8 (16 bytes), `0x48` |= bit 25. SIO
/// and COM1 are legacy decodes the FCH reset task already enables (D14F3
/// `0x48` = 7, `0x44` = `0xff03ffd5`), and the reset/HwAcpi tasks take
/// wide I/O 0 for `0xe00`, leaving two windows: the KCS in wide 2 as the
/// vendor, and wide 1 for `0xa00` (holds the AST2500 mailbox, LDN `0xe` at
/// `0xa40`). `0x3e0`/`0x2e0` (purpose unknown; SUART3/4 default to
/// `0x3e8`/`0x2e8`) are not opened.
pub const WIDE_IO: [WideWindow; 2] = [
    WideWindow {
        which: WideIo::W1,
        base: 0xa00,
        small: false,
    },
    WideWindow {
        which: WideIo::W2,
        base: 0xca0,
        small: true,
    },
];

/// Open the BMC decodes on socket 0's FCH, after the FCH reset tasks.
pub fn open_lpc_decodes<H: FchHw>(hw: &H) {
    WIDE_IO
        .iter()
        .for_each(|w| lpc::set_wide_io(hw, w.which, w.base, w.small));
}

/// One BMC SCU read-modify-write: `scu[offset] = scu[offset] & and | or`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScuUpdate {
    pub offset: u32,
    pub and: u32,
    pub or: u32,
}

/// AST2500PeiInit table `0xffe6ab7c` (observed bytes; Milan `0xffdaf9bc`
/// same SCU00 value): unlock, SCU84[23:16] = UART1 pin functions on,
/// SCU90[5:4] cleared.
pub const SCU_UPDATES: [ScuUpdate; 3] = [
    ScuUpdate {
        offset: 0x00,
        and: 0,
        or: SCU_PROTECTION_KEY,
    },
    ScuUpdate {
        offset: 0x84,
        and: 0xff00_ffff,
        or: 0x00ff_0000,
    },
    ScuUpdate {
        offset: 0x90,
        and: 0xffff_ffcf,
        or: 0,
    },
];

/// The Super I/O configuration: COM1 only (AMI `$SIODATA` lists UAR1,
/// iLPC2AHB and the mailbox; no keyboard controller).
pub fn sio_config() -> SuperIoConfig {
    SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: COM1_PORT,
            irq: COM1_IRQ,
            baud_rate: BAUD,
        }),
        ..SuperIoConfig::default()
    }
}

/// The firmware console: COM1 at 115200 8N1.
pub const CONSOLE: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: COM1_PORT as u64,
    },
    clock_freq: 1_843_200,
    baud_rate: BAUD,
};

/// Super I/O and SCU setup, after the LPC decodes are open. Port I/O;
/// hardware only.
pub fn early_init() -> Result<Ast2400, DeviceError> {
    let mut sio = Ast2400::new_at_base(sio_config(), SIO_PORT)?;
    sio.init()?;
    SCU_UPDATES
        .iter()
        .for_each(|u| sio.ahb_update32(SCU_BASE + u.offset, u.and, u.or));
    Ok(sio)
}

/// The BMC's KCS interface.
///
/// # Safety
/// The KCS window ([`open_lpc_decodes`]) is open and nothing else drives it.
pub unsafe fn kcs<D: DelayNs>(delay: D) -> Kcs<D> {
    // SAFETY: forwarded to the caller.
    unsafe { Kcs::new(KCS_PORT, KCS_SPACING, delay) }
}

/// The KCS interface as ACPI sees it (`IPI0001` and SPMI).
pub const IPMI_ACPI: IpmiKcsAcpi = IpmiKcsAcpi {
    port: KCS_PORT,
    spacing: KCS_SPACING as u8,
    uid: 0,
    spec_revision: 0x0200,
};

/// AML for `\_SB.PCI0.LPCB`: the COM1 device.
pub fn lpc_aml() -> Vec<u8> {
    superio_dsdt_aml(&sio_config())
}

/// The board part of the SP3 ACPI inputs; `lpc_aml` is [`lpc_aml`]'s
/// output. The 8042 flag follows the donors' FADT (they set it though the
/// board has no PS/2 port).
pub fn acpi_board(lpc_aml: &[u8]) -> AcpiBoard<'_> {
    AcpiBoard {
        ipmi: Some(IPMI_ACPI),
        i8042: true,
        lpc_aml,
        sb_aml: &[],
        // Vendor Setup "Security Device Support" defaults to Disabled.
        discrete_tpm: false,
    }
}

/// Tyan OEM NetFn and IANA (0x0019FD).
const NETFN_TYAN: u8 = 0x2e;
const TYAN_IANA: [u8; 3] = [0xfd, 0x19, 0x00];
/// The OEM parameter the M.2 query reads.
const M2_PARAM: u8 = 0x34;

/// The second command of the M.2 query after the first (cmd 0) returned
/// `first`: 4 on success, `0x41` when AMI's transport would have returned
/// EFI_DEVICE_ERROR (an error completion code or a short response;
/// PeiIpmiBmcInitialize `ffe73695`), nothing on a transport failure.
fn second_command(first: Result<(), IpmiError>) -> Option<u8> {
    match first {
        Ok(()) => Some(0x04),
        Err(IpmiError::Completion(_) | IpmiError::Short) => Some(0x41),
        Err(IpmiError::Timeout | IpmiError::State(_)) => None,
    }
}

/// Decode the answer data (after the completion code): `{iana[3], status,
/// value}`. Status 1 is an error; value 0 asks for the Gen3 cap, 1 for
/// none. AMI reads a zero-filled 5-byte buffer, so missing bytes count as
/// 0 (a short answer caps).
fn m2_answer(data: &[u8]) -> Option<bool> {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0);
    match (byte(3), byte(4)) {
        (1, _) => None,
        (_, 0) => Some(true),
        (_, 1) => Some(false),
        _ => None,
    }
}

/// The Tyan OEM M.2 query (Rome `ffe45ff1`, Milan `ffd973aa`): NetFn 2Eh
/// cmd 0 `{fd 19 00}`, then cmd 4 (or `0x41`) `{fd 19 00 34}`. `true` when
/// the BMC asks to cap the M.2 links (lanes 64, 68) at Gen3; the vendor
/// also sets PCD `0x1bb`/`0x1f3` to 3 (cap) or 4, which nothing reads. What
/// the BMC setting represents is unknown (§10 Q4). Any failure leaves the
/// links uncapped, as the vendor does.
pub fn m2_gen3_cap<D, P, S>(kcs: &mut Kcs<D, P, S>) -> bool
where
    D: DelayNs,
    P: Readable<T = u8> + Writeable<T = u8>,
    S: Readable<T = u8, R = Status::Register> + Writeable<T = u8, R = Status::Register>,
{
    let mut buffer = [0u8; 16];
    let first = kcs
        .request(NETFN_TYAN, 0x00, &TYAN_IANA, &mut buffer)
        .map(|_| ());
    let Some(command) = second_command(first) else {
        return false;
    };
    let [a, b, c] = TYAN_IANA;
    kcs.request(NETFN_TYAN, command, &[a, b, c, M2_PARAM], &mut buffer)
        .ok()
        .and_then(m2_answer)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m2_query_decisions() {
        assert_eq!(second_command(Ok(())), Some(4));
        assert_eq!(second_command(Err(IpmiError::Completion(0xc1))), Some(0x41));
        assert_eq!(second_command(Err(IpmiError::Timeout)), None);
        assert_eq!(m2_answer(&[0xfd, 0x19, 0x00, 0x00, 0x00]), Some(true));
        assert_eq!(m2_answer(&[0xfd, 0x19, 0x00, 0x00, 0x01]), Some(false));
        assert_eq!(
            m2_answer(&[0xfd, 0x19, 0x00, 0x01, 0x00]),
            None,
            "status error"
        );
        assert_eq!(m2_answer(&[0xfd, 0x19, 0x00, 0x00, 0x07]), None);
        assert_eq!(m2_answer(&[0xfd, 0x19, 0x00]), Some(true), "zero-filled");
    }

    #[test]
    fn acpi_inputs() {
        let aml = lpc_aml();
        // COM1 claims 0x3f8, 8 ports.
        assert!(
            aml.windows(8)
                .any(|w| w == [0x47, 1, 0xf8, 3, 0xf8, 3, 1, 8])
        );
        let board = acpi_board(&aml);
        assert_eq!(board.ipmi.map(|k| (k.port, k.spacing)), Some((0xca2, 1)));
        // The KCS ports sit in a decoded window; the mailbox too.
        let decoded = |port: u16| {
            WIDE_IO
                .iter()
                .any(|w| (w.base..w.base + w.size()).contains(&port))
        };
        assert!(decoded(KCS_PORT) && decoded(KCS_PORT + KCS_SPACING) && decoded(0xa5f));
    }
}
