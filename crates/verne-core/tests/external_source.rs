//! An adapter defined here rather than in an adapter crate, so the test fails if
//! `Source` stops being implementable from outside.

use std::fmt;

use verne_core::{
    Item, ItemKind, Losses, Outcome, Report, Source, SourceDescription, Target, Verdict,
};

#[derive(Debug, PartialEq, Eq)]
enum AtlasError {
    Unreadable,
}

impl fmt::Display for AtlasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AtlasError::Unreadable => f.write_str("the atlas could not be read"),
        }
    }
}

impl std::error::Error for AtlasError {}

struct Atlas {
    fail: bool,
}

impl Atlas {
    fn ok() -> Self {
        Atlas { fail: false }
    }

    fn broken() -> Self {
        Atlas { fail: true }
    }
}

impl Source for Atlas {
    type Error = AtlasError;

    fn describe(&self) -> SourceDescription {
        SourceDescription::new("Atlas volume", "/data/atlas.vol").with_detail("3 sheets")
    }

    fn inventory(&self) -> Result<Vec<Item>, Self::Error> {
        if self.fail {
            return Err(AtlasError::Unreadable);
        }
        Ok(vec![
            Item::new(
                "sheet 1",
                ItemKind::FeatureCollection,
                "12 points",
                Verdict::faithful(Target::Ptolemy),
            ),
            Item::new(
                "sheet 2",
                ItemKind::Styling,
                "one line symbol",
                Verdict::approximated(
                    Target::Jung,
                    Losses::one("dash patterns are dropped").and("line caps are dropped"),
                ),
            ),
            Item::new(
                "sheet 3",
                ItemKind::ViewDependentDisplay,
                "a saved camera",
                Verdict::unsupported("GeoLang stores no camera"),
            ),
            Item::new(
                "sheet 4 | annex",
                ItemKind::Metadata,
                "title \"a | b\"\nsecond line",
                Verdict::faithful(Target::Geogit),
            ),
            Item::new(
                "sheet 5",
                ItemKind::Temporal,
                "edit history",
                Verdict::not_applicable("an atlas volume is printed once and never revised"),
            ),
        ])
    }
}

fn table_rows(markdown: &str) -> Vec<&str> {
    markdown
        .lines()
        .filter(|line| line.starts_with('|'))
        .skip(2)
        .collect()
}

#[test]
fn summary_counts_the_items() {
    let report = Report::build(&Atlas::ok()).expect("the atlas reads");
    let summary = report.summary;
    assert_eq!(summary.total, 5);
    assert_eq!(summary.faithful, 2);
    assert_eq!(summary.approximated, 1);
    assert_eq!(summary.unsupported, 1);
    assert_eq!(summary.not_applicable, 1);
    assert_eq!(
        summary.total,
        summary.faithful + summary.approximated + summary.unsupported + summary.not_applicable
    );

    let mut counted = (0, 0, 0, 0);
    for item in &report.items {
        match item.verdict.outcome() {
            Outcome::Faithful => counted.0 += 1,
            Outcome::Approximated => counted.1 += 1,
            Outcome::Unsupported => counted.2 += 1,
            Outcome::NotApplicable => counted.3 += 1,
        }
    }
    assert_eq!(
        counted,
        (
            summary.faithful,
            summary.approximated,
            summary.unsupported,
            summary.not_applicable
        )
    );
}

#[test]
fn markdown_has_the_summary_and_one_row_per_item() {
    let report = Report::build(&Atlas::ok()).expect("the atlas reads");
    let markdown = report.to_markdown();
    assert!(
        markdown.contains("5 items: 2 faithful, 1 approximated, 1 unsupported, 1 not applicable.")
    );
    assert!(markdown.contains("Atlas volume. 3 sheets."));
    assert_eq!(table_rows(&markdown).len(), report.items.len());
}

#[test]
fn markdown_cells_cannot_break_the_table() {
    let report = Report::build(&Atlas::ok()).expect("the atlas reads");
    let markdown = report.to_markdown();
    let row = table_rows(&markdown)
        .into_iter()
        .find(|row| row.contains("annex"))
        .expect("the annex row is in the table");

    assert!(row.contains(r"sheet 4 \| annex"));
    assert!(row.contains(r#"title "a \| b" second line"#));
    assert!(!row.contains('\n'));
    // six columns, so seven pipes, and none of the cell text adds one
    assert_eq!(row.matches('|').count() - row.matches(r"\|").count(), 7);
}

#[test]
fn json_carries_the_summary_and_the_verdicts() {
    let report = Report::build(&Atlas::ok()).expect("the atlas reads");
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).expect("valid json");

    assert_eq!(json["summary"]["total"], 5);
    assert_eq!(json["summary"]["faithful"], 2);
    assert_eq!(json["summary"]["approximated"], 1);
    assert_eq!(json["summary"]["unsupported"], 1);
    assert_eq!(json["summary"]["not_applicable"], 1);
    assert_eq!(json["source"]["format"], "Atlas volume");

    let items = json["items"].as_array().expect("items is an array");
    assert_eq!(items.len(), 5);

    let not_applicable = items
        .iter()
        .find(|item| item["verdict"]["outcome"] == "not_applicable")
        .expect("one not applicable item");
    assert!(not_applicable["verdict"].get("target").is_none());
    assert!(
        not_applicable["verdict"]["reason"]
            .as_str()
            .expect("a reason")
            .contains("printed once")
    );

    let unsupported = items
        .iter()
        .find(|item| item["verdict"]["outcome"] == "unsupported")
        .expect("one unsupported item");
    assert!(unsupported["verdict"].get("target").is_none());
    assert_eq!(unsupported["verdict"]["reason"], "GeoLang stores no camera");

    let faithful = items
        .iter()
        .find(|item| item["verdict"]["outcome"] == "faithful")
        .expect("one faithful item");
    assert_eq!(faithful["verdict"]["target"]["component"], "ptolemy");
    assert_eq!(
        faithful["verdict"]["target"]["holds"],
        "features and attributes"
    );

    let approximated = items
        .iter()
        .find(|item| item["verdict"]["outcome"] == "approximated")
        .expect("one approximated item");
    assert_eq!(approximated["verdict"]["target"]["component"], "jung");
    assert_eq!(
        approximated["verdict"]["target"]["holds"],
        "symbology and cartographic styling"
    );
    assert_eq!(
        approximated["verdict"]["losses"]
            .as_array()
            .expect("losses is an array")
            .len(),
        2
    );
}

#[test]
fn build_propagates_the_adapter_error() {
    let error = Report::build(&Atlas::broken()).expect_err("the broken atlas fails");
    assert_eq!(error, AtlasError::Unreadable);
}
