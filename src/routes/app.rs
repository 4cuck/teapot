//! The route table.

use axum::Router;

use crate::AppState;

/// Axum matches in merge order, so anything that would be swallowed by
/// `timeline`'s greedy `/{username}` must be merged before it.
pub fn router() -> Router<AppState> {
   Router::new()
      .merge(super::pages::router())
      .merge(super::unsupported::router())
      .merge(super::debug::router())
      .merge(super::redirect::router())
      .merge(super::intent::router())
      .merge(super::search::router())
      .merge(super::preferences::router())
      .merge(super::embed::router())
      .merge(super::status::router())
      .merge(super::media::router())
      .merge(super::rss::router())
      .merge(super::list::router())
      .merge(super::notes::router())
      .merge(super::unsupported::i_catchall_router())
      .merge(super::timeline::router())
}
