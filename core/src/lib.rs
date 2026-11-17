//! GeoSquare V2 grid core, spec 2.0.
//!
//! This crate covers the rules that need no CRS: the GID codec, Int64 packing, integer
//! topology, and spatial column detection. Point-to-cell and polygon work need a CRS
//! and come in a later phase.
//!
//! The reference for every behaviour is the Python oracle in `geosquare-v2`. The
//! conformance vectors in `vectors/spec-2.0` are the executable form of that contract.

pub mod codec;
pub mod detect;
pub mod packing;
pub mod topology;

pub use codec::{Cell, CodecError};
pub use packing::PackingError;
