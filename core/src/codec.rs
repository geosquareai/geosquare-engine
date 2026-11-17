//! Canonical indices and GID codec (spec 2.0, sections 3, 5 and 6).
//!
//! A cell is `(level, x_idx, y_idx)` inside one domain grid. Its GID has one character
//! per level. Odd levels use a 5x5 alphabet and even levels use a 2x2 alphabet.

use std::fmt;

/// Root square side in metres, shared by every domain.
pub const ROOT_SIDE_M: f64 = 50_000_000.0;
/// Deepest level in spec 2.0.
pub const MAX_LEVEL: u8 = 14;
/// Refinement factor per level: `[5, 2, 5, 2, ...]`.
pub const SUBDIVISIONS: [u64; 14] = [5, 2, 5, 2, 5, 2, 5, 2, 5, 2, 5, 2, 5, 2];

/// Characters for 5x5 levels. Index is `row * 5 + column`.
const BASE5: &[u8; 25] = b"23456789CEFGHJLMNPQRTVWXY";
/// Characters for 2x2 levels. Index is `row * 2 + column`.
const BASE2: &[u8; 4] = b"2345";

/// A cell inside one domain grid. The domain is not part of this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cell {
    pub level: u8,
    pub x_idx: u64,
    pub y_idx: u64,
}

/// Errors from the codec. Invalid input is always an error, never a repaired value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    InvalidLevel(u8),
    IndexOutOfRange { level: u8, x_idx: u64, y_idx: u64 },
    InvalidGid(String),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::InvalidLevel(level) => write!(f, "level {level} is outside 0..=14"),
            CodecError::IndexOutOfRange {
                level,
                x_idx,
                y_idx,
            } => {
                write!(
                    f,
                    "indices ({x_idx}, {y_idx}) are outside the level {level} grid"
                )
            }
            CodecError::InvalidGid(gid) => write!(f, "GID {gid:?} is not a valid V2 GID"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Number of cells along one root edge at `level`.
pub fn side_cell_count(level: u8) -> Result<u64, CodecError> {
    if level > MAX_LEVEL {
        return Err(CodecError::InvalidLevel(level));
    }
    Ok(SUBDIVISIONS[..level as usize].iter().product())
}

/// Cell side length in metres at `level`.
pub fn cell_side_m(level: u8) -> Result<f64, CodecError> {
    Ok(ROOT_SIDE_M / side_cell_count(level)? as f64)
}

/// Check that a cell is inside the grid. Returns the cells per edge at its level.
pub fn validate(cell: Cell) -> Result<u64, CodecError> {
    let n = side_cell_count(cell.level)?;
    if cell.x_idx >= n || cell.y_idx >= n {
        return Err(CodecError::IndexOutOfRange {
            level: cell.level,
            x_idx: cell.x_idx,
            y_idx: cell.y_idx,
        });
    }
    Ok(n)
}

fn alphabet(step: u64) -> &'static [u8] {
    if step == 5 {
        &BASE5[..]
    } else {
        &BASE2[..]
    }
}

/// Encode a cell as its GID. Level 0 gives the empty string.
pub fn encode(cell: Cell) -> Result<String, CodecError> {
    validate(cell)?;
    let level = cell.level as usize;
    let (mut x, mut y) = (cell.x_idx, cell.y_idx);
    let mut chars = Vec::with_capacity(level);
    for &step in SUBDIVISIONS[..level].iter().rev() {
        let column = x % step;
        let row = y % step;
        let code = (row * step + column) as usize;
        chars.push(alphabet(step)[code] as char);
        x /= step;
        y /= step;
    }
    chars.reverse();
    Ok(chars.into_iter().collect())
}

/// Decode a GID. The level is the GID length. Any other character is an error.
pub fn decode(gid: &str) -> Result<Cell, CodecError> {
    let bytes = gid.as_bytes();
    if bytes.len() > MAX_LEVEL as usize {
        return Err(CodecError::InvalidGid(gid.to_string()));
    }
    let (mut x, mut y) = (0u64, 0u64);
    for (i, &byte) in bytes.iter().enumerate() {
        let step = SUBDIVISIONS[i];
        let code = alphabet(step)
            .iter()
            .position(|&c| c == byte)
            .ok_or_else(|| CodecError::InvalidGid(gid.to_string()))? as u64;
        let row = code / step;
        let column = code % step;
        x = x * step + column;
        y = y * step + row;
    }
    Ok(Cell {
        level: bytes.len() as u8,
        x_idx: x,
        y_idx: y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_sizes_match_the_product_contract() {
        assert_eq!(side_cell_count(0), Ok(1));
        assert_eq!(side_cell_count(1), Ok(5));
        assert_eq!(side_cell_count(14), Ok(10_000_000));
        assert_eq!(cell_side_m(12), Ok(50.0));
        assert_eq!(side_cell_count(15), Err(CodecError::InvalidLevel(15)));
    }

    #[test]
    fn round_trip_known_gid() {
        let cell = decode("H2X545M5X4E2").unwrap();
        assert_eq!(
            cell,
            Cell {
                level: 12,
                x_idx: 475168,
                y_idx: 491792
            }
        );
        assert_eq!(encode(cell).unwrap(), "H2X545M5X4E2");
    }

    #[test]
    fn rejects_lowercase_and_wrong_alphabet() {
        assert!(decode("h2x545v39").is_err());
        assert!(decode("J3N2M75").is_err());
        assert!(decode("2222222222222222").is_err());
    }
}
