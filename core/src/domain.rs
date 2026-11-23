//! The 11 spec 2.0 country domains and their grid CRS (registry 2.0.0).
//!
//! The values come from the profiles in `geosquare-v2/release/profiles/*.v2.json`, which
//! are the source of the signed registry. Every number here is copied from the
//! `crs_wkt2` of its profile. Change the values only with a new registry release.
//!
//! Domain support follows `conformance/spec-2.0/manifest.json`. A domain with
//! `production_ready: false` is provisional and must not be released.

use crate::codec::ROOT_SIDE_M;
use crate::projection::{AreaFrame, DatumOperation, Ellipsoid, FrameParams, GridCrs, Projection};

/// Registry version this table is pinned to.
pub const REGISTRY_VERSION: &str = "2.0.0";
/// Profile version of the grid CRS definitions in this table.
pub const PROFILE_VERSION: &str = "2.2.0-explicit-datum";

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

    /// True when the profile has passed the production CRS gate and its operation is available.
    pub fn crs_released(&self) -> bool {
        self.production_ready && self.crs_available()
    }

    /// True when the engine has an explicit technical operation for the profile.
    pub fn crs_available(&self) -> bool {
        !CRS_DOMAINS_BLOCKED.contains(&self.code)
    }
}

/// No technical operations are blocked in this engine. Candidate domains still require opt-in.
pub const CRS_DOMAINS_BLOCKED: [&str; 0] = [];

/// Look up a domain by its code, such as `"ID"`.
pub fn by_code(code: &str) -> Option<&'static Domain> {
    DOMAINS.iter().find(|domain| domain.code == code)
}

/// Look up a domain by its numeric ID, such as `1` for `"ID"`.
pub fn by_id(id: u16) -> Option<&'static Domain> {
    DOMAINS.iter().find(|domain| domain.id == id)
}

/// Profile version of the datasets written by geosquare-v2 1.0.0. Its grid CRS is the same as
/// [`PROFILE_VERSION`]. Only the datum step differs: PROJ 9.5.1 chose each operation per point
/// from its area of use, and [`LEGACY_DOMAINS`] keeps that choice so old GIDs stay the same.
pub const LEGACY_PROFILE_VERSION: &str = "2.1.0-static-datum";

/// Areas of use `[west, south, east, north]` that PROJ 9.5.1 applied the legacy operations to.
const ID_AREA: [f64; 4] = [92.01, -13.95, 141.46, 7.79];
const VN_AREA: [f64; 4] = [102.14, 8.33, 109.53, 23.4];
const VN_SOUTH_AREA: [f64; 4] = [104.24, 9.35, 107.11, 11.04];
const LA_AREA: [f64; 4] = [100.09, 13.92, 107.64, 22.5];
const PH_AREA: [f64; 4] = [116.04, 3.0, 129.95, 22.18];

/// VN-2000 to WGS 84 (1). PROJ 9.5.1 applied it inside `VN_SOUTH_AREA`.
const VN_OPERATION_1: FrameParams = FrameParams {
    x_m: -192.873,
    y_m: -39.382,
    z_m: -111.202,
    rx_arcsec: 0.00205,
    ry_arcsec: 0.0005,
    rz_arcsec: -0.00335,
    scale_ppm: 0.0188,
    inverse: true,
};

/// The Lao 1997 geocentric translation, as a frame with no rotation and no scale.
const LA_TRANSLATION: FrameParams = FrameParams {
    x_m: -44.585,
    y_m: 131.212,
    z_m: 39.544,
    rx_arcsec: 0.0,
    ry_arcsec: 0.0,
    rz_arcsec: 0.0,
    scale_ppm: 0.0,
    inverse: false,
};

const fn frame_of(op: DatumOperation) -> FrameParams {
    match op {
        DatumOperation::CoordinateFrame {
            x_m,
            y_m,
            z_m,
            rx_arcsec,
            ry_arcsec,
            rz_arcsec,
            scale_ppm,
            inverse,
        } => FrameParams {
            x_m,
            y_m,
            z_m,
            rx_arcsec,
            ry_arcsec,
            rz_arcsec,
            scale_ppm,
            inverse,
        },
        _ => panic!("the legacy table needs a coordinate-frame operation"),
    }
}

/// The 11 domains as geosquare-v2 1.0.0 computed them. Index order follows [`DOMAINS`]:
/// ID 0, VN 1, LA 4, PH 7. The other domains have no datum shift, so they are unchanged.
const fn legacy_domains() -> [Domain; 11] {
    let mut d = DOMAINS;
    d[0].grid_crs.datum_operation = DatumOperation::AreaFrames {
        first: AreaFrame {
            area: ID_AREA,
            frame: frame_of(DOMAINS[0].grid_crs.datum_operation),
        },
        second: None,
    };
    d[1].grid_crs.datum_operation = DatumOperation::AreaFrames {
        first: AreaFrame {
            area: VN_SOUTH_AREA,
            frame: VN_OPERATION_1,
        },
        second: Some(AreaFrame {
            area: VN_AREA,
            frame: frame_of(DOMAINS[1].grid_crs.datum_operation),
        }),
    };
    d[4].grid_crs.datum_operation = DatumOperation::AreaFrames {
        first: AreaFrame {
            area: LA_AREA,
            frame: LA_TRANSLATION,
        },
        second: None,
    };
    d[7].grid_crs.datum_operation = DatumOperation::AreaFrames {
        first: AreaFrame {
            area: PH_AREA,
            frame: frame_of(DOMAINS[7].grid_crs.datum_operation),
        },
        second: None,
    };
    d
}

/// The domains for reading profile [`LEGACY_PROFILE_VERSION`] datasets.
pub static LEGACY_DOMAINS: [Domain; 11] = legacy_domains();

/// Look up a domain under a dataset's profile version.
pub fn by_code_for_profile(code: &str, profile_version: &str) -> Option<&'static Domain> {
    match profile_version {
        PROFILE_VERSION => by_code(code),
        LEGACY_PROFILE_VERSION => LEGACY_DOMAINS.iter().find(|domain| domain.code == code),
        _ => None,
    }
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
            datum_operation: DatumOperation::CoordinateFrame {
                x_m: -0.2773,
                y_m: 0.0534,
                z_m: 0.4819,
                rx_arcsec: 0.0192857593841035,
                ry_arcsec: -0.00589917345866696,
                rz_arcsec: 0.00199870597253436,
                scale_ppm: -0.028,
                inverse: false,
            },
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
        production_ready: false,
        grid_crs: GridCrs {
            ellipsoid: Ellipsoid::new(6378137.0, 298.257223563),
            datum_operation: DatumOperation::CoordinateFrame {
                x_m: -191.90441429,
                y_m: -39.30318279,
                z_m: -111.45032835,
                rx_arcsec: -0.00928836,
                ry_arcsec: 0.01975479,
                rz_arcsec: -0.00427372,
                scale_ppm: 0.252906278,
                inverse: true,
            },
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
            datum_operation: DatumOperation::None,
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
            datum_operation: DatumOperation::None,
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
            datum_operation: DatumOperation::GeocentricTranslation {
                x_m: -44.585,
                y_m: 131.212,
                z_m: 39.544,
            },
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
            datum_operation: DatumOperation::None,
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
            datum_operation: DatumOperation::None,
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
            datum_operation: DatumOperation::CoordinateFrame {
                x_m: -127.62,
                y_m: -67.24,
                z_m: -47.04,
                rx_arcsec: 3.068,
                ry_arcsec: -4.903,
                rz_arcsec: -1.578,
                scale_ppm: -1.06,
                inverse: true,
            },
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
            datum_operation: DatumOperation::None,
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
            datum_operation: DatumOperation::None,
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
            datum_operation: DatumOperation::None,
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

    /// GIDs written by geosquare-v2 1.0.0 under PROJ 9.5.1 (the signed registry pin).
    /// VN 9.80 N and LA 100.0 E lie in the cases the legacy datum rule changes.
    #[test]
    fn legacy_profile_reproduces_geosquare_v2_1_0_gids() {
        let cases = [
            (
                "VN",
                106.9492673256204,
                9.804505192464923,
                14,
                "H2Y3R2C5E243V4",
            ),
            (
                "VN",
                106.15885694488477,
                15.218925236358746,
                12,
                "H2Y5X373V2W4",
            ),
            (
                "VN",
                106.15885694488477,
                15.218925236358746,
                14,
                "H2Y5X373V2W4L3",
            ),
            (
                "LA",
                100.00753028497924,
                18.5829608512224,
                12,
                "H46323Y455Y2",
            ),
            (
                "LA",
                100.00753028497924,
                18.5829608512224,
                14,
                "H46323Y455Y294",
            ),
            (
                "LA",
                104.73257650510985,
                18.463674669301646,
                12,
                "H52223L4P475",
            ),
            (
                "ID",
                107.02127000657265,
                -6.489789411968728,
                12,
                "H2X545C335Y3",
            ),
        ];
        for (code, lon, lat, level, gid) in cases {
            let domain = by_code_for_profile(code, LEGACY_PROFILE_VERSION).unwrap();
            let cell =
                crate::geo::point_to_cell_with_options(domain, lon, lat, level, true).unwrap();
            assert_eq!(
                crate::codec::encode(cell).unwrap(),
                gid,
                "{code} {lon} {lat} L{level}"
            );
        }
    }

    #[test]
    fn legacy_table_differs_only_in_the_datum_of_its_domains() {
        for (current, legacy) in DOMAINS.iter().zip(LEGACY_DOMAINS.iter()) {
            assert_eq!(current.code, legacy.code);
            assert_eq!(current.grid_crs.ellipsoid, legacy.grid_crs.ellipsoid);
            assert_eq!(current.grid_crs.projection, legacy.grid_crs.projection);
            let changed = current.grid_crs.datum_operation != legacy.grid_crs.datum_operation;
            assert_eq!(
                changed,
                ["ID", "VN", "LA", "PH"].contains(&current.code),
                "{}",
                current.code
            );
        }
    }

    #[test]
    fn profile_versions_select_their_own_domain_table() {
        let current = by_code_for_profile("VN", PROFILE_VERSION).unwrap();
        let legacy = by_code_for_profile("VN", LEGACY_PROFILE_VERSION).unwrap();
        assert_ne!(
            current.grid_crs.datum_operation,
            legacy.grid_crs.datum_operation
        );
        assert!(by_code_for_profile("VN", "9.9.9").is_none());
    }

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
        assert_eq!(provisional, ["VN", "KH", "MM", "TH", "TL"]);
    }
}
