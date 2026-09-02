use std::{
   any::Any,
   collections::BTreeMap,
   future::Future,
   iter,
   slice,
   sync::LazyLock,
};

use axum::{
   http::{
      StatusCode,
      header,
   },
   response::{
      Html,
      IntoResponse as _,
      Response,
   },
};

use tokio::sync::Semaphore;

use crate::{
   AppState,
   cache::{
      Hit,
      keys as cache_keys,
      ttl,
   },
   config::Config,
   error::{
      Error,
      Result,
   },
   types::{
      AccountContext,
      Conversation,
      Profile,
      Timeline,
      Translation,
      Tweet,
      Tweets,
      User,
   },
   views::layout,
};

/// Caps background profile prefetches so they cannot crowd out live pages.
static PROFILE_PREFETCH: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(3));
const PREFETCH_PROFILES: usize = 4;

#[derive(Clone)]
struct CachedRss {
   body:   String,
   min_id: Option<i64>,
}

/// Fresh cache hit, or a stale value plus a single background refresh.
pub fn swr_take<T, F, Fut>(state: &AppState, key: &str, refresh: F) -> Option<T>
where
   T: Any + Clone + Send + Sync,
   F: FnOnce(AppState) -> Fut + Send + 'static,
   Fut: Future<Output = ()> + Send + 'static,
{
   match state.cache.lookup(key) {
      Some(Hit::Fresh(value)) => Some(value),
      Some(Hit::Stale(value)) => {
         if let Some(guard) = state.cache.start_refresh(key) {
            let state = state.clone();
            tokio::spawn(async move {
               let _guard = guard;
               refresh(state).await;
            });
         }
         Some(value)
      },
      None => None,
   }
}

/// Cached user, or a stub that only has `id` so tweets can start immediately.
pub fn user_hint(state: &AppState, username: &str) -> Option<User> {
   let key = cache_keys::user(username);
   if let Some(Hit::Fresh(user) | Hit::Stale(user)) = state.cache.lookup(&key) {
      return Some(user);
   }
   let id = state.cache.get::<String>(&cache_keys::username_id(username))?;
   Some(User {
      id,
      username: username.to_owned(),
      ..User::default()
   })
}

/// Persist the bidirectional id mapping. Does not overwrite a full user card.
pub fn remember_user_id(state: &AppState, user: &User) {
   if user.id.is_empty() || user.username.is_empty() {
      return;
   }
   state
      .cache
      .set(&cache_keys::user_id(&user.id), &user.username, ttl::USER_ID_MAPPING);
   state.cache.set(
      &cache_keys::username_id(&user.username),
      &user.id,
      ttl::USER_ID_MAPPING,
   );
}

/// Store a full user record plus id mappings.
pub fn store_user(state: &AppState, user: &User) {
   remember_user_id(state, user);
   if user.id.is_empty() || user.username.is_empty() {
      return;
   }
   state
      .cache
      .set_swr(&cache_keys::user(&user.username), user, ttl::DEFAULT, ttl::DEFAULT_STALE);
}

/// Store a first-page profile and its user.
pub fn store_profile(state: &AppState, profile: &Profile) {
   store_user(state, &profile.user);
   state.cache.set_swr(
      &cache_keys::profile(&profile.user.username),
      profile,
      ttl::DEFAULT,
      ttl::DEFAULT_STALE,
   );
}

/// Seed id mappings from tweets already on the page so a later profile click
/// can fetch tweets in the same wave as `UserByScreenName`.
pub fn remember_tweets(state: &AppState, tweets: &[Tweet]) {
   for tweet in tweets {
      for user in tweet_users(tweet) {
         remember_user_id(state, user);
      }
   }
}

/// Warm a few uncached profiles in the background after search/timeline HTML.
pub fn prefetch_profiles(state: &AppState, tweets: &[Tweet]) {
   let mut names = Vec::new();
   for tweet in tweets {
      let name = tweet.user.username.to_lowercase();
      if name.is_empty() || names.iter().any(|seen: &String| seen == &name) {
         continue;
      }
      if state.cache.get::<Profile>(&cache_keys::profile(&name)).is_some() {
         continue;
      }
      names.push(name);
      if names.len() >= PREFETCH_PROFILES {
         break;
      }
   }
   if names.is_empty() {
      return;
   }

   let state = state.clone();
   tokio::spawn(async move {
      for name in names {
         let Ok(permit) = PROFILE_PREFETCH.try_acquire() else {
            break;
         };
         let hint = user_hint(&state, &name);
         if let Ok(profile) = state.api.get_profile_hinted(&name, None, hint.as_ref()).await {
            store_profile(&state, &profile);
         }
         drop(permit);
      }
   });
}

/// Fetch a user, using cache when available.
pub async fn get_cached_user(state: &AppState, username: &str) -> Result<User> {
   let cache_key = cache_keys::user(username);
   if let Some(cached) = swr_take(state, &cache_key, {
      let username = username.to_owned();
      move |state| async move {
         if let Ok(user) = state.api.get_user(&username).await {
            store_user(&state, &user);
         }
      }
   }) {
      return Ok(cached);
   }
   let fetched = state.api.get_user(username).await?;
   store_user(state, &fetched);
   Ok(fetched)
}

pub fn apply_context_to_user(user: &mut User, context: &AccountContext) {
   user.account_based_in.clone_from(&context.account_based_in);
   user.connection_source.clone_from(&context.connection_source);
   user.location_accurate = context.location_accurate;
}

/// Apply About Account from cache only. A miss is filled in the background.
pub fn apply_cached_account_context(state: &AppState, user: &mut User) {
   let username = user.username.to_lowercase();
   if username.is_empty() {
      return;
   }
   let key = cache_keys::account_context(&username);
   if let Some(context) = state.cache.get::<AccountContext>(&key) {
      apply_context_to_user(user, &context);
      return;
   }
   let Some(guard) = state.cache.start_refresh(&key) else {
      return;
   };
   let state = state.clone();
   tokio::spawn(async move {
      let _guard = guard;
      let _ = load_account_context(&state, &username).await;
   });
}

/// Load About Account, overlapping community cache and X when needed.
pub async fn load_account_context(state: &AppState, username: &str) -> Option<AccountContext> {
   let username = username.to_lowercase();
   if username.is_empty() {
      return None;
   }
   let key = cache_keys::account_context(&username);
   if let Some(cached) = state.cache.get::<AccountContext>(&key) {
      return (!cached.is_empty()).then_some(cached);
   }

   let community = community_contexts(state, slice::from_ref(&username)).await;
   let resolved = match community.into_values().next() {
      Some(context) => context,
      None => {
         state
            .api
            .get_account_context(&username)
            .await
            .unwrap_or_else(|err| {
               tracing::debug!(username, "About Account unavailable: {err}");
               AccountContext::default()
            })
      },
   };
   state.cache.set(&key, &resolved, ttl::ACCOUNT_CONTEXT);
   (!resolved.is_empty()).then_some(resolved)
}

/// Resolve about-account data for many users at once without spending X
/// quota.
pub async fn account_contexts(
   state: &AppState,
   usernames: &[String],
) -> BTreeMap<String, AccountContext> {
   let mut resolved = BTreeMap::new();
   let mut missing = Vec::new();
   for username in usernames {
      let username = username.to_lowercase();
      match state
         .cache
         .get::<AccountContext>(&cache_keys::account_context(&username))
      {
         Some(context) => {
            resolved.insert(username, context);
         },
         None => missing.push(username),
      }
   }

   for (username, context) in community_contexts(state, &missing).await {
      state.cache.set(
         &cache_keys::account_context(&username),
         &context,
         ttl::ACCOUNT_CONTEXT,
      );
      resolved.insert(username, context);
   }

   resolved.retain(|_, context| !context.is_empty());
   resolved
}

/// Decorate every author rendered by a conversation.
pub async fn enrich_conversation(state: &AppState, conversation: &mut Conversation) {
   let names = tweets(conversation).flat_map(tweet_users).collect();
   let Some(contexts) = resolve_for(state, names).await else {
      return;
   };
   for tweet in tweets_mut(conversation) {
      apply_to_tweet(tweet, &contexts);
   }
}

/// Decorate every author rendered by a timeline, search page or list.
pub async fn enrich_tweet_groups(state: &AppState, groups: &mut [Tweets]) {
   let names = groups
      .iter()
      .flatten()
      .flat_map(tweet_users)
      .collect::<Vec<_>>();
   let Some(contexts) = resolve_for(state, names).await else {
      return;
   };
   for tweet in groups.iter_mut().flatten() {
      apply_to_tweet(tweet, &contexts);
   }
}

async fn resolve_for(
   state: &AppState,
   users: Vec<&User>,
) -> Option<BTreeMap<String, AccountContext>> {
   let mut names = users
      .into_iter()
      .map(|user| user.username.to_lowercase())
      .filter(|name| !name.is_empty())
      .collect::<Vec<_>>();
   names.sort_unstable();
   names.dedup();
   if names.is_empty() {
      return None;
   }
   let contexts = account_contexts(state, &names).await;
   (!contexts.is_empty()).then_some(contexts)
}

fn apply_to_tweet(tweet: &mut Tweet, contexts: &BTreeMap<String, AccountContext>) {
   let apply = |user: &mut User| {
      if let Some(context) = contexts.get(&user.username.to_lowercase()) {
         user.account_based_in.clone_from(&context.account_based_in);
         user
            .connection_source
            .clone_from(&context.connection_source);
         user.location_accurate = context.location_accurate;
      }
   };
   apply(&mut tweet.user);
   if let Some(quote) = tweet.quote.as_deref_mut() {
      apply(&mut quote.user);
   }
   if let Some(retweet) = tweet.retweet.as_deref_mut() {
      apply(&mut retweet.user);
      if let Some(quote) = retweet.quote.as_deref_mut() {
         apply(&mut quote.user);
      }
   }
}

fn tweet_users(tweet: &Tweet) -> Vec<&User> {
   let mut users = vec![&tweet.user];
   if let Some(quote) = tweet.quote.as_deref() {
      users.push(&quote.user);
   }
   if let Some(retweet) = tweet.retweet.as_deref() {
      users.push(&retweet.user);
      if let Some(quote) = retweet.quote.as_deref() {
         users.push(&quote.user);
      }
   }
   users
}

fn tweets(conversation: &Conversation) -> impl Iterator<Item = &Tweet> {
   iter::once(&conversation.tweet)
      .chain(&conversation.before.content)
      .chain(&conversation.after.content)
      .chain(
         conversation
            .replies
            .content
            .iter()
            .flat_map(|chain| chain.content.iter()),
      )
}

fn tweets_mut(conversation: &mut Conversation) -> impl Iterator<Item = &mut Tweet> {
   iter::once(&mut conversation.tweet)
      .chain(&mut conversation.before.content)
      .chain(&mut conversation.after.content)
      .chain(
         conversation
            .replies
            .content
            .iter_mut()
            .flat_map(|chain| chain.content.iter_mut()),
      )
}

async fn community_contexts(
   state: &AppState,
   usernames: &[String],
) -> BTreeMap<String, AccountContext> {
   if !state.config.config.x_posed_community_cache || usernames.is_empty() {
      return BTreeMap::new();
   }
   state
      .api
      .get_community_account_contexts(usernames)
      .await
      .unwrap_or_else(|err| {
         tracing::debug!("X-Posed community cache unavailable: {err}");
         BTreeMap::new()
      })
}

/// Fetch one tweet, reusing either the conversation or tweet cache.
pub async fn get_cached_tweet(state: &AppState, id: &str) -> Result<Tweet> {
   if let Some(conversation) = state
      .cache
      .get::<Conversation>(&cache_keys::conversation(id))
   {
      return Ok(conversation.tweet);
   }
   let key = cache_keys::tweet(id);
   if let Some(tweet) = state.cache.get::<Tweet>(&key) {
      return Ok(tweet);
   }
   let tweet = state.api.get_tweet(id).await?;
   state.cache.set(&key, &tweet, ttl::DEFAULT);
   Ok(tweet)
}

/// Translate a tweet once per backend and cache the result.
pub async fn get_cached_translation(
   state: &AppState,
   tweet: &Tweet,
   kagi_token: Option<&str>,
) -> Result<Translation> {
   let backend = if kagi_token.is_some() {
      "kagi"
   } else {
      "strato"
   };
   let key = cache_keys::translation(tweet.id, backend);
   if let Some(translation) = state.cache.get::<Translation>(&key) {
      return Ok(translation);
   }
   let _permit = state.translation_limiter.acquire().await?;
   let translation = state.api.translate_auto(tweet, kagi_token).await?;
   if !translation.text.is_empty() {
      state.cache.set(&key, &translation, ttl::TRANSLATION);
   }
   Ok(translation)
}

/// Build an RSS response with `Content-Type` and `Min-Id` headers.
pub fn rss_response(rss: String, tweets: &[Tweet]) -> Response {
   let min_id = tweets.iter().map(|tweet| tweet.id).min();
   rss_response_with_min_id(rss, min_id)
}

/// Build an RSS response when the caller already has the minimum ID.
pub fn rss_response_with_min_id(rss: String, min_id: Option<i64>) -> Response {
   let mut response = (
      [(header::CONTENT_TYPE, "application/rss+xml; charset=utf-8")],
      rss,
   )
      .into_response();
   if let Some(id) = min_id {
      response.headers_mut().insert(
         header::HeaderName::from_static("min-id"),
         header::HeaderValue::from(id),
      );
   }
   response
}

/// Check RSS cache and return early if hit.
pub fn check_rss_cache(state: &AppState, key: &str) -> Option<Response> {
   let cached = state.cache.get::<CachedRss>(key)?;
   Some(rss_response_with_min_id(cached.body, cached.min_id))
}

/// Cache an RSS result.
pub fn cache_rss(state: &AppState, key: &str, rss: &str, min_id: Option<i64>) {
   let cached = CachedRss {
      body: rss.to_owned(),
      min_id,
   };
   state
      .cache
      .set(key, &cached, state.config.cache.rss_minutes * 60);
}

/// Extract tweet groups and cursor from a Timeline.
/// Preserves conversation grouping. Each inner [`Vec<Tweet>`] is a conversation
/// thread (parent → reply chain) from a single `profile-conversation-*` entry.
pub fn extract_timeline(timeline: Timeline) -> (Vec<Tweets>, Option<String>) {
   (timeline.content, timeline.bottom)
}

/// Render a failed API request.
///
/// Running out of this caller's budget, running out of the instance's upstream
/// quota, and an actual fault are three different things to the reader, so they
/// get distinct statuses and messages rather than one generic error page.
pub fn api_error(config: &Config, err: &Error) -> Response {
   api_error_titled(config, err, "Error")
}

/// [`api_error`] with the title used when the failure has no better one.
pub fn api_error_titled(config: &Config, err: &Error, generic: &str) -> Response {
   let (status, title, message) = classify_error(err, generic);

   if matches!(err, Error::TransientUpstream) {
      tracing::warn!(error = ?err, "upstream empty");
   } else if status.is_server_error() {
      tracing::error!(error = ?err, "request failed");
   } else if status == StatusCode::TOO_MANY_REQUESTS {
      tracing::debug!(error = ?err, "request refused");
   }

   let markup = layout::render_error(config, title, message);
   (status, Html(markup.into_string())).into_response()
}

fn classify_error<'a>(err: &'a Error, generic: &'a str) -> (StatusCode, &'a str, &'a str) {
   let (status, title, message) = err.presentation();
   if status == StatusCode::INTERNAL_SERVER_ERROR {
      (status, generic, message)
   } else {
      (status, title, message)
   }
}
