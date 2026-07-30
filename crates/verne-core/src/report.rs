use serde::Serialize;

use crate::Source;
use crate::model::{Item, Outcome, SourceDescription};

/// How the items broke down, so the summary in markdown and the summary in
/// JSON come from one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub total: usize,
    pub faithful: usize,
    pub approximated: usize,
    pub unsupported: usize,
    /// Things the source cannot have, so nothing was carried or lost.
    pub not_applicable: usize,
}

impl Counts {
    fn of(items: &[Item]) -> Self {
        let mut counts = Counts {
            total: items.len(),
            faithful: 0,
            approximated: 0,
            unsupported: 0,
            not_applicable: 0,
        };
        for item in items {
            match item.verdict.outcome() {
                Outcome::Faithful => counts.faithful += 1,
                Outcome::Approximated => counts.approximated += 1,
                Outcome::Unsupported => counts.unsupported += 1,
                Outcome::NotApplicable => counts.not_applicable += 1,
            }
        }
        counts
    }

    pub fn sentence(&self) -> String {
        let mut sentence = format!(
            "{} items: {} faithful, {} approximated, {} unsupported",
            self.total, self.faithful, self.approximated, self.unsupported
        );
        // only said when there is one, so a report on a source where the
        // question never arises does not carry a zero about it
        if self.not_applicable > 0 {
            sentence.push_str(&format!(", {} not applicable", self.not_applicable));
        }
        sentence.push('.');
        sentence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub source: SourceDescription,
    pub summary: Counts,
    pub items: Vec<Item>,
}

impl Report {
    pub fn new(source: SourceDescription, items: Vec<Item>) -> Self {
        Report {
            summary: Counts::of(&items),
            source,
            items,
        }
    }

    /// Describe a source and inventory it in one step.
    pub fn build<S: Source>(source: &S) -> Result<Self, S::Error> {
        let items = source.inventory()?;
        Ok(Report::new(source.describe(), items))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report holds only serialisable values")
    }

    pub fn to_markdown(&self) -> String {
        let mut lines = vec![
            format!("# Verne inventory: {}", self.source.location),
            String::new(),
            match &self.source.detail {
                Some(detail) => format!("{}. {}.", self.source.format, detail),
                None => format!("{}.", self.source.format),
            },
            String::new(),
            format!("**{}**", self.summary.sentence()),
            String::new(),
            "| Location | Kind | Detail | Verdict | Target | What is lost |".to_string(),
            "|---|---|---|---|---|---|".to_string(),
        ];
        for item in &self.items {
            let target = match item.verdict.target() {
                Some(target) => target.to_string(),
                None => "none".to_string(),
            };
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                cell(&item.location),
                item.kind,
                cell(&item.detail),
                item.verdict.outcome(),
                cell(&target),
                cell(&item.verdict.shortfall()),
            ));
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

fn cell(text: &str) -> String {
    text.replace('|', r"\|").replace('\n', " ")
}
