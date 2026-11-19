"""QGIS menu and toolbar wiring for the GeoSquare E14 plugin."""

from qgis.PyQt.QtWidgets import QAction

from .gid_layer_qgis import add_cells_from_gid, rollup_to_level


class E14QgisPlugin:
    def __init__(self, iface):
        self.iface = iface
        self.menu = "&GeoSquare E14"
        self.toolbar = None
        self.actions = []

    def initGui(self):
        self.toolbar = self.iface.addToolBar("GeoSquare E14")
        self.toolbar.setObjectName("GeoSquareE14Toolbar")
        self._add_action("Cells from GID field", add_cells_from_gid)
        self._add_action("Rollup to level", rollup_to_level)

    def _add_action(self, text, callback):
        action = QAction(text, self.iface.mainWindow())
        action.triggered.connect(lambda checked=False: callback(self.iface))
        self.iface.addPluginToMenu(self.menu, action)
        self.toolbar.addAction(action)
        self.actions.append(action)

    def unload(self):
        for action in self.actions:
            self.iface.removePluginMenu(self.menu, action)
            self.iface.removeToolBarIcon(action)
            if self.toolbar is not None:
                self.toolbar.removeAction(action)
        if self.toolbar is not None:
            self.toolbar.deleteLater()
        self.actions.clear()
        self.toolbar = None
