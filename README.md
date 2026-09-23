# Verne

[![CI](https://github.com/GeoLang/verne/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/verne/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

Verne reads a third-party geospatial source and reports, item by item, whether
GeoLang can hold it exactly, only approximately, or not at all. It reads KML and
KMZ files, Esri file geodatabases, and ArcGIS feature and map services.

`verne extract` writes a geodatabase or a service out as an extraction directory
that ptolemy can load, with a GeoPackage beside it when the source is a
geodatabase. `verne load` creates what the extraction describes in a running
ptolemy. No command writes to the source.

## Scope

Reading a file uses no network and no credentials. Verne does not follow a KML
NetworkLink or fetch an overlay image, so anything behind a URL is reported and
not inspected. It never opens a raster. An inventory does not open attachment
blobs. An extraction does, to write them out.

A service is read over its REST API with read requests only. Some of them are
POSTs, because a list of object ids or a filter clause does not fit in a URL.
`verne load` is the only command that writes, and it writes only to ptolemy.

The KML and service adapters are pure Rust and need no GDAL. The geodatabase
adapter needs GDAL 3.8 or later and is built only with the CLI's `gdb` feature.
It opens a `.gdb` directory through GDAL's OpenFileGDB driver by name, so Esri's
FileGDB SDK is never loaded. A mobile geodatabase (`.geodatabase`) is not read,
see [TODO.md](TODO.md).

## Usage

```bash
# markdown report on stdout
verne inspect sites.kml

# also write the report as JSON
verne inspect sites.kmz --json sites.json

# geodatabases need the gdb feature at build time
cargo install --path crates/verne-cli --features gdb
verne inspect wells.gdb
verne extract wells.gdb --out ./wells-extract --operator you@example.com

# a FeatureServer or MapServer, whole or scoped to one layer id
verne inspect https://host/arcgis/rest/services/Wells/FeatureServer
verne extract https://host/arcgis/rest/services/Wells/FeatureServer \
    --out ./wells-extract --operator you@example.com

# a portal's feature services, one per line: URL, title, (owner)
verne services https://www.arcgis.com --owner your-org-account

# create the extraction in a running ptolemy
export VERNE_PTOLEMY_TOKEN=<bearer token with the editor or admin role>
verne load ./wells-extract --ptolemy http://localhost:3000
```

A binary built without `gdb` refuses a `.gdb` path. KML and KMZ can be inspected
but not extracted.

Credentials come from the environment and never from arguments, which other
users can read in the process list.

| Variable | Used by | Meaning |
|---|---|---|
| `VERNE_ARCGIS_TOKEN` | `inspect`, `extract`, `services` | ArcGIS token, sent as is. Takes precedence over the client credentials. |
| `VERNE_ARCGIS_CLIENT_ID`, `VERNE_ARCGIS_CLIENT_SECRET` | same | Verne mints a token by OAuth `client_credentials` and mints a new one a minute before it expires. Set both or neither. |
| `VERNE_ARCGIS_PORTAL` | same | Portal the token is minted at. Defaults to `https://www.arcgis.com`. |
| `VERNE_PTOLEMY_TOKEN` | `load` | ptolemy bearer token that may write. Required. |

With no ArcGIS variable set, the service is read anonymously. The client secret
is sent only as the form body of the token request, never in a URL, an error
message or a log line.

## Verdicts

Every row of the report carries one of four verdicts:

- **faithful**: carried without loss, naming the component it goes to.
- **approximated**: carried with something left behind, and the row says what.
  `Losses` has no empty constructor, so an approximated verdict always names a
  loss.
- **unsupported**: no place in GeoLang, with the reason.
- **not applicable**: the source cannot have the thing at all, for example
  versioning in a file geodatabase, which is an enterprise geodatabase feature.

An inventory that cannot be produced is an error, never an empty list, so a
source verne failed to read does not look like an empty one.

## Geodatabases

Inventoried: feature classes and tables with their fields, aliases and domain
bindings, the system and `__ATTACH` tables, feature datasets, coded and range
domains, subtypes, relationship classes, attachments, annotation and dimension
classes, layer metadata records, and every catalogue item GDAL reads no
definition for.

GDAL does not model subtypes, so verne reads them from the layer definition XML.
It also does not model annotation text and symbology, topologies and their
rules, geometric and utility networks, parcel fabrics, terrains, mosaic
datasets, relationship rules, attribute rules or contingent values. Verne names
those from the catalogue and carries nothing of them.

Working on the adapter needs GDAL's headers and its python bindings
(`python3-gdal`). `crates/verne-gdb/tests/fixture.py` builds the test
geodatabases at test time. GDAL cannot write Esri subtypes, an annotation class
or a topology, so the fixture writes those parts as definition XML straight into
the catalogue.

### What a plain conversion loses

Counted on one USGS National Hydrography geodatabase (41 tables, public domain),
against a GeoPackage written by GDAL's vector translate, the code behind
`ogr2ogr`:

| | in the geodatabase | the GeoPackage keeps | verne carries |
|---|---|---|---|
| domains | 62 | 18 | 62 |
| relationship classes | 10 | 0 | 10 |
| subtypes | 5 sets | 0 | 5 sets |

42 of the 62 domains are used only through subtypes. GDAL has no subtypes, so it
does not see those domains in use and does not write them. GDAL reads all 10
relationship classes and its GeoPackage writer keeps none. Verne reads the
domains and relationship classes through GDAL and the subtypes from the
catalogue XML, and writes all three into `sidecar.json`.

## Feature services

`verne inspect` and `verne extract` take a FeatureServer or MapServer URL and
read the service resource, each layer and table, `/query` for features, and the
attachment routes for blobs. A URL ending in a layer id, which is the form
`verne services` prints, limits verne to that layer. A relationship whose other
side is outside that layer is reported and not followed.

Inventoried: layers and tables with their fields, aliases and domain bindings,
coded and range domains, subtypes with their defaults and per-subtype domain
assignments, relationships as both ends describe them, attachments, each
layer's drawing info, time settings, metadata records, annotation and dimension
layers, and whether the service fronts versioned data.

A map service adds three kinds of row:

- A group layer. ptolemy has no container above a dataset, so its members
  become separate datasets and the grouping is only a report row.
- A raster layer, named as a terrano item and not fetched.
- Versioning per layer. A MapServer states it per layer rather than once for the
  service, so the report names the layers that front versioned data.

Differences from a geodatabase on disk, each named in the report:

- The service reprojects. Every query asks for EPSG:4326 in `outSR` and verne
  does no coordinate arithmetic. Esri does not document which datum
  transformation the service applies. Each page is fetched a second time by
  object id without `outSR`, and the untransformed geometry is kept on each
  insert with its EPSG code, or its WKT when only Esri's own authority names
  it. When verne cannot state the reference at all, the original is dropped
  and the log says so per layer.
- There is no GeoPackage.
- A relationship read from a layer has no forward or backward label and no
  rules, so ptolemy's labels are created empty.

Features are fetched a page at a time, `maxRecordCount` per page, ordered by the
object id field, until the service stops setting `exceededTransferLimit`. A layer
that caps its answers and cannot page is fetched once, and the log says the rest
was not fetched. A Date attribute arrives as epoch milliseconds and is written as
RFC 3339 text, because the schema declares the column a string.

Attachments are listed through `queryAttachments` where the layer supports it
and one feature at a time where it does not. Each blob is downloaded and
attached to its feature through the object ids the feature pass recorded. A blob
whose feature the extraction did not write is skipped and counted.

`--gdb-version <name>` reads one named version of an enterprise service. Every
query carries the name, and a wrong name fails the open. The version tree is not
carried: listing and diffing versions goes through the Version Management
resource, which needs an editing privilege and read-session locks. A service
that tracks changes but publishes no `changeTrackingInfo` is not asked for
changes, because the generation window would have to come from registering a
sync replica, which writes to the server. Both refusals are report rows with the
reason.

A failed request is an error naming the route, including an HTTP 200 whose body
is an ArcGIS error object.

### Deltas

`verne extract <url> --since <dir>` re-reads a service that was already
extracted and loaded. `<dir>` is the earlier full extraction. Only what changed
is written, as insert, update and delete operations for ptolemy's commit route.
`verne load` commits them onto the datasets the first load created and creates
nothing. `--since` is refused for a geodatabase.

A delta takes one of two paths, and the report says which ran and why:

- **Server changes.** A full extraction of a service that publishes
  `changeTrackingInfo.layerServerGens` writes those generations to
  `server-gens.json`. A later `--since` sends them to `extractChanges`, which
  answers with the object ids edited since, and only those rows are fetched.
  The delta records the generations its window ended at.
- **Local diff.** The whole service is fetched again and paired with the
  previous feature files by object id. This runs when the previous extraction
  recorded no generations, the service stopped tracking changes, a queryable
  layer has no generation or no object id field, or the service refuses
  `extractChanges`. One run uses one path for every layer.

On both paths a hash of geometry and properties decides whether a row changed,
and an update keeps the feature id the first extraction minted, so ptolemy keeps
one history per feature.

A delta on the server changes path can be the basis of the next `--since`. It
writes three things into its own directory for that: the generations,
`object-ids/` (per row, the ptolemy feature id and a hash of what was last
written), and `attachment-ids/` (per attachment global id, the object id, size
and content type last seen). A delta without `object-ids/` is refused as a
basis, and so is any delta for a local diff, because both would read every
untouched row as new or deleted.

Attachment edits are carried on both paths:

- The server path takes the attachment edits the change file names. Each is
  paired with the loaded attachment by its `globalId`, falling back to object id
  and name where either side has no global id. An add of an attachment ptolemy
  already holds at the same size and content type is dropped without fetching
  the bytes, since consecutive windows report edits on their shared boundary
  twice. An add whose size or content type differs becomes a replacement. The
  parent of an added attachment is fetched with `where <globalIdField> IN
  (...)` when the parent itself did not change. A layer with no global id field
  has all of its attachment edits skipped and counted.
- The local diff lists attachments the way a full extraction does and pairs them
  the same way. A pair whose size or content type differs is a replacement and
  its bytes are fetched. A pair that agrees costs no request. A listing with no
  pair is an add, and a record with no pair is a delete. That includes the
  attachments of a deleted feature, because ptolemy's feature delete leaves its
  attachments in place. Two attachments of one name on one feature are counted
  and skipped.

Not carried by a delta, and named in the log: relationship classes are not
diffed, a layer with no object id field cannot be paired, and a layer that
disappeared from the service keeps its features in ptolemy.

## Extraction

`verne extract` writes into the `--out` directory:

```
features.gpkg   geodatabase only: features and attributes in the source's own
                reference, written by GDAL's vector translate with -preserve_fid,
                because relationship classes key on OBJECTID and OpenFileGDB
                gives it as the feature id, not a field
features/       one file per dataset, one JSON line per feature, each line an
                insert operation for ptolemy's commit route, in EPSG:4326 with
                the original geometry beside it
attachments/    the attachment blobs, one file each
sidecar.json    datasets, column schemas, coded and range domains, subtypes,
                relationship classes, attachments, symbology, and the log
```

An extraction of a service that publishes a generation window also writes
`server-gens.json`. A delta writes `attachment-ids/`, and a server changes delta
also writes `object-ids/`. Only a later `--since` reads these.

The structs in `sidecar.json` match ptolemy's request bodies field for field, so
the loader posts them without translating. Four fields differ:

- A subtype's `domain_assignments` names its domains, and a relationship class
  names its two datasets. Those rows have no ids until the load creates them,
  so the sidecar holds names and the loader substitutes the ids.
- An attachment names a file in `attachments/`, and the loader base64-encodes it
  into the body.
- A dataset's drawing info is the source's own document. The loader wraps it in
  a tag naming its format and posts it as ptolemy's free-form symbol.

### Why the features are written twice

The features are in the GeoPackage and again in `features/`. `verne-load` builds
without GDAL, so it cannot open the GeoPackage. Reading it with a SQLite crate
would mean decoding GeoPackage geometry headers and re-deriving JSON types from
SQLite's dynamic ones, work GDAL has already done. One line per feature also
lets the load stream the file.

The extraction mints the feature ids, and ptolemy's insert accepts them as
`feature_id`. The load reads nothing back, so an attachment can only name its
feature by an id already written down.

### Attachments

An Esri `__ATTACH` table is carried when a media relationship points at it. Each
blob is attached to its feature. ptolemy's attachment holds the bytes, name,
content type and size. Every other column of the source row goes into its
metadata JSON, which ptolemy stores and nothing in the platform reads.

Two cases are skipped, counted in the log with the reason: a blob table no media
relationship points at, since nothing says which class it belongs to, and an
`__ATTACH` row whose key matches no feature the extraction wrote.

A service attachment's `globalId` is recorded beside the blob in the sidecar.
ptolemy mints its own attachment id and the extraction never sees it, so the
global id is what a later delta pairs an edit on. A geodatabase extraction has
none, and an edit to an attachment without one is reported as unpairable.

ptolemy reads a feature attachment upload with axum's 2 MB default body limit,
so a blob much over 1.5 MB is refused with 413 and the load stops, naming the
route. For the same reason an insert over 1 MB is not written to the feature
file. A geodatabase row that size stays in the GeoPackage, and the log names the
rows and their sizes.

For a domain bound directly to a field, the GeoPackage also keeps the domain's
description and its split and merge policies, which ptolemy has no field for.
A domain used only through a subtype is not in the GeoPackage at all, see the
table above.

### Symbology

A feature or map service publishes each layer's `drawingInfo`: renderer,
symbols, class breaks or unique values, label classes and transparency. The
whole object goes into the sidecar unchanged, and the load posts it as one
symbology rule wrapped in `{"format": "esri-drawing-info", "drawingInfo": ...}`.
`min_scale`, `max_scale` and `filter_expression` are left unset, which ptolemy
reads as every scale and every feature.

Nothing in the platform renders this format yet. ptolemy stores the document and
serves it back, and the report row says so. A geodatabase carries no symbology,
since its drawing is in layer files verne does not read. A delta does not resend
it.

### Field aliases and types

Each Esri field alias goes onto ptolemy's dataset schema as the field's `alias`.
ptolemy stores it and no screen in the platform shows it yet, which the report
says.

Column types map onto ptolemy's six schema field types: string, integer, float,
boolean, array and object. A date, a binary column or a list gets the nearest
type, and the report and the log name the column and its source type. A binary
column is declared a string and no value is written into it. Bytes reach
ptolemy only through an attachment relationship.

### The extraction log

The log has one entry per report row: carried, carried with the report's losses,
or skipped with a reason. The verdict decides whether an entry reads as carried
or approximated, so the report and the log agree. The log also records losses
that only show up while reading rows:

- a geodatabase domain used by several datasets becomes one ptolemy domain per
  dataset, and the copies are independent
- a subtype default arrives as the text the definition XML holds
- a row with no shape in a class that has geometry is written with an empty
  geometry
- a row whose insert is over 1 MB is not written
- an `__ATTACH` row that matches no feature is not attached

`--operator` and an RFC 3339 timestamp are recorded in every log. `--operator`
is required.

## Loading

`verne load` creates, for each dataset: the dataset, its schema, one symbology
rule from its drawing info, its domains, a branch called `main`, and its
features. Then it creates the subtypes, which name domains by id, then the
relationship classes, which need both datasets, and last the attachments. ptolemy
creates no branch with a dataset, so the loader makes `main`.

Features are committed in batches of up to 500, and a batch is sent early when
the next feature would take it past 1 MB, to stay inside axum's 2 MB default. The
schema is set before the features, so ptolemy validates them against it.

Z and M values are kept: the WKB uses the ISO encoding and PostGIS stores both.
The dataset's `geometry_type` names only 2D shapes, and the report says so.

On a delta, `verne load` finds each dataset by name and its `main` branch,
commits the operations and applies the attachment edits. An add is an upload. A
replacement deletes the loaded copy and then uploads the new bytes, since
ptolemy has no route that changes an attachment. The loader finds the loaded
copy by name on the feature:

- two attachments of that name on the feature: the edit is refused and printed
- none, on a replacement: the bytes are uploaded as a first copy and printed
- none, on a delete: nothing is deleted and it is printed

There is no rollback. A load that fails part way leaves what it created, and the
error names the route that refused it.

The loader creates the datasets itself, because ptolemy gives the creator an
admin grant on a dataset and every write route checks the caller's role. A
token for someone else's datasets would need a grant the loader cannot create.

### Coordinates

ptolemy's commit reads every WKB as EPSG:4326, so every dataset declares `srid`
4326 and the feature files are transformed into it by GDAL (geodatabase) or by
the service (feature service). The GeoPackage is not transformed and keeps the
source's reference, so the two outputs hold different coordinates for the same
features. The log says so for each class.

Each transformed feature also carries `native_geometry_wkb_hex`, the untouched
geometry, with `native_srid` when one EPSG code names the reference, or
`native_crs_wkt` (WKT2) when none does, as with a compound reference such as
NAD83 with NAVD88 height. ptolemy stores it and returns it byte for byte from
`GET /api/v1/branches/{id}/features/{feature}/native`. A reference GDAL cannot
state at all keeps its original in the GeoPackage only, and the log says so.

PROJ picks the coordinate operation for each pair of references by area, so two
classes covering different places can get different operations. Neither GDAL
nor verne reports the accuracy of the chosen operation, so the report names the
loss without a number. `crates/verne-gdb/tests/fixture.py` builds a NAD83 / UTM
zone 19N class with one point at easting 500000, and the test checks the loader
would send a longitude near 69 degrees west.

A class that cannot be reprojected, because it has no spatial reference or GDAL
knows no operation for the pair, has its features skipped for the load. Its
dataset, schema, domains and subtypes are still created, the features stay in
the GeoPackage, and the log gives the reason.

## Tests and the demo

`crates/verne-arcgis/tests/live.rs` inventories and extracts a real service when
`VERNE_ARCGIS_URL` is set, and skips otherwise. CI sets none.

`crates/verne-load/tests/live.rs` loads a sidecar into a real ptolemy when
`VERNE_PTOLEMY_URL` and `VERNE_PTOLEMY_TOKEN` are set. With `VERNE_REQUIRE_LIVE`
set, a missing ptolemy fails the tests instead of skipping them. The CI
`live-load` job starts `postgis/postgis:16-3.4` and
`ghcr.io/geolang/ptolemy:master`, waits on `/api/v1/readyz`, mints an HS256
token with the `editor` role, and runs these tests with `VERNE_REQUIRE_LIVE`
set.

`demo/migration-loop.sh` runs a full extract, load, delta and delta load against
a live service and a scratch ptolemy, then checks the counts through ptolemy's
ArcGIS FeatureServer routes. It reads `PTOLEMY_URL`, `VERNE_PTOLEMY_TOKEN`,
`SERVICE_URL` (defaults to Esri's public Wildfire sample) and `OUT`. With
`--force-ops` it edits a copy of the full extraction first, so the delta carries
an insert, an update and a delete even when the service has not changed.

## Writing an adapter

Implement `verne_core::Source` in any crate that depends on `verne-core`. The
trait has two methods, `describe` and `inventory`, both taking `&self`, and an
associated error type. There is no registry or dynamic loading: the binary's
build picks the adapters.

## Crates

```
verne-core     inventory model, verdicts, reports, and the sidecar model
verne-kml      KML and KMZ adapter
verne-gdb      Esri file geodatabase adapter and extraction, behind its gdal feature
verne-arcgis   ArcGIS feature and map service adapter and extraction, no GDAL
verne-load     ptolemy loader, no GDAL
verne-cli      the verne binary
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
