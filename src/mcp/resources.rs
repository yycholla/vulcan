use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpResourceTemplate {
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeMcpResourceTemplate {
    pub uri_template: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub variable_count: usize,
}

impl McpResourceTemplate {
    pub fn to_safe_template(&self) -> SafeMcpResourceTemplate {
        SafeMcpResourceTemplate {
            uri_template: self.uri_template.clone(),
            name: self.name.as_ref().map(|value| sanitize_text(value)),
            description: self.description.as_ref().map(|value| sanitize_text(value)),
            mime_type: self.mime_type.as_ref().map(|value| sanitize_text(value)),
            variable_count: count_template_variables(&self.uri_template),
        }
    }
}

fn sanitize_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
        .collect::<String>()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn count_template_variables(value: &str) -> usize {
    let mut count = 0usize;
    let mut in_var = false;
    for ch in value.chars() {
        match (in_var, ch) {
            (false, '{') => in_var = true,
            (true, '}') => {
                count += 1;
                in_var = false;
            }
            _ => {}
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_template_safe_view_keeps_metadata_inert() {
        let template = McpResourceTemplate {
            uri_template: "file:///{workspace}/{path}".into(),
            name: Some(" Repo\nFiles ".into()),
            description: Some("Read-only\nresource list".into()),
            mime_type: Some("text/plain".into()),
        };
        let safe = template.to_safe_template();
        assert_eq!(safe.name.as_deref(), Some("Repo Files"));
        assert_eq!(safe.description.as_deref(), Some("Read-only resource list"));
        assert_eq!(safe.variable_count, 2);
    }
}
