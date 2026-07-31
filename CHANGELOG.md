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
- The untransformed originals ride on REST inserts too: each page is fetched a
  second time by its object ids in the layer's own reference, and the original
  lands beside the working copy as its EPSG code, or as the reference's WKT
  when only Esri's own authority names it.
- OAuth client_credentials: with `VERNE_ARCGIS_CLIENT_ID` and
  `VERNE_ARCGIS_CLIENT_SECRET` set, verne mints its own token against the
  portal named by `VERNE_ARCGIS_PORTAL` (arcgis.com by default) and re-mints
  it before it expires. A ready `VERNE_ARCGIS_TOKEN` still wins.
- `verne services <portal-url>`: list the feature services a portal holds
  through `sharing/rest/search`, one URL per line, with `--owner` to narrow
  to an account. The same credentials apply, so a private portal lists what
  the token may see.
- A FeatureServer URL ending in a layer id scopes inspect and extract to that
  one layer, which is the shape a portal's item URLs come in, so the listing
  pipes straight into `verne inspect`.
- `--gdb-version` reads a named geodatabase version of a versioned enterprise
  service: the name rides on every query, count, feature page, native pass
  and attachment listing, and a wrong name fails the open loudly. The
  versioning report row says what is deliberately not done: no version
  enumeration or differences, which need the Version Management resource's
  editing privilege and read locks.
- MapServer roots read through the same contract: group layers become
  hierarchy rows and their members flat datasets, raster layers are named for
  terrano and not fetched, per-layer `isDataVersioned` reaches the versioning
  row, and an object id declared only as a field still orders the pages and
  keys the native pass.
- `verne extract --since <dir>` diffs a feature service against an earlier
  full extraction and writes only the insert, update and delete operations of
  ptolemy's commit route, paired by object id with a hash deciding changed
  from unchanged; `verne load` commits the delta onto the datasets the first
  load created and creates nothing. Relationship classes are not diffed, nor
  are attachments on this path, and a layer without an object id field gets no
  delta, all said in the log. serde_json's `float_roundtrip` is on throughout:
  its best-effort parsing does not round-trip its own shortest output, which
  made server-computed floats hash as changed on every delta.
- A delta asks the service what changed where the service can say. A full
  extraction of a service that tracks changes and publishes
  `changeTrackingInfo.layerServerGens` records those generations in
  `server-gens.json` beside the sidecar; `--since` sends them to
  `extractChanges`, polls the job it starts, reads the change file its result
  URL redirects to, and fetches only the object ids it names through the same
  page machinery a full extraction uses, so date rewriting, the reference the
  geometry arrives in and the untransformed originals stay on one code path.
  The change file is read for its ids alone, never for its geometries. The
  delta records the generations the window ended at. No new flag: `--since`
  picks the path and the report says which ran.
  - The local diff still runs where the server cannot answer, all or nothing
    per run: no generations recorded, change tracking gone, a queryable layer
    with no generation or no object id field, or a refused request. The report
    row gives the reason, and its attachment row says they were not carried,
    because reading the features again says nothing about a blob.
  - A delta on the `extractChanges` path carries the attachment edits too. An
    add or a replacement is fetched off the URL the change file's record names,
    on the service's own host and through the same authed client; a replacement
    or a delete pairs by the service's `globalId` against what the previous
    extraction wrote down, which is why every extraction now records an
    attachment's `globalId` beside it in the sidecar (optional, and absent means
    unpairable, so an old sidecar loads unchanged). An add's parent is named by
    global id, and one that did not itself change is resolved with the service's
    own `where <globalIdField> IN (...)`. A delta's own attachment index lands in
    `attachment-ids/` for the next delta of the chain to pair against. An edit
    that pairs with nothing, and every attachment edit on a layer with no global
    id column, is counted and named in the report rather than guessed at or
    fatal. The report row that used to say attachments are not diffed now says
    what was carried, with counts.
  - `verne load` applies them: an add is an upload, and a replacement is the
    loaded copy deleted and the new bytes uploaded in that order, since ptolemy
    has no route that changes an attachment. The loaded copy is found by name on
    the feature, because ptolemy minted its id and no extraction ever saw one, so
    two attachments of one name on one feature refuses that operation with a
    reason instead of picking one.
  - The token does not follow the result URL's redirect: it points at a signed
    file on storage, and reqwest only drops `Authorization` across a host
    boundary while the token rides in `X-Esri-Authorization`, so the redirect
    is followed by hand and the signed URL fetched bare.
  - `returnIdsOnly` is not used. On a live service it answers with empty edits
    for windows the async job returns thousands of rows for.
  - A delta on this path is a basis for the next one, so a migration window is
    a chain of cheap deltas rather than one. Each writes an object id index
    into `object-ids/`, a line per row naming the feature id ptolemy holds it
    under and a hash of what was last written, which is the basis it was given
    with its own operations applied. Its feature files cannot serve: they hold
    only the rows it touched, so a row edited in two windows running would
    find no feature id and land as a second copy of itself. A delta with no
    object id index, and a delta whose next run would fall back to the local
    diff, are refused by name rather than mispaired. A missing attachment index
    is not refused: an attachment edit that pairs with nothing is a count and a
    reason.
  - The change hash is FNV-1a rather than the standard library's hasher, whose
    value is documented as not to be relied on across releases: a chained
    delta compares against a hash an earlier run wrote down.
