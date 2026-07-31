# Verne

[![CI](https://github.com/GeoLang/verne/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/verne/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

Read-only inventory of a third-party geospatial source. Verne opens a file or a
hosted service, lists what is in it, and says of each thing whether GeoLang
could hold it faithfully, would have to approximate it, or has no home for it
at all.

It can then act on that report: `verne extract` writes the source out as a
sidecar ptolemy can load, with a GeoPackage beside it when the source is a
geodatabase, and `verne load` creates what the sidecar describes in a running
ptolemy. The source is never written to, in either.

## Scope

v0.4 reads KML and KMZ, Esri file geodatabases, and hosted ArcGIS feature
services over their REST API, and extracts a geodatabase or a feature service
into what ptolemy loads: the datasets and their semantics, the features
themselves, and the attachments on the features they belong to. A geodatabase
extraction also writes a GeoPackage; a feature service extraction does not,
because writing one is GDAL's work and the REST side builds without GDAL.

Reading a file reaches no network and takes no credentials. Verne does not
follow a NetworkLink or fetch an overlay image, so anything behind a URL in a
file is reported as outside the inventory rather than inspected, and it never
opens a raster. An inventory does not open an attachment blob either; an
extraction does, because carrying one means writing the bytes out. A feature
service is read over HTTP by nature, with GETs and nothing else, and its token
comes from `VERNE_ARCGIS_TOKEN`, never an argument; a public service needs
none. `verne load` is still the only command that writes anywhere, and it
writes to ptolemy.

The KML and feature service sides are pure Rust and need no GDAL. The
geodatabase side is behind the `gdb` feature and needs GDAL 3.8 or later, read
through GDAL's own OpenFileGDB driver: Esri's FileGDB SDK is never loaded, and
the open call names the driver it allows. Without the feature the geodatabase
adapter is not built at all and the rest of verne is unaffected.

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

# a hosted feature service, by its FeatureServer root; a private one reads
# VERNE_ARCGIS_TOKEN, or mints its own token from VERNE_ARCGIS_CLIENT_ID and
# VERNE_ARCGIS_CLIENT_SECRET
verne inspect https://host/arcgis/rest/services/Wells/FeatureServer
verne extract https://host/arcgis/rest/services/Wells/FeatureServer \
    --out ./wells-extract --operator you@example.com

# list a portal's feature services, one URL per line
verne services https://www.arcgis.com --owner your-org-account

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

## Feature services

`verne inspect` and `verne extract` take a FeatureServer URL and read it
through the documented REST API: the service resource, each layer and table,
`/query` for the features, and the attachment routes for the blobs. A URL
ending in a layer id, which is how a portal names its items and how `verne
services` prints them, scopes verne to that one layer: only it is read, and a
relationship whose other side is out of scope is reported rather than
followed. The scope is the operator's own services with their own
credentials: verne records who ran the extraction and touches nothing it is
not pointed at.

Credentials come from the environment, never arguments. A ready token in
`VERNE_ARCGIS_TOKEN` wins; failing that, `VERNE_ARCGIS_CLIENT_ID` and
`VERNE_ARCGIS_CLIENT_SECRET` have verne mint its own token by OAuth
client_credentials against the portal in `VERNE_ARCGIS_PORTAL` (arcgis.com
when unset) and re-mint it a minute before it expires; failing both, the
service is read as the public. The secret goes to the token route as a form
body and nowhere else: not in a URL, an error, or a log line.

What is inventoried: layers and tables with their fields, aliases and domain
bindings, coded and range domains, subtypes with their defaults and per-subtype
domain assignments, relationships as both of their ends tell them, attachments,
renderers by name, time awareness, metadata records, and whether the service
fronts versioned data (verne reads only the version the service answers with).

Three things differ from a geodatabase on disk, and the report names each:

- **The service does the reprojecting.** Every query asks for EPSG:4326 in
  `outSR` and verne does no coordinate arithmetic, which is what keeps this
  side GDAL-free; Esri does not document which datum transformation answers
  that, so the report says the transform happened without naming a number.
  The coordinates as the service stores them still survive it: each page is
  fetched a second time by its object ids, untransformed and POSTed because a
  page of ids does not fit in a URL, and the original rides on each insert as
  its EPSG code, or as the reference's WKT definition when only Esri's own
  authority names it. A reference verne cannot state at all is the one case
  the original stays behind, and the log says so per layer.
- **No GeoPackage.** The feature files and the sidecar are the whole
  extraction.
- **A layer's relationship description carries no forward or backward label
  and no rules**, so ptolemy's labels are created empty and the report says it
  cannot see rules at all.

Features come down `/query` a page at a time, `maxRecordCount` per page and
ordered by the object id field, until the service stops saying
`exceededTransferLimit`, which Esri's docs say can outlive the last full page.
A layer that caps its answers and cannot page is fetched once, and the log
says the rest of the layer was left behind. A Date attribute arrives as epoch
milliseconds and is written as RFC 3339 text, since the schema declares the
column a string.

Attachments are listed through `queryAttachments` where the layer supports it
and one feature at a time where it does not, then each blob is downloaded and
lands on the feature it belongs to through the object ids the feature pass
recorded. A blob that belongs to no feature the extraction wrote is skipped
and counted, never guessed onto another one.

A failed request is an error naming the route, including the ones ArcGIS
answers with HTTP 200 and an error object in the body.

## Extraction

`verne extract` writes four things into the directory it is given:

```
features.gpkg — the features and attributes in the source's own spatial
                reference, converted by GDAL's own vector translate.
                -preserve_fid is always on, because a geodatabase keys its
                relationship classes on OBJECTID and OpenFileGDB gives that as
                the feature id rather than as a field.
features/     — one file per dataset, one line of JSON per feature, each line a
                whole insert operation of ptolemy's commit route, transformed
                to EPSG:4326, ptolemy's working reference, with the untouched
                original beside it named by EPSG code or by WKT
attachments/  — the blobs out of the __ATTACH tables, one file each
sidecar.json  — the datasets, their column schemas, coded and range domains,
                subtypes, relationship classes and attachments to create in
                ptolemy, plus the log
```

From a feature service the GeoPackage is absent and everything else is the
same, so `verne load` reads both extractions without knowing which it was
handed.

The sidecar's structs mirror ptolemy's request bodies field for field, so
loading is a POST of each struct and not a translation that can drift from the
API. Three fields cannot mirror one. Two are ids of rows that do not exist until
the load is running: a subtype's `domain_assignments` names its domains, and a
relationship class names its two datasets, so both are typed as names and the
loader swaps them. The third is an attachment's bytes, which ptolemy takes as
base64 in the body: a blob in `sidecar.json` would make the file unreadable, so
the sidecar names a file beside it and the loader encodes it.

### The features, twice

The features are in the GeoPackage and again in `features/`. That is deliberate
and it costs disk: on one USGS geodatabase the GeoPackage is 17 MB and the
feature files 33 MB. `verne-load` builds and ships without GDAL, which it has to
keep doing, so it cannot open the GeoPackage; reading one with a SQLite crate
instead would mean verne decoding the GeoPackage geometry header itself and
re-deriving each column's JSON type out of SQLite's dynamic one, which is work
GDAL has already done, done again by hand, in the one crate with no GDAL to
check it. One line per feature also means a load streams the file rather than
holding a table in memory.

The feature ids are minted by the extraction and not by ptolemy, whose insert
takes an optional `feature_id`. That is what lets an attachment name the feature
it belongs to: the load never reads anything back, so the only ids it can key on
are the ones already written down.

### Attachments

An Esri `__ATTACH` table is carried when a media relationship points at it, and
each blob lands on the feature it belongs to. ptolemy's attachment holds the
bytes, the name, the content type and the size; every other column of the source
row goes into its metadata JSON, which is stored and which nothing in the
platform reads.

Two things are refused rather than guessed at. A blob table no media
relationship points at is skipped: ptolemy would take the bytes on the dataset
instead, but nothing in the geodatabase says which class they belong to, and an
attachment on the wrong feature is a worse answer than one that did not arrive.
An `__ATTACH` row whose key matches no feature the extraction wrote is skipped
the same way. Both are counted in the log with the reason.

ptolemy reads an upload with axum's JSON extractor at its 2 MB default, so a
blob much over 1.5 MB comes back 413 and the load stops there naming the route.
Same limit, same reason, on the features: a single insert over 1 MB is not
written to the feature file at all, since no batching could ever commit it. It
stays in the GeoPackage and the log says which rows and how big. One real
feature hits this, the outermost hydrologic unit boundary of a USGS
geodatabase, a single polygon of 2.7 MB.

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
it had. A binary column is the one whose values do not follow: ptolemy's
properties are JSON, so the column is declared a string and no value is written
into it. Bytes reach ptolemy only where an attachment relationship points at
them.

### The extraction log

Every row of the report gets one entry saying what became of it: carried whole,
carried with the report's own words for what was left behind, or skipped with a
reason. Whether an entry reads as carried or approximated is decided by the
verdict and not by the caller, so a report and the log beside it cannot give
different accounts of the same thing. The log also records the losses the
conversion itself takes, which no verdict covers, because they are only known by
reading the rows: one geodatabase domain becomes one ptolemy domain per dataset
that uses it, and the copies come apart from each other; a subtype default
arrives as the text the definition XML holds; a row with no shape in a class
that has one is written with an empty geometry; a row whose insert is over the
limit is not written at all; an `__ATTACH` row that matches no feature is not
attached to anything.

The operator who ran it is recorded, along with an RFC 3339 timestamp. Both are
required rather than optional: an extraction has to be able to say who made it.

## Loading

`verne load` creates datasets first, each with its schema, then the domains that
hang off it, then a branch and the features on it, then the subtypes, which name
a domain by id, then the relationship classes, which name two datasets and
cannot be created before both exist, and last the attachments, each of which
hangs off a feature on a branch.

The branch is the loader's doing: ptolemy creates none with a dataset, and a
dataset with no branch cannot be committed to, so the features would have
nowhere to go. Every dataset gets one called `main`.

Features go in batches of up to 500, and a batch is flushed early when the next
feature would take it past 1 MB. ptolemy raises no body limit of its own, so
axum's 2 MB default is what a commit has to fit inside; 500 keeps the number of
commits down on a table of points, and the byte count is what stops a table of
polygons building a body that comes back 413. The schema is set before the
features so ptolemy validates them against it, which is why a column arriving as
a type the schema does not declare is left out at extraction rather than sent.

On one USGS geodatabase (41 tables, 8.3 MB) the extraction takes 0.4 seconds and
the load 9.4, for 29,602 features in 100 commits.

Z and M survive: the WKB is written in the ISO encoding, which says both in the
type code, and PostGIS keeps them, so the loss the report names there is the
narrow one it says it is, that `geometry_type` on the dataset names 2D shapes
only and nothing in ptolemy records that the dataset holds them.

### The two outputs hold different coordinates

ptolemy's commit hands every WKB to `ST_GeomFromWKB(..., 4326)`, on insert and
on update, so EPSG:4326 is not a default that could be talked out of: it is the
only thing the working geometry column ever holds. **The features that go to ptolemy are
transformed into 4326 by GDAL. The GeoPackage is not touched and keeps the
source's own reference.** A GeoPackage holds any CRS and it is the artefact a
reader keeps, so reprojecting it would be a loss taken for nothing. The two
files therefore hold different coordinates for the same features, on purpose,
and the log says so on every class it is true of.

Sending the source's coordinates unchanged, which is what verne did before, is
only nearly harmless when the source is geographic. The USGS file is NAD83 in
degrees, so passing it through was out by a datum shift. Most Esri data is
projected, state plane or UTM, and passing that through sends metres or feet to
be read as degrees, which is not an error of a few metres but a coordinate with
no meaning. `crates/verne-gdb/tests/fixture.py` builds a NAD83 / UTM zone 19N
class for exactly this, one point on the zone's central meridian at easting
500000, and the test asserts what the loader would send is near 69 degrees west
rather than anywhere near 500000.

Every dataset therefore declares `srid` 4326, because that is what its working
geometry is by the time ptolemy has it. The original is not lost with it: every
transformed feature also carries `native_geometry_wkb_hex`, the untouched
geometry, with its reference as `native_srid` when a single EPSG code names it
or as `native_crs_wkt`, the full WKT2 definition, when none does, which is what
a compound reference such as NAD83 with NAVD88 height comes as. ptolemy stores
the original beside the working copy and returns it byte for byte from
`GET /branches/{id}/features/{feature}/native`. Only a reference GDAL cannot
state at all leaves its original in the GeoPackage alone, and the log says so
per class.

The transformation itself is the cost, and it is GDAL's: verne does no
coordinate arithmetic. PROJ picks the coordinate operation for the pair of
references, and picks it by area, so two classes covering different places can
be transformed by different operations. NAD83 to WGS 84 involves no grid files;
PROJ has 53 candidate operations for it and they declare accuracies from 1.5 m
to 4 m, and on the fixture's point in northern Maine it uses `NAD83 to WGS 84
(6)`, declared at 1.5 m, which moves the point about 0.4 m. That is the honest
size of it for this source. It is not a general figure: a source on an older
datum with no grid installed is worse, and neither GDAL nor verne reports the
accuracy of the operation it chose, so the report says what is lost without
naming a number it cannot get.

A class verne cannot reproject is not sent at all. A layer with no spatial
reference has nothing to transform out of and its numbers would be read as
degrees, and a pair GDAL knows no operation for cannot be transformed either.
Both are skipped for the load with the reason, counted in the log, and both are
still in the GeoPackage: the dataset, its schema, its domains and its subtypes
are all still created, and only the features are refused.

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
verne-core   — the inventory model, verdicts, reports, and the sidecar model
verne-kml    — the KML and KMZ adapter
verne-gdb    — the Esri file geodatabase adapter and its extraction, behind
               the `gdal` feature
verne-arcgis — the ArcGIS Feature Service adapter and its extraction: REST,
               no GDAL
verne-load   — the ptolemy loader: HTTP, no GDAL
verne-cli    — the command-line interface
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
