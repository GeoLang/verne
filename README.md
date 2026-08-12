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
service is read over HTTP by nature, with reads and nothing else, some of them
POSTed because a list of ids or a filter clause does not fit in a URL, and its
token comes from `VERNE_ARCGIS_TOKEN`, never an argument; a public service needs
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

`verne inspect` and `verne extract` take a FeatureServer or MapServer URL and
read it through the documented REST API: the service resource, each layer and
table, `/query` for the features, and the attachment routes for the blobs. A
URL ending in a layer id, which is how a portal names its items and how
`verne services` prints them, scopes verne to that one layer: only it is
read, and a relationship whose other side is out of scope is reported rather
than followed. The scope is the operator's own services with their own
credentials: verne records who ran the extraction and touches nothing it is
not pointed at.

A map service adds three things a feature service does not have, and each
gets its row. A group layer is the map's tree: ptolemy has no container above
a dataset, so the grouping survives as a report row and its members become
flat datasets. A raster layer is named for terrano and not fetched. And a
MapServer states versioning per layer rather than once at the root, so the
report names exactly which layers front versioned data; verne reads only the
version the service answers with, and says so.

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
the drawing info each layer publishes, time awareness, metadata records, and
whether the service fronts versioned data (verne reads only the version the
service answers with).

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

An enterprise service fronting versioned data can be read one named version
at a time: `--gdb-version` puts the name on every query, so the counts, the
features and the attachments all describe that version's state, and a wrong
name fails the open rather than reading as an empty service. The tree itself
is not carried, and the report says so: enumerating versions and diffing them
is the Version Management resource, which demands an editing privilege and
read-session locks verne will not take. A service that tracks changes but
publishes no `changeTrackingInfo` is asked nothing either, because the
generation window `extractChanges` needs would then only come from registering
a sync replica, which writes state on the server. Each refusal is a report row
with the reason.

A failed request is an error naming the route, including the ones ArcGIS
answers with HTTP 200 and an error object in the body.

A service already extracted and loaded once can be re-read as a delta:
`--since <dir>` points at the earlier full extraction, and only what changed
lands on disk, as the insert, update and delete operations of ptolemy's
commit route. `verne load` commits them onto the datasets the first load
created and creates nothing.

Where the service can say what changed, it is asked. A full extraction of a
service that publishes `changeTrackingInfo.layerServerGens` records those
generations in `server-gens.json` beside the sidecar, and a later `--since`
sends them back to `extractChanges`: the job it starts answers with the object
ids edited since, and only those rows are fetched. The delta records the
generations the window ended at, so the next one carries on from there. There
is no flag for this: `--since` picks the path, and either way what lands is a
sidecar `verne load` reads.

The attachment edits a change file names are carried too. An add or a
replacement is bytes, fetched off the URL the record names, which is on the
service's own host and carries no signature, so the token rides on it as it does
everywhere else. Every edit is a pairing, including an add: the change file names
an attachment by the service's `globalId`, and the extraction that loaded it
wrote that id down beside the feature it went onto and the name it went up under,
which is the only handle ptolemy leaves on it. An add names its feature by global
id too, and a parent that did not itself change is not among the rows the delta
fetched, so it is asked for with the service's own `where <globalIdField> IN
(...)`.

An add is not taken on trust for being an add. A window that ends where the next
one begins reports the edits on that boundary twice, so an add of an attachment
ptolemy already holds at that size and content type is counted and dropped with no
bytes fetched, and one whose size or content type has moved since becomes a
replacement. Without that the loader would put a second attachment of one name on
the feature, which is the one state it cannot act on afterwards. The pairing falls
back to the object id and the name where a global id is missing on either side,
the same way the local diff's does, and what the service last said about each
attachment is written into `attachment-ids/` so the next delta of the chain can
make the same judgement.

On the load an add is an upload, and a replacement is the loaded copy deleted and
the new bytes uploaded in that order, because ptolemy has no route that changes an
attachment. Two attachments of one name on one feature is a
pairing the loader will not pick between, and it refuses that operation and says
so. So does an edit to an attachment nothing was ever loaded under, and a layer
with no global id column has all of its attachment edits skipped: each is a count
and a reason in the report rather than a guess or a failed run.

A delta on that path is a basis for the next one, so a migration window is a
chain of cheap deltas rather than one. Three things have to carry over, and all
of them are written into the delta's own directory: the generations, an object
id index in `object-ids/`, one line per row saying which feature id ptolemy
holds it under and a hash of what was last written for it, and an attachment
index in `attachment-ids/` doing the same by global id, with the object id, size
and content type each was last known by. The indexes are what the
feature files cannot be, because they hold only the rows that delta touched, and
without them a row edited in two windows running would come back as a second
copy of itself. Each delta writes the indexes it was given with its own
operations applied.

Otherwise the diff is verne's own: the full current state is fetched again and
paired with the previous feature files by object id. Either way a hash of
geometry and properties decides changed from unchanged, an update keeps the
feature id the first extraction minted so its history in ptolemy stays one
feature, and the report says which of the two ran and why. It is the local
diff whenever the previous extraction recorded no generations, the service
stopped tracking changes, a queryable layer has no generation or no object id
field, or the service refuses the request: one run is all one way, never half
of each.

The local diff carries the attachments too, listed the way a full extraction
lists them and paired with the ones the previous extraction wrote down: by
`globalId` where both sides have one, and otherwise by the object id the
attachment hangs off and the name it is under, so a service keeping no attachment
global ids is diffed rather than skipped. A pair whose size or content type moved
is a replacement and its bytes are fetched, a pair that agrees is counted and
costs no traffic, a listing that paired with nothing is an add, and a record that
paired with nothing is a delete. That last one includes an attachment on a
feature the delta deleted: ptolemy's delete writes a new version of the feature
and leaves the attachments hanging off it, so the delete has to be written or the
blob outlives the feature. Two attachments of one name on one feature is a
pairing neither verne nor the loader can pick between, so the group is counted
and named and nothing is written for it. The report says how many were added,
replaced, deleted and left as they were, and the delta writes the same
`attachment-ids/` index the other path does.

What a delta does not carry is named in the log: relationship classes are not
diffed; a layer without an object id field cannot be paired at all; and a layer
that vanished from the service keeps its features in ptolemy rather than having a
diff delete a whole dataset. A delta is not a basis for a local diff, and not a
basis at all without its object id index: both would read every row it left alone
as vanished or as new, and it is refused by name rather than mispaired. A missing
attachment index is not refused, because an attachment edit that pairs with
nothing is a count and a reason.

`demo/migration-loop.sh` runs the whole story against a live service and a
scratch ptolemy: full extract, load, delta, delta load, then verifies
ptolemy's own FeatureServer facade serves the migrated counts. `--force-ops`
mutates a copy of the full extraction first, so the delta demonstrably
carries an insert, an update and a delete even when the service is quiet.

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
                subtypes, relationship classes, attachments and symbology to
                create in ptolemy, plus the log
```

From a feature service the GeoPackage is absent and everything else is the
same, so `verne load` reads both extractions without knowing which it was
handed. A feature service that publishes a generation window leaves a fifth
file, `server-gens.json`, which is the cursor the next `--since` sends back to
`extractChanges`. A delta leaves an `attachment-ids/` beside it, one file per
dataset saying which feature and name ptolemy holds each attachment under and
what the service last said about its bytes, and one that rode that path leaves an
`object-ids/` as well, saying which feature id ptolemy holds each object id under.
Nothing but a later delta reads any of them.

The sidecar's structs mirror ptolemy's request bodies field for field, so
loading is a POST of each struct and not a translation that can drift from the
API. Four fields cannot mirror one. Two are ids of rows that do not exist until
the load is running: a subtype's `domain_assignments` names its domains, and a
relationship class names its two datasets, so both are typed as names and the
loader swaps them. The third is an attachment's bytes, which ptolemy takes as
base64 in the body: a blob in `sidecar.json` would make the file unreadable, so
the sidecar names a file beside it and the loader encodes it. The fourth is a
dataset's drawing info, which is not a ptolemy shape at all but the source's own
document, so the loader wraps it in the tag naming its format and posts that as
ptolemy's free-form symbol.

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

Where the source has an id of its own for an attachment, a feature service's
`globalId`, the sidecar records it beside the blob. ptolemy mints its own id and
never tells the extraction, so that recorded id is the only thing a later delta
can pair a change to the same attachment on; a sidecar with none, which is every
geodatabase extraction and every service that keeps no global ids, says so by
leaving the field out, and an edit to one of those attachments is reported as
unpairable rather than guessed at.

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

### Symbology

A feature or map service publishes a layer's `drawingInfo`: the renderer, its
symbols, its class breaks or unique values, the label classes and the
transparency. The whole object is written into the sidecar on the dataset,
verbatim, and the load posts it as one symbology rule on that dataset, wrapped in
`{"format": "esri-drawing-info", "drawingInfo": ...}` so whatever comes to read
it knows what it is looking at. `min_scale`, `max_scale` and `filter_expression`
are left off, which ptolemy reads as every scale and every feature, and that is
what a layer's own drawing info says.

Verbatim is the point. An Esri symbol has more in it than any model verne could
write down without deciding which parts matter, and nothing in the platform reads
the format yet, so there is nothing to decide for. ptolemy stores the document
and serves it back; what draws with it is whatever client knows Esri's symbols.
The report row says exactly that, so nobody reads a carried style as a rendered
one.

A geodatabase carries none of this: its drawing lives in the layer files beside
it, which verne does not read. A delta does not carry it either, because the
style the full load created stands and a second rule would be a second style on
the dataset.

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

`verne load` creates datasets first, each with its schema and the one symbology
rule its drawing info becomes, then the domains that hang off it, then a branch
and the features on it, then the subtypes, which name a domain by id, then the
relationship classes, which name two datasets and cannot be created before both
exist, and last the attachments, each of which hangs off a feature on a branch.

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
gated on `VERNE_PTOLEMY_URL` and `VERNE_PTOLEMY_TOKEN`. A mocked version would
only prove the loader agrees with itself, and the failure worth catching is a
request shape drifting from ptolemy's real API.

CI runs it. The `live-load` job starts `postgis/postgis:16-3.4` and
`ghcr.io/geolang/ptolemy:master`, waits on ptolemy's `/api/v1/readyz`, which
answers only once its migrations are applied, mints an HS256 token with the
`editor` role against the throwaway secret it passed the container, and runs
the three tests against it. It also sets `VERNE_REQUIRE_LIVE`, which turns the
tests' skip path into a failure: without that, a job that stood ptolemy up
wrongly would pass on three skipped tests and read as coverage. Nothing else in
CI sets it, so `cargo test` with no ptolemy still skips them.

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
