"""Build the geodatabase the tests read.

Run by the test harness, never at build time. Needs the GDAL python bindings
(python3-gdal); no network. Everything here is written with the same open
driver verne reads with, so a fixture cannot claim more than the driver does.

Relationships are added in a session of their own: on GDAL 3.8 OpenFileGDB
refuses AddRelationship while the geodatabase it has just created is still
open, and returns False rather than raising. Every call that answers with a
bool is checked here, so a fixture that did not come out whole fails at the
call that failed and not in a test three files away.

usage: python3 fixture.py <path-to-create.gdb>
"""

import os
import shutil
import sys

from osgeo import gdal, ogr, osr

gdal.UseExceptions()


def build(path):
    if os.path.exists(path):
        shutil.rmtree(path)

    driver = gdal.GetDriverByName("OpenFileGDB")
    ds = driver.Create(path, 0, 0, 0, gdal.GDT_Unknown)
    srs = osr.SpatialReference()
    srs.ImportFromEPSG(4326)

    wells = ds.CreateLayer("wells", srs, ogr.wkbPoint)
    name = ogr.FieldDefn("well_name", ogr.OFTString)
    name.SetAlternativeName("Well name")
    wells.CreateField(name)
    wells.CreateField(ogr.FieldDefn("depth", ogr.OFTInteger))
    wells.CreateField(ogr.FieldDefn("status", ogr.OFTString))
    wells.CreateField(ogr.FieldDefn("logo", ogr.OFTBinary))

    feature = ogr.Feature(wells.GetLayerDefn())
    feature.SetField("well_name", "Alpha")
    feature.SetField("depth", 120)
    feature.SetField("status", "A")
    feature.SetGeometry(ogr.CreateGeometryFromWkt("POINT (1 2)"))
    wells.CreateFeature(feature)

    # a feature dataset, which is the geodatabase's grouping of classes
    ds.CreateLayer("pads", srs, ogr.wkbPolygon, options=["FEATURE_DATASET=Water"])

    # an annotation class is an ordinary polygon layer until its definition
    # says otherwise, which is patched in below
    ds.CreateLayer("well_labels", srs, ogr.wkbPolygon)

    # a projected class, which is what most Esri data is. Its coordinates are
    # metres, and ptolemy reads whatever it is committed as degrees, so this is
    # the layer that proves an extraction transforms rather than passes through.
    # Easting 500000 is the central meridian of UTM zone 19, which is -69.
    utm = osr.SpatialReference()
    utm.ImportFromEPSG(26919)
    plots = ds.CreateLayer("plots", utm, ogr.wkbPoint)
    plots.CreateField(ogr.FieldDefn("plot_id", ogr.OFTInteger))
    plot = ogr.Feature(plots.GetLayerDefn())
    plot.SetField("plot_id", 1)
    plot.SetGeometry(ogr.CreateGeometryFromWkt("POINT (500000 5150000)"))
    plots.CreateFeature(plot)

    # a compound reference, NAD83 horizontal with NAVD88 height, which comes
    # back from OpenFileGDB with no authority code at all. This is what real
    # hydrography carries, and the layer that proves the original travels with
    # its WKT definition when no single EPSG code names it.
    compound = osr.SpatialReference()
    compound.SetFromUserInput("EPSG:4269+5703")
    gauges = ds.CreateLayer("gauges", compound, ogr.wkbPoint25D)
    gauges.CreateField(ogr.FieldDefn("gauge_id", ogr.OFTInteger))
    gauge = ogr.Feature(gauges.GetLayerDefn())
    gauge.SetField("gauge_id", 1)
    gauge.SetGeometry(ogr.CreateGeometryFromWkt("POINT Z (-69.1 46.5 12.5)"))
    gauges.CreateFeature(gauge)

    # a class with geometry and no spatial reference at all, which cannot be
    # transformed and must not be sent
    stray = ds.CreateLayer("stray_points", None, ogr.wkbPoint)
    stray.CreateField(ogr.FieldDefn("note", ogr.OFTString))
    lost = ogr.Feature(stray.GetLayerDefn())
    lost.SetField("note", "no reference")
    lost.SetGeometry(ogr.CreateGeometryFromWkt("POINT (7 8)"))
    stray.CreateFeature(lost)

    inspections = ds.CreateLayer("inspections", None, ogr.wkbNone)
    inspections.CreateField(ogr.FieldDefn("well_id", ogr.OFTInteger))
    inspections.CreateField(ogr.FieldDefn("note", ogr.OFTString))
    # a second table bound to the same domain: one domain in the geodatabase
    # becomes one per dataset in ptolemy
    inspections.CreateField(ogr.FieldDefn("status", ogr.OFTString))

    coded = ogr.CreateCodedFieldDomain(
        "status_codes",
        "well status",
        ogr.OFTString,
        ogr.OFSTNone,
        {"A": "Active", "P": "Plugged"},
    )
    ok(ds.AddFieldDomain(coded), "add the status_codes domain")
    drilled = ogr.CreateRangeFieldDomain(
        "depth_range", "drilled depth", ogr.OFTInteger, ogr.OFSTNone, 0, True, 5000, True
    )
    ok(ds.AddFieldDomain(drilled), "add the depth_range domain")

    for layer_name, field_name, domain_name in (
        ("wells", "status", "status_codes"),
        ("wells", "depth", "depth_range"),
        ("inspections", "status", "status_codes"),
    ):
        layer = ds.GetLayerByName(layer_name)
        index = layer.GetLayerDefn().GetFieldIndex(field_name)
        current = layer.GetLayerDefn().GetFieldDefn(index)
        altered = ogr.FieldDefn(current.GetName(), current.GetType())
        altered.SetDomainName(domain_name)
        layer.AlterFieldDefn(index, altered, ogr.ALTER_DOMAIN_FLAG)

    attach = ds.CreateLayer("wells__ATTACH", None, ogr.wkbNone)
    attach.CreateField(ogr.FieldDefn("REL_OBJECTID", ogr.OFTInteger))
    attach.CreateField(ogr.FieldDefn("CONTENT_TYPE", ogr.OFTString))
    attach.CreateField(ogr.FieldDefn("ATT_NAME", ogr.OFTString))
    attach.CreateField(ogr.FieldDefn("DATA_SIZE", ogr.OFTInteger))
    attach.CreateField(ogr.FieldDefn("DATA", ogr.OFTBinary))
    photo = ogr.Feature(attach.GetLayerDefn())
    photo.SetField("REL_OBJECTID", 1)
    photo.SetField("CONTENT_TYPE", "image/png")
    photo.SetField("ATT_NAME", "photo.png")
    photo.SetField("DATA_SIZE", 4)
    photo.SetFieldBinaryFromHexString("DATA", "89504E47")
    attach.CreateFeature(photo)

    # a blob table with no relationship pointing at it, which happens when a
    # class is deleted and its attachments are left behind
    orphan = ds.CreateLayer("pads__ATTACH", None, ogr.wkbNone)
    orphan.CreateField(ogr.FieldDefn("REL_OBJECTID", ogr.OFTInteger))
    orphan.CreateField(ogr.FieldDefn("DATA", ogr.OFTBinary))

    ds = None

    add_relationships(path)
    patch_definitions(path)
    verify(path)


def ok(result, what):
    """GDAL answers these with a bool, and UseExceptions does not turn a False
    into an exception, so an unchecked call fails silently."""
    if not result:
        raise SystemExit(f"fixture: GDAL would not {what}: {gdal.GetLastErrorMsg()}")


def add_relationships(path):
    ds = gdal.OpenEx(path, gdal.OF_VECTOR | gdal.OF_UPDATE)

    inspected = gdal.Relationship(
        "wells_inspections", "wells", "inspections", gdal.GRC_ONE_TO_MANY
    )
    inspected.SetLeftTableFields(["OBJECTID"])
    inspected.SetRightTableFields(["well_id"])
    inspected.SetForwardPathLabel("has inspections")
    inspected.SetBackwardPathLabel("inspected well")
    inspected.SetType(gdal.GRT_COMPOSITE)
    ok(ds.AddRelationship(inspected), "add the wells_inspections relationship")

    media = gdal.Relationship(
        "wells_attach", "wells", "wells__ATTACH", gdal.GRC_ONE_TO_MANY
    )
    media.SetLeftTableFields(["OBJECTID"])
    media.SetRightTableFields(["REL_OBJECTID"])
    media.SetRelatedTableType("media")
    ok(ds.AddRelationship(media), "add the wells_attach relationship")

    ds = None


# GDAL cannot write Esri subtypes, an annotation class or a topology, and it
# does not read what it cannot write. What it does do is hand back the
# definition blob whole, so these are written into the catalogue by hand, to
# Esri's geodatabase XML schema, and read back through the driver like any
# other definition.
SUBTYPES = """  <SubtypeFieldName>status</SubtypeFieldName>
  <DefaultSubtypeCode>1</DefaultSubtypeCode>
  <Subtypes xsi:type="typens:ArrayOfSubtype">
    <Subtype xsi:type="typens:Subtype">
      <SubtypeName>Active well</SubtypeName>
      <SubtypeCode>1</SubtypeCode>
      <FieldInfos xsi:type="typens:ArrayOfSubtypeFieldInfo">
        <SubtypeFieldInfo xsi:type="typens:SubtypeFieldInfo">
          <FieldName>depth</FieldName>
          <DomainName>depth_range</DomainName>
          <DefaultValue xsi:type="xs:int">100</DefaultValue>
        </SubtypeFieldInfo>
      </FieldInfos>
    </Subtype>
    <Subtype xsi:type="typens:Subtype">
      <SubtypeName>Plugged well</SubtypeName>
      <SubtypeCode>2</SubtypeCode>
    </Subtype>
  </Subtypes>
"""

METADATA = (
    "<metadata><dataIdInfo><idAbs>The wells of the fixture</idAbs>"
    "</dataIdInfo></metadata>"
)

TOPOLOGY = (
    '<?xml version="1.0" encoding="UTF-8"?>'
    '<typens:DETopology xmlns:typens="http://www.esri.com/schemas/ArcGIS/10.3">'
    "<Name>Water_Topology</Name><DatasetType>esriDTTopology</DatasetType>"
    "</typens:DETopology>"
)


def patch_definitions(path):
    ds = gdal.OpenEx(
        path, gdal.OF_VECTOR | gdal.OF_UPDATE, open_options=["LIST_ALL_TABLES=YES"]
    )
    items = ds.GetLayerByName("GDB_Items")
    end = "</typens:DEFeatureClassInfo>"
    for item in items:
        name = item.GetField("Name")
        definition = item.GetField("Definition")
        if not definition:
            continue
        if name == "wells":
            item.SetField("Definition", definition.replace(end, SUBTYPES + end))
            item.SetField("Documentation", METADATA)
            items.SetFeature(item)
        elif name == "well_labels":
            item.SetField(
                "Definition",
                definition.replace(
                    "<FeatureType>esriFTSimple</FeatureType>",
                    "<FeatureType>esriFTAnnotation</FeatureType>",
                ),
            )
            items.SetFeature(item)

    topology = ogr.Feature(items.GetLayerDefn())
    topology.SetField("Name", "Water_Topology")
    # the catalogue keys an item by its type GUID; the value is not read back,
    # only the definition is, so any well-formed GUID does here
    topology.SetField("Type", "{B7E2E7A5-1C1D-4E3C-9D8B-3A5A6E7C8D90}")
    topology.SetField("Definition", TOPOLOGY)
    items.CreateFeature(topology)
    ds = None


def verify(path):
    """What the tests read back, checked here so a fixture that came out short
    says so itself."""
    ds = gdal.OpenEx(path, gdal.OF_VECTOR, open_options=["LIST_ALL_TABLES=YES"])
    expected = {
        "layers": [
            "wells",
            "pads",
            "well_labels",
            "plots",
            "stray_points",
            "inspections",
            "wells__ATTACH",
            "pads__ATTACH",
        ],
        "domains": ["status_codes", "depth_range"],
        "relationships": ["wells_inspections", "wells_attach"],
    }
    found = {
        "layers": [ds.GetLayer(i).GetName() for i in range(ds.GetLayerCount())],
        "domains": list(ds.GetFieldDomainNames() or []),
        "relationships": list(ds.GetRelationshipNames() or []),
    }
    for what, names in expected.items():
        missing = [name for name in names if name not in found[what]]
        if missing:
            raise SystemExit(
                f"fixture: {path} is missing {what} {missing}; "
                f"GDAL {gdal.__version__} gave {found[what]}"
            )
    ds = None


if __name__ == "__main__":
    build(sys.argv[1])
