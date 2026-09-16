# TODO

- [ ] Read ArcGIS Pro's mobile geodatabase (`.geodatabase`) as a source. Parked
      on 2026-09-16: GDAL 3.11.5 opens the file and hands back no geometry, so
      there is nothing to give a verdict on short of decoding Esri's blobs
      ourselves.

      What happens. `ogrinfo` opens the file with the SQLite driver and lists
      the feature tables with their attributes and feature counts, but every
      layer reports `Geometry: None`, the `Shape` column comes through as
      `Binary`, and the layer SRS is `(unknown)`. The geometry is an Esri
      ST_Geometry blob registered in Esri's own `st_geometry_columns` table,
      not in `geometry_columns`, and the open prints `no such module: VSRS` and
      `no such module: VTSpindex` for the Esri SQLite extensions the file
      expects. `ogr2ogr -f GPKG` writes the attributes and drops the geometry.

      Drivers tried, against `LA_Trails.geodatabase` (ArcGIS Online item
      `cb1b20748a9f4d128dad8a87244e3e37`) and `ContingentValuesBirdNests.geodatabase`
      (item `e12b54ea799f4606a2712157cf9f6e41`), both downloaded from
      `https://www.arcgis.com/sharing/rest/content/items/<id>/data`:
      - SQLite, the driver GDAL picks by itself. Opens, attributes only.
      - OpenFileGDB, forced with `-if OpenFileGDB`: `ERROR 4: not recognized as
        being in a supported file format`.
      - FileGDB, Esri's own SDK driver, is not built into this GDAL and is
        ruled out anyway.

      GDAL 3.11.5 docs. `doc/source/drivers/vector/openfilegdb.rst`: "The
      OpenFileGDB driver provides read, write and update access to vector
      layers of File Geodatabases (.gdb directories) created by ArcGIS 10 and
      above", and "The dataset name must be the directory/folder name, and it
      must end with the .gdb extension."
      `doc/source/drivers/vector/sqlite.rst`: "The driver looks for a
      geometry_columns table laid out as defined loosely according to OGC
      Simple Features standards", and "If geometry_columns is not found, each
      table is treated as a layer. Layers with a WKT_GEOMETRY field will be
      treated as spatial tables, and the WKT_GEOMETRY column will be read as
      Well Known Text geometry." Neither page names `.geodatabase`,
      ST_Geometry or the mobile geodatabase.

      Two ways out, neither one takeable now: load Esri's closed ST_Geometry
      SQLite extension through `OGR_SQLITE_LOAD_EXTENSIONS`, which cannot be
      shipped with verne, or decode the ST_Geometry blob, which Esri does not
      document. Recheck when GDAL grows a driver that names the format.
