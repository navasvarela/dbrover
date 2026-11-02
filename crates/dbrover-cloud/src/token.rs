//! Access tokens, as Google Cloud and Microsoft Entra ID hand them out: a bearer secret that
//! expires within the hour, which a data source fetches when it connects.

use std::future::Future;
use std::time::{Duration, SystemTime};

use dbrover_core::Password;
use tokio::sync::Mutex;

/// How long before its expiry a token is replaced. A connection opened with a token that is
/// about to expire can fail mid-handshake; Google's and Azure's own libraries refresh this
/// early too.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

/// A token and when it stops being accepted. Its `Debug` never prints the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub secret: Password,
    pub expires_at: SystemTime,
}

impl AccessToken {
    /// Whether the token can still be handed out at `now`: it has more than
    /// [`REFRESH_MARGIN`] left.
    pub fn is_fresh_at(&self, now: SystemTime) -> bool {
        self.expires_at
            .duration_since(now)
            .is_ok_and(|left| left > REFRESH_MARGIN)
    }
}

/// The last token one sign-in fetched, reused until [`REFRESH_MARGIN`] before it expires, so
/// that every console opened in a row doesn't ask the provider again.
#[derive(Debug, Default)]
pub struct TokenCache {
    /// Held while a token is fetched, so connections opened together fetch it once.
    last: Mutex<Option<AccessToken>>,
}

impl TokenCache {
    /// The cached token if it is still fresh, else the one `fetch` returns, which is kept.
    ///
    /// # Errors
    ///
    /// `fetch`'s error; the cache is left as it was.
    pub async fn token<F, E>(&self, fetch: impl FnOnce() -> F) -> Result<AccessToken, E>
    where
        F: Future<Output = Result<AccessToken, E>>,
    {
        self.token_at(SystemTime::now(), fetch).await
    }

    async fn token_at<F, E>(
        &self,
        now: SystemTime,
        fetch: impl FnOnce() -> F,
    ) -> Result<AccessToken, E>
    where
        F: Future<Output = Result<AccessToken, E>>,
    {
        let mut last = self.last.lock().await;
        if let Some(token) = last.as_ref().filter(|token| token.is_fresh_at(now)) {
            return Ok(token.clone());
        }
        let token = fetch().await?;
        *last = Some(token.clone());
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn token(secret: &str, expires_at: SystemTime) -> AccessToken {
        AccessToken {
            secret: Password::new(secret),
            expires_at,
        }
    }

    #[test]
    fn debug_hides_the_secret() {
        let shown = format!("{:?}", token("ya29.secret", SystemTime::UNIX_EPOCH));
        assert!(!shown.contains("ya29"), "{shown}");
    }

    #[tokio::test]
    async fn a_token_is_reused_until_five_minutes_before_it_expires() {
        let cache = TokenCache::default();
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let fetches = Cell::new(0);
        // Each token fetched is named after the fetch and lasts an hour from `now`.
        let fetch = |now: SystemTime| {
            fetches.set(fetches.get() + 1);
            let secret = format!("token {}", fetches.get());
            async move { Ok::<_, ()>(token(&secret, now + Duration::from_secs(3600))) }
        };
        for (seconds_in, expected) in [
            (0, "token 1"),
            (3600 - 301, "token 1"),
            (3600 - 300, "token 2"),
            (3600 - 300 + 3600 - 301, "token 2"),
        ] {
            let now = start + Duration::from_secs(seconds_in);
            let got = cache.token_at(now, || fetch(now)).await.unwrap();
            assert_eq!(got.secret.expose(), expected, "{seconds_in} s in");
        }
        assert_eq!(fetches.get(), 2);
    }

    #[tokio::test]
    async fn a_failed_fetch_keeps_nothing() {
        let cache = TokenCache::default();
        let failed = cache
            .token(|| async { Err::<AccessToken, _>("refused") })
            .await;
        assert_eq!(failed, Err("refused"));
        let later = SystemTime::now() + Duration::from_secs(3600);
        let got = cache
            .token(|| async { Ok::<_, ()>(token("new", later)) })
            .await
            .unwrap();
        assert_eq!(got.secret.expose(), "new");
    }
}
