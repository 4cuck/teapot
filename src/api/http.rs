use std::{
   collections::{
      HashMap,
      VecDeque,
   },
   fmt::Write as _,
   future::Future as _,
   io::{
      Error as IoError,
      ErrorKind,
      Read as _,
   },
   pin::Pin,
   result::Result as StdResult,
   str,
   sync::{
      Arc,
      Mutex,
   },
   task::{
      Context,
      Poll,
   },
   time::{
      Duration,
      Instant as StdInstant,
   },
};

use axum::http::{
   HeaderMap,
   Method,
   Uri,
   header,
};
use bytes::Bytes;
use flate2::read::GzDecoder;
use http_body_util::{
   BodyExt as _,
   Full,
};
use hyper::{
   StatusCode,
   body::{
      self as hyper_body,
      Frame,
   },
   client::conn::http1,
   http::uri::PathAndQuery,
};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{
   client::legacy::{
      Client,
      connect::HttpConnector,
   },
   rt::{
      TokioExecutor,
      TokioIo,
   },
};
use serde::de::DeserializeOwned;
use tokio::{
   io::{
      AsyncReadExt as _,
      AsyncWriteExt as _,
   },
   net::TcpStream,
   time::{
      Instant,
      Sleep,
      sleep,
      timeout,
   },
};

use crate::error::{
   Error,
   Result,
};

type Connector = hyper_rustls::HttpsConnector<HttpConnector>;

const DEFAULT_BODY_LIMIT: usize = 32 * 1024 * 1024; // 32 MiB
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_TIMEOUT: Duration = Duration::from_secs(60);
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const TUNNEL_IDLE: Duration = Duration::from_secs(45);
const TUNNEL_PER_KEY: usize = 2;

type TunnelSender = http1::SendRequest<Full<Bytes>>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TunnelKey {
   proxy_host:  String,
   proxy_port:  u16,
   origin_host: String,
   origin_port: u16,
}

struct TunnelPool {
   idle: Mutex<HashMap<TunnelKey, VecDeque<(TunnelSender, StdInstant)>>>,
}

impl TunnelPool {
   fn new() -> Arc<Self> {
      Arc::new(Self {
         idle: Mutex::new(HashMap::new()),
      })
   }

   fn take(&self, key: &TunnelKey) -> Option<TunnelSender> {
      let mut map = self.idle.lock().ok()?;
      let queue = map.get_mut(key)?;
      let now = StdInstant::now();
      while let Some((sender, since)) = queue.pop_front() {
         if sender.is_closed() || now.saturating_duration_since(since) > TUNNEL_IDLE {
            continue;
         }
         return Some(sender);
      }
      None
   }

   fn put(&self, key: TunnelKey, sender: TunnelSender) {
      if sender.is_closed() {
         return;
      }
      let Ok(mut map) = self.idle.lock() else {
         return;
      };
      let queue = map.entry(key).or_default();
      while queue.len() >= TUNNEL_PER_KEY {
         queue.pop_front();
      }
      queue.push_back((sender, StdInstant::now()));
   }
}

enum ResponseBody {
   Incoming(hyper_body::Incoming),
   Buffered(Bytes),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
   Http,
   Socks5,
}

/// Parsed proxy configuration.
#[derive(Clone)]
pub struct ProxyConfig {
   pub host:       String,
   pub port:       u16,
   pub kind:       ProxyKind,
   pub auth:       Option<String>,
   pub socks_user: Option<String>,
   pub socks_pass: Option<String>,
}

/// Lightweight HTTP client wrapping hyper-util's connection-pooling client.
///
/// When a proxy is configured, HTTPS requests are tunneled via HTTP CONNECT.
#[derive(Clone)]
#[expect(
   clippy::module_name_repetitions,
   reason = "HttpClient is clearer than Client"
)]
pub struct HttpClient {
   inner:           Client<Connector, Full<Bytes>>,
   default_headers: HeaderMap,
   proxy:           Option<ProxyConfig>,
   tls:             Arc<rustls::ClientConfig>,
   tunnels:         Arc<TunnelPool>,
}

/// Response wrapper providing convenience methods.
pub struct Response {
   status:  StatusCode,
   headers: HeaderMap,
   body:    ResponseBody,
}

/// Response body with both an overall deadline and an idle-chunk deadline.
pub struct TimedBody {
   inner:    hyper_body::Incoming,
   deadline: Pin<Box<Sleep>>,
   idle:     Pin<Box<Sleep>>,
   done:     bool,
}

impl TimedBody {
   fn new(inner: hyper_body::Incoming) -> Self {
      Self {
         inner,
         deadline: Box::pin(sleep(BODY_TIMEOUT)),
         idle: Box::pin(sleep(BODY_IDLE_TIMEOUT)),
         done: false,
      }
   }
}

impl hyper_body::Body for TimedBody {
   type Data = Bytes;
   type Error = IoError;

   fn poll_frame(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
   ) -> Poll<Option<StdResult<Frame<Self::Data>, Self::Error>>> {
      if self.done {
         return Poll::Ready(None);
      }
      if self.deadline.as_mut().poll(cx).is_ready() {
         self.done = true;
         return Poll::Ready(Some(Err(IoError::new(
            ErrorKind::TimedOut,
            "response body deadline exceeded",
         ))));
      }
      if self.idle.as_mut().poll(cx).is_ready() {
         self.done = true;
         return Poll::Ready(Some(Err(IoError::new(
            ErrorKind::TimedOut,
            "response body stalled",
         ))));
      }

      match Pin::new(&mut self.inner).poll_frame(cx) {
         Poll::Ready(Some(Ok(frame))) => {
            self.idle.as_mut().reset(Instant::now() + BODY_IDLE_TIMEOUT);
            Poll::Ready(Some(Ok(frame)))
         },
         Poll::Ready(Some(Err(err))) => {
            self.done = true;
            Poll::Ready(Some(Err(IoError::other(err))))
         },
         Poll::Ready(None) => {
            self.done = true;
            Poll::Ready(None)
         },
         Poll::Pending => Poll::Pending,
      }
   }

   fn is_end_stream(&self) -> bool {
      self.done || self.inner.is_end_stream()
   }

   fn size_hint(&self) -> hyper_body::SizeHint {
      self.inner.size_hint()
   }
}

impl HttpClient {
   pub fn new(proxy_url: &str, proxy_auth: &str) -> Self {
      let roots = rustls_native_certs::load_native_certs()
         .certs
         .into_iter()
         .fold(rustls::RootCertStore::empty(), |mut store, cert| {
            let _ = store.add(cert);
            store
         });

      let mut tls_config = rustls::ClientConfig::builder()
         .with_root_certificates(roots.clone())
         .with_no_client_auth();
      tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];
      tls_config.resumption = rustls::client::Resumption::in_memory_sessions(512);
      let tls = Arc::new(tls_config);

      let connector = HttpsConnectorBuilder::new()
         .with_tls_config(
            rustls::ClientConfig::builder()
               .with_root_certificates(roots)
               .with_no_client_auth(),
         )
         .https_or_http()
         .enable_http1()
         .build();

      let inner = Client::builder(TokioExecutor::new())
         .pool_idle_timeout(Duration::from_secs(90))
         .build(connector);

      let proxy = if proxy_url.is_empty() {
         None
      } else {
         Some(parse_proxy(proxy_url, proxy_auth))
      };

      Self {
         inner,
         default_headers: HeaderMap::new(),
         proxy,
         tls,
         tunnels: TunnelPool::new(),
      }
   }

   /// Create a client with default headers applied to every request.
   pub fn with_default_headers(mut self, headers: HeaderMap) -> Self {
      self.default_headers = headers;
      self
   }

   /// Override the default proxy for this client.
   #[must_use]
   pub fn with_default_proxy(mut self, proxy: ProxyConfig) -> Self {
      self.proxy = Some(proxy);
      self
   }

   /// Send a request with a given method, optional extra headers, and body.
   async fn send(
      &self,
      method: Method,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
   ) -> Result<Response> {
      self
         .send_on(method, uri, extra_headers, body, None)
         .await
   }

   async fn send_on(
      &self,
      method: Method,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
      via: Option<&ProxyConfig>,
   ) -> Result<Response> {
      timeout(REQUEST_TIMEOUT, async {
         if let Some(proxy) = via.or(self.proxy.as_ref()) {
            self
               .send_via_proxy(proxy, method, uri, extra_headers, body)
               .await
         } else {
            self.send_direct(method, uri, extra_headers, body).await
         }
      })
      .await
      .map_err(|_| Error::Internal("HTTP request timed out".into()))?
   }

   /// Direct request through hyper's connection pool.
   async fn send_direct(
      &self,
      method: Method,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
   ) -> Result<Response> {
      let parsed: Uri = uri
         .parse()
         .map_err(|err| Error::Internal(format!("invalid URI: {err}")))?;

      let mut builder = hyper::Request::builder().method(method).uri(parsed);
      for (key, value) in &self.default_headers {
         builder = builder.header(key, value);
      }
      for (key, value) in extra_headers {
         builder = builder.header(key, value);
      }

      let request = builder
         .body(Full::new(body))
         .map_err(|err| Error::Internal(format!("build request: {err}")))?;

      let resp = self
         .inner
         .request(request)
         .await
         .map_err(|err| Error::Internal(format!("HTTP request failed: {err}")))?;

      let (parts, body) = resp.into_parts();
      Ok(Response {
         status:  parts.status,
         headers: parts.headers,
         body:    ResponseBody::Incoming(body),
      })
   }

   /// Send request through HTTP CONNECT proxy tunnel.
   async fn send_via_proxy(
      &self,
      proxy: &ProxyConfig,
      method: Method,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
   ) -> Result<Response> {
      let parsed: Uri = uri
         .parse()
         .map_err(|err| Error::Internal(format!("invalid URI: {err}")))?;

      let target_host = parsed
         .host()
         .ok_or_else(|| Error::Internal("no host in URI".into()))?;
      let target_port = parsed.port_u16().unwrap_or_else(|| {
         if parsed.scheme_str() == Some("https") {
            443
         } else {
            80
         }
      });
      let is_https = parsed.scheme_str() == Some("https");
      let path_and_query = parsed.path_and_query().map_or("/", PathAndQuery::as_str);
      let key = TunnelKey {
         proxy_host:  proxy.host.clone(),
         proxy_port:  proxy.port,
         origin_host: target_host.to_owned(),
         origin_port: target_port,
      };

      if (proxy.kind == ProxyKind::Socks5 || is_https)
         && let Some(sender) = self.tunnels.take(&key)
      {
         match self
            .proxy_http1(
               sender,
               method.clone(),
               path_and_query,
               target_host,
               extra_headers,
               body.clone(),
               None,
            )
            .await
         {
            Ok((resp, sender)) => {
               self.tunnels.put(key, sender);
               return Ok(resp);
            },
            Err(err) => {
               tracing::debug!("idle proxy tunnel dropped: {err}");
            },
         }
      }

      // TCP connect to proxy
      let mut stream = TcpStream::connect((&*proxy.host, proxy.port))
         .await
         .map_err(|err| Error::Internal(format!("proxy connect: {err}")))?;
      let _ = stream.set_nodelay(true);

      if proxy.kind == ProxyKind::Socks5 {
         socks5_connect(&mut stream, proxy, target_host, target_port).await?;
         return self
            .send_origin_request(stream, is_https, &parsed, method, extra_headers, body, key)
            .await;
      }

      if is_https {
         // CONNECT handshake
         let mut connect_req = format!(
            "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
         );
         if let Some(ref auth) = proxy.auth {
            let _ = write!(connect_req, "Proxy-Authorization: Basic {auth}\r\n");
         }
         connect_req.push_str("\r\n");

         stream
            .write_all(connect_req.as_bytes())
            .await
            .map_err(|err| Error::Internal(format!("proxy CONNECT write: {err}")))?;

         // Read the CONNECT response and look for the end of its HTTP headers
         let mut buf = vec![0_u8; 4096];
         let mut filled = 0;
         loop {
            let n = stream
               .read(&mut buf[filled..])
               .await
               .map_err(|err| Error::Internal(format!("proxy CONNECT read: {err}")))?;
            if n == 0 {
               return Err(Error::Internal("proxy closed during CONNECT".into()));
            }
            filled += n;
            if filled >= 4 && buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
               break;
            }
            if filled >= buf.len() {
               return Err(Error::Internal("proxy CONNECT response too large".into()));
            }
         }

         let response_line = str::from_utf8(&buf[..filled])
            .map_err(|_| Error::Internal("proxy CONNECT: invalid UTF-8".into()))?;
         if !response_line.starts_with("HTTP/1.1 200") && !response_line.starts_with("HTTP/1.0 200")
         {
            let first_line = response_line.lines().next().unwrap_or("(empty)");
            return Err(Error::Internal(format!(
               "proxy CONNECT rejected: {first_line}"
            )));
         }

         self
            .send_origin_request(stream, true, &parsed, method, extra_headers, body, key)
            .await
      } else {
         let _ = key;
         // Send plain HTTP proxy requests with an absolute URI
         let (mut sender, conn) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|err| Error::Internal(format!("proxy HTTP handshake: {err}")))?;

         tokio::spawn(async move {
            if let Err(err) = conn.await {
               tracing::debug!("proxy connection closed: {err}");
            }
         });

         let mut builder = hyper::Request::builder()
            .method(method)
            .uri(uri) // absolute URI for HTTP proxy
            .header(header::HOST, target_host);

         if let Some(ref auth) = proxy.auth {
            builder = builder.header("Proxy-Authorization", format!("Basic {auth}"));
         }
         for (key, value) in &self.default_headers {
            builder = builder.header(key, value);
         }
         for (key, value) in extra_headers {
            builder = builder.header(key, value);
         }

         let request = builder
            .body(Full::new(body))
            .map_err(|err| Error::Internal(format!("build proxied request: {err}")))?;

         let resp = sender
            .send_request(request)
            .await
            .map_err(|err| Error::Internal(format!("proxied request failed: {err}")))?;

         let (parts, body) = resp.into_parts();
         Ok(Response {
            status:  parts.status,
            headers: parts.headers,
            body:    ResponseBody::Incoming(body),
         })
      }
   }

   /// HTTP/1.1 on an already-tunneled stream (CONNECT or SOCKS5).
   async fn send_origin_request(
      &self,
      stream: TcpStream,
      is_https: bool,
      parsed: &Uri,
      method: Method,
      extra_headers: &HeaderMap,
      body: Bytes,
      key: TunnelKey,
   ) -> Result<Response> {
      let target_host = parsed
         .host()
         .ok_or_else(|| Error::Internal("no host in URI".into()))?;
      let path_and_query = parsed.path_and_query().map_or("/", PathAndQuery::as_str);

      let sender = if is_https {
         let server_name = rustls::pki_types::ServerName::try_from(target_host.to_owned())
            .map_err(|err| Error::Internal(format!("invalid server name: {err}")))?;
         let tls_connector = tokio_rustls::TlsConnector::from(Arc::clone(&self.tls));
         let tls_stream = tls_connector
            .connect(server_name, stream)
            .await
            .map_err(|err| Error::Internal(format!("proxy TLS handshake: {err}")))?;
         Self::http1_handshake(TokioIo::new(tls_stream)).await?
      } else {
         Self::http1_handshake(TokioIo::new(stream)).await?
      };
      let (resp, sender) = self
         .proxy_http1(
            sender,
            method,
            path_and_query,
            target_host,
            extra_headers,
            body,
            None,
         )
         .await?;
      self.tunnels.put(key, sender);
      Ok(resp)
   }

   async fn http1_handshake<IO>(io: TokioIo<IO>) -> Result<TunnelSender>
   where
      IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
   {
      let (sender, conn) = http1::handshake(io)
         .await
         .map_err(|err| Error::Internal(format!("proxy HTTP handshake: {err}")))?;
      tokio::spawn(async move {
         if let Err(err) = conn.await {
            tracing::debug!("proxy connection closed: {err}");
         }
      });
      Ok(sender)
   }

   #[expect(
      clippy::too_many_arguments,
      reason = "mirrors the origin HTTP/1.1 request"
   )]
   async fn proxy_http1(
      &self,
      mut sender: TunnelSender,
      method: Method,
      uri: &str,
      host: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
      proxy_auth: Option<&str>,
   ) -> Result<(Response, TunnelSender)> {
      let mut builder = hyper::Request::builder()
         .method(method)
         .uri(uri)
         .header(header::HOST, host)
         .header(header::CONNECTION, "keep-alive");
      if let Some(auth) = proxy_auth {
         builder = builder.header("Proxy-Authorization", format!("Basic {auth}"));
      }
      for (key, value) in &self.default_headers {
         builder = builder.header(key, value);
      }
      for (key, value) in extra_headers {
         builder = builder.header(key, value);
      }
      let request = builder
         .body(Full::new(body))
         .map_err(|err| Error::Internal(format!("build proxied request: {err}")))?;
      let resp = sender
         .send_request(request)
         .await
         .map_err(|err| Error::Internal(format!("proxied request failed: {err}")))?;
      let (parts, body) = resp.into_parts();
      let collected = timeout(BODY_TIMEOUT, body.collect())
         .await
         .map_err(|_| Error::Internal("response body deadline exceeded".into()))?
         .map_err(|err| Error::Internal(format!("read body: {err}")))?;
      let buf = collected.to_bytes();
      if buf.len() > DEFAULT_BODY_LIMIT {
         return Err(Error::Internal(format!(
            "response body exceeded {DEFAULT_BODY_LIMIT} bytes"
         )));
      }
      Ok((
         Response {
            status:  parts.status,
            headers: parts.headers,
            body:    ResponseBody::Buffered(buf),
         },
         sender,
      ))
   }

   /// Send a GET request.
   pub async fn get(&self, uri: &str) -> Result<Response> {
      self
         .send(Method::GET, uri, &HeaderMap::new(), Bytes::new())
         .await
   }

   /// Send a GET request with additional headers.
   pub async fn get_with_headers(&self, uri: &str, extra_headers: &HeaderMap) -> Result<Response> {
      self
         .send(Method::GET, uri, extra_headers, Bytes::new())
         .await
   }

   /// GET through an explicit proxy (session-pinned SOCKS5).
   pub async fn get_on(
      &self,
      uri: &str,
      extra_headers: &HeaderMap,
      via: Option<&ProxyConfig>,
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
      self.send(Method::POST, uri, extra_headers, body).await
   }

   /// POST through an explicit proxy (session-pinned SOCKS5).
   pub async fn post_on(
      &self,
      uri: &str,
      extra_headers: &HeaderMap,
      body: Bytes,
      via: Option<&ProxyConfig>,
   ) -> Result<Response> {
      self
         .send_on(Method::POST, uri, extra_headers, body, via)
         .await
   }

   /// Send a HEAD request.
   pub async fn head(&self, uri: &str) -> Result<Response> {
      self
         .send(Method::HEAD, uri, &HeaderMap::new(), Bytes::new())
         .await
   }
}

/// SOCKS5 CONNECT with username/password auth (RFC 1928 + 1929).
async fn socks5_connect(
   stream: &mut TcpStream,
   proxy: &ProxyConfig,
   target_host: &str,
   target_port: u16,
) -> Result<()> {
   let user = proxy.socks_user.as_deref().unwrap_or("");
   let pass = proxy.socks_pass.as_deref().unwrap_or("");
   if user.len() > 255 || pass.len() > 255 {
      return Err(Error::Internal("SOCKS5 credentials too long".into()));
   }

   stream
      .write_all(&[0x05, 0x01, 0x02])
      .await
      .map_err(|err| Error::Internal(format!("SOCKS5 greeting: {err}")))?;
   let mut method = [0_u8; 2];
   stream
      .read_exact(&mut method)
      .await
      .map_err(|err| Error::Internal(format!("SOCKS5 greeting read: {err}")))?;
   if method[0] != 0x05 || method[1] != 0x02 {
      return Err(Error::Internal(format!(
         "SOCKS5 auth method rejected: {method:?}"
      )));
   }

   let mut auth = Vec::with_capacity(3 + user.len() + pass.len());
   auth.push(0x01);
   auth.push(u8::try_from(user.len()).unwrap_or(0));
   auth.extend_from_slice(user.as_bytes());
   auth.push(u8::try_from(pass.len()).unwrap_or(0));
   auth.extend_from_slice(pass.as_bytes());
   stream
      .write_all(&auth)
      .await
      .map_err(|err| Error::Internal(format!("SOCKS5 auth: {err}")))?;
   let mut auth_reply = [0_u8; 2];
   stream
      .read_exact(&mut auth_reply)
      .await
      .map_err(|err| Error::Internal(format!("SOCKS5 auth read: {err}")))?;
   if auth_reply[1] != 0 {
      return Err(Error::Internal(format!(
         "SOCKS5 authentication failed: {}",
         auth_reply[1]
      )));
   }

   let host = target_host.as_bytes();
   if host.len() > 255 {
      return Err(Error::Internal("SOCKS5 target hostname too long".into()));
   }
   let mut req = Vec::with_capacity(7 + host.len());
   req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03]);
   req.push(u8::try_from(host.len()).unwrap_or(0));
   req.extend_from_slice(host);
   req.extend_from_slice(&target_port.to_be_bytes());
   stream
      .write_all(&req)
      .await
      .map_err(|err| Error::Internal(format!("SOCKS5 connect: {err}")))?;

   let mut hdr = [0_u8; 4];
   stream
      .read_exact(&mut hdr)
      .await
      .map_err(|err| Error::Internal(format!("SOCKS5 reply: {err}")))?;
   if hdr[0] != 0x05 || hdr[1] != 0 {
      return Err(Error::Internal(format!(
         "SOCKS5 connect failed: status {}",
         hdr[1]
      )));
   }
   let skip = match hdr[3] {
      1 => 4 + 2,
      4 => 16 + 2,
      3 => {
         let mut len = [0_u8; 1];
         stream
            .read_exact(&mut len)
            .await
            .map_err(|err| Error::Internal(format!("SOCKS5 bind addr: {err}")))?;
         usize::from(len[0]) + 2
      },
      atyp => {
         return Err(Error::Internal(format!(
            "SOCKS5 unknown address type {atyp}"
         )));
      },
   };
   let mut rest = vec![0_u8; skip];
   stream
      .read_exact(&mut rest)
      .await
      .map_err(|err| Error::Internal(format!("SOCKS5 bind addr: {err}")))?;
   Ok(())
}

/// Parse proxy URL (e.g. `http://host:port`) and optional `user:pass` auth.
fn parse_proxy(url: &str, auth: &str) -> ProxyConfig {
   let stripped = url
      .strip_prefix("https://")
      .or_else(|| url.strip_prefix("http://"))
      .unwrap_or(url);
   let (host, port) = if let Some((host_part, port_part)) = stripped.rsplit_once(':') {
      (host_part.to_owned(), port_part.parse().unwrap_or(8080))
   } else {
      (stripped.to_owned(), 8080)
   };
   let auth = if auth.is_empty() {
      None
   } else {
      Some(data_encoding::BASE64.encode(auth.as_bytes()))
   };
   ProxyConfig {
      host,
      port,
      kind: ProxyKind::Http,
      auth,
      socks_user: None,
      socks_pass: None,
   }
}

impl Response {
   pub const fn status(&self) -> StatusCode {
      self.status
   }

   pub const fn headers(&self) -> &HeaderMap {
      &self.headers
   }

   pub fn into_body(self) -> TimedBody {
      match self.body {
         ResponseBody::Incoming(body) => TimedBody::new(body),
         ResponseBody::Buffered(_) => {
            panic!("into_body is for streamed media responses, not proxied API calls")
         },
      }
   }

   /// Collect the response body as bytes, decompressing gzip if needed.
   pub async fn bytes(self) -> Result<Bytes> {
      self.bytes_limited(DEFAULT_BODY_LIMIT).await
   }

   /// Collect the response body as bytes, rejecting oversized bodies.
   pub async fn bytes_limited(self, max_bytes: usize) -> Result<Bytes> {
      let is_gzip = self
         .headers
         .get(header::CONTENT_ENCODING)
         .and_then(|val| val.to_str().ok())
         .is_some_and(|val| val.contains("gzip"));

      let collected = match self.body {
         ResponseBody::Buffered(bytes) => bytes,
         ResponseBody::Incoming(incoming) => {
            let mut body = TimedBody::new(incoming);
            let mut collected = Vec::new();
            while let Some(frame) = body.frame().await {
               let frame = frame.map_err(|err| Error::Internal(format!("read body: {err}")))?;
               let Ok(chunk) = frame.into_data() else {
                  continue;
               };
               if collected.len().saturating_add(chunk.len()) > max_bytes {
                  return Err(Error::Internal(format!(
                     "response body exceeded {max_bytes} bytes"
                  )));
               }
               collected.extend_from_slice(&chunk);
            }
            Bytes::from(collected)
         },
      };

      if collected.len() > max_bytes {
         return Err(Error::Internal(format!(
            "response body exceeded {max_bytes} bytes"
         )));
      }

      if is_gzip {
         let gz = GzDecoder::new(collected.as_ref());
         let mut decoded = Vec::new();
         let mut limited = gz.take(max_bytes.saturating_add(1) as u64);
         limited
            .read_to_end(&mut decoded)
            .map_err(|err| Error::Internal(format!("gzip decode: {err}")))?;
         if decoded.len() > max_bytes {
            return Err(Error::Internal(format!(
               "decoded response body exceeded {max_bytes} bytes"
            )));
         }
         Ok(Bytes::from(decoded))
      } else {
         Ok(collected)
      }
   }

   /// Collect the response body as a UTF-8 string.
   pub async fn text(self) -> Result<String> {
      let data = self.bytes().await?;
      String::from_utf8(data.to_vec())
         .map_err(|err| Error::Internal(format!("invalid UTF-8: {err}")))
   }

   /// Deserialize the response body as JSON.
   pub async fn json<T>(self) -> Result<T>
   where
      T: DeserializeOwned,
   {
      let data = self.bytes().await?;
      serde_json::from_slice(&data).map_err(Into::into)
   }
}
