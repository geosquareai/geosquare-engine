//! Grid-CRS indexing: projected metres to cell, against `projected.json`.
//!
//! This needs no PROJ. It checks the half-open cell rule, the closed root edge, and the
//! rejection of points outside the root square, for all 11 domains.

use geosquare_core::codec;
use geosquare_core::domain;
use geosquare_core::geo::{self, GeoError};
use serde_json::Value as Json;
use std::path::PathBuf;

fn load_projected() -> Json {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/spec-2.0/projected.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("projected.json is valid JSON")
}

#[test]
fn projected_metres_index_to_the_expected_cell_in_every_domain() {
    let vectors = load_projected();
    let cases = vectors["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 528);

    let mut failures = Vec::new();
    for case in cases {
        let code = case["domain_code"].as_str().unwrap();
        let domain = domain::by_code(code).unwrap_or_else(|| panic!("unknown domain {code}"));
        let level = case["level"].as_u64().unwrap() as u8;
        let x = case["x_m"].as_f64().unwrap();
        let y = case["y_m"].as_f64().unwrap();
        let got = geo::cell_from_projected(domain, x, y, level);

        match &case["expect"] {
            Json::String(text) if text == "reject" => {
                if !matches!(got, Err(GeoError::OutOfDomain { .. })) {
                    failures.push(format!(
                        "{code} ({x}, {y}) level {level}: expected reject, got {got:?}"
                    ));
                }
            }
            expected => {
                let gid = expected["gid"].as_str().unwrap();
                let x_idx = expected["x_idx"].as_u64().unwrap();
                let y_idx = expected["y_idx"].as_u64().unwrap();
                match got {
                    Ok(cell) if cell.x_idx == x_idx && cell.y_idx == y_idx => {
                        let encoded = codec::encode(cell).unwrap();
                        if encoded != gid {
                            failures.push(format!("{code}: index matches but GID {encoded} != {gid}"));
                        }
                    }
                    other => failures.push(format!(
                        "{code} ({x}, {y}) level {level}: expected {gid} ({x_idx}, {y_idx}), got {other:?}"
                    )),
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
