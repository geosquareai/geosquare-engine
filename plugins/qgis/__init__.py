"""GeoSquare E14 QGIS plugin entry point."""


def classFactory(iface):
    from .plugin import E14QgisPlugin

    return E14QgisPlugin(iface)
