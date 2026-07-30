# Verne

[![CI](https://github.com/GeoLang/verne/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/verne/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

Read-only inventory of a third-party geospatial source. Verne opens a file, lists
what is in it, and says of each thing whether GeoLang could hold it faithfully,
would have to approximate it, or has no home for it at all.

It can then act on that report: `verne extract` writes a geodatabase out as a
GeoPackage and a sidecar, and `verne load` creates what the sidecar describes in
a running ptolemy. The source is never written to, in either.

## Scope

v0.3 reads KML and KMZ, and Esri file geodatabases, and extracts a geodatabase
into a GeoPackage and a ptolemy sidecar.

Reading and extracting reach no network and take no credentials. Verne does not
follow a NetworkLink or fetch an overlay image, so anything behind a URL is
reported as outside the inventory rather than inspected, and it never opens a
raster or an attachment blob. Attachments are not extracted at all: the blobs
are a slice of work of their own. `verne load` is the one command that reaches a
network, and the only one that takes a credential.

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

# write the features and the semantics out
verne extract wells.gdb --out ./wells-extract --operator you@example.com

# create them in a running ptolemy
export VERNE_PTOLEMY_TOKEN=<a bearer token that may write>
verne load ./wells-extract --ptolemy http://localhost:3000
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

### What a plain conversion loses

Counted on one USGS National Hydrography geodatabase (41 tables, public domain),
against a GeoPackage written by GDAL's own vector translate, which is the code
behind `ogr2ogr`:

| | in the geodatabase | the GeoPackage keeps | verne carries |
|---|---|---|---|
| domains | 62 | 18 | 62 |
| relationship classes | 10 | 0 | 10 |
| subtypes | 5 sets | 0 | 5 sets |

Only 20 of those 62 domains are bound straight to a field. The other 42 are
reached only through subtypes, and GDAL has no subtype at all, so it cannot see
them being used and does not carry them: miss the one construct and two thirds of
the domains go with it, without the output looking any different. GDAL's reader
sees all 10 relationship classes and its GeoPackage writer keeps none of them.

Verne reads the domains and the relationship classes through GDAL, so that part
is GDAL's work and not verne's. What verne adds is the subtypes, out of the
catalogue XML; carrying all of it into somewhere that can hold it; and saying
what did not survive.

## Extraction

`verne extract` writes three things into the directory it is given:

```
features.gpkg — the features and attributes, converted by GDAL's own vector
                translate. -preserve_fid is always on, because a geodatabase
                keys its relationship classes on OBJECTID and OpenFileGDB gives
                that as the feature id rather than as a field.
sidecar.json  — the datasets, their column schemas, coded and range domains,
                subtypes and relationship classes to create in ptolemy, plus
                the log
```

The sidecar's structs mirror ptolemy's request bodies field for field, so
loading is a POST of each struct and not a translation that can drift from the
API. Two fields cannot mirror one, because ptolemy wants the id of a row that
does not exist until the load is running: a subtype's `domain_assignments` names
its domains, and a relationship class names its two datasets. Both are typed as
names, and the loader swaps them for the ids the load minted.

The GeoPackage holds some things ptolemy does not: for a domain bound straight
to a field, the domain itself with its description and its split and merge
policies and that binding. So a loss the report names against ptolemy is not
always a loss in the file verne writes on the way.

It is not the whole picture though, and the sidecar is not a copy of it. GDAL
carries a domain into a GeoPackage only where it can see a field using it, and a
domain reached through a subtype is invisible to it. On a real geodatabase that
is most of them — see below.

### Field aliases

An Esri field carries a name and a human label beside it, and the label is what
the source's users have always read the column by. It goes onto ptolemy's
dataset schema, one `alias` per field, and is stored there.

Nothing in the platform displays it. Carrying it is not the same as showing it,
and the report says which of the two happened rather than the more flattering
one: the label survives the migration and no screen has caught up with it yet.

A column's type goes the same way, mapped onto ptolemy's six field types. Where
none of them names the source type, as with a date, a binary column or a list,
the nearest is used and both the report and the log name the column and the type
it had.

### The extraction log

Every row of the report gets one entry saying what became of it: carried whole,
carried with the report's own words for what was left behind, or skipped with a
reason. Whether an entry reads as carried or approximated is decided by the
verdict and not by the caller, so a report and the log beside it cannot give
different accounts of the same thing. The log also records the losses the
conversion itself takes, which no verdict covers: one geodatabase domain becomes
one ptolemy domain per dataset that uses it, and the copies come apart from each
other; a subtype default arrives as the text the definition XML holds.

The operator who ran it is recorded, along with an RFC 3339 timestamp. Both are
required rather than optional: an extraction has to be able to say who made it.

## Loading

`verne load` creates datasets first, each with its schema and then the domains
that hang off it, then the subtypes, which name a domain by id, then the
relationship classes, which name two datasets and cannot be created before both
exist.

The token comes from `VERNE_PTOLEMY_TOKEN` and is never an argument, which would
put it in the process list. ptolemy grants the creator of a dataset an admin row
on it and gates every mutating route on a write ladder, so the loader has to
create the datasets itself: pointed at somebody else's, it would need a grant it
has no way to mint.

There is no rollback. A load that fails part way leaves what it already created
and the error names the route that refused it.

`crates/verne-load/tests/live.rs` loads a sidecar into a real ptolemy and is
gated on `VERNE_PTOLEMY_URL` and `VERNE_PTOLEMY_TOKEN`. **CI does not set
either, so CI does not cover the loader.** A mocked version would only prove the
loader agrees with itself, and the failure worth catching is a request shape
drifting from ptolemy's real API. Automating it would need ptolemy to publish a
container image or an OpenAPI spec; it does neither.

## Writing an adapter

Implement `verne_core::Source` in any crate that depends on `verne-core`, in this
workspace or outside it. The trait has two methods, `describe` and `inventory`,
both taking `&self`, and the adapter keeps its own error type. There is no
registry and no dynamic loading: whoever builds the binary picks the adapters.

## Crates

```
verne-core — the inventory model, verdicts, reports, and the sidecar model
verne-kml  — the KML and KMZ adapter
verne-gdb  — the Esri file geodatabase adapter and its extraction, behind the
             `gdal` feature
verne-load — the ptolemy loader: HTTP, no GDAL
verne-cli  — the command-line interface
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
