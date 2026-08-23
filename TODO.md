# TODO

- [ ] Read ArcGIS Pro's mobile geodatabase (`.geodatabase`, SQLite-based) as a
      source, alongside the classic file geodatabase. Open question: whether
      GDAL's driver covers enough of it or the SQLite format is read directly,
      the way the KML side stays pure Rust. Same contract as the other
      adapters: read-only, every item gets a verdict.
