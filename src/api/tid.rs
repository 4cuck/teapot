//! Transaction ID generation for Twitter/X API requests.
//!
//! Uses the `xitter-txid` crate to generate client transaction IDs
//! matching what the X web app sends. This is required for cookie-based
//! sessions to get full API responses (e.g., conversation-grouped entries
//! in `UserTweetsAndReplies`).

use std::{
   collections::HashMap,
   sync::Arc,
   time::{
      Duration,
      Instant,
   },
};

use axum::http::{
   HeaderMap,
   header,
};
use tokio::sync::{
   Mutex,
   RwLock,
};
use xitter_txid::transaction::ClientTransaction;

use super::{
   ProxyPool,
   auth::SessionPool,
   http::{
      Egress,
      HttpClient,
   },
};

/// Cached transaction ID client that refreshes periodically.
#[derive(Clone)]
pub struct TidClient {
   inner:      Arc<RwLock<Option<ClientTransaction>>>,
   http:       HttpClient,
   sessions:   SessionPool,
   proxies:    Option<Arc<ProxyPool>>,
   last_fetch: Arc<Mutex<Instant>>,
   refused:    Arc<RwLock<HashMap<String, Instant>>>,
}

/// How often to refresh the TID client.
const REFRESH_INTERVAL: Duration = Duration::from_hours(1);

/// How long to wait before retrying a failed bootstrap.
const RETRY_INTERVAL: Duration = Duration::from_mins(5);

/// How long an endpoint keeps being asked without a transaction ID after X
/// refused one. Short enough that the endpoint recovers on its own once X
/// starts accepting IDs again.
const REFUSAL_INTERVAL: Duration = Duration::from_mins(10);

impl TidClient {
   pub fn new(http: HttpClient, sessions: SessionPool, proxies: Option<Arc<ProxyPool>>) -> Self {
      Self {
         inner: Arc::new(RwLock::new(None)),
         http,
         sessions,
         proxies,
         last_fetch: Arc::new(Mutex::new(
            Instant::now().checked_sub(REFRESH_INTERVAL).unwrap(),
         )),
         refused: Arc::new(RwLock::new(HashMap::new())),
      }
   }

   /// Generate a transaction ID for a request path, or [`None`] if TID is
   /// unavailable.
   pub async fn generate(&self, path: &str) -> Option<String> {
      self.generate_for("GET", path).await
   }

   /// Same as [`Self::generate`], with an explicit HTTP method. X hashes the
   /// method into the transaction ID, so POSTs must not reuse the GET variant.
   pub async fn generate_for(&self, method: &str, path: &str) -> Option<String> {
      if self.is_refused(method, path).await {
         return None;
      }
      self.ensure_fresh().await;
      let guard = self.inner.read().await;
      guard
         .as_ref()
         .map(|ct| ct.generate_transaction_id(method, path))
   }

   /// Record that X answered this request with a bodyless 404, which is how it
   /// refuses a transaction ID it will not accept.
   pub async fn note_refused(&self, method: &str, path: &str) {
      self
         .refused
         .write()
         .await
         .insert(Self::refusal_key(method, path), Instant::now());
   }

   async fn is_refused(&self, method: &str, path: &str) -> bool {
      let key = Self::refusal_key(method, path);
      let refused_at = self.refused.read().await.get(&key).copied();
      match refused_at {
         Some(at) if at.elapsed() < REFUSAL_INTERVAL => true,
         Some(_) => {
            // Expired entries are dropped here so the map cannot grow forever.
            // Only an expired entry takes the write lock; the common case of no
            // entry at all stays a shared read.
            self.refused.write().await.remove(&key);
            false
         },
         None => false,
      }
   }

   fn refusal_key(method: &str, path: &str) -> String {
      format!("{method} {path}")
   }

   /// Refresh the TID client if stale. Uses `try_lock` so only one task
   /// performs the refresh. Concurrent callers skip it and use the existing
   /// (possibly stale) client.
   ///
   /// Only the very first caller waits for the bootstrap, since there is
   /// nothing else to serve. Once a client exists, the hourly refresh runs in
   /// the background so no request pays for two page fetches.
   async fn ensure_fresh(&self) {
      let Ok(mut last) = self.last_fetch.try_lock() else {
         return; // another task is already refreshing
      };

      if last.elapsed() < REFRESH_INTERVAL {
         return;
      }

      if self.inner.read().await.is_none() {
         let outcome = self.fetch_client().await;
         Self::record_refresh(&mut last, &outcome);
         if let Ok(ct) = outcome {
            *self.inner.write().await = Some(ct);
         }
         return;
      }

      // Claim the window before spawning so concurrent callers do not each
      // start a refresh; a failure rewinds it to the retry interval.
      *last = Instant::now();
      drop(last);
      let this = self.clone();
      tokio::spawn(async move {
         let outcome = this.fetch_client().await;
         let mut last = this.last_fetch.lock().await;
         Self::record_refresh(&mut last, &outcome);
         if let Ok(ct) = outcome {
            *this.inner.write().await = Some(ct);
         }
      });
   }

   fn record_refresh(last: &mut Instant, outcome: &Result<ClientTransaction, String>) {
      match outcome {
         Ok(_) => {
            *last = Instant::now();
            tracing::info!("TID client refreshed");
         },
         Err(err) => {
            tracing::warn!("Failed to refresh TID client: {err}");
            // Back off, or a persistent failure re-bootstraps on every request.
            *last = Instant::now()
               .checked_sub(REFRESH_INTERVAL.saturating_sub(RETRY_INTERVAL))
               .unwrap_or_else(Instant::now);
         },
      }
   }

   /// Fetch the x.com homepage and ondemand JS to create a new
   /// [`ClientTransaction`].
   ///
   /// This is a page load, so it carries navigation fetch metadata rather than
   /// the API client's XHR defaults, and it goes out as the browser of the
   /// account whose cookie it uses. Client hints and user agent stay with that
   /// browser profile.
   async fn fetch_client(&self) -> Result<ClientTransaction, String> {
      let mut headers = HeaderMap::new();
      headers.insert(
         header::ACCEPT,
         header::HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/\
             apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
         ),
      );
      headers.insert(
         "sec-fetch-dest",
         header::HeaderValue::from_static("document"),
      );
      headers.insert(
         "sec-fetch-mode",
         header::HeaderValue::from_static("navigate"),
      );
      headers.insert("sec-fetch-site", header::HeaderValue::from_static("none"));
      headers.insert("sec-fetch-user", header::HeaderValue::from_static("?1"));
      headers.insert(
         "upgrade-insecure-requests",
         header::HeaderValue::from_static("1"),
      );
      headers.insert("priority", header::HeaderValue::from_static("u=0, i"));

      // Logged-out visitors get a stripped shell built from a different bundle
      // that carries no chunk manifest, so the bootstrap needs a session cookie
      // to reach the client-web app the transaction ID is derived from.
      let (session_id, cookie) = self
         .sessions
         .cookie_header()
         .ok_or("no cookie session available for TID bootstrap")?;
      headers.insert(
         header::COOKIE,
         cookie
            .parse()
            .map_err(|_| "invalid cookie header value".to_owned())?,
      );
      let egress = Egress {
         session: session_id,
         proxy:   self
            .proxies
            .as_ref()
            .map(|pool| pool.for_session(session_id)),
      };

      let home_html = self
         .http
         .get_on("https://x.com", &headers, Some(&egress))
         .await
         .map_err(|err| format!("fetch x.com: {err}"))?
         .text()
         .await
         .map_err(|err| format!("read x.com body: {err}"))?;

      let js_url = ClientTransaction::extract_ondemand_url(&home_html)
         .map_err(|err| format!("extract ondemand URL: {err}"))?;

      // The script is a subresource, fetched cross-site from the CDN.
      headers.remove(header::COOKIE);
      headers.remove("sec-fetch-user");
      headers.remove("upgrade-insecure-requests");
      headers.insert(header::ACCEPT, header::HeaderValue::from_static("*/*"));
      headers.insert("sec-fetch-dest", header::HeaderValue::from_static("script"));
      headers.insert("sec-fetch-mode", header::HeaderValue::from_static("no-cors"));
      headers.insert(
         "sec-fetch-site",
         header::HeaderValue::from_static("cross-site"),
      );
      headers.insert("priority", header::HeaderValue::from_static("u=1"));
      headers.insert(
         header::REFERER,
         header::HeaderValue::from_static("https://x.com/"),
      );
      let js_text = self
         .http
         .get_on(&js_url, &headers, Some(&egress))
         .await
         .map_err(|err| format!("fetch ondemand JS: {err}"))?
         .text()
         .await
         .map_err(|err| format!("read ondemand JS body: {err}"))?;

      ClientTransaction::new(&home_html, &js_text)
         .map_err(|err| format!("create TID client: {err}"))
   }
}
