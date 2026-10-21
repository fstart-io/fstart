//! PSP (MP0) interfaces the x86 side uses on SP3 (PSP_APOB, FIRMWARE_IMAGE).
//!
//! - [`mailbox`]: the C2P (CPU to PSP) mailbox with the per-generation
//!   precheck and status rules, over an MMIO or SMN transport.
//! - [`command`]: typed C2P command buffers (SmmInfo, BootDone, LockDFReg,
//!   LockFCHReg, the Milan DRTM pair, capability queries, PSB disable).
//! - [`bar`]: the PSP BAR on IOHC0 and the `PSP_ADDR` MSR value.
//! - [`directory`]: EFS, combo and PSP/BIOS directory reader (reset image,
//!   APOB destination, microcode, PMU, APCB).
//!
//! None of the commands keeps the PSP from booting (PSP_APOB §3.3): every
//! sender in AGESA is gated by a PCD and the boot does not depend on them.

pub mod bar;
pub mod command;
pub mod directory;
pub mod mailbox;

pub use command::{C2pBuffer, Command};
pub use mailbox::{C2pTransport, Mailbox, MmioTransport, PspError, SmnTransport};
