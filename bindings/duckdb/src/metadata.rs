//! Descriptive metadata of a cell dataset, beyond the identity the writer fills in itself.
//!
//! Contract fields (geosquare-v2 cell manifest): `value_fields[].unit`, `source.*`,
//! `coverage.*`, `provenance.notes`. Fields the contract has no place for (it forbids extra
//! keys) go to a separate document, `dataset_info`: title, description, data period and
//! version, and a description per column. Iceberg keeps it in the table property
//! `geosquare.dataset_info` (plus each column's `doc` in the schema); flat and partitioned
//! datasets keep it in `<path>/dataset_info.json`.

use crate::sql::{Params, E};
use serde_json::{json, Map, Value as Json};

pub const INFO_KEY: &str = "geosquare.dataset_info";
pub const INFO_FILE: &str = "dataset_info.json";

/// Per-column text: one value for every value column, or `{column or pattern: value}`
/// where a pattern may use `*` (e.g. `{"people_*": "persons"}`; exact names win).
#[derive(Debug, Clone, Default)]
pub struct PerColumn {
    all: Option<String>,
    map: Vec<(String, String)>,
}

impl PerColumn {
    fn parse(text: Option<String>, what: &str) -> Result<Self, E> {
        let Some(t) = text else {
            return Ok(Self::default());
        };
        let t = t.trim().to_string();
        if !t.starts_with('{') {
            return Ok(Self {
                all: Some(t),
                map: Vec::new(),
            });
        }
        match serde_json::from_str::<Json>(&t) {
            Ok(Json::Object(m)) => Ok(Self {
                all: None,
                map: m
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            v.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| v.to_string()),
                        )
                    })
                    .collect(),
            }),
            _ => Err(format!("{what} must be text or a JSON object {{column: text}}").into()),
        }
    }

    fn is_empty(&self) -> bool {
        self.all.is_none() && self.map.is_empty()
    }

    /// The value for `column`: exact key, else the longest matching pattern, else `all`.
    pub fn get(&self, column: &str) -> Option<&str> {
        if let Some((_, v)) = self.map.iter().find(|(k, _)| k == column) {
            return Some(v);
        }
        self.map
            .iter()
            .filter(|(k, _)| k.contains('*') && glob(k, column))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, v)| v.as_str())
            .or(self.all.as_deref())
    }
}

fn glob(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !s.starts_with(first) || !s.ends_with(last) || s.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for p in &parts[1..parts.len() - 1] {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    true
}

/// Descriptive metadata given to write_cell_dataset or update_cell_metadata.
#[derive(Debug, Clone, Default)]
pub struct Extra {
    pub units: PerColumn,
    pub descriptions: PerColumn,
    pub title: Option<String>,
    pub description: Option<String>,
    pub data_period: Option<String>,
    pub data_version: Option<String>,
    pub source: Option<Map<String, Json>>,
    pub coverage: Option<Map<String, Json>>,
    pub notes: Option<String>,
}

fn object(text: Option<String>, what: &str) -> Result<Option<Map<String, Json>>, E> {
    match text {
        None => Ok(None),
        Some(t) => match serde_json::from_str::<Json>(&t) {
            Ok(Json::Object(m)) => Ok(Some(m)),
            _ => Err(format!("{what} must be a JSON object").into()),
        },
    }
}

impl Extra {
    pub fn from_params(p: &Params) -> Result<Self, E> {
        Ok(Self {
            units: PerColumn::parse(p.text("units"), "units")?,
            descriptions: PerColumn::parse(p.text("column_descriptions"), "column_descriptions")?,
            title: p.text("title"),
            description: p.text("description"),
            data_period: p.text("data_period"),
            data_version: p.text("data_version"),
            source: object(p.text("source"), "source")?,
            coverage: object(p.text("coverage"), "coverage")?,
            notes: p.text("notes"),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
            && self.descriptions.is_empty()
            && self.title.is_none()
            && self.description.is_none()
            && self.data_period.is_none()
            && self.data_version.is_none()
            && self.source.is_none()
            && self.coverage.is_none()
            && self.notes.is_none()
    }
}

/// Merge `given` into the manifest section `name`; only keys of the contract are allowed.
fn merge_section(
    manifest: &mut Json,
    name: &str,
    given: &Option<Map<String, Json>>,
) -> Result<(), E> {
    let Some(given) = given else { return Ok(()) };
    let section = manifest
        .get_mut(name)
        .and_then(Json::as_object_mut)
        .ok_or_else(|| format!("manifest has no {name} section"))?;
    for (k, v) in given {
        if !section.contains_key(k) {
            let allowed: Vec<&String> = section.keys().collect();
            return Err(format!(
                "{name}.{k} is not a field of the cell manifest; allowed: {allowed:?}"
            )
            .into());
        }
        section.insert(k.clone(), v.clone());
    }
    Ok(())
}

/// Apply the contract fields of `e` to a cell manifest.
pub fn apply(manifest: &mut Json, e: &Extra) -> Result<(), E> {
    if let Some(fields) = manifest
        .get_mut("value_fields")
        .and_then(Json::as_array_mut)
    {
        for f in fields {
            let name = f
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(unit) = e.units.get(&name) {
                f["unit"] = json!(unit);
            }
        }
    }
    merge_section(manifest, "source", &e.source)?;
    merge_section(manifest, "coverage", &e.coverage)?;
    if let Some(n) = &e.notes {
        manifest["provenance"]["notes"] = json!(n);
    }
    Ok(())
}

/// The dataset_info document: `prev` updated with `e`, units mirrored from the manifest.
pub fn info(prev: Option<&Json>, manifest: &Json, e: &Extra) -> Json {
    let mut out = prev
        .cloned()
        .filter(Json::is_object)
        .unwrap_or_else(|| json!({}));
    for (k, v) in [
        ("title", &e.title),
        ("description", &e.description),
        ("data_period", &e.data_period),
        ("data_version", &e.data_version),
    ] {
        if let Some(v) = v {
            out[k] = json!(v);
        } else if out.get(k).is_none() {
            out[k] = Json::Null;
        }
    }
    let mut columns = out
        .get("columns")
        .and_then(Json::as_object)
        .cloned()
        .unwrap_or_default();
    for f in manifest
        .get("value_fields")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let name = f.get("name").and_then(Json::as_str).unwrap_or("");
        let entry = columns
            .entry(name.to_string())
            .or_insert_with(|| json!({"description": null}));
        if let Some(d) = e.descriptions.get(name) {
            entry["description"] = json!(d);
        }
        entry["unit"] = f.get("unit").cloned().unwrap_or(Json::Null);
    }
    out["columns"] = Json::Object(columns);
    out
}

/// Set each field's `doc` in an Iceberg schema from the dataset_info column descriptions.
pub fn schema_docs(schema: &mut Json, info: &Json) {
    let cols = info.get("columns").and_then(Json::as_object);
    for f in schema
        .get_mut("fields")
        .and_then(Json::as_array_mut)
        .into_iter()
        .flatten()
    {
        let name = f
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        match cols
            .and_then(|c| c.get(&name))
            .and_then(|c| c.get("description"))
            .and_then(Json::as_str)
        {
            Some(d) => {
                f["doc"] = json!(d);
            }
            None => {
                if let Some(o) = f.as_object_mut() {
                    o.remove("doc");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let p = PerColumn::parse(Some(r#"{"people_*": "persons", "people_*_night": "persons at night", "people_counts": "all"}"#.into()), "x").unwrap();
        assert_eq!(p.get("people_counts"), Some("all"));
        assert_eq!(p.get("people_gen_z_night"), Some("persons at night"));
        assert_eq!(p.get("people_gen_z"), Some("persons"));
        assert_eq!(p.get("hazard_eq"), None);
        let a = PerColumn::parse(Some("index 0-1".into()), "x").unwrap();
        assert_eq!(a.get("anything"), Some("index 0-1"));
        assert!(glob("a*b*c", "aXXbYYc") && !glob("a*b*c", "aXXc"));
    }
}
