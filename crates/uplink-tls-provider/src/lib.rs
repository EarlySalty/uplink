//! Begrenzter Caddy→Infisical-Zertifikatsabgleich. Keine eigene Zertifikatsausstellung.
use rustls::{client::danger::ServerCertVerifier, sign::CertifiedKey};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

pub const CERTIFICATE_SECRET: &str = "UPLINK_TLS_CERTIFICATE";
pub const PRIVATE_KEY_SECRET: &str = "UPLINK_TLS_PRIVATE_KEY";
#[cfg(test)]
#[path = "tests.rs"]
mod network_tests;
const MAX_CERTIFICATE: usize = 65536;
const MAX_KEY: usize = 16384;
const MAX_RESPONSE: usize = 262144;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Configuration,
    File,
    FilePermissions,
    FileChanged,
    InputTooLarge,
    Identity,
    Certificate,
    KeyMismatch,
    Transport,
    Rejected(u16),
    Response,
    Verification,
    Deadline,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Configuration => "TLS-Provider-Konfiguration ist ungültig.",
            Self::File => "Eine konfigurierte Quelldatei ist nicht lesbar.",
            Self::FilePermissions => "Eine Quelldatei hat unzulässige Rechte oder Eigentümer.",
            Self::FileChanged => "Zertifikatsquelle wurde während des Lesens geändert.",
            Self::InputTooLarge => "TLS-Provider-Eingabe überschreitet die Grenze.",
            Self::Identity => "Vorhandene Infisical-Identität ist ungültig.",
            Self::Certificate => {
                "Zertifikatskette, Hostname oder Gültigkeitszeit sind nicht bestätigt."
            }
            Self::KeyMismatch => "Zertifikat und privater Schlüssel passen nicht zusammen.",
            Self::Transport => "Infisical ist nicht rechtzeitig erreichbar.",
            Self::Rejected(_) => "Infisical hat den TLS-Abgleich abgewiesen.",
            Self::Response => "Infisical-Antwort ist ungültig oder zu groß.",
            Self::Verification => "Das zurückgelesene TLS-Paar bestätigt den Abgleich nicht.",
            Self::Deadline => "TLS-Abgleich hat seine zulässige Frist überschritten.",
        })
    }
}
impl std::error::Error for Error {}

pub struct Secret(Zeroizing<Vec<u8>>);
impl Secret {
    fn new(value: Vec<u8>) -> Self {
        Self(Zeroizing::new(value))
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([geschützt])")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub hostname: String,
    pub certificate_file: PathBuf,
    pub private_key_file: PathBuf,
    pub source_owner_uid: u32,
    pub identity_file: PathBuf,
    pub identity_owner_uid: u32,
    pub project_id: String,
    pub environment: String,
    pub secret_path: String,
    pub infisical_port: u16,
}
impl Config {
    fn validate(&self) -> Result<()> {
        let component = |value: &str| max_component(value);
        if self.hostname.is_empty()
            || self.hostname.len() > 253
            || !self
                .hostname
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            || self.hostname.parse::<std::net::IpAddr>().is_ok()
            || self.infisical_port == 0
            || self.project_id.len() != 36
            || self.project_id.bytes().enumerate().any(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b != b'-'
                } else {
                    !b.is_ascii_hexdigit()
                }
            })
            || !component(&self.environment)
            || !self.secret_path.starts_with('/')
            || self.secret_path.len() > 256
            || self
                .secret_path
                .split('/')
                .skip(1)
                .any(|s| !s.is_empty() && !component(s))
            || [
                &self.certificate_file,
                &self.private_key_file,
                &self.identity_file,
            ]
            .iter()
            .any(|p| {
                !p.is_absolute()
                    || p.components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
            })
            || self.certificate_file == self.private_key_file
            || self.certificate_file == self.identity_file
            || self.private_key_file == self.identity_file
        {
            return Err(Error::Configuration);
        }
        ServerName::try_from(self.hostname.clone()).map_err(|_| Error::Configuration)?;
        Ok(())
    }
}
fn max_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub struct Material {
    certificate: Secret,
    private_key: Secret,
}
impl fmt::Debug for Material {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Material([geschützt])")
    }
}

fn validate_material(
    material: &Material,
    hostname: &str,
    roots: Arc<rustls::RootCertStore>,
    now: UnixTime,
) -> Result<()> {
    if material.certificate.0.is_empty()
        || material.certificate.0.len() > MAX_CERTIFICATE
        || material.private_key.0.is_empty()
        || material.private_key.0.len() > MAX_KEY
    {
        return Err(Error::InputTooLarge);
    }
    let certs = CertificateDer::pem_slice_iter(&material.certificate.0)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::Certificate)?;
    if certs.is_empty() || certs.len() > 10 {
        return Err(Error::Certificate);
    }
    let mut keys = PrivateKeyDer::pem_slice_iter(&material.private_key.0);
    let key = keys
        .next()
        .ok_or(Error::KeyMismatch)?
        .map_err(|_| Error::KeyMismatch)?;
    if keys.next().is_some() {
        return Err(Error::KeyMismatch);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let key = CertifiedKey::from_der(certs, key, &provider).map_err(|_| Error::KeyMismatch)?;
    key.keys_match().map_err(|_| Error::KeyMismatch)?;
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .map_err(|_| Error::Certificate)?;
    let name = ServerName::try_from(hostname.to_owned()).map_err(|_| Error::Certificate)?;
    verifier
        .verify_server_cert(&key.cert[0], &key.cert[1..], &name, &[], now)
        .map_err(|_| Error::Certificate)?;
    Ok(())
}

async fn read_file(path: &Path, owner: u32, private: bool, limit: usize) -> Result<Secret> {
    use std::os::unix::fs::MetadataExt;
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(path)
        .await
        .map_err(|_| Error::File)?;
    let before = file.metadata().await.map_err(|_| Error::File)?;
    if !before.is_file()
        || before.uid() != owner
        || before.mode() & if private { 0o077 } else { 0o022 } != 0
    {
        return Err(Error::FilePermissions);
    }
    if before.len() == 0 || before.len() > limit as u64 {
        return Err(Error::InputTooLarge);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    let mut reader = file.take(limit as u64 + 1);
    tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut bytes))
        .await
        .map_err(|_| Error::Deadline)?
        .map_err(|_| Error::File)?;
    let after = reader.get_ref().metadata().await.map_err(|_| Error::File)?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err(Error::InputTooLarge);
    }
    if before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || bytes.len() as u64 != after.len()
    {
        return Err(Error::FileChanged);
    }
    Ok(Secret::new(std::mem::take(&mut bytes)))
}

pub async fn load_config(path: &Path) -> Result<Config> {
    if !path.is_absolute() {
        return Err(Error::Configuration);
    }
    let source = read_file(path, 0, false, 16384).await?;
    let config: Config =
        toml::from_str(std::str::from_utf8(&source.0).map_err(|_| Error::Configuration)?)
            .map_err(|_| Error::Configuration)?;
    config.validate()?;
    Ok(config)
}

fn identity(source: &[u8]) -> Result<Secret> {
    #[derive(Deserialize)]
    struct ExistingIdentity<'a> {
        #[serde(rename = "serviceToken", borrow)]
        token: &'a str,
    }
    if source.len() > 65536 {
        return Err(Error::InputTooLarge);
    }
    let value: ExistingIdentity<'_> =
        serde_json::from_slice(source).map_err(|_| Error::Identity)?;
    let token = value.token.trim();
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_control()) {
        return Err(Error::Identity);
    }
    Ok(Secret::new(token.as_bytes().to_vec()))
}

#[derive(Deserialize)]
struct ReadReply {
    secret: ReadEntry,
}
#[derive(Deserialize)]
struct ReadEntry {
    #[serde(rename = "secretKey")]
    name: String,
    #[serde(rename = "secretValue")]
    value: String,
    #[serde(rename = "secretValueHidden", default)]
    hidden: bool,
}
impl Drop for ReadEntry {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

struct Vault {
    client: reqwest::Client,
    authorization: reqwest::header::HeaderValue,
    base: String,
    project: String,
    environment: String,
    path: String,
}
impl Vault {
    fn new(config: &Config, token: &Secret) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| Error::Transport)?;
        let mut authorization = Zeroizing::new(b"Bearer ".to_vec());
        authorization.extend_from_slice(&token.0);
        let mut authorization = reqwest::header::HeaderValue::from_bytes(&authorization)
            .map_err(|_| Error::Identity)?;
        authorization.set_sensitive(true);
        Ok(Self {
            client,
            authorization,
            base: format!("http://127.0.0.1:{}", config.infisical_port),
            project: config.project_id.clone(),
            environment: config.environment.clone(),
            path: config.secret_path.clone(),
        })
    }
    async fn read(&self, name: &'static str) -> Result<Option<Secret>> {
        let response = self
            .client
            .get(format!("{}/api/v4/secrets/{name}", self.base))
            .query(&[
                ("projectId", self.project.as_str()),
                ("environment", self.environment.as_str()),
                ("secretPath", self.path.as_str()),
                ("type", "shared"),
                ("expandSecretReferences", "false"),
                ("includeImports", "false"),
                ("viewSecretValue", "true"),
            ])
            .header(reqwest::header::AUTHORIZATION, self.authorization.clone())
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Error::Rejected(response.status().as_u16()));
        }
        let body = bounded_body(response).await?;
        let mut reply: ReadReply = serde_json::from_slice(&body).map_err(|_| Error::Response)?;
        if reply.secret.name != name || reply.secret.hidden {
            return Err(Error::Response);
        }
        let limit = if name == CERTIFICATE_SECRET {
            MAX_CERTIFICATE
        } else {
            MAX_KEY
        };
        if reply.secret.value.len() > limit {
            return Err(Error::Response);
        }
        Ok(Some(Secret::new(
            std::mem::take(&mut reply.secret.value).into_bytes(),
        )))
    }
    async fn write(&self, name: &'static str, value: &Secret) -> Result<()> {
        if name != CERTIFICATE_SECRET && name != PRIVATE_KEY_SECRET {
            return Err(Error::Configuration);
        }
        #[derive(Serialize)]
        struct Write<'a> {
            #[serde(rename = "workspaceId")]
            workspace: &'a str,
            environment: &'a str,
            #[serde(rename = "secretPath")]
            path: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
            #[serde(rename = "secretValue")]
            value: &'a str,
        }
        let payload = Write {
            workspace: &self.project,
            environment: &self.environment,
            path: &self.path,
            kind: "shared",
            value: std::str::from_utf8(&value.0).map_err(|_| Error::Certificate)?,
        };
        for method in [reqwest::Method::PATCH, reqwest::Method::POST] {
            let mut body = Zeroizing::new(Vec::new());
            serde_json::to_writer(&mut *body, &payload).map_err(|_| Error::Response)?;
            let response = self
                .client
                .request(
                    method.clone(),
                    format!("{}/api/v3/secrets/raw/{name}", self.base),
                )
                .header(reqwest::header::AUTHORIZATION, self.authorization.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes::Bytes::from_owner(body))
                .send()
                .await
                .map_err(|_| Error::Transport)?;
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                return Ok(());
            }
            // Derselbe bewährte Schreibvertrag: ausschließlich 404 erlaubt Anlegen.
            // Fehlerkörper können den privaten PEM-Key spiegeln und werden nie gelesen.
            if method == reqwest::Method::PATCH && status == 404 {
                continue;
            }
            return Err(Error::Rejected(status));
        }
        Err(Error::Verification)
    }
}
async fn bounded_body(mut response: reqwest::Response) -> Result<Zeroizing<Vec<u8>>> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE as u64)
    {
        return Err(Error::Response);
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(part) = response.chunk().await.map_err(|_| Error::Transport)? {
        if body.len().saturating_add(part.len()) > MAX_RESPONSE {
            return Err(Error::Response);
        }
        body.extend_from_slice(&part);
    }
    Ok(body)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Unchanged,
    Updated,
}

async fn sync_material(
    vault: &Vault,
    source: &Material,
    hostname: &str,
    roots: Arc<rustls::RootCertStore>,
) -> Result<SyncStatus> {
    validate_material(source, hostname, roots.clone(), UnixTime::now())?;
    let old_cert = vault.read(CERTIFICATE_SECRET).await?;
    let old_key = vault.read(PRIVATE_KEY_SECRET).await?;
    let cert_changed = old_cert
        .as_ref()
        .is_none_or(|s| s.0.as_slice() != source.certificate.0.as_slice());
    let key_changed = old_key
        .as_ref()
        .is_none_or(|s| s.0.as_slice() != source.private_key.0.as_slice());
    if !cert_changed && !key_changed {
        return Ok(SyncStatus::Unchanged);
    }
    if cert_changed {
        vault.write(CERTIFICATE_SECRET, &source.certificate).await?;
    }
    if key_changed {
        vault.write(PRIVATE_KEY_SECRET, &source.private_key).await?;
    }
    let actual = Material {
        certificate: vault
            .read(CERTIFICATE_SECRET)
            .await?
            .ok_or(Error::Verification)?,
        private_key: vault
            .read(PRIVATE_KEY_SECRET)
            .await?
            .ok_or(Error::Verification)?,
    };
    if actual.certificate.0.as_slice() != source.certificate.0.as_slice()
        || actual.private_key.0.as_slice() != source.private_key.0.as_slice()
    {
        return Err(Error::Verification);
    }
    validate_material(&actual, hostname, roots, UnixTime::now())?;
    Ok(SyncStatus::Updated)
}

/// Ein Durchlauf besitzt ausschließlich zwei feste Zielnamen. Der Aufrufer startet
/// ihn seriell; der systemd-Oneshot wird von seinem Timer nicht überlappt.
pub async fn synchronize(config: &Config) -> Result<SyncStatus> {
    config.validate()?;
    tokio::time::timeout(Duration::from_secs(40), async {
        let source = Material {
            certificate: read_file(
                &config.certificate_file,
                config.source_owner_uid,
                false,
                MAX_CERTIFICATE,
            )
            .await?,
            private_key: read_file(
                &config.private_key_file,
                config.source_owner_uid,
                true,
                MAX_KEY,
            )
            .await?,
        };
        let roots = Arc::new(rustls::RootCertStore::from_iter(
            webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
        ));
        // Gültigkeit vor dem Lesen der Schreibidentität und vor irgendeinem Netzaufruf.
        validate_material(&source, &config.hostname, roots.clone(), UnixTime::now())?;
        let document = read_file(
            &config.identity_file,
            config.identity_owner_uid,
            true,
            65536,
        )
        .await?;
        let token = identity(&document.0)?;
        let vault = Vault::new(config, &token)?;
        drop(document);
        drop(token);
        sync_material(&vault, &source, &config.hostname, roots).await
    })
    .await
    .map_err(|_| Error::Deadline)?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn material(name: &str) -> (Material, Arc<rustls::RootCertStore>) {
        let cert = rcgen::generate_simple_self_signed(vec![name.into()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.cert.der().clone()).unwrap();
        (
            Material {
                certificate: Secret::new(cert.cert.pem().into_bytes()),
                private_key: Secret::new(cert.signing_key.serialize_pem().into_bytes()),
            },
            Arc::new(roots),
        )
    }
    #[test]
    fn valid_material_requires_matching_key_hostname_chain_and_time() {
        let (first, roots) = material("test.example");
        assert_eq!(
            validate_material(&first, "test.example", roots.clone(), UnixTime::now()),
            Ok(())
        );
        assert_eq!(
            validate_material(&first, "other.example", roots.clone(), UnixTime::now()),
            Err(Error::Certificate)
        );
        assert_eq!(
            validate_material(
                &first,
                "test.example",
                roots.clone(),
                UnixTime::since_unix_epoch(Duration::ZERO)
            ),
            Err(Error::Certificate)
        );
        let (second, wrong_roots) = material("test.example");
        assert_eq!(
            validate_material(&first, "test.example", wrong_roots, UnixTime::now()),
            Err(Error::Certificate)
        );
        let mixed = Material {
            certificate: first.certificate,
            private_key: second.private_key,
        };
        assert_eq!(
            validate_material(&mixed, "test.example", roots, UnixTime::now()),
            Err(Error::KeyMismatch)
        );
        assert!(!format!("{mixed:?}").contains("PRIVATE KEY"));
    }
}
