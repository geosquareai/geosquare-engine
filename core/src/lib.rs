//! GeoSquare V2 grid core, spec 2.0.
//!
//! This crate covers the GID codec, Int64 packing, integer topology, column detection,
//! and the grid CRS: lon/lat to cell, and cell to WGS84 polygon. The CRS rules come from
//! the signed registry pinned in [`domain`]. Parity with PROJ is tested per domain against
//! `conformance/spec-2.0/points.json`.
//!
//! The reference for every behaviour is the Python oracle in `geosquare-v2`. The
//! conformance vectors in `vectors/spec-2.0` are the executable form of that contract.

pub mod codec;
pub mod detect;
pub mod domain;
pub mod geo;
pub mod packing;
pub mod projection;
pub mod topology;

pub use codec::{Cell, CodecError};
pub use geo::GeoError;
pub use packing::PackingError;
