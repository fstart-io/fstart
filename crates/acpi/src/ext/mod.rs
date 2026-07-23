//! Extended AML operations not provided by `acpi_tables`.
//!
//! Each sub-module adds AML constructs that implement the [`Aml`] trait,
//! extending the upstream crate's coverage for firmware use cases.
//!
//! [`Aml`]: acpi_tables::Aml

pub mod break_op;
pub mod cond_ref_of;
pub mod inc_dec;
pub mod logical;
pub mod sleep;
pub mod thermal_zone;
