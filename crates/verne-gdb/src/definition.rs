//! The Esri layer definition XML, which GDAL hands back whole and does not
//! interpret. Subtypes and the annotation flag live only here.
//!
//! No GDAL in this module, so it builds and is tested without the `gdal`
//! feature. Element names follow Esri's geodatabase XML schema, which is what
//! the `GDB_Items.Definition` blob holds.

use quick_xml::Reader;
use quick_xml::events::Event;

/// One subtype: a code in the subtype field, and what that code implies.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Subtype {
    pub code: String,
    pub name: String,
    /// Per-field domain and default, as `SubtypeFieldInfo` gives them.
    pub fields: Vec<SubtypeField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubtypeField {
    pub name: String,
    pub domain: Option<String>,
    pub default_value: Option<String>,
}

impl SubtypeField {
    fn is_empty(&self) -> bool {
        self.name.is_empty() && self.domain.is_none() && self.default_value.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Definition {
    /// `\FeatureDataset\name`, or `\name` for a layer at the top.
    pub catalog_path: Option<String>,
    /// `esriFTSimple`, `esriFTAnnotation`, `esriFTDimension` and the rest.
    pub feature_type: Option<String>,
    pub subtype_field: Option<String>,
    pub default_subtype: Option<String>,
    pub subtypes: Vec<Subtype>,
}

impl Definition {
    /// The feature dataset a layer sits in, from its catalog path.
    pub fn feature_dataset(&self) -> Option<&str> {
        let path = self.catalog_path.as_ref()?;
        let mut parts = path.trim_start_matches('\\').split('\\');
        let first = parts.next()?;
        // a path with one part is the layer itself, sitting at the top
        parts.next().map(|_| first)
    }

    /// Annotation and dimension classes carry graphics an ordinary feature
    /// class does not.
    pub fn drawn_feature_type(&self) -> Option<&str> {
        match self.feature_type.as_deref() {
            Some(kind @ ("esriFTAnnotation" | "esriFTCoverageAnnotation" | "esriFTDimension")) => {
                Some(kind)
            }
            _ => None,
        }
    }
}

/// Parse what verne needs out of a layer definition. A definition verne cannot
/// read is not an error: GDAL returns an empty one for a system table.
pub fn parse(xml: &str) -> Definition {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut definition = Definition::default();
    let mut elements: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut subtype = Subtype::default();
    let mut field = SubtypeField::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                elements.push(local_name(e.name().as_ref()));
                text.clear();
            }
            Ok(Event::Text(e)) => text.push_str(&e.decode().unwrap_or_default()),
            Ok(Event::CData(e)) => text.push_str(&String::from_utf8_lossy(e.as_ref())),
            Ok(Event::End(_)) => {
                let Some(name) = elements.pop() else { continue };
                let parent = elements.last().map(String::as_str).unwrap_or_default();
                let value = text.trim().to_string();
                text.clear();
                match name.as_str() {
                    "CatalogPath" => definition.catalog_path = some(value),
                    "FeatureType" => definition.feature_type = some(value),
                    "SubtypeFieldName" => definition.subtype_field = some(value),
                    "DefaultSubtypeCode" => definition.default_subtype = some(value),
                    "SubtypeName" => subtype.name = value,
                    "SubtypeCode" => subtype.code = value,
                    "FieldName" if parent == "SubtypeFieldInfo" => field.name = value,
                    "DomainName" if parent == "SubtypeFieldInfo" => field.domain = some(value),
                    "DefaultValue" if parent == "SubtypeFieldInfo" => {
                        field.default_value = some(value)
                    }
                    "SubtypeFieldInfo" => {
                        if !field.is_empty() {
                            subtype.fields.push(std::mem::take(&mut field));
                        }
                    }
                    "Subtype" => definition.subtypes.push(std::mem::take(&mut subtype)),
                    _ => {}
                }
            }
            // a definition verne cannot parse is reported as one it did not get
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    definition
}

/// The element name without its namespace prefix: the blob uses `typens:` on
/// the root and bare names inside it.
fn local_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    match name.split_once(':') {
        Some((_, local)) => local.to_string(),
        None => name.into_owned(),
    }
}

fn some(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

/// The root element of an item's definition, which is how the geodatabase says
/// what kind of item it is: `DEFeatureClassInfo`, `DETopology` and so on.
pub fn root_element(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                return Some(local_name(e.name().as_ref()));
            }
            Ok(Event::Decl(_)) | Ok(Event::Comment(_)) | Ok(Event::Text(_)) => {}
            _ => return None,
        }
    }
}
