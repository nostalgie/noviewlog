use crate::types::{FilterRule, FilterType, LogRecord};

#[derive(Default)]
pub struct FilterEngine {
    filters: Vec<FilterRule>,
}

impl FilterEngine {
    pub fn new(filters: Vec<FilterRule>) -> Self {
        Self { filters }
    }

    pub fn set_filters(&mut self, filters: Vec<FilterRule>) {
        self.filters = filters;
    }

    pub fn filters(&self) -> &[FilterRule] {
        &self.filters
    }

    pub fn filters_mut(&mut self) -> &mut Vec<FilterRule> {
        &mut self.filters
    }

    pub fn is_visible(&self, record: &LogRecord) -> bool {
        self.is_visible_text(&record.text)
    }

    /// Same include/exclude rules as Records, for live-screen overlay lines.
    pub fn is_visible_text(&self, text: &str) -> bool {
        let filters = &self.filters;
        if !filters.iter().any(|f| f.enabled) {
            return true;
        }

        let mut has_include = false;
        for filter in filters.iter().filter(|f| f.enabled) {
            if filter.filter_type == FilterType::Exclude {
                if filter.regex.as_ref().is_some_and(|re| re.is_match(text)) {
                    return false;
                }
            } else {
                has_include = true;
            }
        }

        if !has_include {
            return true;
        }

        filters.iter().any(|f| {
            f.enabled
                && f.filter_type == FilterType::Include
                && f.regex.as_ref().is_some_and(|re| re.is_match(text))
        })
    }

    pub fn filter_records<'a>(&self, records: &'a [LogRecord]) -> Vec<&'a LogRecord> {
        records.iter().filter(|r| self.is_visible(r)).collect()
    }
}
