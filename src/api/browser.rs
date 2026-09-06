//! The browser each account pretends to be.
//!
//! Every cookie session presents one real browser's TLS and HTTP/2 fingerprint
//! and the headers to match. The engine is drawn from the session id, so an
//! account keeps the same browser across restarts the way it keeps its proxy
//! port, and the mix is weighted like real desktop traffic rather than
//! uniform. Within an engine the newest version X still accepts is used; that
//! is found by a handshake at startup, since a profile can advertise something
//! X's edge rejects (Chrome 152 did at the time of writing).

use std::{
   collections::HashMap,
   fmt,
   sync::OnceLock,
   time::Duration,
};

use primp::{
   Impersonate,
   ImpersonateOS,
};
use tokio::task::JoinSet;

use super::http::ProxyConfig;

/// Browser families a session can present as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
   Chrome,
   Edge,
   Firefox,
   Safari,
   Opera,
}

impl Engine {
   const ALL: [Self; 5] = [
      Self::Chrome,
      Self::Edge,
      Self::Firefox,
      Self::Safari,
      Self::Opera,
   ];

   /// Profiles newest first. Only the newest that X accepts is ever used.
   fn candidates(self) -> &'static [Impersonate] {
      match self {
         Self::Chrome => {
            &[
               Impersonate::ChromeV152,
               Impersonate::ChromeV151,
               Impersonate::ChromeV150,
               Impersonate::ChromeV149,
               Impersonate::ChromeV148,
               Impersonate::ChromeV147,
               Impersonate::ChromeV146,
            ]
         },
         Self::Edge => {
            &[
               Impersonate::EdgeV151,
               Impersonate::EdgeV150,
               Impersonate::EdgeV149,
               Impersonate::EdgeV148,
               Impersonate::EdgeV147,
               Impersonate::EdgeV146,
            ]
         },
         Self::Firefox => {
            &[
               Impersonate::FirefoxV151,
               Impersonate::FirefoxV150,
               Impersonate::FirefoxV149,
               Impersonate::FirefoxV148,
               Impersonate::FirefoxV147,
               Impersonate::FirefoxV146,
            ]
         },
         Self::Safari => {
            &[
               Impersonate::SafariV26_4,
               Impersonate::SafariV26_3,
               Impersonate::SafariV26,
               Impersonate::SafariV18_5,
            ]
         },
         Self::Opera => {
            &[
               Impersonate::OperaV135,
               Impersonate::OperaV134,
               Impersonate::OperaV133,
               Impersonate::OperaV132,
            ]
         },
      }
   }
}

impl fmt::Display for Engine {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.write_str(match *self {
         Self::Chrome => "chrome",
         Self::Edge => "edge",
         Self::Firefox => "firefox",
         Self::Safari => "safari",
         Self::Opera => "opera",
      })
   }
}

/// One session's browser: an engine and an operating system it really ships
/// on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
   pub engine: Engine,
   pub os:     ImpersonateOS,
}

impl Identity {
   /// The profile to build a client from: this engine's newest accepted
   /// version.
   #[must_use]
   pub fn profile(self) -> Impersonate {
      accepted_profile(self.engine)
   }
}

impl fmt::Display for Identity {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(f, "{} on {:?}", self.engine, self.os)
   }
}

/// Approximate desktop browser share, in tenths of a percent.
const ENGINE_WEIGHTS: [(Engine, u64); 5] = [
   (Engine::Chrome, 650),
   (Engine::Edge, 130),
   (Engine::Safari, 90),
   (Engine::Firefox, 80),
   (Engine::Opera, 50),
];

/// Operating systems each engine is seen on, weighted. Safari stays on macOS
/// and Edge on Windows; a Safari-on-Windows user agent does not exist.
fn os_weights(engine: Engine) -> &'static [(ImpersonateOS, u64)] {
   match engine {
      Engine::Chrome => {
         &[
            (ImpersonateOS::Windows, 75),
            (ImpersonateOS::MacOS, 20),
            (ImpersonateOS::Linux, 5),
         ]
      },
      Engine::Edge => &[(ImpersonateOS::Windows, 90), (ImpersonateOS::MacOS, 10)],
      Engine::Firefox => {
         &[
            (ImpersonateOS::Windows, 70),
            (ImpersonateOS::Linux, 20),
            (ImpersonateOS::MacOS, 10),
         ]
      },
      Engine::Safari => &[(ImpersonateOS::MacOS, 100)],
      Engine::Opera => &[(ImpersonateOS::Windows, 100)],
   }
}

/// The browser a session presents as, derived from its id alone so it is the
/// same on every start.
#[must_use]
pub fn identity_for(session_id: i64) -> Identity {
   let mut seed = splitmix64(session_id as u64 ^ 0x7465_6170_6177_7421);
   let engine = pick(&ENGINE_WEIGHTS, &mut seed);
   let os = pick(os_weights(engine), &mut seed);
   Identity { engine, os }
}

fn pick<T: Copy>(weights: &[(T, u64)], seed: &mut u64) -> T {
   let total: u64 = weights.iter().map(|&(_, weight)| weight).sum();
   *seed = splitmix64(*seed);
   let mut roll = *seed % total.max(1);
   for &(item, weight) in weights {
      if roll < weight {
         return item;
      }
      roll -= weight;
   }
   weights[0].0
}

/// Deterministic 64-bit mixer, so identities do not depend on an RNG.
fn splitmix64(mut state: u64) -> u64 {
   state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
   let mut result = state;
   result = (result ^ (result >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
   result = (result ^ (result >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
   result ^ (result >> 31)
}

/// Newest version per engine that completed a TLS handshake with X.
static ACCEPTED: OnceLock<HashMap<Engine, Impersonate>> = OnceLock::new();

/// Newest known-accepted fallback when the probe has not run or an engine
/// could not be tested: the second newest candidate, since the newest is the
/// one most likely to run ahead of what X's edge understands.
fn fallback_profile(engine: Engine) -> Impersonate {
   let candidates = engine.candidates();
   candidates.get(1).copied().unwrap_or(candidates[0])
}

fn accepted_profile(engine: Engine) -> Impersonate {
   ACCEPTED
      .get()
      .and_then(|accepted| accepted.get(&engine).copied())
      .unwrap_or_else(|| fallback_profile(engine))
}

/// How long one probe handshake may take before the version is skipped.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Find, per engine, the newest profile X accepts, by handshaking with
/// x.com through `proxy` (or directly). Runs once at startup; a second call
/// is a no-op.
pub async fn probe_accepted(proxy: Option<ProxyConfig>) {
   if ACCEPTED.get().is_some() {
      return;
   }
   let mut probes = JoinSet::new();
   for engine in Engine::ALL {
      let proxy = proxy.clone();
      probes.spawn(async move { (engine, newest_accepted(engine, proxy.as_ref()).await) });
   }
   let mut accepted = HashMap::new();
   while let Some(joined) = probes.join_next().await {
      let Ok((engine, profile)) = joined else {
         continue;
      };
      match profile {
         Some(profile) => {
            accepted.insert(engine, profile);
         },
         None => {
            tracing::warn!(%engine, "no browser profile completed a handshake with X, using fallback");
         },
      }
   }
   let summary = Engine::ALL
      .iter()
      .map(|engine| {
         format!(
            "{engine}={:?}",
            accepted
               .get(engine)
               .copied()
               .unwrap_or_else(|| fallback_profile(*engine))
         )
      })
      .collect::<Vec<_>>()
      .join(" ");
   tracing::info!("browser profiles accepted by X: {summary}");
   let _ = ACCEPTED.set(accepted);
}

async fn newest_accepted(engine: Engine, proxy: Option<&ProxyConfig>) -> Option<Impersonate> {
   for &profile in engine.candidates() {
      if handshakes(profile, proxy).await {
         return Some(profile);
      }
      tracing::debug!(%engine, ?profile, "X refused this browser profile");
   }
   None
}

/// Whether a client built from `profile` gets any HTTP answer from x.com. The
/// status does not matter; a refused ClientHello never gets one.
async fn handshakes(profile: Impersonate, proxy: Option<&ProxyConfig>) -> bool {
   let mut builder = primp::Client::builder()
      .impersonate(profile)
      .impersonate_os(ImpersonateOS::Windows)
      .redirect(primp::redirect::Policy::none())
      .timeout(PROBE_TIMEOUT);
   builder = match proxy.and_then(|proxy| primp::Proxy::all(proxy.url()).ok()) {
      Some(proxy) => builder.proxy(proxy),
      None => builder.no_proxy(),
   };
   let Ok(client) = builder.build() else {
      return false;
   };
   client.get("https://x.com/").send().await.is_ok()
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn identity_is_stable_per_session() {
      for id in [1_i64, 2, 57, 225, 9_999] {
         assert_eq!(identity_for(id), identity_for(id));
      }
   }

   #[test]
   fn os_always_matches_engine() {
      for id in 0..2_000_i64 {
         let identity = identity_for(id);
         let allowed = os_weights(identity.engine);
         assert!(
            allowed.iter().any(|&(os, _)| os == identity.os),
            "{identity} is not a browser that exists"
         );
      }
   }

   #[test]
   fn mix_is_weighted_towards_chrome() {
      let mut counts: HashMap<Engine, usize> = HashMap::new();
      for id in 0..5_000_i64 {
         *counts.entry(identity_for(id).engine).or_default() += 1;
      }
      let chrome = counts[&Engine::Chrome];
      assert!(chrome > 2_500 && chrome < 4_000, "chrome share {chrome}/5000");
      for engine in Engine::ALL {
         assert!(counts.get(&engine).copied().unwrap_or(0) > 0, "{engine} never picked");
      }
   }

   #[test]
   fn fallback_is_second_newest() {
      assert_eq!(fallback_profile(Engine::Chrome), Impersonate::ChromeV151);
      assert_eq!(fallback_profile(Engine::Safari), Impersonate::SafariV26_3);
   }
}
