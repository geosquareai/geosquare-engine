"""Manual QGIS check for the E14 plugin. Run it inside QGIS, not with pytest.

Why manual: this check needs the QGIS GUI application. The Qt dialogs are
replaced by scripted answers, so the run needs no clicks.

Steps:
1. Build the engine wheel: cd bindings/python && uvx maturin build -o WHEELS
2. Unzip the wheel into SITE (any folder). The folder must contain geosquare_engine.
3. In QGIS, open Plugins > Python Console and run:
   import os
   os.environ.update(GSQ_SITE="SITE", GSQ_PLUGIN_SRC="plugins/qgis",
                     GSQ_POINTS="vectors/spec-2.0/points.json",
                     RESULT_FILE="/tmp/e14-result.json", QGIS_PREFIX="")
   exec(open("plugins/qgis/tests/manual_qgis_check.py").read())
The script prints PASS or FAIL for each check. It writes RESULT_FILE only when it starts QGIS itself.
"""


import json
import os
import shutil
import sys

SITE = os.environ["GSQ_SITE"]
PLUGIN_SRC = os.environ["GSQ_PLUGIN_SRC"]
POINTS = os.environ["GSQ_POINTS"]

pkg = os.path.join(SITE, "geosquare_e14")
shutil.rmtree(pkg, ignore_errors=True)
shutil.copytree(PLUGIN_SRC, pkg, ignore=shutil.ignore_patterns("tests", "__pycache__"))
sys.path.insert(0, SITE)

from qgis.core import (  # noqa: E402
    QgsApplication,
    QgsDistanceArea,
    QgsFeature,
    QgsField,
    QgsMapLayer,
    QgsProject,
    QgsVectorLayer,
)
from qgis.PyQt.QtCore import QVariant  # noqa: E402
from qgis.PyQt.QtWidgets import QToolBar, QWidget  # noqa: E402

qgs = QgsApplication.instance()
own_app = qgs is None
if own_app:
    QgsApplication.setPrefixPath(os.environ["QGIS_PREFIX"], True)
    qgs = QgsApplication([], True)
    qgs.initQgis()

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("PASS " if ok else "FAIL ") + name + (f" :: {detail}" if detail else ""), flush=True)


try:
    import geosquare_engine as gs
    import geosquare_e14.gid_layer_qgis as adapter
    import geosquare_e14.plugin as plugin_module

    class FakeInputDialog:
        item_answers: list = []
        int_answers: list = []
        asked: list = []

        @staticmethod
        def getItem(parent, title, label, items, current, editable):
            FakeInputDialog.asked.append((title, list(items)))
            return FakeInputDialog.item_answers.pop(0), True

        @staticmethod
        def getInt(parent, title, label, value, minimum, maximum, step):
            FakeInputDialog.asked.append((title, [minimum, maximum]))
            return FakeInputDialog.int_answers.pop(0), True

    class FakeMessageBox:
        shown: list = []

        @staticmethod
        def critical(parent, title, text):
            FakeMessageBox.shown.append(text)

    adapter.QInputDialog = FakeInputDialog
    adapter.QMessageBox = FakeMessageBox

    class FakeIface:
        def __init__(self):
            self.active = None
            self.main = QWidget()
            self.menu_actions = []

        def mainWindow(self):
            return self.main

        def activeLayer(self):
            return self.active

        def setActiveLayer(self, layer):
            self.active = layer

        def addToolBar(self, name):
            return QToolBar(name, self.main)

        def addPluginToMenu(self, menu, action):
            self.menu_actions.append((menu, action.text()))

        def removePluginMenu(self, menu, action):
            pass

        def removeToolBarIcon(self, action):
            pass

    def make_source(rows):
        layer = QgsVectorLayer("Point?crs=EPSG:4326", "source gids", "memory")
        layer.dataProvider().addAttributes(
            [QgsField("gid", QVariant.String), QgsField("population", QVariant.Double)]
        )
        layer.updateFields()
        features = []
        for gid, population in rows:
            feature = QgsFeature(layer.fields())
            feature.setAttributes([gid, population])
            features.append(feature)
        layer.dataProvider().addFeatures(features)
        QgsProject.instance().addMapLayer(layer)
        return layer

    def area_m2(layer, feature):
        measure = QgsDistanceArea()
        measure.setSourceCrs(layer.crs(), QgsProject.instance().transformContext())
        measure.setEllipsoid("WGS84")
        return measure.measureArea(feature.geometry())

    with open(POINTS, encoding="utf-8") as handle:
        case = next(c for c in json.load(handle)["cases"] if c["domain_code"] == "ID")
    levels = sorted(int(k) for k in case["gids"])
    level = 9 if 9 in levels else levels[-1]
    gid_a = case["gids"][str(level)]
    side_m = gs.cell_side_m(level)

    # 1. Cells from GID field
    iface = FakeIface()
    source = make_source([(gid_a, 12.0)])
    FakeInputDialog.item_answers = ["gid", "ID (released)"]
    iface.setActiveLayer(source)
    adapter.add_cells_from_gid(iface)
    cells = iface.activeLayer()
    check(
        "cells layer is added and active",
        cells is not None and cells is not source and cells.type() == QgsMapLayer.VectorLayer,
    )
    names = [field.name() for field in cells.fields()]
    check("cells layer has gid, level and population", {"gid", "level", "population"} <= set(names), str(names))
    check("cells layer is EPSG:4326", cells.crs().authid() == "EPSG:4326", cells.crs().authid())
    check("one feature per input row", cells.featureCount() == 1, str(cells.featureCount()))
    feature = next(iter(cells.getFeatures()))
    area = area_m2(cells, feature)
    expected = side_m * side_m
    check(
        f"level {level} cell area is near {side_m:.0f} m squared",
        abs(area - expected) / expected < 0.10,
        f"measured {area:.4g} m2, expected {expected:.4g} m2",
    )
    check("level field equals the GID level", int(feature["level"]) == level, str(feature["level"]))

    # 2. Rollup to a coarser level
    target = level - 1
    FakeInputDialog.int_answers = [target]
    adapter.rollup_to_level(iface)
    rolled = iface.activeLayer()
    check("rollup layer is added and active", rolled is not None and rolled is not cells)
    total_in = sum(f["population"] for f in cells.getFeatures())
    total_out = sum(f["population"] for f in rolled.getFeatures())
    check(
        "rollup keeps the population total",
        abs(total_in - total_out) < 1e-9,
        f"in {total_in}, out {total_out}",
    )
    levels_out = {int(f["level"]) for f in rolled.getFeatures()}
    check("rollup output sits at the target level", levels_out == {target}, str(levels_out))

    # 3. Invalid GID shows an error and adds no layer
    FakeMessageBox.shown = []
    bad = make_source([("not-a-gid", 1.0)])
    before = len(QgsProject.instance().mapLayers())
    FakeInputDialog.item_answers = ["gid", "ID (released)"]
    iface.setActiveLayer(bad)
    adapter.add_cells_from_gid(iface)
    check("invalid GID shows an error dialog", len(FakeMessageBox.shown) == 1, str(FakeMessageBox.shown))
    check(
        "invalid GID adds no cell layer",
        len(QgsProject.instance().mapLayers()) == before,
    )

    # 4. Plugin wiring: menu actions exist and the action runs the same operation
    plugin_iface = FakeIface()
    plugin = plugin_module.E14QgisPlugin(plugin_iface)
    plugin.initGui()
    check(
        "plugin adds two menu actions",
        [text for _, text in plugin_iface.menu_actions] == ["Cells from GID field", "Rollup to level"],
        str(plugin_iface.menu_actions),
    )
    action_source = make_source([(gid_a, 5.0)])
    FakeInputDialog.item_answers = ["gid", "ID (released)"]
    plugin_iface.setActiveLayer(action_source)
    count_before = len(QgsProject.instance().mapLayers())
    plugin.actions[0].trigger()
    check("menu action creates a cells layer", len(QgsProject.instance().mapLayers()) == count_before + 1)
    plugin.unload()
    check("unload removes the toolbar", plugin.toolbar is None and plugin.actions == [])
except Exception as exc:  # report the failure instead of hiding it
    import traceback

    traceback.print_exc()
    check("script ran to the end", False, repr(exc))
finally:
    if own_app:
        qgs.exitQgis()

failed = [name for name, ok, _ in results if not ok]
print(json.dumps({"passed": len(results) - len(failed), "failed": failed}), flush=True)
if own_app:
    with open(os.environ["RESULT_FILE"], "w", encoding="utf-8") as handle:
        json.dump({"failed": failed, "checks": results}, handle, indent=2)
    sys.stdout.flush()
    os._exit(1 if failed or not results else 0)
