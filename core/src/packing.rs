//! Signed Int64 packing (spec 2.0, section 7).
//!
//! Layout: bit 63 is 0, bits 62 to 54 hold the domain ID (1 to 511), bits 53 to 50 hold
//! the level, bits 49 to 1 hold the path, and bit 0 is 0. The path packs one code per
//! level, coarse level first: 5 bits for a 5x5 level and 2 bits for a 2x2 level.

use crate::codec::{self, Cell, CodecError, MAX_LEVEL, SUBDIVISIONS};

/// Smallest valid domain ID.
pub const DOMAIN_ID_MIN: u16 = 1;
/// Largest valid domain ID (9 bits).
pub const DOMAIN_ID_MAX: u16 = 511;

const PATH_MASK: u64 = (1u64 << 49) - 1;

/// Errors from packing and unpacking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackingError {
    InvalidDomain(u16),
    InvalidCell(CodecError),
    InvalidPacked(i64),
}

impl From<CodecError> for PackingError {
    fn from(error: CodecError) -> Self {
        PackingError::InvalidCell(error)
    }
}

fn width(step: u64) -> u32 {
    if step == 5 {
        5
    } else {
        2
    }
}

/// Number of path bits used by a cell at `level`.
pub fn path_bits(level: u8) -> u32 {
    SUBDIVISIONS[..level as usize]
        .iter()
        .map(|&step| width(step))
        .sum()
}

/// Pack a domain and a cell into the signed Int64 value.
pub fn pack(domain_id: u16, cell: Cell) -> Result<i64, PackingError> {
    if !(DOMAIN_ID_MIN..=DOMAIN_ID_MAX).contains(&domain_id) {
        return Err(PackingError::InvalidDomain(domain_id));
    }
    codec::validate(cell)?;
    let level = cell.level as usize;
    let (mut x, mut y) = (cell.x_idx, cell.y_idx);
    let mut codes = Vec::with_capacity(level);
    for &step in SUBDIVISIONS[..level].iter().rev() {
        codes.push((y % step) * step + (x % step));
        x /= step;
        y /= step;
    }
    codes.reverse();

    let mut path: u64 = 0;
    for (&step, &code) in SUBDIVISIONS[..level].iter().zip(codes.iter()) {
        path = (path << width(step)) | code;
    }
    let value = ((domain_id as u64) << 54) | ((cell.level as u64) << 50) | (path << 1);
    Ok(value as i64)
}

/// Unpack a signed Int64 value into `(domain_id, cell)`. Any inconsistency is an error.
///
/// The value must re-pack to exactly the same number. This rejects unused path bits and
/// child codes outside the valid range.
pub fn unpack(value: i64) -> Result<(u16, Cell), PackingError> {
    let reject = || PackingError::InvalidPacked(value);
    if value < 0 || value & 1 == 1 {
        return Err(reject());
    }
    let bits = value as u64;
    let domain_id = ((bits >> 54) & 0x1FF) as u16;
    let level = ((bits >> 50) & 0xF) as u8;
    if !(DOMAIN_ID_MIN..=DOMAIN_ID_MAX).contains(&domain_id) || level > MAX_LEVEL {
        return Err(reject());
    }
    let path = (bits >> 1) & PATH_MASK;
    let total = path_bits(level);
    if path >> total != 0 {
        return Err(reject());
    }

    let (mut x, mut y) = (0u64, 0u64);
    let mut remaining = total;
    for &step in SUBDIVISIONS[..level as usize].iter() {
        let w = width(step);
        remaining -= w;
        let code = (path >> remaining) & ((1u64 << w) - 1);
        if code >= step * step {
            return Err(reject());
        }
        let row = code / step;
        let column = code % step;
        x = x * step + column;
        y = y * step + row;
    }

    let cell = Cell {
        level,
        x_idx: x,
        y_idx: y,
    };
    match pack(domain_id, cell) {
        Ok(repacked) if repacked == value => Ok((domain_id, cell)),
        _ => Err(reject()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_round_trip() {
        let cell = Cell {
            level: 12,
            x_idx: 475168,
            y_idx: 491792,
        };
        let value = pack(1, cell).unwrap();
        assert!(value > 0);
        assert_eq!(unpack(value), Ok((1, cell)));
    }

    #[test]
    fn rejects_bad_domain_and_bits() {
        let cell = Cell {
            level: 1,
            x_idx: 0,
            y_idx: 0,
        };
        assert_eq!(pack(0, cell), Err(PackingError::InvalidDomain(0)));
        assert!(unpack(1 << 50).is_err()); // domain 0
        assert!(unpack(pack(1, cell).unwrap() | 1).is_err()); // reserved bit
        assert!(unpack(pack(1, cell).unwrap() | (1 << 20)).is_err()); // unused path bits
    }
}
