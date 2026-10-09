use std::collections::HashMap;

use serde::{
   Deserialize,
   Serialize,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimit {
   pub limit:     i32,
   pub remaining: i32,
   pub reset:     i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
   #[default]
   OAuth,
   Cookie,
}

/// Immutable session credentials, loaded once and `Arc`-shared.
#[derive(Debug, Clone, Serialize)]
pub struct SessionCredentials {
   pub id:           i64,
   pub username:     String,
   pub kind:         SessionKind,
   pub oauth_token:  String,
   pub oauth_secret: String,
   pub auth_token:   String,
   pub ct0:          String,
}

/// Mutable rate-limit state, stored separately in the pool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionLimits {
   pub limited:    bool,
   pub limited_at: i64,
   /// The credentials themselves were refused, which no amount of waiting
   /// fixes, so this is kept apart from the rate-limit state that expires.
   #[serde(skip)]
   pub rejected:        bool,
   /// Sensitive-media and search-safety filters have already been turned off
   /// for this session, so startup does not POST the same settings again.
   #[serde(default)]
   pub filters_cleared:  bool,
   /// An adult birthdate has already been written so X stops age-gating media.
   #[serde(default)]
   pub age_gate_cleared: bool,
   pub apis:             HashMap<String, RateLimit>,
   /// Operations X answers with an empty 404 from this account while another
   /// account gets a result, by operation name, with when that was seen.
   #[serde(default)]
   pub refused_ops:      HashMap<String, i64>,
   /// Operations this account has been seen getting results for, by
   /// operation name, with when.
   #[serde(default)]
   pub verified_ops:     HashMap<String, i64>,
}

/// How long a globally-limited session stays limited before auto-recovery (15
/// min).
const GLOBAL_LIMIT_DURATION_SECS: i64 = 15 * 60;

/// How long an account sits out an operation X refused it, before it is
/// tried again in case X lifted the restriction.
const REFUSED_OP_SECS: i64 = 24 * 60 * 60;

/// `SearchTimeline` from `ph2fARFabkwfxqmSKQ1OPw/SearchTimeline`, so a mark
/// outlives a change of query id.
fn operation_name(api: &str) -> &str {
   api.rsplit('/').next().unwrap_or(api)
}

/// Result of taking one call from a session's local window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Spend {
   /// Remaining was decremented.
   Metered,
   /// No active window, so the call proceeds without a local hold.
   Unmetered,
   /// The account cannot make this call.
   Denied,
}

impl SessionLimits {
   /// Check if rate limited for a specific API.
   pub fn is_limited(&self, api: &str) -> bool {
      if self.is_globally_limited() {
         return true;
      }
      if let Some(limit) = self.apis.get(api) {
         let now = time::OffsetDateTime::now_utc().unix_timestamp();
         if limit.remaining == 0 && limit.reset > now {
            return true;
         }
      }
      false
   }

   /// Whether X refused this account `api` recently.
   #[must_use]
   pub fn refuses(&self, api: &str) -> bool {
      let now = time::OffsetDateTime::now_utc().unix_timestamp();
      self
         .refused_ops
         .get(operation_name(api))
         .is_some_and(|at| now - at < REFUSED_OP_SECS)
   }

   /// Leave this account out of `api` for [`REFUSED_OP_SECS`].
   pub fn refuse(&mut self, api: &str) {
      let now = time::OffsetDateTime::now_utc().unix_timestamp();
      self
         .refused_ops
         .retain(|_, at| now - *at < REFUSED_OP_SECS);
      self
         .refused_ops
         .insert(operation_name(api).to_owned(), now);
      self.verified_ops.remove(operation_name(api));
   }

   /// Whether this account has been seen getting results for `api`.
   #[must_use]
   pub fn verified(&self, api: &str) -> bool {
      self.verified_ops.contains_key(operation_name(api))
   }

   /// Record that this account got results for `api`.
   pub fn verify(&mut self, api: &str) {
      let now = time::OffsetDateTime::now_utc().unix_timestamp();
      self
         .verified_ops
         .insert(operation_name(api).to_owned(), now);
   }

   /// Whether it is still unknown if X lets this account call `api`.
   #[must_use]
   pub fn unchecked(&self, api: &str) -> bool {
      !self.refuses(api) && !self.verified(api)
   }

   /// Spend one local call against `api` when its window is known.
   ///
   /// The decrement happens before the request is handed out, so the next
   /// acquire sees the account as exhausted instead of piling onto it.
   pub(crate) fn try_spend(&mut self, api: &str) -> Spend {
      if self.rejected || self.is_globally_limited() || self.refuses(api) {
         return Spend::Denied;
      }
      let now = time::OffsetDateTime::now_utc().unix_timestamp();
      let Some(rate) = self.apis.get_mut(api) else {
         return Spend::Unmetered;
      };
      if rate.reset <= now {
         return Spend::Unmetered;
      }
      if rate.remaining <= 0 {
         return Spend::Denied;
      }
      rate.remaining -= 1;
      Spend::Metered
   }

   /// Give back a local spend when the call never reached X's quota.
   pub(crate) fn refund_spend(&mut self, api: &str) {
      let now = time::OffsetDateTime::now_utc().unix_timestamp();
      let Some(rate) = self.apis.get_mut(api) else {
         return;
      };
      if rate.reset <= now {
         return;
      }
      if rate.limit > 0 && rate.remaining >= rate.limit {
         return;
      }
      rate.remaining += 1;
   }

   /// Whether the session-wide limit is set and has not yet expired.
   #[must_use]
   pub fn is_globally_limited(&self) -> bool {
      // Cleared on the next mutable access rather than here.
      self.limited
         && time::OffsetDateTime::now_utc().unix_timestamp() - self.limited_at
            < GLOBAL_LIMIT_DURATION_SECS
   }

   pub fn update_limit(&mut self, api: &str, limit: i32, remaining: i32, reset: i64) {
      self.apis.insert(api.to_owned(), RateLimit {
         limit,
         remaining,
         reset,
      });
   }

   /// Record a single API as exhausted for the rest of the window.
   ///
   /// For a 429 that arrives without `x-rate-limit-*` headers, where nothing
   /// would otherwise be recorded and the session would retry immediately.
   pub fn limit_endpoint(&mut self, api: &str) {
      let now = time::OffsetDateTime::now_utc().unix_timestamp();
      let limit = self.apis.get(api).map_or(0, |rate| rate.limit);
      self.update_limit(api, limit, 0, now + GLOBAL_LIMIT_DURATION_SECS);
   }
}

/// Authentication session for Twitter API (used for JSONL deserialization).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
   pub id:         i64,
   pub username:   String,
   pub pending:    i32,
   pub limited:    bool,
   pub limited_at: i64,
   pub apis:       HashMap<String, RateLimit>,
   pub kind:       SessionKind,

   // OAuth credentials
   pub oauth_token:  String,
   pub oauth_secret: String,

   // Cookie credentials
   pub auth_token: String,
   pub ct0:        String,
}

impl Session {
   /// Split into immutable credentials and mutable rate-limit state.
   pub fn into_credentials_and_limits(self) -> (SessionCredentials, SessionLimits) {
      (
         SessionCredentials {
            id:           self.id,
            username:     self.username,
            kind:         self.kind,
            oauth_token:  self.oauth_token,
            oauth_secret: self.oauth_secret,
            auth_token:   self.auth_token,
            ct0:          self.ct0,
         },
         SessionLimits {
            limited:         self.limited,
            limited_at:      self.limited_at,
            rejected:         false,
            filters_cleared:  false,
            age_gate_cleared: false,
            apis:             self.apis,
            refused_ops:      HashMap::new(),
            verified_ops:     HashMap::new(),
         },
      )
   }
}
