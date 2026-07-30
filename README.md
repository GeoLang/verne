# Verne

[![CI](https://github.com/GeoLang/verne/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/verne/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

Read-only inventory of a third-party geospatial source. Verne opens a file, lists
what is in it, and says of each thing whether GeoLang could hold it faithfully,
would have to approximate it, or has no home for it at all. It converts nothing
and writes nothing back: the report is the product.

## Scope

v0.1 reads KML and KMZ, and nothing else. Pure Rust, no GDAL, no network access,
no credentials. Verne does not follow a NetworkLink or fetch an overlay image, so
anything behind a URL is reported as outside the inventory rather than inspected.

## Usage

```bash
# print a markdown report
verne inspect sites.kml

# print the markdown report and also write it as JSON
verne inspect sites.kmz --json sites.json
```

## Verdicts

Every row of the report carries one of three verdicts:

- **faithful** — carried across without loss, naming the component it goes to.
- **approximated** — carried across with something left behind, and the row has
  to say what. `Losses` cannot be constructed empty, so an approximated verdict
  that names nothing lost cannot be written down.
- **unsupported** — no home in GeoLang, with the reason.

An inventory that cannot be produced is an error, never an empty list, so a
source verne failed to read is not mistaken for a clean one.

## Writing an adapter

Implement `verne_core::Source` in any crate that depends on `verne-core`, in this
workspace or outside it. The trait has two methods, `describe` and `inventory`,
both taking `&self`, and the adapter keeps its own error type. There is no
registry and no dynamic loading: whoever builds the binary picks the adapters.

## Crates

```
verne-core — the inventory model, verdicts, and markdown/JSON reports
verne-kml  — the KML and KMZ adapter
verne-cli  — the command-line interface
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
