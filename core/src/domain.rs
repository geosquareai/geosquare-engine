//! The 11 spec 2.0 country domains and their grid CRS (registry 2.0.0).
//!
//! The values come from the profiles in `geosquare-v2/release/profiles/*.v2.json`, which
//! are the source of the signed registry. Every number here is copied from the
//! `crs_wkt2` of its profile. Change the values only with a new registry release.
//!
//! Domain support follows `conformance/spec-2.0/manifest.json`. A domain with
//! `production_ready: false` is provisional and must not be released.

use crate::codec::ROOT_SIDE_M;
use crate::projection::{Ellipsoid, GridCrs, Projection};

/// Registry version this table is pinned to.
pub const REGISTRY_VERSION: &str = "2.0.0";
/// Profile version of the grid CRS definitions in this table.
pub const PROFILE_VERSION: &str = "2.1.0-static-datum";

/// One country domain: its ID, code, grid origin, and grid CRS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Domain {
    pub id: u16,
    pub code: &'static str,
    pub origin_x_m: f64,
    pub origin_y_m: f64,
    pub production_ready: bool,
    pub grid_crs: GridCrs,
}

impl Domain {
    /// Root square side in metres. Spec 2.0 uses the same side for every domain.
    pub const fn root_side_m(&self) -> f64 {
        ROOT_SIDE_M
    }

    /// True when lon/lat to cell matches pyproj for this domain (see [`CRS_DATUM_SHIFT_UNDECLARED`]).
    pub fn crs_released(&self) -> bool {
        !CRS_DATUM_SHIFT_UNDECLARED.contains(&self.code)
    }
}

/// Domains whose lon/lat results differ from pyproj 3.7.1 (PROJ 9.5.1), because PROJ applies a
/// datum shift from WGS 84 that the profile does not declare.
///
/// Measured on 300 random points inside each boundary. Residual is the grid CRS distance from
/// pyproj:
///
/// - ID: 0.31 m max, 2 of 300 points in another 50 m cell.
/// - LA: 24.7 m max, 114 of 300 points in another 50 m cell.
/// - VN: 226.8 m max, 300 of 300 points in another 50 m cell.
/// - PH: 235.1 m max, 300 of 300 points in another 50 m cell.
///
/// The other seven domains match to under 0.1 micrometre. These four stay unreleased until the
/// spec declares how inputs relate to the grid datum.
pub const CRS_DATUM_SHIFT_UNDECLARED: [&str; 4] = ["ID", "LA", "VN", "PH"];

/// Look up a domain by its code, such as `"ID"`.
pub fn by_code(code: &str) -> Option<&'static Domain> {
    DOMAINS.iter().find(|domain| domain.code == code)
}

/// Look up a domain by its numeric ID, such as `1` for `"ID"`.
pub fn by_id(id: u16) -> Option<&'static Domain> {
    DOMAINS.iter().find(|domain| domain.id == id)
}

/// All spec 2.0 domains, in registry order.
pub const DOMAINS: [Domain; 11] = [
    // ID: Indonesia. Grid CRS GEOSQUARE:ID_SRGI2013_STATIC2012_LCC_V2.
    Domain {
        id: 1,
        code: "ID",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: true,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: -2.55029564999998,
                lon_origin: 118.01521445,
                lat_1: -8.18864928333332,
                lat_2: 3.08805798333335,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // VN: Viet Nam. Grid CRS GEOSQUARE:VN_VN2000_LCC_V2.
    Domain {
        id: 2,
        code: "VN",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: true,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 16.0,
                lon_origin: 107.5,
                lat_1: 11.0,
                lat_2: 21.0,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // BN: Brunei Darussalam. Grid CRS GEOSQUARE:BN_GDBD2009_LCC_V2.
    Domain {
        id: 3,
        code: "BN",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: true,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257222101),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 4.52406018063286,
                lon_origin: 114.722756845902,
                lat_1: 4.17561629985239,
                lat_2: 4.87250406141333,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // KH: Cambodia. Grid CRS GEOSQUARE:KH_LCC_V2.
    Domain {
        id: 4,
        code: "KH",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: false,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 12.30190505,
                lon_origin: 104.9807535,
                lat_1: 10.70968015,
                lat_2: 13.89412995,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // LA: Lao PDR. Grid CRS GEOSQUARE:LA_LAO1997_LCC_V2.
    Domain {
        id: 5,
        code: "LA",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: true,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378245.0, 298.3),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 18.2061856,
                lon_origin: 103.8596618,
                lat_1: 15.3418453333334,
                lat_2: 21.0705258666667,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // MY: Malaysia. Grid CRS GEOSQUARE:MY_GDM2000_LCC_V2.
    Domain {
        id: 6,
        code: "MY",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: true,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257222101),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 4.10831925000002,
                lon_origin: 109.4548108,
                lat_1: 1.93865375000001,
                lat_2: 6.27798475000003,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // MM: Myanmar. Grid CRS GEOSQUARE:MM_LCC_FRACTION_1_6_V2.
    Domain {
        id: 7,
        code: "MM",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: false,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 19.108395482,
                lon_origin: 96.6713193360001,
                lat_1: 12.816966562,
                lat_2: 25.3998244020001,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // PH: Philippines. Grid CRS GEOSQUARE:PH_PRS92_LCC_V2.
    Domain {
        id: 8,
        code: "PH",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: true,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378206.4, 294.978698213898),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 12.8545389070001,
                lon_origin: 120.441429557,
                lat_1: 7.34304263366674,
                lat_2: 18.3660351803334,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // SG: Singapore. Grid CRS GEOSQUARE:SG_SVY21_TM_V2.
    Domain {
        id: 9,
        code: "SG",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: true,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            projection: Projection::TransverseMercator {
                lat_origin: 1.3299809,
                lon_origin: 103.8469117,
                scale: 1.0,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // TH: Thailand. Grid CRS GEOSQUARE:TH_LCC_FRACTION_1_6_V2.
    Domain {
        id: 10,
        code: "TH",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: false,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: 13.03884235,
                lon_origin: 101.4903096,
                lat_1: 8.08818145000001,
                lat_2: 17.98950325,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
    // TL: Timor-Leste. Grid CRS GEOSQUARE:TL_LCC_V2.
    Domain {
        id: 11,
        code: "TL",
        origin_x_m: -25_000_000.0,
        origin_y_m: -25_000_000.0,
        production_ready: false,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            projection: Projection::LambertConformalConic2Sp {
                lat_origin: -8.81553470000003,
                lon_origin: 125.6918511,
                lat_1: -9.27462523333337,
                lat_2: -8.35644416666669,
            },
            false_easting_m: 0.0,
            false_northing_m: 0.0,
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_codes_are_unique_and_complete() {
        let mut ids: Vec<u16> = DOMAINS.iter().map(|d| d.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=11).collect::<Vec<u16>>());
        let mut codes: Vec<&str> = DOMAINS.iter().map(|d| d.code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 11);
    }

    #[test]
    fn every_domain_shares_the_spec_root_square() {
        for domain in &DOMAINS {
            assert_eq!(domain.origin_x_m, -25_000_000.0, "{}", domain.code);
            assert_eq!(domain.origin_y_m, -25_000_000.0, "{}", domain.code);
            assert_eq!(domain.root_side_m(), 50_000_000.0, "{}", domain.code);
        }
    }

    #[test]
    fn lookups_find_the_same_domain() {
        assert_eq!(by_code("SG").map(|d| d.id), Some(9));
        assert_eq!(by_id(1).map(|d| d.code), Some("ID"));
        assert!(by_code("XX").is_none());
        assert!(by_id(0).is_none());
    }

    #[test]
    fn provisional_domains_are_marked() {
        let provisional: Vec<&str> = DOMAINS
            .iter()
            .filter(|d| !d.production_ready)
            .map(|d| d.code)
            .collect();
        assert_eq!(provisional, ["KH", "MM", "TH", "TL"]);
    }
}
