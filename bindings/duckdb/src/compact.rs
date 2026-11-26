//! `cell_compact_set`: a set of GIDs that may mix levels (geosquare-v2
//! docs/cell-dataset-contract.md section 2, reference `geosquare_v2.compact`).
//!
//! The area of a set is the union of its cells. The canonical form has no cell inside
//! another row and no complete child group (it is replaced by its parent). Parents and
//! children come from the codec; a child's GID is its parent's GID plus one character.

use geosquare_core::{codec, topology, Cell};
use std::collections::{BTreeSet, HashMap, HashSet};

type E = Box<dyn std::error::Error + Send + Sync>;

fn cell(gid: &str) -> Result<Cell, E> {
    codec::decode(gid).map_err(|e| format!("invalid GID {gid:?}: {e}").into())
}

fn parent_gid(c: Cell) -> Result<Option<String>, E> {
    topology::parent(c).map(|p| codec::encode(p).map_err(E::from)).transpose()
}

fn child_count(level: u8) -> usize {
    let s = codec::SUBDIVISIONS[level as usize] as usize;
    s * s
}

/// Whether `gid` lies inside one of `members` (a strict ancestor), by the prefix rule.
fn has_ancestor(gid: &str, members: &HashSet<&str>) -> bool {
    (0..gid.len()).any(|n| members.contains(&gid[..n]))
}

/// The canonical compact form of `gids`, sorted: descendants of a present row are dropped,
/// then complete child groups are replaced by their parent, deepest level first.
pub fn canonicalize<'a>(gids: impl IntoIterator<Item = &'a str>) -> Result<Vec<String>, E> {
    let present: HashSet<&str> = gids.into_iter().collect();
    for g in &present {
        cell(g)?;
    }
    let mut by_level: Vec<HashSet<String>> = vec![HashSet::new(); codec::MAX_LEVEL as usize + 1];
    for g in &present {
        if !has_ancestor(g, &present) {
            by_level[g.len()].insert(g.to_string());
        }
    }
    for level in (1..=codec::MAX_LEVEL as usize).rev() {
        let mut groups: HashMap<String, usize> = HashMap::new();
        for g in &by_level[level] {
            if let Some(p) = parent_gid(cell(g)?)? {
                *groups.entry(p).or_default() += 1;
            }
        }
        let full = child_count(level as u8 - 1);
        for (p, n) in groups {
            if n == full {
                by_level[level].retain(|g| g[..level - 1] != *p);
                by_level[level - 1].insert(p);
            }
        }
    }
    let mut out: Vec<String> = by_level.into_iter().flatten().collect();
    out.sort();
    Ok(out)
}

/// Why `rows` (gid, level) is not a canonical compact set; None when it is.
pub fn check(rows: &[(String, Option<i64>)]) -> Result<Option<String>, E> {
    let mut seen: HashSet<&str> = HashSet::new();
    for (g, level) in rows {
        let decoded = match codec::decode(g) {
            Ok(c) => c.level as i64,
            Err(e) => return Ok(Some(format!("invalid GID {g:?}: {e}"))),
        };
        if let Some(l) = level {
            if *l != decoded {
                return Ok(Some(format!("level {l} does not match GID {g:?} at level {decoded}")));
            }
        }
        if !seen.insert(g.as_str()) {
            return Ok(Some(format!("duplicate row for GID {g:?}")));
        }
    }
    for g in &seen {
        if let Some(n) = (0..g.len()).find(|n| seen.contains(&g[..*n])) {
            return Ok(Some(format!("GID {g:?} is inside row {:?}", &g[..n])));
        }
    }
    let mut sorted: Vec<&str> = seen.iter().copied().collect();
    sorted.sort();
    if canonicalize(sorted.iter().copied())? != sorted {
        return Ok(Some(
            "set is not canonical: a complete child group must be replaced by its parent".into(),
        ));
    }
    Ok(None)
}

/// Every level-`level` descendant of the rows, sorted and de-duplicated.
pub fn expand<'a>(gids: impl IntoIterator<Item = &'a str>, level: u8) -> Result<Vec<String>, E> {
    if level > codec::MAX_LEVEL {
        return Err(format!("level must be 0 to {}", codec::MAX_LEVEL).into());
    }
    let mut out = BTreeSet::new();
    for g in gids {
        let c = cell(g)?;
        if level < c.level {
            return Err(format!("target level {level} is below row level {} for GID {g:?}", c.level).into());
        }
        let mut frontier = vec![c];
        for _ in c.level..level {
            frontier = frontier
                .into_iter()
                .map(topology::children)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
        }
        for f in frontier {
            out.insert(codec::encode(f)?);
        }
    }
    Ok(out.into_iter().collect())
}

/// Whether `gid` is a row of the set or lies under one.
pub fn contains<'a>(gids: impl IntoIterator<Item = &'a str>, gid: &str) -> Result<bool, E> {
    cell(gid)?;
    let members: HashSet<&str> = gids.into_iter().collect();
    Ok(members.contains(gid) || has_ancestor(gid, &members))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert_eq!(canonicalize(["22", "23", "24", "25"]).unwrap(), ["2"]);
        assert_eq!(canonicalize(["2", "22"]).unwrap(), ["2"]);
        let roots: Vec<String> = "23456789CEFGHJLMNPQRTVWXY".chars().map(String::from).collect();
        assert_eq!(canonicalize(roots.iter().map(String::as_str)).unwrap(), [""]);
        assert_eq!(expand(["2", "32"], 2).unwrap(), ["22", "23", "24", "25", "32"]);
        assert!(expand(["2"], 0).is_err());
        assert!(contains(["2"], "25").unwrap() && !contains(["2"], "32").unwrap());
        assert!(check(&[("2".into(), Some(1)), ("22".into(), Some(2))]).unwrap().is_some());
        assert!(check(&[("".into(), Some(0))]).unwrap().is_none());
    }
}
