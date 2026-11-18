//! CRS parity: lon/lat to GID against `points.json`, which the Python oracle generated with
//! pyproj 3.7.1 (PROJ 9.5.1).
//!
//! A domain whose explicit datum operation is blocked must refuse lon/lat, so it never
//! returns a cell based on an unapproved operation.

use geosquare_core::codec;
use geosquare_core::domain;
use geosquare_core::geo::{self, GeoError};
use serde_json::Value as Json;
use std::path::PathBuf;

fn load_points() -> Json {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/spec-2.0/points.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("points.json is valid JSON")
}

#[test]
fn released_domains_match_pyproj_at_every_listed_level() {
    let points = load_points();
    let cases = points["cases"].as_array().unwrap();
    assert_eq!(
        cases.len(),
        12,
        "points.json holds 11 domains plus one extra ID point"
    );

    let mut failures = Vec::new();
    let mut released_checked = Vec::new();
    for case in cases {
        let code = case["domain_code"].as_str().unwrap();
        let domain = domain::by_code(code).unwrap_or_else(|| panic!("unknown domain {code}"));
        let lon = case["lon"].as_f64().unwrap();
        let lat = case["lat"].as_f64().unwrap();

        // The vector's support flag must match the registry table.
        let expected_ready = case["production_ready"].as_bool().unwrap();
        assert_eq!(
            domain.production_ready, expected_ready,
            "{code} support flag"
        );

        for (level, gid) in case["gids"].as_object().unwrap() {
            let level: u8 = level.parse().unwrap();
            let expected = gid.as_str().unwrap();
            let got = geo::point_to_cell(domain, lon, lat, level);
            if domain.crs_released() {
                let value = got
                    .map_err(|e| e.to_string())
                    .and_then(|cell| codec::encode(cell).map_err(|e| e.to_string()));
                if value.as_deref() != Ok(expected) {
                    failures.push(format!(
                        "{code} ({lon}, {lat}) level {level}: expected {expected}, got {value:?}"
                    ));
                }
            } else if got != Err(GeoError::CrsNotReleased { code: domain.code }) {
                failures.push(format!("{code} is not released but returned {got:?}"));
            }
        }
        if domain.crs_released() {
            released_checked.push(code);
        }
    }

    eprintln!(
        "blocked explicit datum operations: {:?}",
        domain::CRS_DOMAINS_BLOCKED
    );
    assert!(
        failures.is_empty(),
        "parity failures:\n{}",
        failures.join("\n")
    );
    assert_eq!(
        released_checked.len(),
        12,
        "all 11 domains plus one extra ID case have released operations"
    );
}
