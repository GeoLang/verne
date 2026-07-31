# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- `verne-arcgis`: hosted ArcGIS feature services over their REST API, no GDAL,
  always built. `verne inspect` and `verne extract` take a FeatureServer root
  URL; the token comes from `VERNE_ARCGIS_TOKEN` as an `X-Esri-Authorization`
  header and a public service needs none.
- The REST extraction writes the same feature files, attachments and sidecar a
  geodatabase extraction does, minus the GeoPackage, so `verne load` reads both
  without knowing which it was handed. Features come down `/query` a page at a
  time in EPSG:4326 (the service transforms; no native original rides on the
  inserts), Date attributes are rewritten from epoch milliseconds to RFC 3339,
  and attachments are listed through `queryAttachments` or one feature at a
  time and attributed through object ids.
- Coded and range domains, subtypes with defaults and per-subtype domain
  assignments, and relationship classes paired from their two layer ends are
  carried into the sidecar; a relationship's labels are created empty because
  the REST layer description has none, and the report says so.
- ArcGIS's habit of answering a failed request with HTTP 200 and an error
  object in the body surfaces as an error naming the route.
