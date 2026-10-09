//! Find out which cookie sessions X lets search, before visitors rely on them.
//!
//! X refuses some accounts search outright: every `SearchTimeline` from them
//! is an empty 404, while the same account still loads profiles and posts.
//! Accounts nobody has checked are searched once each in the background,
//! spaced out. Those that get results go first for search from then on; the
//! refused ones sit out search for a day and are checked again after that.

use std::time::{
   Duration,
   Instant,
};

use tokio::time::sleep;

use super::{
   ApiClient,
   SessionLease,
   endpoints,
   schema::SearchTimelineData,
};
use crate::{
   error::{
      Error,
      Result,
   },
   types::SessionKind,
};

/// Gap between two checks, so a batch of new accounts does not reach X as a
/// burst of searches.
const CHECK_GAP: Duration = Duration::from_secs(3);
/// How often the pool is looked over for accounts to check.
const CHECK_EVERY: Duration = Duration::from_mins(30);
/// A result this recent from any account shows X is answering searches, so
/// an empty answer belongs to the account that got it.
const ANSWERED_WITHIN: Duration = Duration::from_mins(1);
/// Everyday searches, varied so the checks are not one query over and over.
const QUERIES: &[&str] = &[
   "news", "weather", "music", "football", "coffee", "movies", "travel", "food", "art", "science",
];

#[expect(
   clippy::multiple_inherent_impl,
   reason = "the search check is split from GraphQL endpoint methods"
)]
impl ApiClient {
   /// Check unchecked accounts now, then every half hour.
   pub fn spawn_search_check(&self) {
      let client = self.clone();
      tokio::spawn(async move {
         loop {
            client.check_search_accounts().await;
            sleep(CHECK_EVERY).await;
         }
      });
   }

   async fn check_search_accounts(&self) {
      let api = endpoints::GRAPH_SEARCH_TIMELINE;
      let ids = self.sessions.unchecked_for(api).await;
      if ids.is_empty() {
         return;
      }
      tracing::info!(count = ids.len(), "checking which accounts X lets search");

      let mut answered: Option<Instant> = None;
      let (mut allowed, mut refused, mut unknown) = (0_usize, 0_usize, 0_usize);
      for id in ids {
         match self.search_as(id).await {
            Ok(()) => {
               self.sessions.mark_verified(id, api).await;
               answered = Some(Instant::now());
               allowed += 1;
            },
            Err(Error::TransientUpstream) => {
               let others_answered = answered.is_some_and(|at| at.elapsed() < ANSWERED_WITHIN) || {
                  let ok = self.another_account_searches(id).await;
                  if ok {
                     answered = Some(Instant::now());
                  }
                  ok
               };
               if others_answered {
                  self.sessions.mark_refused(id, api).await;
                  refused += 1;
               } else {
                  // X answered nobody, so this says nothing about the account.
                  unknown += 1;
               }
            },
            // Out of searches, busy, or a network error: next round.
            Err(_) => unknown += 1,
         }
         sleep(CHECK_GAP).await;
      }
      tracing::info!(allowed, refused, unknown, "search check done");
   }

   async fn search_as(&self, session_id: i64) -> Result<()> {
      let session = self
         .sessions
         .acquire_id(session_id, endpoints::GRAPH_SEARCH_TIMELINE)
         .await?;
      self.search_with(&session).await
   }

   /// Whether some other account, known-good ones first, gets a result.
   async fn another_account_searches(&self, excluded: i64) -> bool {
      let api = endpoints::GRAPH_SEARCH_TIMELINE;
      let Ok(session) = self
         .sessions
         .acquire_excluding(api, Some(SessionKind::Cookie), Some(excluded))
         .await
      else {
         return false;
      };
      let ok = self.search_with(&session).await.is_ok();
      if ok {
         self.sessions.mark_verified(session.id, api).await;
      }
      ok
   }

   async fn search_with(&self, session: &SessionLease) -> Result<()> {
      let query = QUERIES[session.id.unsigned_abs() as usize % QUERIES.len()];
      self
         .graphql_request_inner::<SearchTimelineData>(
            session,
            endpoints::GRAPH_SEARCH_TIMELINE,
            &endpoints::search_vars(query, None, "Latest"),
            endpoints::SEARCH_FEATURES,
            None,
         )
         .await
         .map(|_| ())
   }
}
