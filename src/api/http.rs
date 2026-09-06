//! Outbound HTTP, presented as a real browser.
//!
//! Every request goes out through a [`primp`] client that carries a browser's
//! TLS and HTTP/2 fingerprint and headers. Requests made on behalf of an
//! account use a client built for that account's [`Identity`] and pinned
//! proxy, so one session always looks like one browser on one IP; anything
//! else (media, bootstrap fetches) uses a shared default client.

use std::{
   collections::HashMap,
   error::Error as _,
   fmt::Write as _,
   pin::Pin,
   result::Result as StdResult,
   sync::{
      Arc,
      Mutex,
   },
   task::{
      Context,
      Poll,
   },
   time::Duration,
};

use axum::http::{
   HeaderMap,
   HeaderValue,
   Method,
   StatusCode,
   header,
};
use bytes::Bytes;
use futures_core::Stream;
use http_body::Frame;

use super::browser::{
   self,
   Engine,
   Identity,
};
use crate::error::{
   Error,
   Result,
};

const DEFAULT_BODY_LIMIT: usize = 32 * 1024 * 1024; // 32 MiB
/// Whole API call, connect to last byte.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A media stream may run long; this is the cap on the whole transfer.
const MEDIA_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest silence tolerated mid-body.
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Idle connections are kept this long. A session's connection to X is
/// multiplexed HTTP/2 and pinged, so a dead one is noticed rather than hung on.
const POOL_IDLE: Duration = Duration::from_secs(300);
const POOL_PER_HOST: usize = 4;
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(45);
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
   Http,
   Socks5,
}

/// Parsed proxy configuration.
#[derive(Clone, Debug)]
pub struct ProxyConfig {
   pub host:     String,
   pub port:     u16,
   pub kind:     ProxyKind,
   pub username: Option<String>,
   pub password: Option<String>,
}

impl ProxyConfig {
   /// Proxy URL in the form the client takes. SOCKS5 resolves the origin's
   /// name at the proxy, so X sees the exit's DNS rather than ours.
   #[must_use]
   pub fn url(&self) -> String {
      let scheme = match self.kind {
         ProxyKind::Http => "http",
         ProxyKind::Socks5 => "socks5h",
      };
      let mut url = format!("{scheme}://");
      if let Some(ref user) = self.username {
         url.push_str(&percent_encoding::utf8_percent_encode(
            user,
            percent_encoding::NON_ALPHANUMERIC,
         )
         .to_string());
         if let Some(ref pass) = self.password {
            url.push(':');
            url.push_str(&percent_encoding::utf8_percent_encode(
               pass,
               percent_encoding::NON_ALPHANUMERIC,
            )
            .to_string());
         }
         url.push('@');
      }
      let _ = write!(url, "{}:{}", self.host, self.port);
      url
   }
}

/// Who a request goes out as: the account whose browser it presents, and the
/// proxy that account is pinned to.
#[derive(Clone, Debug)]
pub struct Egress {
   pub session: i64,
   pub proxy:   Option<ProxyConfig>,
}

/// What a client's requests are for, which decides the fetch-context headers
/// a browser would send with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
   /// XHR from x.com's web app to its API.
   Api,
   /// Images and video pulled from X's CDN by a page.
   Media,
}

impl Purpose {
   const fn timeout(self) -> Duration {
      match self {
         Self::Api => REQUEST_TIMEOUT,
         Self::Media => MEDIA_TIMEOUT,
      }
   }
}

/// Browser-impersonating client with one connection pool per account.
#[derive(Clone)]
#[expect(
   clippy::module_name_repetitions,
   reason = "HttpClient is clearer than Client"
)]
pub struct HttpClient {
   purpose:       Purpose,
   default:       primp::Client,
   default_proxy: Option<ProxyConfig>,
   extra_headers: HeaderMap,
   sessions:      Arc<Mutex<HashMap<i64, primp::Client>>>,
}

/// Response wrapper providing convenience methods.
pub struct Response {
   inner: primp::Response,
}

impl HttpClient {
   /// A client for `purpose`, optionally through an HTTP proxy from the
   /// config. Requests without an [`Egress`] present as the default identity:
   /// the newest accepted Chrome on Windows.
   pub fn new(proxy_url: &str, proxy_auth: &str, purpose: Purpose) -> Self {
      let default_proxy = (!proxy_url.is_empty()).then(|| parse_proxy(proxy_url, proxy_auth));
      let extra_headers = HeaderMap::new();
      let default = build_client(
         default_identity(),
         default_proxy.as_ref(),
         purpose,
         &extra_headers,
      )
      .unwrap_or_else(|err| {
         tracing::error!("browser client for the default identity failed to build: {err}");
         // Without impersonation the client is still a working HTTP client.
         primp::Client::builder()
            .redirect(primp::redirect::Policy::none())
            .timeout(purpose.timeout())
            .build()
            .expect("plain HTTP client builds without I/O")
      });
      Self {
         purpose,
         default,
         default_proxy,
         extra_headers,
         sessions: Arc::new(Mutex::new(HashMap::new())),
      }
   }

   /// Headers added to every request this client makes.
   #[must_use]
   pub fn with_default_headers(mut self, headers: HeaderMap) -> Self {
      self.extra_headers = headers;
      self.rebuild_default();
      self
   }

   /// Route requests made without an [`Egress`] through `proxy`.
   #[must_use]
   pub fn with_default_proxy(mut self, proxy: ProxyConfig) -> Self {
      self.default_proxy = Some(proxy);
      self.rebuild_default();
      self
   }

   fn rebuild_default(&mut self) {
      match build_client(
         default_identity(),
         self.default_proxy.as_ref(),
         self.purpose,
         &self.extra_headers,
      ) {
         Ok(client) => self.default = client,
         Err(err) => tracing::error!("browser client for the default identity failed to build: {err}"),
      }
   }

   /// The client for an account, built on first use and kept for the life of
   /// the process, so its connections and fingerprint persist.
   fn client_for(&self, via: Option<&Egress>) -> Result<primp::Client> {
      let Some(egress) = via else {
         return Ok(self.default.clone());
      };
      if let Some(client) = self
         .sessions
         .lock()
         .ok()
         .and_then(|clients| clients.get(&egress.session).cloned())
      {
         return Ok(client);
      }
      let identity = browser::identity_for(egress.session);
      let client = build_client(identity, egress.proxy.as_ref(), self.purpose, &self.extra_headers)?;
      tracing::debug!(session_id = egress.session, %identity, "browser client built");
      if let Ok(mut clients) = self.sessions.lock() {
         clients.entry(egress.session).or_insert_with(|| client.clone());
      }
      Ok(client)
   }

   /// Send a GET request.
   pub async fn get(&self, uri: &str) -> Result<Response> {
      self
         .send_on(Method::GET, uri, &HeaderMap::new(), Bytes::new(), None)
         .await
   }

   /// Send a GET request with additional headers.
   pub async fn get_with_headers(&self, uri: &str, extra_headers: &HeaderMap) -> Result<Response> {
      self
         .send_on(Method::GET, uri, extra_headers, Bytes::new(), None)
         .await
   }

   /// GET as an account, through its pinned proxy.
   pub async fn get_on(
      &self,
      uri: &str,
      extra_headers: &HeaderMap,
      via: Option<&Egress>,
   ) -> Result<Response> {
      self
         .send_on(Method::GET, uri, extra_headers, Bytes::new(), via)
         .await
   }

   /// Send a POST request with additional headers and a body.
   pub async fn post_with_headers(
      &self,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
   ) -> Result<Response> {
      self
         .send_on(Method::POST, uri, extra_headers, body, None)
         .await
   }

   /// POST as an account, through its pinned proxy.
   pub async fn post_on(
      &self,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
      via: Option<&Egress>,
   ) -> Result<Response> {
      self
         .send_on(Method::POST, uri, extra_headers, body, via)
         .await
   }

   /// Send a HEAD request.
   pub async fn head(&self, uri: &str) -> Result<Response> {
      self
         .send_on(Method::HEAD, uri, &HeaderMap::new(), Bytes::new(), None)
         .await
   }

   async fn send_on(
      &self,
      method: Method,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
      via: Option<&Egress>,
   ) -> Result<Response> {
      let client = self.client_for(via)?;
      let send = || {
         let mut request = client
            .request(method.clone(), uri)
            .headers(extra_headers.clone());
         if !body.is_empty() {
            request = request.body(body.clone());
         }
         request.send()
      };
      let inner = match send().await {
         Ok(inner) => inner,
         // A pooled connection the far end closed a moment ago fails on the
         // first write ("broken pipe", "connection reset") rather than with a
         // GOAWAY the client would retry itself. Safe to repeat for requests
         // that change nothing.
         Err(err) if is_idempotent(&method) && is_transport_blip(&err) => {
            tracing::debug!(uri, "retrying after transport error: {}", describe(err));
            send().await.map_err(describe)?
         },
         Err(err) => return Err(describe(err)),
      };
      Ok(Response { inner })
   }
}

/// The browser presented when no account is involved.
fn default_identity() -> Identity {
   Identity {
      engine: Engine::Chrome,
      os:     primp::ImpersonateOS::Windows,
   }
}

fn build_client(
   identity: Identity,
   proxy: Option<&ProxyConfig>,
   purpose: Purpose,
   extra_headers: &HeaderMap,
) -> Result<primp::Client> {
   let mut builder = primp::Client::builder()
      .impersonate(identity.profile())
      .impersonate_os(identity.os)
      // Redirects are the caller's business: t.co resolution reads them.
      .redirect(primp::redirect::Policy::none())
      .timeout(purpose.timeout())
      .read_timeout(BODY_IDLE_TIMEOUT)
      .pool_idle_timeout(POOL_IDLE)
      .pool_max_idle_per_host(POOL_PER_HOST)
      .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
      .http2_keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
      .http2_keep_alive_while_idle(true)
      .tcp_nodelay(true);
   builder = match proxy {
      Some(proxy) => {
         builder.proxy(
            primp::Proxy::all(proxy.url())
               .map_err(|err| Error::Internal(format!("proxy {}:{}: {err}", proxy.host, proxy.port)))?,
         )
      },
      None => builder.no_proxy(),
   };
   let mut client = builder
      .build()
      .map_err(|err| Error::Internal(format!("browser client for {identity}: {err}")))?;
   shape_headers(client.headers_mut(), purpose, extra_headers);
   Ok(client)
}

/// Turn the profile's page-navigation headers into the set a browser sends
/// with the kind of request this client makes.
///
/// The profile ships the headers of a top-level page load. An XHR to the API
/// or an image fetch carries the same identity headers (`user-agent`,
/// `sec-ch-ua*`, `accept-language`, `accept-encoding`) but a different fetch
/// context, and never `upgrade-insecure-requests` or `sec-fetch-user`. Keys
/// are only rewritten where the profile sent them, so Safari, which sends no
/// `sec-fetch-*` at all, stays Safari.
fn shape_headers(headers: &mut HeaderMap, purpose: Purpose, extra: &HeaderMap) {
   headers.remove("upgrade-insecure-requests");
   headers.remove("sec-fetch-user");

   let (accept, dest, mode, site, priority) = match purpose {
      Purpose::Api => ("*/*", "empty", "cors", "same-origin", "u=1, i"),
      Purpose::Media => {
         (
            "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            "image",
            "no-cors",
            "cross-site",
            "u=1, i",
         )
      },
   };
   for (name, value) in [
      (header::ACCEPT, accept),
      (header::HeaderName::from_static("sec-fetch-dest"), dest),
      (header::HeaderName::from_static("sec-fetch-mode"), mode),
      (header::HeaderName::from_static("sec-fetch-site"), site),
      (header::HeaderName::from_static("priority"), priority),
   ] {
      if headers.contains_key(&name) {
         headers.insert(name, HeaderValue::from_static(value));
      }
   }
   if purpose == Purpose::Api {
      headers.insert(header::REFERER, HeaderValue::from_static("https://x.com/"));
   }
   for (name, value) in extra {
      headers.insert(name.clone(), value.clone());
   }
}

const fn is_idempotent(method: &Method) -> bool {
   matches!(*method, Method::GET | Method::HEAD)
}

/// A failure to send at all, as opposed to a timeout, a connect failure or a
/// bad response: the far end dropped a connection we thought was open.
fn is_transport_blip(err: &primp::Error) -> bool {
   err.is_request() && !err.is_timeout() && !err.is_connect() && !err.is_body() && !err.is_decode()
}

/// Error text that keeps the cause, since the client's own `Display` stops at
/// "error sending request".
fn describe(err: primp::Error) -> Error {
   if err.is_timeout() {
      return Error::Internal("HTTP request timed out".into());
   }
   let mut text = if err.is_connect() {
      String::from("connect failed")
   } else {
      err.to_string()
   };
   let mut source = err.source();
   while let Some(cause) = source {
      let _ = write!(text, ": {cause}");
      source = cause.source();
   }
   Error::Internal(text)
}

/// Parse proxy URL (e.g. `http://host:port`) and optional `user:pass` auth.
fn parse_proxy(url: &str, auth: &str) -> ProxyConfig {
   let (kind, stripped) = if let Some(rest) = url
      .strip_prefix("socks5h://")
      .or_else(|| url.strip_prefix("socks5://"))
   {
      (ProxyKind::Socks5, rest)
   } else {
      (
         ProxyKind::Http,
         url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url),
      )
   };
   let (host, port) = if let Some((host_part, port_part)) = stripped.rsplit_once(':') {
      (host_part.to_owned(), port_part.parse().unwrap_or(8080))
   } else {
      (stripped.to_owned(), 8080)
   };
   let (username, password) = match auth.split_once(':') {
      Some((user, pass)) => (Some(user.to_owned()), Some(pass.to_owned())),
      None if auth.is_empty() => (None, None),
      None => (Some(auth.to_owned()), None),
   };
   ProxyConfig {
      host,
      port,
      kind,
      username,
      password,
   }
}

impl Response {
   pub fn status(&self) -> StatusCode {
      self.inner.status()
   }

   pub fn headers(&self) -> &HeaderMap {
      self.inner.headers()
   }

   /// The body as a stream, for media that is relayed rather than read.
   pub fn into_body(self) -> StreamingBody {
      StreamingBody {
         inner: Box::pin(self.inner.bytes_stream()),
      }
   }

   /// Collect the response body as bytes; compression is already undone.
   pub async fn bytes(self) -> Result<Bytes> {
      self.bytes_limited(DEFAULT_BODY_LIMIT).await
   }

   /// Collect the response body as bytes, rejecting oversized bodies.
   pub async fn bytes_limited(mut self, max_bytes: usize) -> Result<Bytes> {
      if let Some(declared) = self.inner.content_length()
         && usize::try_from(declared).is_ok_and(|declared| declared > max_bytes)
      {
         return Err(Error::Internal(format!(
            "response body exceeded {max_bytes} bytes"
         )));
      }
      let mut collected = Vec::new();
      while let Some(chunk) = self.inner.chunk().await.map_err(describe)? {
         if collected.len().saturating_add(chunk.len()) > max_bytes {
            return Err(Error::Internal(format!(
               "response body exceeded {max_bytes} bytes"
            )));
         }
         collected.extend_from_slice(&chunk);
      }
      Ok(Bytes::from(collected))
   }

   /// Collect the response body as a UTF-8 string.
   pub async fn text(self) -> Result<String> {
      let data = self.bytes().await?;
      String::from_utf8(data.into())
         .map_err(|err| Error::Internal(format!("invalid UTF-8: {err}")))
   }
}

/// A response body relayed chunk by chunk.
pub struct StreamingBody {
   inner: Pin<Box<dyn Stream<Item = primp::Result<Bytes>> + Send>>,
}

impl http_body::Body for StreamingBody {
   type Data = Bytes;
   type Error = std::io::Error;

   fn poll_frame(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
   ) -> Poll<Option<StdResult<Frame<Self::Data>, Self::Error>>> {
      match self.inner.as_mut().poll_next(cx) {
         Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
         Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(std::io::Error::other(err)))),
         Poll::Ready(None) => Poll::Ready(None),
         Poll::Pending => Poll::Pending,
      }
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn socks_proxy_url_carries_credentials() {
      let proxy = ProxyConfig {
         host:     "proxy.example".into(),
         port:     10001,
         kind:     ProxyKind::Socks5,
         username: Some("user".into()),
         password: Some("p@ss:word".into()),
      };
      assert_eq!(proxy.url(), "socks5h://user:p%40ss%3Aword@proxy.example:10001");
   }

   #[test]
   fn http_proxy_from_config() {
      let proxy = parse_proxy("http://squid.local:3128", "alice:secret");
      assert_eq!(proxy.kind, ProxyKind::Http);
      assert_eq!(proxy.url(), "http://alice:secret@squid.local:3128");
      let bare = parse_proxy("squid.local", "");
      assert_eq!(bare.url(), "http://squid.local:8080");
   }

   #[test]
   fn api_headers_look_like_an_xhr() {
      let mut headers = HeaderMap::new();
      headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
      headers.insert("upgrade-insecure-requests", HeaderValue::from_static("1"));
      headers.insert("sec-fetch-user", HeaderValue::from_static("?1"));
      headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
      headers.insert("sec-ch-ua", HeaderValue::from_static("\"Chromium\";v=\"151\""));
      shape_headers(&mut headers, Purpose::Api, &HeaderMap::new());
      assert_eq!(headers.get(header::ACCEPT).unwrap(), "*/*");
      assert_eq!(headers.get("sec-fetch-mode").unwrap(), "cors");
      assert!(headers.get("upgrade-insecure-requests").is_none());
      assert!(headers.get("sec-fetch-user").is_none());
      assert_eq!(headers.get("sec-ch-ua").unwrap(), "\"Chromium\";v=\"151\"");
      assert_eq!(headers.get(header::REFERER).unwrap(), "https://x.com/");
   }

   #[test]
   fn safari_gets_no_fetch_metadata_it_never_sends() {
      let mut headers = HeaderMap::new();
      headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
      shape_headers(&mut headers, Purpose::Api, &HeaderMap::new());
      assert!(headers.get("sec-fetch-mode").is_none());
      assert!(headers.get("sec-fetch-dest").is_none());
   }
}
