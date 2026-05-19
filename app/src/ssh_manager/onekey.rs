use anyhow::Result;
use zeroize::Zeroizing;

use warp_ssh_manager::{
    AuthType, KeychainSecretStore, NodeKind, SecretKind, SshRepository, SshSecretStore,
};

pub struct OneKeyCredential {
    pub label: String,
    pub subtitle: String,
    pub secret: Zeroizing<String>,
}

pub fn load_saved_ssh_credentials() -> Result<Vec<OneKeyCredential>> {
    let store = KeychainSecretStore;
    warp_ssh_manager::with_conn(|conn| {
        let nodes = SshRepository::list_nodes(conn)?;
        let mut credentials = Vec::new();

        for node in nodes {
            if node.kind != NodeKind::Server {
                continue;
            }
            let Some(server) = SshRepository::get_server(conn, &node.id)? else {
                continue;
            };
            let kind = match server.auth_type {
                AuthType::Password => SecretKind::Password,
                AuthType::Key => SecretKind::Passphrase,
            };
            let secret = match store.get(&node.id, kind) {
                Ok(Some(secret)) if !secret.is_empty() => secret,
                Ok(Some(_)) | Ok(None) => continue,
                Err(e) => {
                    log::warn!("onekey: failed to read saved ssh credential: {e}");
                    continue;
                }
            };
            let target = if server.username.is_empty() {
                format!("{}:{}", server.host, server.port)
            } else {
                format!("{}@{}:{}", server.username, server.host, server.port)
            };
            let subtitle = match kind {
                SecretKind::Password => target,
                SecretKind::Passphrase => {
                    let key_path = server.key_path.as_deref().unwrap_or("key");
                    format!("{key_path} for {target}")
                }
            };
            credentials.push(OneKeyCredential {
                label: node.name,
                subtitle,
                secret,
            });
        }

        Ok(credentials)
    })
}
