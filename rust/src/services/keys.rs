//! Bearer-key resolution: env bootstrap keys + DB-managed hashed keys.

use crate::domain::ApiKeyUpsert;
use crate::store::Store;
use crate::error::{Error, Result};
use crate::http::auth::AuthInfo;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct EnvKeyEntry {
    pub name: String,
    pub secret: String,
    pub role: String,
    pub scope: Vec<String>,
}

pub struct KeyRegistry {
    store: Arc<dyn Store>,
    by_secret: HashMap<String, EnvKeyEntry>,
}

#[derive(Debug, serde::Serialize)]
pub struct CreatedKey {
    pub name: String,
    pub secret: String,
    pub prefix: String,
    pub role: String,
    pub scope: Vec<String>,
}

impl KeyRegistry {
    pub fn new(store: Arc<dyn Store>, env_entries: Vec<EnvKeyEntry>) -> Self {
        let mut by_secret = HashMap::new();
        for entry in env_entries {
            by_secret.insert(entry.secret.clone(), entry);
        }
        Self { store, by_secret }
    }

    pub fn has_env_keys(&self) -> bool {
        !self.by_secret.is_empty()
    }

    pub async fn is_empty(&self) -> Result<bool> {
        Ok(!self.has_env_keys() && self.store.count_api_keys().await? == 0)
    }

    /// Resolve a bearer secret to an identity. Env keys win on collision.
    pub async fn verify(&self, secret: &str) -> Result<Option<AuthInfo>> {
        if let Some(entry) = self.by_secret.get(secret) {
            return Ok(Some(AuthInfo {
                name: entry.name.clone(),
                role: entry.role.clone(),
                projects: entry.scope.clone(),
            }));
        }
        match self.store.find_api_key_by_hash(&sha256_hex(secret)).await? {
            Some(rec) => Ok(Some(AuthInfo {
                name: rec.name,
                role: rec.role,
                projects: rec.scope,
            })),
            None => Ok(None),
        }
    }

    /// Create a DB-managed key. The plaintext secret is returned exactly once
    /// and only its SHA-256 hash is persisted.
    pub async fn create_key(
        &self,
        name: Option<&str>,
        secret: Option<&str>,
        role: &str,
        scope: &[String],
        created_by: &str,
    ) -> Result<CreatedKey> {
        let secret = secret.unwrap_or(&generate_secret()).to_string();
        let name = match name.map(str::trim).filter(|s| !s.is_empty()) {
            Some(n) => n.to_string(),
            None => format!("agent-{}", &ulid::Ulid::new().to_string()[..6].to_lowercase()),
        };
        if self.store.get_api_key(&name).await?.is_some() {
            return Err(Error::Conflict(format!("Key name already exists: {name}")));
        }
        let prefix: String = secret.chars().take(6).collect();
        self.store
            .upsert_api_key(&ApiKeyUpsert {
                name: name.clone(),
                key_hash: sha256_hex(&secret),
                key_prefix: prefix.clone(),
                scope: scope.to_vec(),
                role: role.to_string(),
                created_by: created_by.to_string(),
            })
            .await?;
        Ok(CreatedKey {
            name,
            secret,
            prefix,
            role: role.to_string(),
            scope: scope.to_vec(),
        })
    }

    pub async fn delete_key(&self, name: &str) -> Result<bool> {
        self.store.delete_api_key(name).await
    }
}

pub fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn generate_secret() -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    let chars: Vec<u8> = (0..48).map(|_| HEX[rng.gen_range(0..16)]).collect();
    format!("wk_{}", String::from_utf8_lossy(&chars))
}
