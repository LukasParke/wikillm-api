//! Project scoping: named bundles of path prefixes + connector ids.

use crate::domain::{ProjectInput, ProjectRecord};
use crate::store::Store;
use crate::error::{Error, Result};
use std::sync::Arc;

pub struct ProjectService {
    store: Arc<dyn Store>,
}

impl ProjectService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    pub async fn list(&self) -> Result<Vec<ProjectRecord>> {
        self.store.list_projects().await
    }

    pub async fn get(&self, name: &str) -> Result<Option<ProjectRecord>> {
        self.store.get_project(name).await
    }

    pub async fn put(&self, input: &ProjectInput) -> Result<()> {
        self.store.put_project(input).await
    }

    pub async fn delete(&self, name: &str) -> Result<bool> {
        self.store.delete_project(name).await
    }

    /// Resolve the path-prefix filter for a request. `["*"]` disables scoping;
    /// `["__none__"]` matches nothing.
    pub async fn resolve_scope_prefixes(
        &self,
        auth_projects: &[String],
        requested_project: Option<&str>,
    ) -> Result<Vec<String>> {
        if let Some(project) = requested_project.filter(|p| p.ne(&"*".to_string())) {
            if !auth_projects.contains(&"*".to_string())
                && !auth_projects.iter().any(|p| p == project)
            {
                return Err(Error::Forbidden(format!("Project not allowed: {project}")));
            }
            return Ok(match self.store.get_project(project).await? {
                Some(p) => project_prefixes(&p),
                None => vec!["__none__".into()],
            });
        }
        if auth_projects.contains(&"*".to_string()) {
            return Ok(vec!["*".into()]);
        }
        if auth_projects.is_empty() {
            return Ok(vec!["__none__".into()]);
        }
        let mut prefixes = Vec::new();
        for name in auth_projects {
            if let Some(project) = self.store.get_project(name).await? {
                prefixes.extend(project_prefixes(&project));
            }
        }
        if prefixes.is_empty() {
            prefixes.push("__none__".into());
        }
        Ok(prefixes)
    }
}

fn project_prefixes(project: &ProjectRecord) -> Vec<String> {
    let mut out = project.prefixes.clone();
    out.extend(project.connectors.iter().map(|c| format!("{c}/")));
    out
}
