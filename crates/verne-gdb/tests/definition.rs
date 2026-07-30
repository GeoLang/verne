//! The layer definition parser, which needs no GDAL: GDAL cannot write Esri
//! subtypes or an annotation class, so these snippets are written by hand to
//! Esri's geodatabase XML schema. The surrounding shape is what GDAL itself
//! returned for a real fixture.

use verne_gdb::definition;

const WELLS: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<typens:DEFeatureClassInfo xmlns:typens="http://www.esri.com/schemas/ArcGIS/10.3" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <CatalogPath>\Water\wells</CatalogPath>
  <Name>wells</Name>
  <DatasetType>esriDTFeatureClass</DatasetType>
  <FeatureType>esriFTSimple</FeatureType>
  <ShapeType>esriGeometryPoint</ShapeType>
  <SubtypeFieldName>status</SubtypeFieldName>
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
      <FieldInfos xsi:type="typens:ArrayOfSubtypeFieldInfo">
        <SubtypeFieldInfo xsi:type="typens:SubtypeFieldInfo">
          <FieldName>depth</FieldName>
        </SubtypeFieldInfo>
      </FieldInfos>
    </Subtype>
  </Subtypes>
</typens:DEFeatureClassInfo>
"##;

#[test]
fn subtypes_come_out_with_their_codes_and_field_infos() {
    let definition = definition::parse(WELLS);
    assert_eq!(definition.subtype_field.as_deref(), Some("status"));
    assert_eq!(definition.default_subtype.as_deref(), Some("1"));
    assert_eq!(definition.subtypes.len(), 2);

    let active = &definition.subtypes[0];
    assert_eq!(active.code, "1");
    assert_eq!(active.name, "Active well");
    assert_eq!(active.fields.len(), 1);
    assert_eq!(active.fields[0].name, "depth");
    assert_eq!(active.fields[0].domain.as_deref(), Some("depth_range"));
    assert_eq!(active.fields[0].default_value.as_deref(), Some("100"));

    // a field info with neither a domain nor a default still names its field
    let plugged = &definition.subtypes[1];
    assert_eq!(plugged.code, "2");
    assert_eq!(plugged.fields[0].domain, None);
    assert_eq!(plugged.fields[0].default_value, None);
}

#[test]
fn the_catalog_path_gives_the_feature_dataset() {
    let definition = definition::parse(WELLS);
    assert_eq!(definition.feature_dataset(), Some("Water"));

    let top_level = definition::parse(
        r"<DEFeatureClassInfo><CatalogPath>\loose</CatalogPath></DEFeatureClassInfo>",
    );
    assert_eq!(top_level.feature_dataset(), None);
}

#[test]
fn an_annotation_class_is_told_apart_from_a_simple_one() {
    assert_eq!(definition::parse(WELLS).drawn_feature_type(), None);

    let annotation = definition::parse(
        r"<DEFeatureClassInfo><FeatureType>esriFTAnnotation</FeatureType></DEFeatureClassInfo>",
    );
    assert_eq!(annotation.drawn_feature_type(), Some("esriFTAnnotation"));

    let dimension = definition::parse(
        r"<DEFeatureClassInfo><FeatureType>esriFTDimension</FeatureType></DEFeatureClassInfo>",
    );
    assert_eq!(dimension.drawn_feature_type(), Some("esriFTDimension"));
}

#[test]
fn an_empty_or_broken_definition_reads_as_nothing_found() {
    assert_eq!(definition::parse(""), definition::Definition::default());
    assert_eq!(
        definition::parse("<DEFeatureClassInfo><Name>cut off"),
        definition::Definition {
            ..Default::default()
        }
    );
    assert_eq!(definition::root_element(""), None);
}

#[test]
fn the_root_element_names_the_kind_of_item() {
    assert_eq!(
        definition::root_element(WELLS).as_deref(),
        Some("DEFeatureClassInfo")
    );
    assert_eq!(
        definition::root_element(
            "<typens:DETopology xmlns:typens=\"x\"><Name>t</Name></typens:DETopology>"
        )
        .as_deref(),
        Some("DETopology")
    );
}
