# Verne

[![CI](https://github.com/GeoLang/verne/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/verne/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

Read-only inventory of a third-party geospatial source. Verne opens a file, lists
what is in it, and says of each thing whether GeoLang could hold it faithfully,
would have to approximate it, or has no home for it at all. It converts nothing
and writes nothing back: the report is the product.

## Scope

v0.2 reads KML and KMZ, and Esri file geodatabases. No network access and no
credentials in either. Verne does not follow a NetworkLink or fetch an overlay
image, so anything behind a URL is reported as outside the inventory rather than
inspected, and it never opens a raster or an attachment blob.

The KML side is pure Rust and needs no GDAL. The geodatabase side is behind the
`gdb` feature and needs GDAL 3.8 or later, read through GDAL's own OpenFileGDB
driver: Esri's FileGDB SDK is never loaded, and the open call names the driver it
allows. Without the feature the geodatabase adapter is not built at all and the
rest of verne is unaffected.

## Usage

```bash
# print a markdown report
verne inspect sites.kml

# print the markdown report and also write it as JSON
verne inspect sites.kmz --json sites.json

# geodatabases need the feature at build time
cargo install --path crates/verne-cli --features gdb
verne inspect wells.gdb
```

## Verdicts

Every row of the report carries one of four verdicts:

- **faithful** — carried across without loss, naming the component it goes to.
- **approximated** — carried across with something left behind, and the row has
  to say what. `Losses` cannot be constructed empty, so an approximated verdict
  that names nothing lost cannot be written down.
- **unsupported** — no home in GeoLang, with the reason.
- **not applicable** — the source cannot have the thing at all, so nothing was
  carried and nothing was lost. A file geodatabase and versioning, for instance:
  versioning is an enterprise geodatabase feature.

An inventory that cannot be produced is an error, never an empty list, so a
source verne failed to read is not mistaken for a clean one.

## Geodatabases

What is inventoried: feature classes and tables with their fields, aliases and
domain bindings, the system and `__ATTACH` tables so the report can name what it
does not interpret, feature datasets, coded and range domains, subtypes,
relationship classes, attachments, annotation and dimension classes, layer
metadata records, and every catalogue item GDAL reads no definition for.

What GDAL does not model, and verne therefore cannot see: subtypes (read out of
the layer definition XML instead), annotation text and symbology, topologies and
their rules, geometric and utility networks, parcel fabrics, terrains, mosaic
datasets, relationship rules, attribute rules and contingent values. Those are
named from the catalogue and go no further.

Working on the adapter needs GDAL's python bindings (`python3-gdal`) as well as
the headers: the test geodatabases are built at test time by
`crates/verne-gdb/tests/fixture.py`. GDAL cannot write Esri subtypes, an
annotation class or a topology, so those parts of the fixture are definition XML
written by hand into the catalogue and read back through the driver.

## Writing an adapter

Implement `verne_core::Source` in any crate that depends on `verne-core`, in this
workspace or outside it. The trait has two methods, `describe` and `inventory`,
both taking `&self`, and the adapter keeps its own error type. There is no
registry and no dynamic loading: whoever builds the binary picks the adapters.

## Crates

```
verne-core — the inventory model, verdicts, and markdown/JSON reports
verne-kml  — the KML and KMZ adapter
verne-gdb  — the Esri file geodatabase adapter, behind the `gdal` feature
verne-cli  — the command-line interface
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
