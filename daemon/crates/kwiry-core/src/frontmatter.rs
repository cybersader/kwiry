use serde_json::Value;

use crate::model::Frontmatter;

const MAX_FRONTMATTER_BYTES: usize = 64 * 1024;

pub(crate) fn parse_frontmatter(source: &str) -> (Frontmatter, Vec<String>, &str, Option<String>) {
    let Some((yaml, body)) = split_frontmatter(source) else {
        return (Frontmatter::default(), Vec::new(), source, None);
    };

    if yaml.len() > MAX_FRONTMATTER_BYTES {
        return (
            Frontmatter::default(),
            Vec::new(),
            body,
            Some(format!(
                "frontmatter exceeds {} bytes and was ignored",
                MAX_FRONTMATTER_BYTES
            )),
        );
    }

    match serde_saphyr::from_str::<Value>(yaml) {
        Ok(value) => (select_fields(&value), select_aliases(&value), body, None),
        Err(error) => (
            Frontmatter::default(),
            Vec::new(),
            body,
            Some(format!("invalid YAML frontmatter: {error}")),
        ),
    }
}

fn split_frontmatter(source: &str) -> Option<(&str, &str)> {
    if !(source.starts_with("---\n") || source.starts_with("---\r\n")) {
        return None;
    }

    let opening_end = source.find('\n')? + 1;
    let mut cursor = opening_end;
    for line in source[opening_end..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let yaml = &source[opening_end..cursor];
            let body = &source[cursor + line.len()..];
            return Some((yaml, body));
        }
        cursor += line.len();
    }
    None
}

fn select_fields(value: &Value) -> Frontmatter {
    let Some(object) = value.as_object() else {
        return Frontmatter::default();
    };

    Frontmatter {
        title: object.get("title").and_then(string_value),
        description: object.get("description").and_then(string_value),
        tags: object.get("tags").map_or_else(Vec::new, string_list),
        status: object.get("status").and_then(string_value),
        date: object.get("date").and_then(string_value),
    }
}

fn select_aliases(value: &Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let Some(value) = object.get("aliases") else {
        return Vec::new();
    };

    let mut aliases = Vec::new();
    for alias in string_list(value) {
        let alias = alias.trim();
        if !alias.is_empty() && !aliases.iter().any(|existing| existing == alias) {
            aliases.push(alias.to_owned());
        }
    }
    aliases
}

fn string_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(values) => values.iter().filter_map(string_value).collect(),
        _ => string_value(value).into_iter().collect(),
    }
}

fn string_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_default_fields() {
        let source =
            "---\ntitle: Note\ntags: [one, two]\nstatus: active\nignored: value\n---\n# Body\n";
        let (frontmatter, aliases, body, warning) = parse_frontmatter(source);

        assert_eq!(frontmatter.title.as_deref(), Some("Note"));
        assert_eq!(frontmatter.tags, ["one", "two"]);
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert!(aliases.is_empty());
        assert_eq!(body, "# Body\n");
        assert!(warning.is_none());
    }

    #[test]
    fn malformed_frontmatter_warns_but_keeps_body() {
        let source = "---\ntags: [broken\n---\nBody\n";
        let (frontmatter, aliases, body, warning) = parse_frontmatter(source);

        assert_eq!(frontmatter, Frontmatter::default());
        assert!(aliases.is_empty());
        assert_eq!(body, "Body\n");
        assert!(warning.unwrap().contains("invalid YAML"));
    }

    #[test]
    fn selects_trimmed_unique_aliases_without_exposing_them_as_frontmatter() {
        let source =
            "---\ntitle: Note\naliases: [IIA 2 line, ' IIA 2 line ', Second Line, '']\n---\nBody\n";
        let (frontmatter, aliases, body, warning) = parse_frontmatter(source);

        assert_eq!(frontmatter.title.as_deref(), Some("Note"));
        assert_eq!(aliases, ["IIA 2 line", "Second Line"]);
        assert_eq!(body, "Body\n");
        assert!(warning.is_none());
    }

    #[test]
    fn accepts_scalar_alias() {
        let source = "---\naliases: IIA 2 line\n---\nBody\n";
        let (_, aliases, _, warning) = parse_frontmatter(source);

        assert_eq!(aliases, ["IIA 2 line"]);
        assert!(warning.is_none());
    }
}
