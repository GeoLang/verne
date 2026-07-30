"""Build the geodatabase the tests read.

Run by the test harness, never at build time. Needs the GDAL python bindings
(python3-gdal); no network. Everything here is written with the same open
driver verne reads with, so a fixture cannot claim more than the driver does.

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

    ds.CreateLayer("pads", srs, ogr.wkbPolygon)

    inspections = ds.CreateLayer("inspections", None, ogr.wkbNone)
    inspections.CreateField(ogr.FieldDefn("well_id", ogr.OFTInteger))
    inspections.CreateField(ogr.FieldDefn("note", ogr.OFTString))

    coded = ogr.CreateCodedFieldDomain(
        "status_codes",
        "well status",
        ogr.OFTString,
        ogr.OFSTNone,
        {"A": "Active", "P": "Plugged"},
    )
    ds.AddFieldDomain(coded)
    drilled = ogr.CreateRangeFieldDomain(
        "depth_range", "drilled depth", ogr.OFTInteger, ogr.OFSTNone, 0, True, 5000, True
    )
    ds.AddFieldDomain(drilled)

    wells = ds.GetLayerByName("wells")
    for field_name, domain_name in (("status", "status_codes"), ("depth", "depth_range")):
        index = wells.GetLayerDefn().GetFieldIndex(field_name)
        current = wells.GetLayerDefn().GetFieldDefn(index)
        altered = ogr.FieldDefn(current.GetName(), current.GetType())
        altered.SetDomainName(domain_name)
        wells.AlterFieldDefn(index, altered, ogr.ALTER_DOMAIN_FLAG)

    inspected = gdal.Relationship(
        "wells_inspections", "wells", "inspections", gdal.GRC_ONE_TO_MANY
    )
    inspected.SetLeftTableFields(["OBJECTID"])
    inspected.SetRightTableFields(["well_id"])
    inspected.SetForwardPathLabel("has inspections")
    inspected.SetBackwardPathLabel("inspected well")
    inspected.SetType(gdal.GRT_COMPOSITE)
    ds.AddRelationship(inspected)

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

    media = gdal.Relationship(
        "wells_attach", "wells", "wells__ATTACH", gdal.GRC_ONE_TO_MANY
    )
    media.SetLeftTableFields(["OBJECTID"])
    media.SetRightTableFields(["REL_OBJECTID"])
    media.SetRelatedTableType("media")
    ds.AddRelationship(media)

    ds = None


if __name__ == "__main__":
    build(sys.argv[1])
