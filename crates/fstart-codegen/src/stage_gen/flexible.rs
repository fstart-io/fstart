//! Flexible-mode service enum dispatch.
//!
//! In Flexible build mode, devices that implement the same service trait are
//! wrapped in a generated enum for runtime dispatch (without trait objects or
//! alloc). This module contains the service trait metadata, helper functions
//! for resolving devices to their enum wrappers, and the codegen functions
//! that emit the enum definitions and trait implementations.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_drivers::{DriverInstance, DriverMeta};
use fstart_types::DeviceConfig;

// =======================================================================
// Service trait metadata
// =======================================================================

/// Description of a service trait's methods for enum dispatch codegen.
pub(super) struct ServiceMethod {
    /// Method signature (without the `&self`)
    /// e.g., "fn write_byte(&self, byte: u8) -> Result<(), ServiceError>"
    signature: &'static str,
    /// How to call the inner variant
    /// e.g., "d.write_byte(byte)"
    delegation: &'static str,
}

/// Distinguishes fstart-native traits from embedded-hal traits for codegen.
pub(super) enum TraitKind {
    /// fstart-native trait with simple method delegation.
    Native { methods: &'static [ServiceMethod] },
    /// embedded-hal I2C trait (`I2c` + `ErrorType`).
    EmbeddedI2c,
    /// embedded-hal SPI trait (`SpiBus` + `ErrorType`).
    EmbeddedSpi,
}

/// Full description of a service trait for enum codegen.
pub(super) struct ServiceTraitInfo {
    /// RON-level service name (e.g., "I2cBus"). Matches the value in
    /// `DeviceConfig::services`.
    pub name: &'static str,
    /// Generated enum name (e.g., "ConsoleDevice")
    pub enum_name: &'static str,
    /// What kind of trait this is and how to dispatch.
    kind: TraitKind,
    /// Accessor name on StageContext (e.g., "console")
    pub accessor: &'static str,
}

impl ServiceTraitInfo {
    /// The Rust trait name to use in generated code (e.g., `I2c` not `I2cBus`).
    pub fn rust_trait_name(&self) -> &'static str {
        match &self.kind {
            TraitKind::Native { .. } => self.name,
            TraitKind::EmbeddedI2c => "I2c",
            TraitKind::EmbeddedSpi => "SpiBus",
        }
    }

    /// Whether the StageContext accessor needs `&mut self` (embedded-hal
    /// traits take `&mut self`).
    pub fn is_mut_accessor(&self) -> bool {
        matches!(self.kind, TraitKind::EmbeddedI2c | TraitKind::EmbeddedSpi)
    }
}

/// Known service traits and their methods for enum dispatch generation.
pub(super) const SERVICE_TRAITS: &[ServiceTraitInfo] = &[
    ServiceTraitInfo {
        name: "Console",
        enum_name: "ConsoleDevice",
        kind: TraitKind::Native {
            methods: &[
                ServiceMethod {
                    signature: "fn write_byte(&self, byte: u8) -> Result<(), ServiceError>",
                    delegation: "d.write_byte(byte)",
                },
                ServiceMethod {
                    signature: "fn read_byte(&self) -> Result<Option<u8>, ServiceError>",
                    delegation: "d.read_byte()",
                },
            ],
        },
        accessor: "console",
    },
    ServiceTraitInfo {
        name: "BlockDevice",
        enum_name: "BlockDeviceEnum",
        kind: TraitKind::Native {
            methods: &[
                ServiceMethod {
                    signature:
                        "fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError>",
                    delegation: "d.read(offset, buf)",
                },
                ServiceMethod {
                    signature:
                        "fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, ServiceError>",
                    delegation: "d.write(offset, buf)",
                },
                ServiceMethod {
                    signature: "fn size(&self) -> u64",
                    delegation: "d.size()",
                },
            ],
        },
        accessor: "block_device",
    },
    ServiceTraitInfo {
        name: "Timer",
        enum_name: "TimerDevice",
        kind: TraitKind::Native {
            methods: &[
                ServiceMethod {
                    signature: "fn delay_us(&self, us: u64)",
                    delegation: "d.delay_us(us)",
                },
                ServiceMethod {
                    signature: "fn timestamp_us(&self) -> u64",
                    delegation: "d.timestamp_us()",
                },
            ],
        },
        accessor: "timer",
    },
    ServiceTraitInfo {
        name: "I2cBus",
        enum_name: "I2cBusDevice",
        kind: TraitKind::EmbeddedI2c,
        accessor: "i2c_bus",
    },
    ServiceTraitInfo {
        name: "SpiBus",
        enum_name: "SpiBusDevice",
        kind: TraitKind::EmbeddedSpi,
        accessor: "spi_bus",
    },
    ServiceTraitInfo {
        name: "GpioController",
        enum_name: "GpioControllerDevice",
        kind: TraitKind::Native {
            methods: &[
                ServiceMethod {
                    signature: "fn get(&self, pin: u32) -> Result<bool, ServiceError>",
                    delegation: "d.get(pin)",
                },
                ServiceMethod {
                    signature: "fn set(&self, pin: u32, value: bool) -> Result<(), ServiceError>",
                    delegation: "d.set(pin, value)",
                },
                ServiceMethod {
                    signature:
                        "fn set_direction(&self, pin: u32, output: bool) -> Result<(), ServiceError>",
                    delegation: "d.set_direction(pin, output)",
                },
            ],
        },
        accessor: "gpio",
    },
];

// =======================================================================
// Helpers
// =======================================================================

/// Find a ServiceTraitInfo by trait name.
fn find_service_trait(name: &str) -> Option<&'static ServiceTraitInfo> {
    SERVICE_TRAITS.iter().find(|s| s.name == name)
}

/// For a given service trait, collect all unique [`DriverMeta`] entries
/// from the board's devices that provide that service.
fn drivers_for_service<'a>(
    devices: &[DeviceConfig],
    instances: &'a [DriverInstance],
    service_name: &str,
) -> Vec<&'a DriverMeta> {
    let mut result: Vec<&DriverMeta> = Vec::new();
    let mut seen_types: Vec<&str> = Vec::new();

    for (dev, inst) in devices.iter().zip(instances.iter()) {
        // Check if this device declares the service
        if !dev.services.iter().any(|s| s.as_str() == service_name) {
            continue;
        }
        let meta = inst.meta();
        // Also verify the driver actually implements this service
        if meta.services.contains(&service_name) && !seen_types.contains(&meta.type_name) {
            seen_types.push(meta.type_name);
            result.push(meta);
        }
    }
    result
}

/// Collect all service trait names that are used by at least one device.
fn active_services(devices: &[DeviceConfig]) -> Vec<&'static str> {
    let mut result: Vec<&'static str> = Vec::new();
    for svc in SERVICE_TRAITS {
        if devices
            .iter()
            .any(|d| d.services.iter().any(|s| s.as_str() == svc.name))
        {
            result.push(svc.name);
        }
    }
    result
}

/// In flexible mode, find the enum type name for a device based on its first
/// service. Returns the enum variant name (driver type name) and the enum
/// type name.
pub(super) fn flexible_enum_for_device(
    dev: &DeviceConfig,
    inst: &DriverInstance,
) -> Option<(&'static str, &'static str)> {
    let meta = inst.meta();
    // Find the first service this device provides that has a service enum
    for svc_str in &dev.services {
        if let Some(svc_info) = find_service_trait(svc_str.as_str()) {
            return Some((svc_info.enum_name, meta.type_name));
        }
    }
    None
}

// =======================================================================
// Code generation — flexible enums
// =======================================================================

/// Generate service enum types and their trait implementations for flexible mode.
pub(super) fn generate_flexible_enums(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> TokenStream {
    let services = active_services(devices);
    let mut tokens = TokenStream::new();

    for svc_name in &services {
        let Some(svc_info) = find_service_trait(svc_name) else {
            continue;
        };

        let drivers = drivers_for_service(devices, instances, svc_name);
        if drivers.is_empty() {
            continue;
        }

        let enum_name = format_ident!("{}", svc_info.enum_name);

        // Generate the enum (same for all trait kinds)
        let variants = drivers.iter().map(|meta| {
            let variant = format_ident!("{}", meta.type_name);
            quote! { #variant(#variant) }
        });

        let doc = format!(
            "Enum dispatch for {} service (Flexible mode).",
            svc_info.name
        );
        tokens.extend(quote! {
            #[doc = #doc]
            #[allow(dead_code)]
            enum #enum_name {
                #(#variants,)*
            }
        });

        // Generate trait impl based on kind
        match &svc_info.kind {
            TraitKind::Native { methods } => {
                tokens.extend(generate_native_enum_impl(svc_info, &drivers, methods));
            }
            TraitKind::EmbeddedI2c => {
                tokens.extend(generate_embedded_i2c_enum_impl(svc_info, &drivers));
            }
            TraitKind::EmbeddedSpi => {
                tokens.extend(generate_embedded_spi_enum_impl(svc_info, &drivers));
            }
        }
    }

    tokens
}

/// Generate native trait impl for a Flexible-mode enum (Console, Timer, etc.).
fn generate_native_enum_impl(
    svc_info: &ServiceTraitInfo,
    drivers: &[&DriverMeta],
    methods: &[ServiceMethod],
) -> TokenStream {
    let enum_name = format_ident!("{}", svc_info.enum_name);
    let trait_name = format_ident!("{}", svc_info.name);

    let method_impls = methods.iter().map(|method| {
        let sig: TokenStream = method.signature.parse().unwrap();
        let del: TokenStream = method.delegation.parse().unwrap();
        let arms = drivers.iter().map(|meta| {
            let variant = format_ident!("{}", meta.type_name);
            quote! { Self::#variant(d) => #del, }
        });
        quote! {
            #sig {
                match self {
                    #(#arms)*
                }
            }
        }
    });

    quote! {
        impl #trait_name for #enum_name {
            #(#method_impls)*
        }
    }
}

/// Generate `ErrorType` + `I2c` impls for a Flexible-mode enum.
///
/// NOTE: The enum's error type is hardcoded to `I2cErrorKind`
/// (`embedded_hal::i2c::ErrorKind`). All I2C driver variants must also use
/// `ErrorKind` as their `ErrorType::Error`. This is the standard type-erased
/// error in embedded-hal and should work for all MMIO-based controller
/// drivers. If a future driver needs a custom error type, this function
/// would need to be extended with error conversion logic.
fn generate_embedded_i2c_enum_impl(
    svc_info: &ServiceTraitInfo,
    drivers: &[&DriverMeta],
) -> TokenStream {
    let enum_name = format_ident!("{}", svc_info.enum_name);

    let transaction_arms = drivers.iter().map(|meta| {
        let variant = format_ident!("{}", meta.type_name);
        quote! { Self::#variant(d) => d.transaction(address, operations), }
    });

    quote! {
        impl I2cErrorType for #enum_name {
            type Error = I2cErrorKind;
        }

        impl I2c for #enum_name {
            fn transaction(
                &mut self,
                address: u8,
                operations: &mut [I2cOperation<'_>],
            ) -> Result<(), Self::Error> {
                match self {
                    #(#transaction_arms)*
                }
            }
        }
    }
}

/// Generate `ErrorType` + `SpiBus` impls for a Flexible-mode enum.
fn generate_embedded_spi_enum_impl(
    svc_info: &ServiceTraitInfo,
    drivers: &[&DriverMeta],
) -> TokenStream {
    let enum_name = format_ident!("{}", svc_info.enum_name);

    // Helper: generate match arms that delegate to variant `d`
    let make_arms = |delegation: &str| -> Vec<TokenStream> {
        let del: TokenStream = delegation.parse().unwrap();
        drivers
            .iter()
            .map(|meta| {
                let variant = format_ident!("{}", meta.type_name);
                quote! { Self::#variant(d) => #del, }
            })
            .collect()
    };

    let read_arms = make_arms("d.read(words)");
    let write_arms = make_arms("d.write(words)");
    let transfer_arms = make_arms("d.transfer(read, write)");
    let transfer_ip_arms = make_arms("d.transfer_in_place(words)");
    let flush_arms = make_arms("d.flush()");

    quote! {
        impl SpiErrorType for #enum_name {
            type Error = SpiErrorKind;
        }

        impl SpiBus for #enum_name {
            fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
                match self { #(#read_arms)* }
            }
            fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
                match self { #(#write_arms)* }
            }
            fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
                match self { #(#transfer_arms)* }
            }
            fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
                match self { #(#transfer_ip_arms)* }
            }
            fn flush(&mut self) -> Result<(), Self::Error> {
                match self { #(#flush_arms)* }
            }
        }
    }
}

/// Generate the enum wrapping for a device after init (Flexible mode only).
pub(super) fn generate_flexible_wrapping(dev: &DeviceConfig, inst: &DriverInstance) -> TokenStream {
    let name_str = dev.name.as_str();
    if let Some((enum_name_str, variant_name_str)) = flexible_enum_for_device(dev, inst) {
        let name = format_ident!("{}", name_str);
        let enum_name = format_ident!("{}", enum_name_str);
        let variant_name = format_ident!("{}", variant_name_str);
        let inner = format_ident!("_{}_inner", name_str);
        quote! { let #name = #enum_name::#variant_name(#inner); }
    } else {
        TokenStream::new()
    }
}
