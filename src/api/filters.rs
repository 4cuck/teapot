//! Turn off X's default sensitive-content filters on cookie sessions.
//!
//! New accounts hide NSFW media and strip it from search. That leaks into
//! teapawt search, so each cookie session is asked once to show sensitive
//! media and to stop filtering search.

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
         "clearing sensitive-content filters on cookie sessions"
      );
      let mut ok = 0_usize;
      for id in ids {
         match self.clear_session_filters(id).await {
            Ok(()) => {
               self.sessions.mark_filters_cleared(id).await;
               ok += 1;
            },
            Err(err) => {
               tracing::warn!(session_id = id, "failed to clear NSFW filters: {err}");
            },
         }
         sleep(Duration::from_millis(250)).await;
      }
      tracing::info!(ok, "sensitive-content filters cleared");
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
         .get_with_headers(endpoints::VERIFY_CREDENTIALS_URL, &headers)
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
         .post_with_headers(
            endpoints::ACCOUNT_SETTINGS_URL,
            &headers,
            Bytes::from_static(DISPLAY_SENSITIVE_BODY.as_bytes()),
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
         .post_with_headers(
            &url,
            &headers,
            Bytes::from_static(SEARCH_SAFETY_BODY.as_bytes()),
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
