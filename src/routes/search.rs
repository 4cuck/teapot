use std::slice;

use axum::{
   Router,
   extract::{
      Path,
      Query as AxumQuery,
      RawQuery,
      State,
   },
   http::{
      StatusCode,
      header::{
         CACHE_CONTROL,
         CONTENT_TYPE,
         REFRESH,
      },
   },
   response::{
      Html,
      IntoResponse as _,
      Redirect,
      Response,
   },
   routing::get,
};
use axum_extra::extract::CookieJar;
use maud::html;
use serde::Deserialize;

use super::helpers;
use crate::{
   AppState,
   cache::{
      keys as cache_keys,
      ttl,
   },
   config::Config,
   error::{
      Error,
      Result,
   },
   types::{
      PaginatedResult,
      Prefs,
      Query,
      QueryKind,
      Timeline,
      Tweet,
      User,
   },
   views::{
      layout,
      search as search_view,
   },
};

/// One card per original tweet. Latest search otherwise lists every retweet
/// of a viral post as its own row.
fn dedup_search_tweets(tweets: Vec<Tweet>) -> Vec<Tweet> {
   let mut order = Vec::new();
   let mut best = std::collections::HashMap::new();
   for tweet in tweets {
      let id = tweet.original_id();
      match best.get(&id) {
         None => {
            order.push(id);
            best.insert(id, tweet);
         },
         Some(existing) if existing.retweet.is_some() && tweet.retweet.is_none() => {
            best.insert(id, tweet);
         },
         _ => {},
      }
   }
   order.into_iter().filter_map(|id| best.remove(&id)).collect()
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
   #[serde(rename = "q")]
   pub query:      Option<String>,
   #[serde(rename = "f")]
   pub filter:     Option<String>,
   pub cursor:     Option<String>,
   // Filter parameters
   pub from:       Option<String>,
   pub since:      Option<String>,
   pub until:      Option<String>,
   pub min_faves:  Option<String>,
   // Filter toggles (f-media=on, e-replies=on, etc.)
   #[serde(rename = "f-media")]
   pub f_media:    Option<String>,
   #[serde(rename = "f-images")]
   pub f_images:   Option<String>,
   #[serde(rename = "f-videos")]
   pub f_videos:   Option<String>,
   #[serde(rename = "f-links")]
   pub f_links:    Option<String>,
   #[serde(rename = "f-news")]
   pub f_news:     Option<String>,
   #[serde(rename = "f-quote")]
   pub f_quote:    Option<String>,
   #[serde(rename = "f-verified")]
   pub f_verified: Option<String>,
   #[serde(rename = "e-replies")]
   pub e_replies:  Option<String>,
   #[serde(rename = "e-retweets")]
   pub e_retweets: Option<String>,
   /// Browser auto-retry count after an empty SearchTimeline 404.
   #[serde(default)]
   pub retry:      u8,
   /// AJAX infinite-scroll request — return a timeline fragment, not a page.
   pub scroll:     Option<String>,
}

impl SearchQuery {
   /// Convert URL parameters to `SearchFilters` for form rendering.
   pub fn to_filters(&self) -> search_view::SearchFilters {
      search_view::SearchFilters {
         media:            self.f_media.as_deref() == Some("on"),
         images:           self.f_images.as_deref() == Some("on"),
         videos:           self.f_videos.as_deref() == Some("on"),
         links:            self.f_links.as_deref() == Some("on"),
         news:             self.f_news.as_deref() == Some("on"),
         quote:            self.f_quote.as_deref() == Some("on"),
         verified:         self.f_verified.as_deref() == Some("on"),
         exclude_replies:  self.e_replies.as_deref() == Some("on"),
         exclude_retweets: self.e_retweets.as_deref() == Some("on"),
         since:            self.since.clone().unwrap_or_default(),
         until:            self.until.clone().unwrap_or_default(),
         min_faves:        self.min_faves.clone().unwrap_or_default(),
      }
   }

   /// Convert URL parameters to `Query` struct.
   fn to_query(&self) -> Query {
      let raw_query = self.query.as_deref().unwrap_or("");

      // Determine query kind from 'f' parameter
      let kind = match self.filter.as_deref() {
         Some("replies") => QueryKind::Replies,
         Some("media") => QueryKind::Media,
         Some("users") => QueryKind::Users,
         Some("top") => QueryKind::Top,
         _ => QueryKind::Posts,
      };

      // Parse the raw query text for inline filters
      let mut query = Query::parse(raw_query, kind);

      // Add from user if specified as parameter
      if let Some(ref from) = self.from {
         for user in from.split(',') {
            let user = user.trim();
            if !user.is_empty() && !query.from_user.contains(&user.to_owned()) {
               query.from_user.push(user.to_owned());
            }
         }
      }

      // Add date filters from parameters
      if let Some(ref since) = self.since
         && query.since.is_empty()
      {
         query.since.clone_from(since);
      }
      if let Some(ref until) = self.until
         && query.until.is_empty()
      {
         query.until.clone_from(until);
      }
      if let Some(ref min) = self.min_faves
         && query.min_likes.is_empty()
      {
         query.min_likes.clone_from(min);
      }

      // Add filter toggles (data-driven to avoid repetition)
      let filter_toggles: &[(&Option<String>, &str)] = &[
         (&self.f_media, "media"),
         (&self.f_images, "images"),
         (&self.f_videos, "videos"),
         (&self.f_links, "links"),
         (&self.f_news, "news"),
         (&self.f_quote, "quote"),
         (&self.f_verified, "verified"),
      ];
      for &(param, name) in filter_toggles {
         if param.as_deref() == Some("on") && !query.filters.iter().any(|filter| filter == name) {
            query.filters.push((*name).to_owned());
         }
      }

      // Add exclude toggles
      let exclude_toggles: &[(&Option<String>, &str)] =
         &[(&self.e_replies, "replies"), (&self.e_retweets, "retweets")];
      for &(param, name) in exclude_toggles {
         if param.as_deref() == Some("on") && !query.excludes.iter().any(|excl| excl == name) {
            query.excludes.push((*name).to_owned());
         }
      }

      query
   }
}

const SEARCH_AUTO_RETRIES: u8 = 2;
const SEARCH_RETRY_WAIT_SECS: u8 = 2;

fn with_retry_param(raw_qs: &str, retry: u8) -> String {
   let rest = raw_qs
      .split('&')
      .filter(|pair| {
         !pair.is_empty() && !pair.starts_with("retry=") && !pair.starts_with("scroll=")
      })
      .collect::<Vec<_>>();
   if rest.is_empty() {
      format!("/search?retry={retry}")
   } else {
      format!("/search?{}&retry={retry}", rest.join("&"))
   }
}

fn is_scroll_request(params: &SearchQuery) -> bool {
   params.scroll.as_deref() == Some("true")
}

fn search_error(
   config: &Config,
   prefs: &Prefs,
   raw_qs: Option<&str>,
   params: &SearchQuery,
   err: &Error,
) -> Response {
   if is_scroll_request(params) {
      // A 200 "Trying again…" page looks like success to infiniteScroll.js,
      // which then drops the Load more sentinel and cannot page further.
      return helpers::api_error_titled(config, err, "Search Error");
   }
   search_upstream_error(config, prefs, raw_qs, params.retry, err)
}

fn search_upstream_error(
   config: &Config,
   prefs: &Prefs,
   raw_qs: Option<&str>,
   attempt: u8,
   err: &Error,
) -> Response {
   if matches!(err, Error::TransientUpstream) && attempt < SEARCH_AUTO_RETRIES {
      let url = with_retry_param(raw_qs.unwrap_or(""), attempt + 1);
      let wait = SEARCH_RETRY_WAIT_SECS;
      tracing::warn!(attempt, %url, "search empty, auto-retrying in the browser");
      let refresh = html! {
          meta http-equiv="refresh" content=(format!("{wait};url={url}"));
      };
      let content = html! {
          div class="panel-container" {
              div class="error-panel" {
                  span { "X did not return a result. Trying again…" }
                  " "
                  a href=(&url) { "Retry now" }
              }
          }
      };
      let markup = layout::PageLayout::new(config, "Try again", content)
         .prefs(prefs)
         .head_extra(&refresh)
         .description("X did not return a result. Trying again…")
         .render();
      return (
         StatusCode::OK,
         [
            (REFRESH, format!("{wait};url={url}")),
            (CACHE_CONTROL, "no-store".to_owned()),
         ],
         Html(markup.into_string()),
      )
         .into_response();
   }
   helpers::api_error_titled(config, err, "Search Error")
}

pub fn router() -> Router<AppState> {
   Router::new()
      .route("/search", get(search))
      .route("/hashtag/{tag}", get(hashtag))
      .route("/opensearch", get(opensearch))
}

async fn search(
   State(state): State<AppState>,
   jar: CookieJar,
   RawQuery(raw_qs): RawQuery,
   AxumQuery(params): AxumQuery<SearchQuery>,
) -> Result<Response> {
   // Strip empty query parameters (e.g. since=&until=&min_faves=) for clean URLs
   if let Some(ref qs) = raw_qs {
      let clean: Vec<&str> = qs
         .split('&')
         .filter(|pair| pair.split_once('=').is_none_or(|(_, val)| !val.is_empty()))
         .collect();
      if clean.len() < qs.split('&').count() {
         let url = if clean.is_empty() {
            "/search".to_owned()
         } else {
            format!("/search?{}", clean.join("&"))
         };
         return Ok(Redirect::to(&url).into_response());
      }
   }

   // Extract prefs from cookies
   let prefs = Prefs::from_cookies(&jar, &state.config);
   let is_scroll = is_scroll_request(&params);

   let raw_q = params.query.clone().unwrap_or_default();

   // Redirect comma-separated usernames to multi-user timeline
   if raw_q.contains(',')
      && raw_q.split(',').all(|segment| {
         let trimmed = segment.trim();
         !trimmed.is_empty()
            && trimmed
               .chars()
               .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '@')
      })
   {
      let cleaned = raw_q
         .split(',')
         .map(|segment| segment.trim().trim_start_matches('@'))
         .collect::<Vec<_>>()
         .join(",");
      return Ok(Redirect::to(&format!("/{cleaned}")).into_response());
   }

   // Check if this is a user search
   let is_user_search = params.filter.as_deref() == Some("users");

   // Handle empty query - show search UI without calling API
   if raw_q.is_empty() && params.from.is_none() {
      let filters = params.to_filters();
      if is_user_search {
         let content = search_view::render_user_search_results(
            &raw_q,
            &[],
            &state.config,
            None,
            None,
            Some(&prefs),
         );
         let markup = layout::PageLayout::new(&state.config, "Search", content)
            .prefs(&prefs)
            .render();
         return Ok(Html(markup.into_string()).into_response());
      }
      let empty_tweets = Vec::new();
      let content = search_view::render_search_results_with_prefs(
         &raw_q,
         &empty_tweets,
         &state.config,
         None,
         Some(&prefs),
         Some(&filters),
         None,
         "tweets",
      );
      let markup = layout::PageLayout::new(&state.config, "Search", content)
         .prefs(&prefs)
         .render();
      return Ok(Html(markup.into_string()).into_response());
   }

   if is_user_search {
      let search_result = if params.cursor.is_none() {
         let cache_key = cache_keys::search_users(&raw_q, None);
         if let Some(cached) = helpers::swr_take::<PaginatedResult<User>, _, _>(&state, &cache_key, {
            let raw_q = raw_q.clone();
            let cache_key = cache_key.clone();
            move |state| async move {
               if let Ok(data) = state.api.search_users(&raw_q, None).await {
                  state
                     .cache
                     .set_swr(&cache_key, &data, ttl::SEARCH, ttl::SEARCH_STALE);
               }
            }
         }) {
            Ok(cached)
         } else {
            let result = state.api.search_users(&raw_q, None).await;
            if let Ok(ref data) = result {
               state
                  .cache
                  .set_swr(&cache_key, data, ttl::SEARCH, ttl::SEARCH_STALE);
            }
            result
         }
      } else {
         state
            .api
            .search_users(&raw_q, params.cursor.as_deref())
            .await
      };

      match search_result {
         Ok(result) => {
            for user in &result.content {
               helpers::remember_user_id(&state, user);
            }
            let cursor = result.bottom.as_deref();

            let newer_url = params.cursor.is_some().then(|| {
               format!(
                  "/search?q={}&f=users",
                  percent_encoding::utf8_percent_encode(&raw_q, percent_encoding::NON_ALPHANUMERIC)
               )
            });
            let content = search_view::render_user_search_results(
               &raw_q,
               &result.content,
               &state.config,
               cursor,
               newer_url.as_deref(),
               Some(&prefs),
            );
            let title = format!("Search ({raw_q}) | Users");
            let canonical = format!(
               "https://x.com/search?q={}&src=typed_query",
               percent_encoding::utf8_percent_encode(&raw_q, percent_encoding::NON_ALPHANUMERIC)
            );
            let referer = format!(
               "/search?q={}&f=users",
               percent_encoding::utf8_percent_encode(&raw_q, percent_encoding::NON_ALPHANUMERIC)
            );
            if is_scroll {
               return Ok(Html(content.into_string()).into_response());
            }
            let markup = layout::PageLayout::new(&state.config, &title, content)
               .prefs(&prefs)
               .canonical(&canonical)
               .referer(&referer)
               .render();
            Ok(Html(markup.into_string()).into_response())
         },
         Err(err) => {
            Ok(search_error(
               &state.config,
               &prefs,
               raw_qs.as_deref(),
               &params,
               &err,
            ))
         },
      }
   } else {
      // Tweet search
      // Parse query with filters
      let query = params.to_query();

      // Build the actual search query for Twitter API
      let api_query = query.build();

      // Map query kind to Twitter API product.
      // Media uses "Latest" + filter:media in query, same as the default.
      let product = match query.kind {
         QueryKind::Top => "Top",
         _ => "Latest",
      };
      let active_tab = match query.kind {
         QueryKind::Top => "top",
         QueryKind::Media => "media",
         _ => "tweets",
      };

      let search_result = if params.cursor.is_none() {
         let cache_key = cache_keys::search_timeline(&api_query, product, None);
         if let Some(cached) = helpers::swr_take::<Timeline, _, _>(&state, &cache_key, {
            let api_query = api_query.clone();
            let cache_key = cache_key.clone();
            move |state| async move {
               if let Ok(data) = state.api.search(&api_query, None, product).await {
                  state
                     .cache
                     .set_swr(&cache_key, &data, ttl::SEARCH, ttl::SEARCH_STALE);
               }
            }
         }) {
            Ok(cached)
         } else {
            let result = state.api.search(&api_query, None, product).await;
            if let Ok(ref data) = result {
               state
                  .cache
                  .set_swr(&cache_key, data, ttl::SEARCH, ttl::SEARCH_STALE);
            }
            result
         }
      } else {
         state
            .api
            .search(&api_query, params.cursor.as_deref(), product)
            .await
      };

      match search_result {
         Ok(timeline) => {
            let mut tweets = dedup_search_tweets(timeline.content.into_iter().flatten().collect());
            helpers::enrich_tweet_groups(&state, slice::from_mut(&mut tweets)).await;
            helpers::remember_tweets(&state, &tweets);
            helpers::prefetch_profiles(&state, &tweets);
            let cursor = timeline.bottom.as_deref();

            // Display the original user query, not the API query
            let display_query = if query.has_filters() {
               &api_query
            } else {
               &raw_q
            };

            let filters = params.to_filters();
            let newer_url = params.cursor.is_some().then(|| {
               format!(
                  "/search?q={}",
                  percent_encoding::utf8_percent_encode(&raw_q, percent_encoding::NON_ALPHANUMERIC)
               )
            });
            let content = search_view::render_search_results_with_prefs(
               display_query,
               &tweets,
               &state.config,
               cursor,
               Some(&prefs),
               Some(&filters),
               newer_url.as_deref(),
               active_tab,
            );
            let title = format!("Search ({raw_q})");
            let canonical = format!(
               "https://x.com/search?f=live&q={}&src=typed_query",
               percent_encoding::utf8_percent_encode(&raw_q, percent_encoding::NON_ALPHANUMERIC)
            );
            let rss_url = format!(
               "/search/rss?f=tweets&q={}",
               percent_encoding::utf8_percent_encode(&raw_q, percent_encoding::NON_ALPHANUMERIC)
            );
            let referer = format!(
               "/search?q={}",
               percent_encoding::utf8_percent_encode(&raw_q, percent_encoding::NON_ALPHANUMERIC)
            );
            if is_scroll {
               return Ok(Html(content.into_string()).into_response());
            }
            let markup = layout::PageLayout::new(&state.config, &title, content)
               .prefs(&prefs)
               .rss(&rss_url)
               .canonical(&canonical)
               .referer(&referer)
               .render();
            Ok(Html(markup.into_string()).into_response())
         },
         Err(err) => {
            Ok(search_error(
               &state.config,
               &prefs,
               raw_qs.as_deref(),
               &params,
               &err,
            ))
         },
      }
   }
}

async fn hashtag(
   State(state): State<AppState>,
   jar: CookieJar,
   Path(tag): Path<String>,
   AxumQuery(query): AxumQuery<SearchQuery>,
) -> Result<Response> {
   // Search for the hashtag
   let hashtag_query = format!("#{tag}");

   search(
      State(state),
      jar,
      RawQuery(None),
      AxumQuery(SearchQuery {
         query:  Some(hashtag_query),
         filter: query.filter,
         cursor: query.cursor,
         scroll: query.scroll,
         ..Default::default()
      }),
   )
   .await
}

async fn opensearch(State(state): State<AppState>) -> Response {
   let xml = format!(
      r#"<?xml version="1.0" encoding="UTF-8"?>
<OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/">
  <ShortName>{}</ShortName>
  <Description>Twitter search via {}</Description>
  <InputEncoding>UTF-8</InputEncoding>
  <Url type="text/html" template="{}/search?q={{searchTerms}}"/>
</OpenSearchDescription>"#,
      state.config.server.title,
      state.config.server.hostname,
      state.config.url_prefix()
   );

   (
      [(CONTENT_TYPE, "application/opensearchdescription+xml")],
      xml,
   )
      .into_response()
}
