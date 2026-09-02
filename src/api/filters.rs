//! Turn off X's default sensitive-content filters on cookie sessions.
//!
//! New accounts hide NSFW media, strip it from search, and age-gate adult
//! media until a birthdate is on the profile. That leaks into teapawt search,
//! so each cookie session is asked once to show sensitive media, stop
//! filtering search, and store an adult birthdate (kept private on the
//! profile).

use std::time::Duration;

use axum::http::header;
use bytes::Bytes;
use serde::Deserialize;
use tokio::time::sleep;

use super::{
   ApiClient,
   SessionLease,
   endpoints,
};
use crate::{
   error::{
      Error,
      Result,
   },
   types::SessionKind,
};

const SEARCH_SAFETY_BODY: &str = r#"{"optInFiltering":false,"optInBlocking":false}"#;
const DISPLAY_SENSITIVE_BODY: &str = "display_sensitive_media=true";
/// X's settings UI posts these exact field names to `account/update_profile`.
/// Visibility is `self` so the date is not shown on the public profile.
const ADULT_BIRTHDATE_BODY: &str = "birthdate_day=15&birthdate_month=6&birthdate_year=1990&\
                                   birthdate_visibility=self&birthdate_year_visibility=self&\
                                   skip_status=true";

#[expect(
   clippy::multiple_inherent_impl,
   reason = "filter sync is split from GraphQL endpoint methods"
)]
impl ApiClient {
   /// Walk cookie sessions in the background and turn off NSFW filters.
   pub fn spawn_filter_sync(&self) {
      let client = self.clone();
      tokio::spawn(async move {
         client.sync_sensitive_filters().await;
      });
   }

   async fn sync_sensitive_filters(&self) {
      let ids = self.sessions.pending_filter_sync_ids().await;
      if ids.is_empty() {
         return;
      }
      tracing::info!(
         count = ids.len(),
         "clearing sensitive-content filters and age gates on cookie sessions"
      );
      let mut ok = 0_usize;
      for id in ids {
         match self.clear_session_filters_retry(id).await {
            Ok(()) => {
               self.sessions.mark_filters_cleared(id).await;
               ok += 1;
            },
            Err(err) => {
               tracing::warn!(
                  session_id = id,
                  "failed to clear NSFW filters or age gate: {err}"
               );
            },
         }
         sleep(Duration::from_millis(250)).await;
      }
      tracing::info!(ok, "sensitive-content filters and age gates cleared");
   }

   async fn clear_session_filters_retry(&self, session_id: i64) -> Result<()> {
      let mut last_err = None;
      for attempt in 0..3_u8 {
         match self.clear_session_filters(session_id).await {
            Ok(()) => return Ok(()),
            Err(err)
               if attempt < 2 && err.to_string().contains("SOCKS5") =>
            {
               last_err = Some(err);
               sleep(Duration::from_secs(1)).await;
            },
            Err(err) => return Err(err),
         }
      }
      Err(last_err.unwrap_or_else(|| Error::Internal("filter sync retry exhausted".into())))
   }

   async fn clear_session_filters(&self, session_id: i64) -> Result<()> {
      let session = self
         .sessions
         .acquire_id(session_id, endpoints::ACCOUNT_SETTINGS)
         .await?;
      if session.kind != SessionKind::Cookie {
         return Ok(());
      }

      let user_id = self.session_user_id(&session).await?;
      self.set_display_sensitive_media(&session).await?;
      self.set_search_safety(&session, &user_id).await?;
      self.set_adult_birthdate(&session).await?;
      Ok(())
   }

   async fn session_user_id(&self, session: &SessionLease) -> Result<String> {
      if let Some(rest_id) = session.username.strip_prefix("x-")
         && rest_id.chars().all(|ch| ch.is_ascii_digit())
         && !rest_id.is_empty()
      {
         return Ok(rest_id.to_owned());
      }

      let path = "/1.1/account/verify_credentials.json";
      let headers = self
         .cookie_rest_headers(session, "GET", path, "application/json")
         .await?;
      let response = self
         .client
         .get_on(
            endpoints::VERIFY_CREDENTIALS_URL,
            &headers,
            self.proxy_for(session).as_ref(),
         )
         .await?;
      let (bytes, _) = self
         .account_response(session, endpoints::VERIFY_CREDENTIALS, response)
         .await?;

      #[derive(Deserialize)]
      struct Verify {
         id_str: Option<String>,
      }
      let verify: Verify = serde_json::from_slice(&bytes)
         .map_err(|err| Error::Internal(format!("verify_credentials parse: {err}")))?;
      verify
         .id_str
         .filter(|id| !id.is_empty())
         .ok_or_else(|| Error::Internal("verify_credentials missing id_str".into()))
   }

   async fn set_display_sensitive_media(&self, session: &SessionLease) -> Result<()> {
      let path = "/1.1/account/settings.json";
      let mut headers = self
         .cookie_rest_headers(session, "POST", path, "application/x-www-form-urlencoded")
         .await?;
      headers.insert(
         header::REFERER,
         header::HeaderValue::from_static("https://x.com/"),
      );
      let response = self
         .client
         .post_on(
            endpoints::ACCOUNT_SETTINGS_URL,
            &headers,
            Bytes::from_static(DISPLAY_SENSITIVE_BODY.as_bytes()),
            self.proxy_for(session).as_ref(),
         )
         .await?;
      let (bytes, _) = self
         .account_response(session, endpoints::ACCOUNT_SETTINGS, response)
         .await?;

      #[derive(Deserialize)]
      struct Settings {
         display_sensitive_media: Option<bool>,
      }
      if let Ok(settings) = serde_json::from_slice::<Settings>(&bytes)
         && settings.display_sensitive_media != Some(true)
      {
         return Err(Error::Internal(
            "account settings did not enable display_sensitive_media".into(),
         ));
      }
      Ok(())
   }

   async fn set_adult_birthdate(&self, session: &SessionLease) -> Result<()> {
      let path = "/1.1/account/update_profile.json";
      let mut headers = self
         .cookie_rest_headers(session, "POST", path, "application/x-www-form-urlencoded")
         .await?;
      headers.insert(
         header::REFERER,
         header::HeaderValue::from_static("https://x.com/settings/profile"),
      );
      let response = self
         .client
         .post_on(
            endpoints::UPDATE_PROFILE_URL,
            &headers,
            Bytes::from_static(ADULT_BIRTHDATE_BODY.as_bytes()),
            self.proxy_for(session).as_ref(),
         )
         .await?;
      let bytes = match self
         .account_response(session, endpoints::UPDATE_PROFILE, response)
         .await
      {
         Ok((bytes, _)) => bytes,
         Err(err) if birthdate_already_locked(&err.to_string()) => return Ok(()),
         Err(err) => return Err(err),
      };
      if birthdate_write_rejected(&bytes) {
         return Err(Error::Internal(
            "update_profile rejected the adult birthdate".into(),
         ));
      }
      Ok(())
   }

   async fn set_search_safety(&self, session: &SessionLease, user_id: &str) -> Result<()> {
      let path = format!("/i/api/1.1/strato/column/User/{user_id}/search/searchSafety");
      let url = format!("https://x.com{path}");
      let mut headers = self
         .cookie_rest_headers(session, "POST", &path, "application/json")
         .await?;
      headers.insert(
         header::REFERER,
         header::HeaderValue::from_static("https://x.com/settings/search"),
      );
      let response = self
         .client
         .post_on(
            &url,
            &headers,
            Bytes::from_static(SEARCH_SAFETY_BODY.as_bytes()),
            self.proxy_for(session).as_ref(),
         )
         .await?;
      let _ = self
         .account_response(session, endpoints::SEARCH_SAFETY, response)
         .await?;
      Ok(())
   }

   async fn cookie_rest_headers(
      &self,
      session: &SessionLease,
      method: &str,
      api_path: &str,
      content_type: &'static str,
   ) -> Result<header::HeaderMap> {
      let (bearer, tid) = self.bearer_and_tid_for(method, api_path).await;
      let mut headers = header::HeaderMap::new();
      headers.insert(
         header::AUTHORIZATION,
         header::HeaderValue::from_str(bearer)
            .map_err(|_| Error::Internal("invalid bearer token value".into()))?,
      );
      headers.insert(
         "x-twitter-auth-type",
         header::HeaderValue::from_static("OAuth2Session"),
      );
      headers.insert(
         "x-csrf-token",
         session
            .ct0
            .parse()
            .map_err(|_| Error::Internal("invalid ct0 header value".into()))?,
      );
      headers.insert(
         header::COOKIE,
         format!("auth_token={}; ct0={}", session.auth_token, session.ct0)
            .parse()
            .map_err(|_| Error::Internal("invalid cookie header value".into()))?,
      );
      headers.insert(
         header::ORIGIN,
         header::HeaderValue::from_static("https://x.com"),
      );
      headers.insert(
         header::CONTENT_TYPE,
         header::HeaderValue::from_static(content_type),
      );
      headers.insert(header::ACCEPT, header::HeaderValue::from_static("*/*"));
      headers.insert(
         "x-twitter-active-user",
         header::HeaderValue::from_static("yes"),
      );
      headers.insert(
         "x-twitter-client-language",
         header::HeaderValue::from_static("en"),
      );
      if let Some(tid) = tid
         && let Ok(val) = tid.parse()
      {
         headers.insert("x-client-transaction-id", val);
      }
      Ok(headers)
   }
}

fn birthdate_already_locked(message: &str) -> bool {
   let lower = message.to_ascii_lowercase();
   lower.contains("birth")
      && (lower.contains("limited")
         || lower.contains("already")
         || lower.contains("cannot change")
         || lower.contains("can't change")
         || lower.contains("can not change"))
}

fn birthdate_write_rejected(body: &[u8]) -> bool {
   #[derive(Deserialize)]
   struct ApiErrors {
      errors: Option<Vec<ApiError>>,
   }
   #[derive(Deserialize)]
   struct ApiError {
      message: Option<String>,
   }
   let Ok(parsed) = serde_json::from_slice::<ApiErrors>(body) else {
      return false;
   };
   parsed.errors.into_iter().flatten().any(|err| {
      err
         .message
         .as_deref()
         .is_some_and(|msg| !birthdate_already_locked(msg))
   })
}
