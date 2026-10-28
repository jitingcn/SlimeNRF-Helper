//! Hardware-independent firmware image and DFU package parsing.
//!
//! Content validation does not authorize a device or authenticate firmware.

pub mod image;
pub mod legacy_dfu;
pub mod secure_dfu;
pub mod zip_envelope;
