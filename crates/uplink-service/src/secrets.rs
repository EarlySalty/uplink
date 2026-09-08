use crate::{
    config::{Config, TlsConfig},
    crypto::Secret,
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
use uplink_ingest::rustls;
use zeroize::Zeroizing;

pub struct ServiceSecrets {
    pub api: Secret,
    pub admin: Secret,
    pub encryption: Secret,
    pub database: Secret,
    pub bot_internal: Secret,
    pub tls_material: Option<(Secret, Secret)>,
}

pub async fn read_fd(fd: u32, limit: usize) -> Result<Secret, &'static str> {
    protect_fd(fd)?;
    let file = tokio::fs::File::open(format!("/proc/self/fd/{fd}"))
        .await
        .map_err(|_| "Credential-FD ist nicht verfügbar.")?;
    let mut data = Zeroizing::new(Vec::new());
    tokio::time::timeout(
        Duration::from_secs(5),
        file.take((limit + 1) as u64).read_to_end(&mut data),
    )
    .await
    .map_err(|_| "Credential-FD hat nicht rechtzeitig geliefert.")?
    .map_err(|_| "Credential-FD konnte nicht gelesen werden.")?;
    if data.is_empty() || data.len() > limit {
        return Err("Credential-Größe ist ungültig.");
    }
    Ok(Secret::new(std::mem::take(&mut *data)))
}

// Nur ausdrücklich injizierte Deskriptoren schützen; der Besitzer behält sie.
// Das Original muss CLOEXEC tragen, nicht nur die zum Lesen geöffnete Kopie.
fn protect_fd(fd: u32) -> Result<(), &'static str> {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    let raw = i32::try_from(fd)
        .ok()
        .filter(|fd| *fd >= 3)
        .ok_or("Credential-FD ist ungültig.")?;
    let flags = fcntl(raw, FcntlArg::F_GETFD).map_err(|_| "Credential-FD ist nicht verfügbar.")?;
    fcntl(
        raw,
        FcntlArg::F_SETFD(FdFlag::from_bits_retain(flags) | FdFlag::FD_CLOEXEC),
    )
    .map_err(|_| "Credential-FD konnte nicht geschützt werden.")?;
    Ok(())
}

/// Vor dem ersten möglichen Prozessstart alle autorisierten Original-FDs schützen.
pub fn protect_configured_fds(config: &Config) -> Result<(), &'static str> {
    protect_fd(config.infisical.credential_fd)?;
    if let TlsConfig::Fds {
        certificate_fd,
        private_key_fd,
    } = config.tls
    {
        protect_fd(certificate_fd)?;
        protect_fd(private_key_fd)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    secrets: Vec<Entry>,
    #[serde(default)]
    imports: Vec<Import>,
}
#[derive(Deserialize)]
struct Import {
    #[serde(default)]
    secrets: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "secretKey")]
    name: String,
    #[serde(rename = "secretValue")]
    value: String,
}
impl Drop for Entry {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.value.zeroize();
    }
}

pub async fn fetch(config: &Config) -> Result<ServiceSecrets, &'static str> {
    let token = read_fd(config.infisical.credential_fd, 8192).await?;
    let token = std::str::from_utf8(token.expose())
        .map_err(|_| "Infisical-Zugang ist ungültig.")?
        .trim();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(config.request_timeout_seconds))
        .no_proxy()
        .build()
        .map_err(|_| "HTTP-Client ist nicht verfügbar.")?;
    let mut response = client
        .get(format!(
            "{}/api/v4/secrets/",
            config.infisical.base_url.trim_end_matches('/')
        ))
        .query(&[
            ("projectId", config.infisical.project_id.as_str()),
            ("environment", config.infisical.environment.as_str()),
            ("secretPath", config.infisical.secret_path.as_str()),
            ("viewSecretValue", "true"),
            ("includeImports", "true"),
            ("recursive", "false"),
        ])
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "Infisical ist nicht erreichbar.")?;
    if !response.status().is_success() {
        return Err("Infisical hat den Zugriff abgewiesen.");
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Infisical-Antwort wurde unterbrochen.")?
    {
        if body.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
            return Err("Infisical-Antwort ist zu groß.");
        }
        body.extend_from_slice(&chunk);
    }
    let reply: Reply =
        serde_json::from_slice(&body).map_err(|_| "Infisical-Antwort ist ungültig.")?;
    let mut names = vec![
        "RS_RELAY_API_SECRET",
        "RS_RELAY_ADMIN_SECRET",
        "RS_RELAY_KEY_ENC",
        "RS_RELAY_DATABASE_URL",
        "RS_RELAY_BOT_INTERNAL_TOKEN",
    ];
    if let TlsConfig::Infisical {
        certificate_secret,
        private_key_secret,
    } = &config.tls
    {
        names.extend([certificate_secret.as_str(), private_key_secret.as_str()]);
    }
    let mut values = HashMap::new();
    for mut entry in reply
        .secrets
        .into_iter()
        .chain(reply.imports.into_iter().flat_map(|i| i.secrets))
    {
        if names.contains(&entry.name.as_str()) {
            let name = std::mem::take(&mut entry.name);
            let secret = Secret::new(std::mem::take(&mut entry.value).into_bytes());
            if values.insert(name, secret).is_some() {
                return Err("Infisical-Zugang ist mehrfach definiert.");
            }
        }
    }
    let mut take = |name: &str| {
        values
            .remove(name)
            .filter(|s| !s.expose().is_empty())
            .ok_or("Benötigter Dienstzugang fehlt.")
    };
    let encryption = take("RS_RELAY_KEY_ENC")?;
    let encryption = Secret::new(
        hex::decode(encryption.expose()).map_err(|_| "Speicherschlüssel ist ungültig.")?,
    );
    if encryption.expose().len() != 32 {
        return Err("Speicherschlüssel ist ungültig.");
    }
    let tls_material = if let TlsConfig::Infisical {
        certificate_secret,
        private_key_secret,
    } = &config.tls
    {
        Some((take(certificate_secret)?, take(private_key_secret)?))
    } else {
        None
    };
    Ok(ServiceSecrets {
        api: take("RS_RELAY_API_SECRET")?,
        admin: take("RS_RELAY_ADMIN_SECRET")?,
        encryption,
        database: take("RS_RELAY_DATABASE_URL")?,
        bot_internal: take("RS_RELAY_BOT_INTERNAL_TOKEN")?,
        tls_material,
    })
}

pub async fn tls(
    config: &Config,
    secrets: &ServiceSecrets,
) -> Result<Arc<rustls::ServerConfig>, &'static str> {
    let fd_material;
    let (cert, key) = match &config.tls {
        TlsConfig::Fds {
            certificate_fd,
            private_key_fd,
        } => {
            fd_material = (
                read_fd(*certificate_fd, 64 * 1024).await?,
                read_fd(*private_key_fd, 16 * 1024).await?,
            );
            (&fd_material.0, &fd_material.1)
        }
        TlsConfig::Infisical { .. } => {
            let material = secrets.tls_material.as_ref().ok_or("TLS-Material fehlt.")?;
            (&material.0, &material.1)
        }
    };
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let certs = CertificateDer::pem_slice_iter(cert.expose())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "TLS-Zertifikat ist ungültig.")?;
    let key =
        PrivateKeyDer::from_pem_slice(key.expose()).map_err(|_| "TLS-Schlüssel ist ungültig.")?;
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "TLS-Protokollkonfiguration ist ungültig.")?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|_| "TLS-Zertifikat und Schlüssel passen nicht zusammen.")?;
    Ok(Arc::new(tls))
}
