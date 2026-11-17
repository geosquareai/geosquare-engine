//! Integer topology: parents, children, descendant ranges, neighbours and distance
//! (spec 2.0, sections 8 and 8.1). Everything here works on integer indices.

use crate::codec::{self, Cell, CodecError, MAX_LEVEL, SUBDIVISIONS};
use crate::packing::{self, PackingError};

/// Parent cell, one level up. Level 0 has no parent.
pub fn parent(cell: Cell) -> Option<Cell> {
    if cell.level == 0 {
        return None;
    }
    let step = SUBDIVISIONS[cell.level as usize - 1];
    Some(Cell {
        level: cell.level - 1,
        x_idx: cell.x_idx / step,
        y_idx: cell.y_idx / step,
    })
}

/// Children one level down: 25 for a 5x5 level, 4 for a 2x2 level. Level 14 has none.
pub fn children(cell: Cell) -> Result<Vec<Cell>, CodecError> {
    codec::validate(cell)?;
    if cell.level >= MAX_LEVEL {
        return Err(CodecError::InvalidLevel(cell.level + 1));
    }
    let step = SUBDIVISIONS[cell.level as usize];
    let mut out = Vec::with_capacity((step * step) as usize);
    for row in 0..step {
        for column in 0..step {
            out.push(Cell {
                level: cell.level + 1,
                x_idx: cell.x_idx * step + column,
                y_idx: cell.y_idx * step + row,
            });
        }
    }
    Ok(out)
}

/// Packed bounds `(lo, hi)` of every level-`child_level` descendant of `parent`
/// for one domain. Spec 2.0, section 8.1.
///
/// `lo` is the first descendant. Every descendant lies in `[lo, hi]`. Some values in
/// the interval are not valid codes, so readers must still decode each value.
pub fn descendant_range(
    domain_id: u16,
    parent: Cell,
    child_level: u8,
) -> Result<(i64, i64), PackingError> {
    if child_level > MAX_LEVEL {
        return Err(PackingError::InvalidCell(CodecError::InvalidLevel(
            child_level,
        )));
    }
    if child_level < parent.level {
        return Err(PackingError::InvalidCell(CodecError::ChildBelowParent {
            parent: parent.level,
            child: child_level,
        }));
    }
    let packed = packing::pack(domain_id, parent)? as u64;
    let parent_bits = packing::path_bits(parent.level);
    let child_bits = packing::path_bits(child_level);
    let parent_path = (packed >> 1) & ((1u64 << parent_bits) - 1);
    let shift = child_bits - parent_bits;
    let base = ((domain_id as u64) << 54) | ((child_level as u64) << 50);
    let lo = base | ((parent_path << shift) << 1);
    let hi = base | (((parent_path << shift) + (1u64 << shift) - 1) << 1);
    Ok((lo as i64, hi as i64))
}

/// Cells in the square of half-width `k` around `cell`, clipped to the grid, including
/// `cell` itself.
pub fn k_disk(cell: Cell, k: u64) -> Result<Vec<Cell>, CodecError> {
    let n = codec::validate(cell)?;
    let (x0, y0) = (cell.x_idx, cell.y_idx);
    let x_lo = x0.saturating_sub(k);
    let x_hi = x0.saturating_add(k).min(n - 1);
    let y_lo = y0.saturating_sub(k);
    let y_hi = y0.saturating_add(k).min(n - 1);
    let mut out = Vec::new();
    for y in y_lo..=y_hi {
        for x in x_lo..=x_hi {
            out.push(Cell {
                level: cell.level,
                x_idx: x,
                y_idx: y,
            });
        }
    }
    Ok(out)
}

/// Cells whose Chebyshev distance from `cell` is exactly `k`, clipped to the grid.
/// `k_ring(cell, 1)` is the Moore neighbourhood and excludes `cell`.
pub fn k_ring(cell: Cell, k: u64) -> Result<Vec<Cell>, CodecError> {
    let disk = k_disk(cell, k)?;
    Ok(disk
        .into_iter()
        .filter(|c| {
            c.x_idx
                .abs_diff(cell.x_idx)
                .max(c.y_idx.abs_diff(cell.y_idx))
                == k
        })
        .collect())
}

/// Grid distance in metres between two cells on the same level.
pub fn distance_m(a: Cell, b: Cell) -> Result<f64, CodecError> {
    if a.level != b.level {
        return Err(CodecError::LevelMismatch {
            first: a.level,
            second: b.level,
        });
    }
    codec::validate(a)?;
    codec::validate(b)?;
    let dx = a.x_idx.abs_diff(b.x_idx) as f64;
    let dy = a.y_idx.abs_diff(b.y_idx) as f64;
    Ok((dx * dx + dy * dy).sqrt() * codec::cell_side_m(a.level)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_counts_follow_the_step() {
        let level0 = Cell {
            level: 0,
            x_idx: 0,
            y_idx: 0,
        };
        assert_eq!(children(level0).unwrap().len(), 25);
        let level1 = Cell {
            level: 1,
            x_idx: 0,
            y_idx: 0,
        };
        assert_eq!(children(level1).unwrap().len(), 4);
        let level14 = Cell {
            level: 14,
            x_idx: 0,
            y_idx: 0,
        };
        assert!(children(level14).is_err());
    }

    #[test]
    fn parent_of_child_is_the_original() {
        let cell = Cell {
            level: 3,
            x_idx: 17,
            y_idx: 9,
        };
        for child in children(cell).unwrap() {
            assert_eq!(parent(child), Some(cell));
        }
        assert_eq!(
            parent(Cell {
                level: 0,
                x_idx: 0,
                y_idx: 0
            }),
            None
        );
    }

    #[test]
    fn ring_excludes_source_and_clips_at_root() {
        let corner = Cell {
            level: 5,
            x_idx: 0,
            y_idx: 0,
        };
        assert_eq!(k_ring(corner, 1).unwrap().len(), 3);
        let inner = Cell {
            level: 6,
            x_idx: 500,
            y_idx: 500,
        };
        assert_eq!(k_ring(inner, 1).unwrap().len(), 8);
        assert!(!k_ring(inner, 1).unwrap().contains(&inner));
    }

    #[test]
    fn huge_k_clips_to_the_grid_without_overflow() {
        let cell = Cell {
            level: 2,
            x_idx: 3,
            y_idx: 4,
        };
        assert_eq!(k_disk(cell, u64::MAX).unwrap().len(), 100);
        assert!(k_ring(cell, u64::MAX).unwrap().is_empty());
    }

    #[test]
    fn distance_rejects_different_levels_with_a_clear_error() {
        let a = Cell {
            level: 3,
            x_idx: 0,
            y_idx: 0,
        };
        let b = Cell {
            level: 4,
            x_idx: 0,
            y_idx: 0,
        };
        assert_eq!(
            distance_m(a, b),
            Err(CodecError::LevelMismatch {
                first: 3,
                second: 4
            })
        );
    }

    #[test]
    fn descendant_range_rejects_child_above_parent() {
        let parent = Cell {
            level: 4,
            x_idx: 0,
            y_idx: 0,
        };
        assert_eq!(
            descendant_range(1, parent, 3),
            Err(PackingError::InvalidCell(CodecError::ChildBelowParent {
                parent: 4,
                child: 3
            }))
        );
    }

    #[test]
    fn descendant_range_contains_children() {
        let parent = Cell {
            level: 4,
            x_idx: 37,
            y_idx: 12,
        };
        let (lo, hi) = descendant_range(1, parent, 5).unwrap();
        for child in children(parent).unwrap() {
            let packed = packing::pack(1, child).unwrap();
            assert!(lo <= packed && packed <= hi);
        }
    }
}
