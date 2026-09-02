//! SOCKS5 exit pool for X API calls.
//!
//! Each cookie session is pinned to one `host:port` so its apparent IP stays
//! stable across restarts. The pin file stores only session id → port; secrets
//! stay in the proxy list.

use std::{
   collections::BTreeMap,
   net::IpAddr,
   path::Path,
   time::Duration,
};

use tokio::{
   fs,
   net::{
      TcpStream,
      lookup_host,
   },
   process::Command,
   time::{
      Instant,
      timeout,
   },
};

use super::http::{
   ProxyConfig,
   ProxyKind,
};
use crate::{
   config::Config,
   error::{
      Error,
      Result,
   },
};

struct Endpoint {
   port:     u16,
   username: String,
   password: String,
}

/// Resolved SOCKS5 endpoints plus the session→port map.
pub struct ProxyPool {
   connect_host: String,
   endpoints:    Vec<Endpoint>,
   pins:         BTreeMap<i64, u16>,
}

impl ProxyPool {
   /// Load `host:port:user:pass` lines, pick the lowest-latency A record, and
   /// restore pins. Returns [`None`] when the file is unset or empty.
   pub async fn load(config: &Config) -> Result<Option<Self>> {
      let path = config.config.socks_proxies_file.trim();
      if path.is_empty() {
         return Ok(None);
      }
      let content = fs::read_to_string(path).await.map_err(|err| {
         Error::InvalidConfig(format!("socksProxiesFile {path:?} unreadable: {err}"))
      })?;
      let mut endpoints = Vec::new();
      let mut hostname = None;
      for (idx, line) in content.lines().enumerate() {
         let line = line.trim();
         if line.is_empty() || line.starts_with('#') {
            continue;
         }
         let (host, port, username, password) = parse_proxy_line(line).ok_or_else(|| {
            Error::InvalidConfig(format!(
               "socksProxiesFile line {}: expected host:port:user:pass",
               idx + 1
            ))
         })?;
         match hostname {
            None => hostname = Some(host),
            Some(ref existing) if existing == &host => {},
            Some(ref existing) => {
               return Err(Error::InvalidConfig(format!(
                  "socksProxiesFile mixes hosts {existing} and {host}; pin one hostname"
               )));
            },
         }
         endpoints.push(Endpoint {
            port,
            username,
            password,
         });
      }
      if endpoints.is_empty() {
         return Ok(None);
      }
      let hostname = hostname.unwrap_or_default();
      let probe_port = endpoints[0].port;
      let connect_host = pick_fastest_host(&hostname, probe_port).await;
      tracing::info!(
         host = %hostname,
         connect = %connect_host,
         endpoints = endpoints.len(),
         "SOCKS5 pool ready"
      );

      let pin_path = session_proxies_path(config);
      let pins = load_pins(&pin_path).await;
      Ok(Some(Self {
         connect_host,
         endpoints,
         pins,
      }))
   }

   /// Assign a stable port to every session and rewrite the pin file.
   pub async fn bind_sessions(mut self, session_ids: &[i64], config: &Config) -> Result<Self> {
      let valid: std::collections::HashSet<u16> =
         self.endpoints.iter().map(|endpoint| endpoint.port).collect();
      self.pins.retain(|_, port| valid.contains(port));

      for id in session_ids {
         if self.pins.contains_key(id) {
            continue;
         }
         let port = least_used_port(&self.endpoints, &self.pins);
         self.pins.insert(*id, port);
      }

      let pin_path = session_proxies_path(config);
      if !pin_path.is_empty() {
         let body = serde_json::to_string_pretty(&self.pins)
            .map_err(|err| Error::Internal(format!("session proxy pins: {err}")))?;
         let tmp = format!("{pin_path}.tmp");
         fs::write(&tmp, body).await?;
         fs::rename(&tmp, &pin_path).await?;
      }
      tracing::info!(
         sessions = session_ids.len(),
         pins = self.pins.len(),
         "SOCKS5 session pins saved"
      );
      Ok(self)
   }

   /// SOCKS5 config for this session, always the same port once pinned.
   #[must_use]
   pub fn for_session(&self, session_id: i64) -> ProxyConfig {
      let port = self.pins.get(&session_id).copied().unwrap_or_else(|| {
         let idx = (session_id.unsigned_abs() as usize) % self.endpoints.len();
         self.endpoints[idx].port
      });
      self.at_port(port)
   }

   /// First endpoint, used for TID bootstrap and other unscoped API fetches.
   #[must_use]
   pub fn first(&self) -> ProxyConfig {
      self.at_port(self.endpoints[0].port)
   }

   fn at_port(&self, port: u16) -> ProxyConfig {
      let endpoint = self
         .endpoints
         .iter()
         .find(|endpoint| endpoint.port == port)
         .unwrap_or(&self.endpoints[0]);
      ProxyConfig {
         host:       self.connect_host.clone(),
         port:       endpoint.port,
         kind:       ProxyKind::Socks5,
         auth:       None,
         socks_user: Some(endpoint.username.clone()),
         socks_pass: Some(endpoint.password.clone()),
      }
   }
}

fn session_proxies_path(config: &Config) -> String {
   let path = config.config.session_proxies_file.trim();
   if path.is_empty() {
      "session-proxies.json".to_owned()
   } else {
      path.to_owned()
   }
}

fn parse_proxy_line(line: &str) -> Option<(String, u16, String, String)> {
   let mut parts = line.splitn(4, ':');
   let host = parts.next()?.to_owned();
   let port = parts.next()?.parse().ok()?;
   let username = parts.next()?.to_owned();
   let password = parts.next()?.to_owned();
   if host.is_empty() || username.is_empty() || password.is_empty() || port == 0 {
      return None;
   }
   Some((host, port, username, password))
}

async fn load_pins(path: &str) -> BTreeMap<i64, u16> {
   if path.is_empty() || !Path::new(path).exists() {
      return BTreeMap::new();
   }
   match fs::read_to_string(path).await {
      Ok(content) => {
         serde_json::from_str(&content).unwrap_or_else(|err| {
            tracing::warn!("Ignoring unreadable session proxy pins: {err}");
            BTreeMap::new()
         })
      },
      Err(_) => BTreeMap::new(),
   }
}

fn least_used_port(endpoints: &[Endpoint], pins: &BTreeMap<i64, u16>) -> u16 {
   let mut counts: BTreeMap<u16, usize> = endpoints.iter().map(|ep| (ep.port, 0)).collect();
   for port in pins.values() {
      if let Some(count) = counts.get_mut(port) {
         *count += 1;
      }
   }
   counts
      .into_iter()
      .min_by_key(|(port, count)| (*count, *port))
      .map(|(port, _)| port)
      .unwrap_or(endpoints[0].port)
}

/// Resolve `hostname` and pick the A record with the lowest ping.
async fn pick_fastest_host(hostname: &str, probe_port: u16) -> String {
   let lookup = format!("{hostname}:{probe_port}");
   let addrs = match lookup_host(&lookup).await {
      Ok(addrs) => addrs,
      Err(err) => {
         tracing::warn!("SOCKS5 DNS for {hostname} failed ({err}); using hostname");
         return hostname.to_owned();
      },
   };
   let mut ips: Vec<IpAddr> = addrs.map(|addr| addr.ip()).collect();
   ips.sort();
   ips.dedup();
   if ips.is_empty() {
      return hostname.to_owned();
   }

   let mut best: Option<(IpAddr, f64)> = None;
   for ip in ips {
      let rtt = probe_ip(ip, probe_port).await;
      tracing::info!(%ip, rtt_ms = ?rtt, "SOCKS5 probe");
      let Some(ms) = rtt else {
         continue;
      };
      match best {
         Some((_, best_ms)) if ms >= best_ms => {},
         _ => best = Some((ip, ms)),
      }
   }
   match best {
      Some((ip, ms)) => {
         tracing::info!(%ip, rtt_ms = ms, "SOCKS5 using lowest-ping address");
         ip.to_string()
      },
      None => hostname.to_owned(),
   }
}

async fn probe_ip(ip: IpAddr, port: u16) -> Option<f64> {
   if let Some(ms) = icmp_rtt_ms(ip).await {
      return Some(ms);
   }
   tcp_rtt_ms(ip, port).await
}

async fn icmp_rtt_ms(ip: IpAddr) -> Option<f64> {
   let output = timeout(
      Duration::from_secs(6),
      Command::new("ping")
         .args(["-c", "3", "-W", "1", "-n", "-q", &ip.to_string()])
         .output(),
   )
   .await
   .ok()?
   .ok()?;
   if !output.status.success() {
      return None;
   }
   let stdout = String::from_utf8_lossy(&output.stdout);
   for line in stdout.lines() {
      if !line.contains("min/avg/max") {
         continue;
      }
      let stats = line.split('=').nth(1)?;
      let avg = stats.split('/').nth(1)?.trim();
      return avg.parse().ok();
   }
   None
}

async fn tcp_rtt_ms(ip: IpAddr, port: u16) -> Option<f64> {
   let start = Instant::now();
   timeout(Duration::from_secs(2), TcpStream::connect((ip, port)))
      .await
      .ok()?
      .ok()?;
   Some(start.elapsed().as_secs_f64() * 1000.0)
}
