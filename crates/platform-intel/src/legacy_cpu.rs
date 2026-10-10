//! CPU-package policy for the legacy, discrete-northbridge Intel flows.
//!
//! These systems can pair one chipset with different CPU packages. Newer
//! integrated platforms keep their CPU selection in their platform binding.
//! Package profiles choose microcode coverage and CAR geometry; the existing
//! model driver still rejects CPUIDs it cannot initialize.

/// Host-clean CPU-package policy, with model operations available in firmware.
pub trait LegacyCpu: 'static {
    const CAR_BASE: u64;
    const CAR_SIZE: u64;
    const MICROCODE_SIGNATURES: &'static [&'static str];

    #[cfg(all(feature = "stage", feature = "mp"))]
    type Driver: fstart_arch::x86::mp::CpuDriver + fstart_arch::x86::cpu::intel::smm::SmmCpu;
    #[cfg(all(feature = "stage", feature = "mp"))]
    fn cpu_driver(pmbase: u32, microcode: Option<&'static [u8]>) -> Self::Driver;
}

const ATOM_MICROCODE: &[&str] = &["06-1c-02", "06-1c-0a"];

/// Diamondville Atom package used by D945GCLF (coreboot socket_441).
pub struct Socket441;

impl LegacyCpu for Socket441 {
    const CAR_BASE: u64 = 0xfefc_0000;
    const CAR_SIZE: u64 = 0x8000;
    const MICROCODE_SIGNATURES: &'static [&'static str] = ATOM_MICROCODE;

    #[cfg(all(feature = "stage", feature = "mp"))]
    type Driver = fstart_arch::x86::cpu::intel::pineview::PineviewCpuDriver;
    #[cfg(all(feature = "stage", feature = "mp"))]
    fn cpu_driver(pmbase: u32, microcode: Option<&'static [u8]>) -> Self::Driver {
        Self::Driver::new(pmbase, microcode)
    }
}

/// Pineview Atom package used by D41S (coreboot socket_FCBGA559).
pub struct Fcbga559;

impl LegacyCpu for Fcbga559 {
    const CAR_BASE: u64 = 0xfefc_0000;
    const CAR_SIZE: u64 = 0x8000;
    const MICROCODE_SIGNATURES: &'static [&'static str] = ATOM_MICROCODE;

    #[cfg(all(feature = "stage", feature = "mp"))]
    type Driver = fstart_arch::x86::cpu::intel::pineview::PineviewCpuDriver;
    #[cfg(all(feature = "stage", feature = "mp"))]
    fn cpu_driver(pmbase: u32, microcode: Option<&'static [u8]>) -> Self::Driver {
        Self::Driver::new(pmbase, microcode)
    }
}

/// LGA775 Core 2 / Celeron model-6FX package policy.
///
/// This is deliberately not a complete LGA775 CPU family: Netburst (F3X/F4X)
/// and Enhanced Core (1067X) need separate model initialization. The default
/// CAR window follows coreboot socket_LGA775; const parameters allow a board
/// to select a larger explicitly budgeted window.
pub struct Lga775Core2<const BASE: u64 = 0xfeff_8000, const SIZE: u64 = 0x8000>;

impl<const BASE: u64, const SIZE: u64> LegacyCpu for Lga775Core2<BASE, SIZE> {
    const CAR_BASE: u64 = BASE;
    const CAR_SIZE: u64 = SIZE;
    const MICROCODE_SIGNATURES: &'static [&'static str] = &[
        "06-0f-02", "06-0f-06", "06-0f-07", "06-0f-0a", "06-0f-0b", "06-0f-0d", "06-16-01",
    ];

    #[cfg(all(feature = "stage", feature = "mp"))]
    type Driver = fstart_arch::x86::cpu::intel::core2_cpu::Core2CpuDriver;
    #[cfg(all(feature = "stage", feature = "mp"))]
    fn cpu_driver(pmbase: u32, microcode: Option<&'static [u8]>) -> Self::Driver {
        Self::Driver::new_desktop(pmbase, microcode)
    }
}

/// Core/Core 2 package profile used by coreboot's X60 and X61 bindings.
///
/// The default CAR window follows socket_m. Const parameters allow a board
/// to retain an explicitly budgeted window, independently of its chipset.
/// Microcode and Core2CpuDriver cover models 6EX (Yonah) and 6FX. Yonah
/// has no long mode, so boards fitted with it must opt in to protected mode.
pub struct SocketM<const BASE: u64 = 0xfefc_0000, const SIZE: u64 = 0x8000>;

impl<const BASE: u64, const SIZE: u64> LegacyCpu for SocketM<BASE, SIZE> {
    const CAR_BASE: u64 = BASE;
    const CAR_SIZE: u64 = SIZE;
    const MICROCODE_SIGNATURES: &'static [&'static str] = &[
        "06-0e-08", "06-0e-0c", "06-0f-02", "06-0f-06", "06-0f-07", "06-0f-0a", "06-0f-0b",
        "06-0f-0d", "06-16-01",
    ];

    #[cfg(all(feature = "stage", feature = "mp"))]
    type Driver = fstart_arch::x86::cpu::intel::core2_cpu::Core2CpuDriver;
    #[cfg(all(feature = "stage", feature = "mp"))]
    fn cpu_driver(pmbase: u32, microcode: Option<&'static [u8]>) -> Self::Driver {
        Self::Driver::new(pmbase, microcode)
    }
}
