//! Runs the spec 2.0 conformance vectors against the Rust core.
//!
//! The vectors are copied from `geosquare-v2` at tag `spec-2.0`. Any mismatch here means
//! the Rust core disagrees with the Python reference.

use geosquare_core::codec::{self, Cell};
use geosquare_core::detect::{self, Column, DType, Kind, Status, Value};
use geosquare_core::packing;
use geosquare_core::topology;
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn load(name: &str) -> Json {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../vectors/spec-2.0")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("vector file is valid JSON")
}

fn cell_from_json(level: &Json, x_idx: &Json, y_idx: &Json) -> Cell {
    Cell {
        level: level.as_u64().unwrap() as u8,
        x_idx: x_idx.as_u64().unwrap(),
        y_idx: y_idx.as_u64().unwrap(),
    }
}

fn gid_of(cell: Cell) -> String {
    codec::encode(cell).unwrap()
}

fn cell_of_gid(gid: &str) -> Cell {
    codec::decode(gid).unwrap_or_else(|error| panic!("{gid:?}: {error}"))
}

fn strings(value: &Json) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

fn sorted_gids(cells: Vec<Cell>) -> Vec<String> {
    let mut gids: Vec<String> = cells.into_iter().map(gid_of).collect();
    gids.sort();
    gids
}

fn decimal_i64(value: &Json) -> i64 {
    value.as_str().unwrap().parse().unwrap()
}

/// Domain code to domain ID, from `codec.json`.
fn domains() -> BTreeMap<String, u16> {
    load("codec.json")["packed_ids_by_domain"]["domains"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(code, id)| (code.clone(), id.as_u64().unwrap() as u16))
        .collect()
}

#[test]
fn codec_cells_round_trip_and_pack_for_id() {
    let codec_json = load("codec.json");
    let cells = codec_json["cells"].as_array().unwrap();
    assert!(!cells.is_empty());
    for record in cells {
        let cell = cell_from_json(&record["level"], &record["x_idx"], &record["y_idx"]);
        let gid = record["gid"].as_str().unwrap();
        assert_eq!(gid_of(cell), gid);
        assert_eq!(cell_of_gid(gid), cell);
        let packed = decimal_i64(&record["packed_id"]);
        assert_eq!(packing::pack(1, cell).unwrap(), packed, "{gid}");
        assert_eq!(packing::unpack(packed).unwrap(), (1, cell));
    }
}

#[test]
fn packed_ids_match_for_every_domain() {
    let codec_json = load("codec.json");
    let domain_ids = domains();
    assert_eq!(domain_ids.len(), 11);
    for record in codec_json["packed_ids_by_domain"]["cells"]
        .as_array()
        .unwrap()
    {
        let cell = cell_from_json(&record["level"], &record["x_idx"], &record["y_idx"]);
        assert_eq!(gid_of(cell), record["gid"].as_str().unwrap());
        for (code, domain_id) in &domain_ids {
            let expected = decimal_i64(&record["packed_ids"][code]);
            assert_eq!(
                packing::pack(*domain_id, cell).unwrap(),
                expected,
                "domain {code}"
            );
            assert_eq!(
                packing::unpack(expected).unwrap(),
                (*domain_id, cell),
                "domain {code}"
            );
        }
    }
}

#[test]
fn invalid_gids_are_rejected() {
    let codec_json = load("codec.json");
    for record in codec_json["invalid_gids"].as_array().unwrap() {
        let input = record["input"].as_str().unwrap();
        assert!(codec::decode(input).is_err(), "accepted {input:?}");
    }
}

#[test]
fn invalid_packed_ids_are_rejected() {
    let codec_json = load("codec.json");
    for record in codec_json["invalid_packed_ids"].as_array().unwrap() {
        let text = record["packed_id"].as_str().unwrap();
        let value: u64 = text.parse().unwrap();
        match i64::try_from(value) {
            // Outside the signed Int64 range: the sign bit is set, which is itself a rejection.
            Err(_) => assert!(value >> 63 == 1, "{text}"),
            Ok(signed) => assert!(packing::unpack(signed).is_err(), "accepted {text}"),
        }
    }
}

#[test]
fn parents_and_children_match() {
    let topology_json = load("topology.json");
    for record in topology_json["parent_of"].as_array().unwrap() {
        let cell = cell_of_gid(record["gid"].as_str().unwrap());
        let parent = topology::parent(cell).expect("level above zero has a parent");
        assert_eq!(gid_of(parent), record["parent_gid"].as_str().unwrap());
        assert_eq!(parent.level as u64, record["level"].as_u64().unwrap() - 1);
    }
    for record in topology_json["children_of"].as_array().unwrap() {
        let cell = cell_of_gid(record["gid"].as_str().unwrap());
        let children = topology::children(cell).unwrap();
        assert_eq!(
            sorted_gids(children),
            strings(&record["child_gids"]),
            "{}",
            record["gid"]
        );
    }
}

fn descendant_count(parent: Cell, child_level: u8) -> usize {
    let mut cells = vec![parent];
    for _ in parent.level..child_level {
        cells = cells
            .iter()
            .flat_map(|cell| topology::children(*cell).unwrap())
            .collect();
    }
    cells.len()
}

#[test]
fn descendant_ranges_match_for_id_and_their_counts() {
    let topology_json = load("topology.json");
    for record in topology_json["descendant_ranges"].as_array().unwrap() {
        let parent = cell_of_gid(record["parent_gid"].as_str().unwrap());
        let child_level = record["child_level"].as_u64().unwrap() as u8;
        let (lo, hi) = topology::descendant_range(1, parent, child_level).unwrap();
        assert_eq!(lo.to_string(), record["lo"].as_str().unwrap());
        assert_eq!(hi.to_string(), record["hi"].as_str().unwrap());
        assert_eq!(
            descendant_count(parent, child_level) as u64,
            record["descendant_count"].as_u64().unwrap()
        );
    }
}

#[test]
fn descendant_ranges_match_for_every_domain() {
    let topology_json = load("topology.json");
    let domain_ids = domains();
    let records = topology_json["descendant_ranges_by_domain"]["records"]
        .as_array()
        .unwrap();
    assert!(!records.is_empty());
    for record in records {
        let parent = cell_of_gid(record["parent_gid"].as_str().unwrap());
        let child_level = record["child_level"].as_u64().unwrap() as u8;
        for (code, bounds) in record["ranges"].as_object().unwrap() {
            let domain_id = domain_ids[code];
            let (lo, hi) = topology::descendant_range(domain_id, parent, child_level).unwrap();
            assert_eq!(lo.to_string(), bounds["lo"].as_str().unwrap(), "{code}");
            assert_eq!(hi.to_string(), bounds["hi"].as_str().unwrap(), "{code}");
        }
    }
}

#[test]
fn neighbour_sets_match() {
    let topology_json = load("topology.json");
    for record in topology_json["neighbours"].as_array().unwrap() {
        let cell = cell_from_json(&record["level"], &record["x_idx"], &record["y_idx"]);
        assert_eq!(
            sorted_gids(topology::k_ring(cell, 1).unwrap()),
            strings(&record["k_ring_1"])
        );
        assert_eq!(
            sorted_gids(topology::k_ring(cell, 2).unwrap()),
            strings(&record["k_ring_2"])
        );
        assert_eq!(
            sorted_gids(topology::k_disk(cell, 1).unwrap()),
            strings(&record["k_disk_1"])
        );
    }
}

#[test]
fn distances_match_the_square_root_formula() {
    let topology_json = load("topology.json");
    for record in topology_json["distances"].as_array().unwrap() {
        let a = cell_of_gid(record["a_gid"].as_str().unwrap());
        let b = cell_of_gid(record["b_gid"].as_str().unwrap());
        let level = a.level;
        assert_eq!(
            codec::cell_side_m(level).unwrap(),
            record["cell_side_m"].as_f64().unwrap()
        );
        let expected = record["distance_m"].as_f64().unwrap();
        let actual = topology::distance_m(a, b).unwrap();
        assert!(
            (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
            "{actual} != {expected}"
        );
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Uri => "uri",
        Kind::Gid => "gid",
        Kind::PackedId => "packed_id",
        Kind::XyIndices => "xy_indices",
        Kind::NotSpatial => "none",
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Certain => "certain",
        Status::NeedsDomain => "needs_domain",
        Status::NeedsLevel => "needs_level",
        Status::NeedsConfirmation => "needs_confirmation",
        Status::Rejected => "rejected",
    }
}

fn column_from_json(name: &str, spec: &Json) -> Column {
    let dtype = match spec["dtype"].as_str().unwrap() {
        "string" => DType::Str,
        "int64" => DType::Int64,
        _ => DType::Other,
    };
    let values = spec["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| match (dtype, item) {
            (_, Json::Null) => Value::Null,
            (DType::Int64, text) => Value::Int(text.as_str().unwrap().parse().unwrap()),
            (_, text) => Value::Str(text.as_str().unwrap().to_string()),
        })
        .collect();
    Column {
        name: name.to_string(),
        dtype,
        values,
    }
}

#[test]
fn detection_cases_match_the_reference() {
    let detect_json = load("detect.json");
    let cases = detect_json["cases"].as_array().unwrap();
    assert!(cases.len() >= 15);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let columns: Vec<Column> = case["columns"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(column, spec)| column_from_json(column, spec))
            .collect();
        let domain = case.get("domain_code").and_then(Json::as_str);
        let level = case
            .get("level")
            .and_then(Json::as_u64)
            .map(|value| value as u8);
        let results = detect::detect_table(&columns, domain, level);
        for (key, expected) in case["expect"].as_object().unwrap() {
            let result = results
                .iter()
                .find(|result| &result.column == key)
                .unwrap_or_else(|| panic!("{name}: no result for {key}"));
            assert_eq!(kind_name(result.kind), expected["kind"], "{name}/{key}");
            assert_eq!(
                status_name(result.status),
                expected["status"],
                "{name}/{key}"
            );
            if let Some(codes) = expected.get("domain_codes") {
                assert_eq!(result.domain_codes, strings(codes), "{name}/{key}");
            }
            if let Some(ids) = expected.get("domain_ids") {
                let ids: Vec<u16> = ids
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| id.as_u64().unwrap() as u16)
                    .collect();
                assert_eq!(result.domain_ids, ids, "{name}/{key}");
            }
            if let Some(levels) = expected.get("levels") {
                let levels: Vec<u8> = levels
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| l.as_u64().unwrap() as u8)
                    .collect();
                assert_eq!(result.levels, levels, "{name}/{key}");
            }
        }
    }
}
