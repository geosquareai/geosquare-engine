"""Thin QGIS adapter for the pure E14 cell-layer operations."""

from __future__ import annotations

import json

from qgis.PyQt.QtCore import QVariant
from qgis.PyQt.QtWidgets import QInputDialog, QMessageBox
from qgis.core import (
    QgsFeature,
    QgsField,
    QgsGeometry,
    QgsMapLayer,
    QgsPointXY,
    QgsProject,
    QgsVectorLayer,
)

from .gid_layer_logic import (
    CellFeature,
    GidLayerError,
    build_cell_features,
    rollup_cell_features,
)


DOMAIN_OPTIONS = (
    ("ID (released)", 1, False),
    ("VN (candidate CRS)", 2, True),
    ("BN (released)", 3, False),
    ("KH (candidate CRS)", 4, True),
    ("LA (released)", 5, False),
    ("MY (released)", 6, False),
    ("MM (candidate CRS)", 7, True),
    ("PH (released)", 8, False),
    ("SG (released)", 9, False),
    ("TH (candidate CRS)", 10, True),
    ("TL (candidate CRS)", 11, True),
)
MAX_SEGMENT_LENGTH_M = 25_000.0


def _error(iface, message: str) -> None:
    QMessageBox.critical(iface.mainWindow(), "GeoSquare E14", message)


def _vector_layer(iface):
    layer = iface.activeLayer()
    if layer is None or layer.type() != QgsMapLayer.VectorLayer:
        _error(iface, "Select a vector layer first.")
        return None
    return layer


def _choose_domain(iface):
    labels = [label for label, _, _ in DOMAIN_OPTIONS]
    label, ok = QInputDialog.getItem(
        iface.mainWindow(),
        "GeoSquare domain",
        "Domain used by the GID grid:",
        labels,
        0,
        False,
    )
    if not ok:
        return None
    return next(item for item in DOMAIN_OPTIONS if item[0] == label)


def _choose_gid_field(iface, layer):
    fields = [field.name() for field in layer.fields()]
    if not fields:
        _error(iface, "The selected vector layer has no fields.")
        return None
    field, ok = QInputDialog.getItem(
        iface.mainWindow(),
        "GID field",
        "Field containing GeoSquare GIDs:",
        fields,
        0,
        False,
    )
    return field if ok else None


def _numeric_fields(layer, gid_field: str) -> list[str]:
    return [
        field.name()
        for field in layer.fields()
        if field.name() not in {gid_field, "gid", "level"} and field.isNumeric()
    ]


def _rows(layer, gid_field: str, numeric_fields: list[str]):
    for feature in layer.getFeatures():
        yield {
            gid_field: feature[gid_field],
            **{name: feature[name] for name in numeric_fields},
        }


def _geometry(ring):
    return QgsGeometry.fromPolygonXY(
        [[QgsPointXY(longitude, latitude) for longitude, latitude in ring]]
    )


def _memory_layer(name: str, numeric_fields: list[QgsField]):
    layer = QgsVectorLayer("Polygon?crs=EPSG:4326", name, "memory")
    if not layer.isValid():
        raise RuntimeError("QGIS could not create the EPSG:4326 memory layer.")
    fields = [QgsField("gid", QVariant.String), QgsField("level", QVariant.Int)]
    fields.extend(QgsField(field) for field in numeric_fields)
    if not layer.dataProvider().addAttributes(fields):
        raise RuntimeError("QGIS could not add fields to the memory layer.")
    layer.updateFields()
    return layer


def _add_features(layer, features: list[CellFeature], numeric_names: list[str]) -> None:
    qgis_features = []
    for feature in features:
        qgis_feature = QgsFeature(layer.fields())
        qgis_feature.setGeometry(_geometry(feature.geometry))
        qgis_feature.setAttributes(
            [feature.gid, feature.level]
            + [feature.attributes[name] for name in numeric_names]
        )
        qgis_features.append(qgis_feature)
    ok, _ = layer.dataProvider().addFeatures(qgis_features)
    if not ok:
        raise RuntimeError("QGIS could not add cell features to the memory layer.")
    layer.updateExtents()


def _publish(iface, layer, domain_id: int, allow_candidate: bool, numeric_names, source_level):
    layer.setCustomProperty("geosquare_e14/domain_id", domain_id)
    layer.setCustomProperty("geosquare_e14/allow_candidate_crs", allow_candidate)
    layer.setCustomProperty("geosquare_e14/numeric_fields", json.dumps(numeric_names))
    layer.setCustomProperty("geosquare_e14/source_level", source_level)
    QgsProject.instance().addMapLayer(layer)
    iface.setActiveLayer(layer)


def add_cells_from_gid(iface) -> None:
    """Run the Cells from GID field action."""
    source = _vector_layer(iface)
    if source is None:
        return
    gid_field = _choose_gid_field(iface, source)
    if gid_field is None:
        return
    domain = _choose_domain(iface)
    if domain is None:
        return
    label, domain_id, allow_candidate = domain
    numeric_names = _numeric_fields(source, gid_field)
    numeric_fields = [source.fields().at(source.fields().indexOf(name)) for name in numeric_names]

    try:
        features = build_cell_features(
            _rows(source, gid_field, numeric_names),
            gid_field=gid_field,
            numeric_fields=numeric_names,
            domain_id=domain_id,
            max_segment_length_m=MAX_SEGMENT_LENGTH_M,
            allow_candidate_crs=allow_candidate,
        )
        output = _memory_layer(f"Cells from {source.name()} ({label})", numeric_fields)
        _add_features(output, features, numeric_names)
        _publish(iface, output, domain_id, allow_candidate, numeric_names, features[0].level)
    except GidLayerError as exc:
        _error(iface, str(exc))
    except Exception as exc:
        _error(iface, f"Could not create cells: {exc}")


def _generated_layer(iface):
    layer = _vector_layer(iface)
    if layer is None:
        return None
    if layer.customProperty("geosquare_e14/domain_id", None) is None:
        _error(iface, "Select a layer created by 'Cells from GID field' first.")
        return None
    return layer


def rollup_to_level(iface) -> None:
    """Run the Rollup to level action."""
    layer = _generated_layer(iface)
    if layer is None:
        return
    try:
        domain_id = int(layer.customProperty("geosquare_e14/domain_id"))
        allow_candidate = bool(layer.customProperty("geosquare_e14/allow_candidate_crs", False))
        numeric_names = json.loads(layer.customProperty("geosquare_e14/numeric_fields", "[]"))
        source_level = int(layer.customProperty("geosquare_e14/source_level"))
    except (TypeError, ValueError, json.JSONDecodeError) as exc:
        _error(iface, f"The selected GID layer has invalid E14 metadata: {exc}")
        return

    target_level, ok = QInputDialog.getInt(
        iface.mainWindow(),
        "Rollup to level",
        f"Target level (0 to {source_level}):",
        source_level,
        0,
        source_level,
        1,
    )
    if not ok:
        return

    fields = {field.name(): field for field in layer.fields()}
    try:
        cell_features = []
        for feature in layer.getFeatures():
            cell_features.append(
                CellFeature(
                    gid=feature["gid"],
                    level=int(feature["level"]),
                    geometry=tuple(),
                    attributes={name: feature[name] for name in numeric_names},
                )
            )
        rolled = rollup_cell_features(
            cell_features,
            target_level=target_level,
            domain_id=domain_id,
            max_segment_length_m=MAX_SEGMENT_LENGTH_M,
            allow_candidate_crs=allow_candidate,
        )
        output = _memory_layer(
            f"Rollup {layer.name()} to level {target_level}",
            [fields[name] for name in numeric_names],
        )
        _add_features(output, rolled, numeric_names)
        _publish(
            iface,
            output,
            domain_id,
            allow_candidate,
            numeric_names,
            target_level,
        )
    except GidLayerError as exc:
        _error(iface, str(exc))
    except Exception as exc:
        _error(iface, f"Could not roll up cells: {exc}")
