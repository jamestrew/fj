use std::io::IsTerminal;

use anyhow::{anyhow, bail, Result};

use crate::api;
use crate::api::template::{RepoTemplate, TemplateKind};
use crate::client::Client;

use super::editor;

pub async fn resolve(
    client: &Client,
    owner: &str,
    repo: &str,
    kind: TemplateKind,
    requested: Option<Option<String>>,
) -> Result<Option<String>> {
    let Some(requested) = requested else {
        return Ok(None);
    };

    let templates = api::template::list(client, owner, repo, kind).await?;
    if templates.is_empty() {
        bail!("no {} templates found", kind.label());
    }

    let template = match requested {
        Some(name) => api::template::select(&templates, kind, &name)?,
        None => prompt_for_template(kind, &templates)?,
    };
    Ok(Some(template.content.clone()))
}

fn prompt_for_template(kind: TemplateKind, templates: &[RepoTemplate]) -> Result<&RepoTemplate> {
    if !std::io::stdin().is_terminal() {
        let names = templates
            .iter()
            .map(|t| t.display_name().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!(
            "--template needs a name when stdin is not interactive. Available {} templates: {}",
            kind.label(),
            names
        ));
    }

    println!("Available {} templates:", kind.label());
    for (index, template) in templates.iter().enumerate() {
        println!("  {}. {}", index + 1, template.display_name());
    }
    let answer = editor::prompt_line("Template")?;
    api::template::select(templates, kind, &answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(name: &str) -> RepoTemplate {
        RepoTemplate {
            name: name.into(),
            file_name: format!("{name}.md"),
            content: "body".into(),
        }
    }

    #[test]
    fn noninteractive_missing_name_lists_available_templates() {
        let templates = vec![template("Bug"), template("Feature")];
        let err = prompt_for_template(TemplateKind::Issue, &templates).unwrap_err();
        assert!(err
            .to_string()
            .contains("Available issue templates: Bug, Feature"));
    }
}
