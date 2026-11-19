//! The GeoSquare v1 grid (the "Petain" grid of GeoSquare Horizon v1): square cells in degrees,
//! each level dividing its parent 5×5 or 2×2 times, one character per level. Ported from
//! `@geosquare/grid` (gidToBBox / lonLatToGid) with the same floating-point steps, so gids and
//! boxes are identical to the v1 apps. Used to move data keyed by v1 gids onto the v2 grid.

use std::error::Error;

type E = Box<dyn Error>;

const ALPHABET: [[char; 5]; 5] = [
    ['2', '3', '4', '5', '6'],
    ['7', '8', '9', 'C', 'E'],
    ['F', 'G', 'H', 'J', 'L'],
    ['M', 'N', 'P', 'Q', 'R'],
    ['T', 'V', 'W', 'X', 'Y'],
];
const DIVISIONS: [usize; 15] = [5, 2, 5, 2, 5, 2, 5, 2, 5, 2, 5, 2, 5, 2, 5];
const LAT_RANGE: [f64; 2] = [-216.0, 233.157642055036];
const LON_RANGE: [f64; 2] = [-217.0, 232.157642055036];
pub const MAX_LEVEL: usize = 15;

fn position(ch: char) -> Option<(usize, usize)> {
    ALPHABET
        .iter()
        .enumerate()
        .find_map(|(r, row)| row.iter().position(|c| *c == ch).map(|c| (r, c)))
}

/// `[min_lon, min_lat, max_lon, max_lat]` of a v1 gid.
pub fn bbox(gid: &str) -> Result<[f64; 4], E> {
    if gid.is_empty() || gid.chars().count() > MAX_LEVEL {
        return Err(
            format!("v1 gid must have 1 to {MAX_LEVEL} characters; received {gid:?}").into(),
        );
    }
    let mut lat = LAT_RANGE;
    let mut lon = LON_RANGE;
    for (i, ch) in gid.chars().enumerate() {
        let div = DIVISIONS[i];
        let (row, col) = position(ch)
            .filter(|(r, c)| *r < div && *c < div)
            .ok_or_else(|| format!("invalid v1 gid {gid:?}: {ch:?} at position {}", i + 1))?;
        let part_x = (lon[1] - lon[0]) / div as f64;
        let part_y = (lat[1] - lat[0]) / div as f64;
        let x0 = lon[0] + part_x * col as f64;
        let y0 = lat[0] + part_y * row as f64;
        lon = [x0, x0 + part_x];
        lat = [y0, y0 + part_y];
    }
    Ok([lon[0], lat[0], lon[1], lat[1]])
}

/// Centre (lon, lat) of a v1 gid.
pub fn center(gid: &str) -> Result<(f64, f64), E> {
    let b = bbox(gid)?;
    Ok(((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0))
}

/// v1 gid of the level-`level` cell holding (lon, lat); level 12 = 50 m, 9 = 1 km.
pub fn from_lonlat(lon: f64, lat: f64, level: usize) -> Result<String, E> {
    if !(1..=MAX_LEVEL).contains(&level) {
        return Err(format!("v1 level must be in [1, {MAX_LEVEL}]; received {level}").into());
    }
    if !lon.is_finite() || !lat.is_finite() {
        return Err("longitude and latitude must be finite".into());
    }
    let mut lat_r = LAT_RANGE;
    let mut lon_r = LON_RANGE;
    let mut gid = String::with_capacity(level);
    for &div in DIVISIONS.iter().take(level) {
        let px = (((lon - lon_r[0]) / (lon_r[1] - lon_r[0])) * div as f64).floor();
        let py = (((lat - lat_r[0]) / (lat_r[1] - lat_r[0])) * div as f64).floor();
        if !(0.0..div as f64).contains(&px) || !(0.0..div as f64).contains(&py) {
            return Err(format!("point ({lon}, {lat}) is outside the v1 grid").into());
        }
        let part_x = (lon_r[1] - lon_r[0]) / div as f64;
        let part_y = (lat_r[1] - lat_r[0]) / div as f64;
        let x0 = lon_r[0] + part_x * px;
        let y0 = lat_r[0] + part_y * py;
        lon_r = [x0, x0 + part_x];
        lat_r = [y0, y0 + part_y];
        gid.push(ALPHABET[py as usize][px as usize]);
    }
    Ok(gid)
}

/// The cell outline as one closed lon/lat ring (counter-clockwise).
pub fn ring(gid: &str) -> Result<Vec<(f64, f64)>, E> {
    let [a, b, c, d] = bbox(gid)?;
    Ok(vec![(a, b), (c, b), (c, d), (a, d)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for &(lon, lat) in &[
            (106.8272, -6.1754),
            (95.3, 5.5),
            (140.7, -2.5),
            (115.2, -8.65),
        ] {
            for level in 1..=MAX_LEVEL {
                let g = from_lonlat(lon, lat, level).unwrap();
                assert_eq!(g.len(), level);
                let b = bbox(&g).unwrap();
                assert!(
                    b[0] <= lon && lon < b[2] && b[1] <= lat && lat < b[3],
                    "{g} {b:?}"
                );
                let (cx, cy) = center(&g).unwrap();
                assert_eq!(from_lonlat(cx, cy, level).unwrap(), g);
            }
        }
        assert!(bbox(&from_lonlat(106.8, -6.2, 12).unwrap()).is_ok());
        assert!(bbox("H2X5").is_err()); // '5' is not in the 2×2 block of level 4
        assert!(bbox("HX").is_err()); // 'X' is not in the 2×2 block of level 2
        assert!(bbox("").is_err() && bbox("A").is_err());
    }
}
