use serde::{Deserialize, Serialize};

use crate::auth::authlib_injector::AuthlibInjectorServer;

pub const SCHEMA_ID: &str = "authlib-injector-servers";
pub const LITTLE_SKIN_URL: &str = "https://littleskin.cn/api/yggdrasil/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthlibInjectorServersFile {
    #[serde(default)]
    pub servers: Vec<AuthlibInjectorServer>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for AuthlibInjectorServersFile {
    fn default() -> Self {
        Self {
            servers: vec![AuthlibInjectorServer {
                url: LITTLE_SKIN_URL.to_string(),
                name: "LittleSkin".to_string(),
                links: Default::default(),
                non_email_login: false,
                metadata: String::new(),
            }],
            extra: Default::default(),
        }
    }
}

impl AuthlibInjectorServersFile {
    pub fn upsert(&mut self, server: AuthlibInjectorServer) {
        if let Some(existing) = self.servers.iter_mut().find(|item| item.url == server.url) {
            *existing = server;
        } else {
            self.servers.push(server);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_server_lists_include_littleskin_like_hmcl() {
        let servers = AuthlibInjectorServersFile::default();
        assert_eq!(servers.servers.len(), 1);
        assert_eq!(servers.servers[0].url, LITTLE_SKIN_URL);
    }

    #[test]
    fn reads_java_hmcl_server_entries_that_only_contain_a_url() {
        let servers: AuthlibInjectorServersFile = serde_json::from_value(serde_json::json!({
            "servers": [{"url": "https://example.test/api/yggdrasil/"}]
        }))
        .unwrap();
        assert_eq!(servers.servers.len(), 1);
        assert_eq!(
            servers.servers[0].url,
            "https://example.test/api/yggdrasil/"
        );
        assert!(servers.servers[0].name.is_empty());
    }
}
