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

/// One inherited bootstrap read; subsequent authorized fetches reuse only RAM.
pub struct SecretReader {
    config: Config,
    token: Secret,
}
impl SecretReader {
    pub async fn new(config: &Config) -> Result<Self, &'static str> {
        let token = read_fd(config.infisical.credential_fd, 8192).await?;
        Ok(Self {
            config: config.clone(),
            token,
        })
    }
    pub async fn fetch(&self) -> Result<ServiceSecrets, &'static str> {
        fetch_with_token(&self.config, &self.token).await
    }
}

pub async fn read_fd(fd: u32, limit: usize) -> Result<Secret, &'static str> {
    protect_fd(fd)?;
    // A real duplicate keeps the provider's already-authorized open description.
    // Reopening /proc/self/fd would recheck inode permissions after a UID change.
    // Both duplicates use F_DUPFD_CLOEXEC on Linux; the original stays with its owner.
    let descriptor = filedescriptor::FileDescriptor::dup(&(fd as i32))
        .map_err(|_| "Credential-FD ist nicht verfügbar.")?;
    let file = descriptor
        .as_file()
        .map_err(|_| "Credential-FD ist nicht verfügbar.")?;
    let file = tokio::fs::File::from_std(file);
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
    SecretReader::new(config).await?.fetch().await
}
async fn fetch_with_token(config: &Config, token: &Secret) -> Result<ServiceSecrets, &'static str> {
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
    let encryption = decode_encryption_key(take("RS_RELAY_KEY_ENC")?)?;
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

fn decode_encryption_key(encoded: Secret) -> Result<Secret, &'static str> {
    use base64::{
        Engine, alphabet,
        engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    };
    // Existing RS_RELAY_KEY_ENC contract: standard Base64, optional padding and
    // ASCII whitespace, exactly 32 decoded bytes. Never guess a second encoding.
    let compact = Zeroizing::new(
        encoded
            .expose()
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>(),
    );
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let mut decoded = Zeroizing::new([0_u8; 32]);
    let length = engine
        .decode_slice(&compact, decoded.as_mut())
        .map_err(|_| "Speicherschlüssel ist ungültig.")?;
    if length != 32 {
        return Err("Speicherschlüssel ist ungültig.");
    }
    Ok(Secret::new(decoded.to_vec()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    #[test]
    fn existing_base64_key_preserves_exact_bytes_and_optional_padding() {
        for bytes in [[0; 32], [7; 32], [255; 32]] {
            let padded = STANDARD.encode(bytes);
            for text in [
                padded.clone(),
                padded.trim_end_matches('=').to_owned(),
                format!(" \n{}\t{}\r\n", &padded[..8], &padded[8..]),
            ] {
                assert!(
                    decode_encryption_key(Secret::new(text.into_bytes()))
                        .unwrap()
                        .matches(&bytes)
                );
            }
        }
    }

    #[test]
    fn encryption_key_rejects_wrong_length_alphabet_hex_and_noncanonical_bits() {
        let mut noncanonical = STANDARD.encode([7; 32]);
        noncanonical.replace_range(42..43, "d");
        for text in [
            String::new(),
            STANDARD.encode([7; 31]),
            STANDARD.encode([7; 33]),
            "07".repeat(32),
            "_".repeat(43),
            "!".repeat(44),
            noncanonical,
        ] {
            assert!(decode_encryption_key(Secret::new(text.into_bytes())).is_err());
        }
    }
}
