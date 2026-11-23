use anyhow::{Context, Result, ensure};
use doso_sdk::Config as SdkConfig;
use ed25519_dalek::{SigningKey, VerifyingKey};
use robot_cloud_common::{is_uuid, load_signing_key, load_verifying_key};
use std::{env, fs, net::IpAddr, time::Duration};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};

pub struct Config {
    pub device_id: String,
    pub signing_key: SigningKey,
    pub gateway_key: VerifyingKey,
    pub cloud_url: String,
    pub cloud_ca: Option<Vec<u8>>,
    pub local_endpoint: String,
    pub local_api_key: String,
    pub local_ca: Option<Vec<u8>>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let config = Self {
            device_id: env::var("DOSO_DEVICE_ID")?,
            signing_key: load_signing_key(&env::var("DOSO_DEVICE_SIGNING_KEY")?)?,
            gateway_key: load_verifying_key(&env::var("DOSO_GATEWAY_PUBLIC_KEY")?)?,
            cloud_url: env::var("DOSO_CONTROL_URL")?,
            cloud_ca: certificate("DOSO_CONTROL_CA")?,
            local_endpoint: env::var("DOSO_CONTROL_LOCAL_ENDPOINT")?,
            local_api_key: env::var("DOSO_CONTROL_LOCAL_API_KEY")?,
            local_ca: certificate("DOSO_CONTROL_LOCAL_CA")?,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(is_uuid(&self.device_id), "invalid_control_device");
        ensure!(
            !self.local_api_key.is_empty(),
            "missing_local_sdk_credential"
        );
        validate_url(&self.cloud_url)?;
        validate_url(&self.local_endpoint)?;
        Ok(())
    }

    pub(super) fn endpoint(&self) -> Result<Endpoint> {
        let mut endpoint =
            Endpoint::from_shared(self.cloud_url.clone())?.connect_timeout(Duration::from_secs(5));
        if self.cloud_url.starts_with("https://") {
            let mut tls = ClientTlsConfig::new().with_native_roots();
            if let Some(certificate) = &self.cloud_ca {
                ensure!(!certificate.is_empty(), "empty_control_ca");
                tls = tls.ca_certificate(Certificate::from_pem(certificate));
            }
            endpoint = endpoint.tls_config(tls)?;
        } else {
            ensure!(self.cloud_ca.is_none(), "control_ca_requires_tls");
        }
        Ok(endpoint)
    }

    pub(super) fn sdk(&self, session_id: &str) -> SdkConfig {
        let mut config = SdkConfig::new(&self.local_endpoint);
        config.client_id = format!("cloud-{session_id}");
        config.api_key = Some(self.local_api_key.clone());
        config.tls_ca_certificate = self.local_ca.clone();
        config.timeout = Duration::from_secs(2);
        config
    }
}

fn certificate(name: &str) -> Result<Option<Vec<u8>>> {
    env::var_os(name)
        .map(|path| fs::read(path).with_context(|| format!("read {name}")))
        .transpose()
}

fn validate_url(value: &str) -> Result<()> {
    let url = url::Url::parse(value)?;
    let host = url.host_str().context("missing_control_host")?;
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    ensure!(
        (url.scheme() == "https" || url.scheme() == "http" && loopback)
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid_control_url"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_is_limited_to_loopback() {
        for url in [
            "http://127.0.0.1:1234",
            "http://[::1]:1234",
            "https://control.example.com",
        ] {
            validate_url(url).unwrap();
        }
        for url in [
            "http://control.example.com",
            "https://user:password@control.example.com",
            "https://control.example.com/?token=secret",
        ] {
            assert!(validate_url(url).is_err());
        }
    }
}
