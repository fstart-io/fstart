//! FIT (Flattened Image Tree) parser.

pub mod parser;

pub use parser::{
    FitArch, FitCompression, FitConfig, FitError, FitHash, FitImage, FitImageNode, FitImageType,
};
