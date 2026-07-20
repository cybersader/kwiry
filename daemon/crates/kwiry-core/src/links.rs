use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

static WIKILINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\[\]]+)\]\]").expect("valid wikilink regex"));

pub(crate) fn extract_wikilinks(source: &str) -> Vec<String> {
    WIKILINK
        .captures_iter(source)
        .filter_map(|capture| capture.get(1))
        .filter_map(|target| {
            let target = target.as_str().split('|').next().unwrap_or_default();
            let target = target.split('#').next().unwrap_or_default().trim();
            (!target.is_empty()).then(|| target.to_owned())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_unique_normalized_targets() {
        let links = extract_wikilinks(
            "See [[note|Alias]], [[folder/page#Heading]], [[note]], and [[#local]].",
        );
        assert_eq!(links, ["folder/page", "note"]);
    }
}
