// deck.gl layer for GeoSquare cells. Use it like H3HexagonLayer: pass rows with a gid and a value.
// deck.gl is a peer dependency. Call the wasm `init()` once before the layer draws.
import { CompositeLayer } from "@deck.gl/core";
import { PolygonLayer } from "@deck.gl/layers";
import { cellPolygons } from "./cells.mjs";

export class GeoSquareLayer extends CompositeLayer {
  renderLayers() {
    const { data, getGid, getValue, domainId, zoom, aggregation, maxSegmentLengthM, getFillColor, ...rest } = this.props;
    const cells = cellPolygons(data, { domainId, getGid, getValue, zoom, aggregation, maxSegmentLengthM });
    return new PolygonLayer(this.getSubLayerProps({ id: "cells", ...rest }), {
      data: cells,
      getPolygon: (d) => d.polygon,
      getFillColor,
      updateTriggers: { getFillColor },
    });
  }
}

GeoSquareLayer.layerName = "GeoSquareLayer";
GeoSquareLayer.defaultProps = {
  data: { type: "data", value: [], async: false },
  getGid: (d) => d.gid,
  getValue: (d) => d.value,
  domainId: undefined,
  zoom: null,
  aggregation: null,
  maxSegmentLengthM: 25000,
  getFillColor: [200, 70, 30, 180],
};
