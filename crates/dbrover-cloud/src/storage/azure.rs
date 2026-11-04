//! Azure Blob Storage's List Blobs: a container's blobs a level at a time, with an Entra ID
//! token from C0's sign-in.

use dbrover_core::Password;

use super::{Level, StorageError, encode, xml};
use crate::http::Https;

/// The version of Blob Storage's REST API asked for. Bearer tokens need 2017-11-09 or later;
/// this is a recent one every Azure region serves.
const API_VERSION: &str = "2023-11-03";

/// A container, where it is, and the token that lists it.
#[derive(Debug, Clone)]
pub struct AzureLocation {
    pub account: String,
    pub container: String,
    /// The account's blob service when it isn't `https://<account>.blob.core.windows.net`
    /// (Azurite, a private endpoint).
    pub endpoint: Option<String>,
    /// `None` for a container that allows public reads.
    pub token: Option<Password>,
}

/// Lists one container.
pub struct AzureLister {
    https: Https,
    location: AzureLocation,
}

impl AzureLister {
    /// # Errors
    ///
    /// [`StorageError::Http`] when TLS can't be set up.
    pub fn new(location: AzureLocation) -> Result<Self, StorageError> {
        Ok(Self {
            https: Https::new()?,
            location,
        })
    }

    /// The folders and files directly under `prefix`, at most `max`, from where `after`, a
    /// previous level's [`Level::next`] (Azure's marker), left off.
    ///
    /// # Errors
    ///
    /// [`StorageError`] when the request fails or Azure refuses it, with its code.
    pub async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        max: usize,
    ) -> Result<Level, StorageError> {
        let url = self.location.list_url(prefix, after, max);
        let headers = [("x-ms-version".to_owned(), API_VERSION.to_owned())];
        let answer = self
            .https
            .get_with_token(&url, self.location.token.as_ref(), &headers)
            .await?;
        if answer.status != 200 {
            return Err(xml::s3_refusal(answer.status, &answer.body));
        }
        xml::azure_level(&answer.body)
    }
}

impl AzureLocation {
    fn list_url(&self, prefix: &str, after: Option<&str>, max: usize) -> String {
        let base = match &self.endpoint {
            Some(endpoint) => endpoint.trim_end_matches('/').to_owned(),
            None => format!("https://{}.blob.core.windows.net", self.account),
        };
        let mut url = format!(
            "{base}/{}?restype=container&comp=list&delimiter=%2F&maxresults={max}&prefix={}",
            encode(&self.container),
            encode(prefix)
        );
        if let Some(after) = after {
            url.push_str("&marker=");
            url.push_str(&encode(after));
        }
        url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_urls_name_the_container_prefix_and_marker() {
        let cases = [
            (
                None,
                "",
                None,
                "https://acme.blob.core.windows.net/raw?restype=container&comp=list\
                 &delimiter=%2F&maxresults=1000&prefix=",
            ),
            (
                Some("http://127.0.0.1:10000/devstoreaccount1/"),
                "2026/",
                Some("2!96!MDAwMDE5"),
                "http://127.0.0.1:10000/devstoreaccount1/raw?restype=container&comp=list\
                 &delimiter=%2F&maxresults=1000&prefix=2026%2F&marker=2%2196%21MDAwMDE5",
            ),
        ];
        for (endpoint, prefix, after, expected) in cases {
            let location = AzureLocation {
                account: "acme".to_owned(),
                container: "raw".to_owned(),
                endpoint: endpoint.map(str::to_owned),
                token: None,
            };
            assert_eq!(location.list_url(prefix, after, 1000), expected);
        }
    }

    /// The token goes only over HTTPS, or to this machine, where Azurite listens.
    #[tokio::test]
    async fn a_token_is_never_sent_over_plain_http_to_another_machine() {
        let lister = AzureLister::new(AzureLocation {
            account: "acme".to_owned(),
            container: "raw".to_owned(),
            endpoint: Some("http://storage.internal:10000/acme".to_owned()),
            token: Some(Password::new("eyJ0eXAi")),
        })
        .unwrap();

        let error = lister.list("", None, 1000).await.unwrap_err();

        assert!(
            matches!(&error, StorageError::Http(crate::HttpError::Insecure(_))),
            "{error}"
        );
    }
}
