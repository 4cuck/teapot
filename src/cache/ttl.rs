/// Standard TTL for user/profile/tweet data.
pub const DEFAULT: u64 = 5 * 60; // 5 minutes

/// Serve a stale profile this long after [`DEFAULT`] while a refresh runs.
pub const DEFAULT_STALE: u64 = 20 * 60;

/// A profile "load more" page. Repeats of the same cursor must not spend
/// another `UserTweets` call.
pub const PROFILE_CURSOR: u64 = 3 * 60;

/// Keep that page servable after [`PROFILE_CURSOR`] without refetching.
pub const PROFILE_CURSOR_STALE: u64 = 15 * 60;

/// First-page search. Short so results stay fresh but repeats skip X.
pub const SEARCH: u64 = 60;

/// Serve a stale first search page this long after [`SEARCH`].
pub const SEARCH_STALE: u64 = 4 * 60;

/// Long TTL for immutable mappings like user ID -> username.
pub const USER_ID_MAPPING: u64 = 24 * 60 * 60; // 1 day

/// Translations are immutable for a tweet revision.
pub const TRANSLATION: u64 = 24 * 60 * 60; // 1 day

pub const ACCOUNT_CONTEXT: u64 = 60 * 24 * 60 * 60; // 60 days
