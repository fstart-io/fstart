//! Measurement block: record what the boot ran.

/// TCG event types (PFP 1.06 §10.4.1; the values below `0x8000_0000` are
/// shared with the TPM 1.2 specifications).
pub mod kind {
    pub const EV_POST_CODE: u32 = 0x01;
    pub const EV_NO_ACTION: u32 = 0x03;
    pub const EV_SEPARATOR: u32 = 0x04;
    pub const EV_ACTION: u32 = 0x05;
    pub const EV_EVENT_TAG: u32 = 0x06;
    pub const EV_S_CRTM_CONTENTS: u32 = 0x07;
    pub const EV_S_CRTM_VERSION: u32 = 0x08;
    pub const EV_PLATFORM_CONFIG_FLAGS: u32 = 0x0A;
    pub const EV_NONHOST_CODE: u32 = 0x0F;
    pub const EV_NONHOST_CONFIG: u32 = 0x10;
    pub const EV_NONHOST_INFO: u32 = 0x11;
    pub const EV_POST_CODE2: u32 = 0x13;
    pub const EV_EFI_ACTION: u32 = 0x8000_0007;
    pub const EV_EFI_PLATFORM_FIRMWARE_BLOB: u32 = 0x8000_0008;
    pub const EV_EFI_PLATFORM_FIRMWARE_BLOB2: u32 = 0x8000_000A;
    pub const EV_EFI_HANDOFF_TABLES2: u32 = 0x8000_000B;
}

/// `BlobDescription` strings of `EV_POST_CODE2` (PFP table 13).
pub mod post_code {
    pub const POST_CODE: &str = "POST CODE";
    pub const SMM_CODE: &str = "SMM CODE";
    pub const ACPI_DATA: &str = "ACPI DATA";
    pub const EMBEDDED_DRIVER: &str = "Embedded UEFI Driver";
}

/// What a measurement is logged as.
#[derive(Clone, Copy, Debug)]
pub struct Event<'a> {
    /// The PCR the bytes extend.
    pub pcr: u32,
    /// The TCG event type (TPM 2.0 numbering).
    pub kind: u32,
    /// What the bytes are: an FFS file name or a firmware blob description.
    pub description: &'a str,
    /// Where the bytes live for whoever checks the log: the flash address
    /// of an FFS file, 0 for a RAM copy without one.
    pub base: u64,
}

/// A record of what the boot ran: a TPM's PCRs and their event log.
///
/// Measure the bytes that run, not a copy of them: the loading chain
/// (`fstart_load`) measures the bytes it verified and then uses, so nothing
/// can change them in between.
pub trait Measure: Sync {
    /// Extend `event.pcr` with `bytes` and log it as `event`.
    fn measure(&self, event: &Event<'_>, bytes: &[u8]);
}

/// A boot that measures nothing: no TPM, or measured boot is off.
pub struct Unmeasured;

impl Measure for Unmeasured {
    fn measure(&self, _: &Event<'_>, _: &[u8]) {}
}

/// Where and as what a load is measured.
#[derive(Clone, Copy)]
pub struct Measurement<'a> {
    pub log: &'a dyn Measure,
    pub event: Event<'a>,
}

impl Measurement<'_> {
    /// A load that is not measured.
    pub const NONE: Measurement<'static> = Measurement {
        log: &Unmeasured,
        event: Event {
            pcr: 0,
            kind: 0,
            description: "",
            base: 0,
        },
    };

    /// Measure `bytes` as this event.
    pub fn record(&self, bytes: &[u8]) {
        self.log.measure(&self.event, bytes);
    }
}
