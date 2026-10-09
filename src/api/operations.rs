//! GraphQL operations as X's web client ships them right now.
//!
//! X gives an operation a new query id on most deploys and, some time later,
//! answers the old id with an empty 404, which a visitor sees as "X did not
//! return a result". The web client carries each operation's live id and the
//! feature names it sends, and the logged-in homepage carries the feature
//! values, so both are read from there. Compiled ids cover the time before the
//! first read and the operations the web client does not have.

use std::{
   collections::{
      BTreeMap,
      HashMap,
      HashSet,
   },
   sync::{
      Arc,
      LazyLock,
   },
};

use axum::http::HeaderMap;
use regex::Regex;
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::Value;
use tokio::{
   fs,
   sync::{
      RwLock,
      Semaphore,
   },
   task::JoinSet,
};

use super::http::{
   Egress,
   HttpClient,
};

/// Operations sent from cookie sessions that the web client also sends.
/// `UserResultByIdQuery`, `MediaTimelineV2`, `AboutAccountQuery` and the
/// replies V2 query come from the Android app and keep their compiled ids.
const WEB_OPERATIONS: &[&str] = &[
   "UserByScreenName",
   "UserTweets",
   "UserTweetsAndReplies",
   "UserRepostsTimeline",
   "UserMedia",
   "TweetDetail",
   "TweetEditHistory",
   "Retweeters",
   "SearchTimeline",
   "ListByRestId",
   "ListBySlug",
   "ListLatestTweetsTimeline",
   "ListMembers",
   "AudioSpaceById",
];

const CLIENT_WEB: &str = "https://abs.twimg.com/responsive-web/client-web";
/// Lazy chunks fetched at once while looking for an operation.
const CHUNK_FETCHES: usize = 16;
const STATE_MARKER: &str = "window.__INITIAL_STATE__=";

static OPERATION_RE: LazyLock<Regex> = LazyLock::new(|| {
   Regex::new(
      r#"queryId:"([A-Za-z0-9_-]+)",operationName:"([A-Za-z0-9_]+)",operationType:"\w+",metadata:\{featureSwitches:\[([^\]]*)\]"#,
   )
   .unwrap()
});
static QUOTED_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]*)""#).unwrap());
static MAIN_BUNDLE_RE: LazyLock<Regex> = LazyLock::new(|| {
   Regex::new(r"https://abs\.twimg\.com/responsive-web/client-web/main\.[0-9a-f]+\.js").unwrap()
});
/// webpack's chunk URL function on the homepage:
/// `(({id:"name",…})[e]||e)+"."+({id:"hash",…})[e]+"a.js"`.
static CHUNK_MAP_RE: LazyLock<Regex> = LazyLock::new(|| {
   Regex::new(r#"\(\(\{([^}]*)\}\)\[\w+\]\|\|\w+\)\+"\."\+\(\{([^}]*)\}\)\[\w+\]\+"a\.js""#)
      .unwrap()
});
static CHUNK_ENTRY_RE: LazyLock<Regex> =
   LazyLock::new(|| Regex::new(r#"(\d+):"([^"]+)""#).unwrap());

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Operation {
   id:       String,
   /// Feature names, in the order the web client sends them.
   names:    Vec<String>,
   /// The `features` query value: `names` with the homepage's values.
   #[serde(default)]
   features: String,
   /// Lazy chunk id and URL it was found in. `None` means the main bundle.
   #[serde(default, skip_serializing_if = "Option::is_none")]
   chunk:    Option<(String, String)>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
   /// Main bundle the entries were read from.
   bundle:     String,
   operations: BTreeMap<String, Operation>,
   /// Operations no chunk of this build had. They are not searched for again
   /// until X deploys.
   #[serde(default)]
   absent:     Vec<String>,
}

/// Live query ids and features, shared by every request.
#[derive(Clone)]
pub struct Operations {
   book: Arc<RwLock<Book>>,
   path: Arc<str>,
}

impl Operations {
   /// Restore what an earlier run read, so a restart starts on live ids.
   pub fn load() -> Self {
      let path = std::env::var("TEAPOT_GRAPHQL_OPS_FILE")
         .unwrap_or_else(|_| "graphql-ops.json".to_owned());
      let book = std::fs::read_to_string(&path)
         .ok()
         .and_then(|text| serde_json::from_str(&text).ok())
         .unwrap_or_default();
      Self {
         book: Arc::new(RwLock::new(book)),
         path: path.into(),
      }
   }

   /// `endpoint` (`id/Name`) and its features as the web client sends them
   /// now, or `None` while this operation has not been read from X's client.
   pub async fn resolve(&self, endpoint: &str) -> Option<(String, String)> {
      let name = endpoint
         .rsplit_once('/')
         .map_or(endpoint, |(_, name)| name);
      let book = self.book.read().await;
      let operation = book.operations.get(name)?;
      Some((format!("{}/{name}", operation.id), operation.features.clone()))
   }

   /// Read the operations from X's client, given the logged-in homepage.
   ///
   /// Script chunks are fetched only when X has deployed since the last read;
   /// otherwise only the feature values are refreshed.
   pub async fn learn(&self, scripts: &HttpClient, egress: &Egress, home: &str) -> Result<(), String> {
      let bundle = MAIN_BUNDLE_RE
         .find(home)
         .map(|found| found.as_str().to_owned())
         .ok_or("no main bundle on the homepage")?;
      let values = feature_values(home).ok_or("no feature switches in the homepage state")?;
      let chunks = chunk_urls(home);

      let (previous, mut absent, same_build) = {
         let book = self.book.read().await;
         (book.operations.clone(), book.absent.clone(), book.bundle == bundle)
      };
      let mut operations = previous.clone();

      if !same_build {
         let js = fetch_script(scripts, egress, &bundle).await?;
         let main = operations_in(&js);
         if main.is_empty() {
            return Err(format!("no GraphQL operations in {bundle}"));
         }
         operations.retain(|_, operation| operation.chunk.is_some());
         for (name, operation) in main {
            if WEB_OPERATIONS.contains(&name.as_str()) {
               operations.insert(name, operation);
            }
         }
         absent.clear();
      }

      // A lazy entry stands while its chunk is the same file.
      let mut hint = HashSet::new();
      operations.retain(|_, operation| {
         match &operation.chunk {
            None => true,
            Some((id, url)) => {
               hint.insert(id.clone());
               chunks.get(id) == Some(url)
            },
         }
      });

      let missing: Vec<&str> = WEB_OPERATIONS
         .iter()
         .copied()
         .filter(|name| !operations.contains_key(*name) && !absent.iter().any(|gone| gone == name))
         .collect();
      if !missing.is_empty() {
         let found = find_lazy(scripts, egress, &chunks, &missing, &hint).await;
         for name in &missing {
            match found.get(*name) {
               Some(operation) => {
                  operations.insert((*name).to_owned(), operation.clone());
               },
               None => {
                  tracing::warn!(
                     operation = name,
                     "X's web client does not have this GraphQL operation; keeping the compiled id"
                  );
                  absent.push((*name).to_owned());
               },
            }
         }
      }

      for operation in operations.values_mut() {
         operation.features = features_json(&operation.names, &values);
      }
      for (name, operation) in &operations {
         if let Some(old) = previous.get(name)
            && old.id != operation.id
         {
            tracing::info!(
               operation = %name,
               from = %old.id,
               to = %operation.id,
               "X moved a GraphQL operation to a new query id"
            );
         }
      }
      if !same_build {
         tracing::info!(
            bundle = %bundle,
            operations = operations.len(),
            "read GraphQL operations from X's web client"
         );
      }

      let text = {
         let mut book = self.book.write().await;
         book.bundle = bundle;
         book.operations = operations;
         book.absent = absent;
         serde_json::to_string_pretty(&*book).map_err(|err| err.to_string())?
      };
      let temporary = format!("{}.tmp", self.path);
      if let Err(err) = fs::write(&temporary, text).await {
         tracing::warn!("could not save GraphQL operations: {err}");
      } else if let Err(err) = fs::rename(&temporary, &*self.path).await {
         tracing::warn!("could not save GraphQL operations: {err}");
      }
      Ok(())
   }
}

/// Fetch one of the web client's scripts the way x.com loads it.
pub(super) async fn fetch_script(
   scripts: &HttpClient,
   egress: &Egress,
   url: &str,
) -> Result<String, String> {
   let response = scripts
      .get_on(url, &HeaderMap::new(), Some(egress))
      .await
      .map_err(|err| format!("fetch {url}: {err}"))?;
   let status = response.status();
   if !status.is_success() {
      return Err(format!("fetch {url}: {status}"));
   }
   response
      .text()
      .await
      .map_err(|err| format!("read {url}: {err}"))
}

/// Look for `wanted` in the lazy chunks. Chunks that held them before are
/// tried first, then every other chunk, stopping once all are found.
async fn find_lazy(
   scripts: &HttpClient,
   egress: &Egress,
   chunks: &HashMap<String, String>,
   wanted: &[&str],
   hint: &HashSet<String>,
) -> HashMap<String, Operation> {
   let (first, rest): (Vec<_>, Vec<_>) = chunks
      .iter()
      .map(|(id, url)| (id.clone(), url.clone()))
      .partition(|(id, _)| hint.contains(id));
   let mut found = scan(scripts, egress, first, wanted).await;
   if wanted.iter().any(|name| !found.contains_key(*name)) {
      let still: Vec<&str> = wanted
         .iter()
         .copied()
         .filter(|name| !found.contains_key(*name))
         .collect();
      found.extend(scan(scripts, egress, rest, &still).await);
   }
   found
}

async fn scan(
   scripts: &HttpClient,
   egress: &Egress,
   chunks: Vec<(String, String)>,
   wanted: &[&str],
) -> HashMap<String, Operation> {
   let mut found = HashMap::new();
   if chunks.is_empty() || wanted.is_empty() {
      return found;
   }
   let permits = Arc::new(Semaphore::new(CHUNK_FETCHES));
   let mut tasks = JoinSet::new();
   for (id, url) in chunks {
      let scripts = scripts.clone();
      let egress = egress.clone();
      let permits = Arc::clone(&permits);
      tasks.spawn(async move {
         let _permit = permits.acquire_owned().await.ok()?;
         let js = fetch_script(&scripts, &egress, &url).await.ok()?;
         Some((id, url, operations_in(&js)))
      });
   }
   while let Some(joined) = tasks.join_next().await {
      let Ok(Some((id, url, operations))) = joined else {
         continue;
      };
      for (name, mut operation) in operations {
         if wanted.contains(&name.as_str()) && !found.contains_key(&name) {
            operation.chunk = Some((id.clone(), url.clone()));
            found.insert(name, operation);
         }
      }
      if wanted.iter().all(|name| found.contains_key(*name)) {
         tasks.abort_all();
         break;
      }
   }
   found
}

/// Every operation definition in a script, by operation name.
fn operations_in(js: &str) -> HashMap<String, Operation> {
   OPERATION_RE
      .captures_iter(js)
      .map(|caps| {
         let names = QUOTED_RE
            .captures_iter(&caps[3])
            .map(|name| name[1].to_owned())
            .collect();
         (
            caps[2].to_owned(),
            Operation {
               id: caps[1].to_owned(),
               names,
               features: String::new(),
               chunk: None,
            },
         )
      })
      .collect()
}

/// Lazy chunk URLs on the homepage, by webpack chunk id.
fn chunk_urls(home: &str) -> HashMap<String, String> {
   let Some(caps) = CHUNK_MAP_RE.captures(home) else {
      return HashMap::new();
   };
   let names: HashMap<&str, &str> = CHUNK_ENTRY_RE
      .captures_iter(caps.get(1).map_or("", |found| found.as_str()))
      .filter_map(|entry| Some((entry.get(1)?.as_str(), entry.get(2)?.as_str())))
      .collect();
   CHUNK_ENTRY_RE
      .captures_iter(caps.get(2).map_or("", |found| found.as_str()))
      .filter_map(|entry| {
         let id = entry.get(1)?.as_str();
         let hash = entry.get(2)?.as_str();
         let file = names.get(id).copied().unwrap_or(id);
         Some((id.to_owned(), format!("{CLIENT_WEB}/{file}.{hash}a.js")))
      })
      .collect()
}

/// Feature switch values from the homepage state, the account's own config
/// over the defaults.
fn feature_values(home: &str) -> Option<HashMap<String, bool>> {
   let start = home.find(STATE_MARKER)? + STATE_MARKER.len();
   let state: Value = serde_json::Deserializer::from_str(home.get(start..)?)
      .into_iter::<Value>()
      .next()?
      .ok()?;
   let switches = state.get("featureSwitch")?;
   let mut values = HashMap::new();
   for config in [switches.get("defaultConfig"), switches.pointer("/user/config")] {
      let Some(Value::Object(config)) = config else {
         continue;
      };
      for (name, entry) in config {
         if let Some(value) = entry.get("value") {
            values.insert(name.clone(), value.as_bool() == Some(true));
         }
      }
   }
   (!values.is_empty()).then_some(values)
}

/// `names` as the JSON object the web client sends, in its order. A name the
/// page has no value for goes out as `false`, as it does from Chrome.
fn features_json(names: &[String], values: &HashMap<String, bool>) -> String {
   let mut out = String::from("{");
   for (index, name) in names.iter().enumerate() {
      if index > 0 {
         out.push(',');
      }
      out.push_str(&Value::String(name.clone()).to_string());
      out.push(':');
      out.push_str(if values.get(name).copied().unwrap_or(false) {
         "true"
      } else {
         "false"
      });
   }
   out.push('}');
   out
}

#[cfg(test)]
mod tests {
   use super::*;

   const MAIN: &str = r#"447423(e){e.exports={queryId:"ph2fARFabkwfxqmSKQ1OPw",operationName:"SearchTimeline",operationType:"query",metadata:{featureSwitches:["rweb_video_screen_enabled","rweb_conversational_replies_downvote_enabled","articles_preview_enabled"],fieldToggles:[]}}},1(e){e.exports={queryId:"AMIBMjtxEEATh4z8V9GtRg",operationName:"UserByScreenName",operationType:"query",metadata:{featureSwitches:[],fieldToggles:["withPayments"]}}}"#;

   const HOME: &str = r#"<script src="https://abs.twimg.com/responsive-web/client-web/main.53d8c7f0078e1404a.js"></script><script>window.__INITIAL_STATE__={"featureSwitch":{"defaultConfig":{"rweb_video_screen_enabled":{"value":true},"articles_preview_enabled":{"value":false}},"user":{"config":{"rweb_video_screen_enabled":{"value":false},"articles_preview_enabled":{"value":true}}}}};window.__META_DATA__={};</script><script>t.u=e=>(({346:"bundle.NotABot",59924:"ondemand.s"})[e]||e)+"."+({346:"fa6be5fd47951aff",573:"a8b840f60117a2f3",59924:"26791a5c30a3bd19"})[e]+"a.js"</script>"#;

   #[test]
   fn reads_operations_and_their_feature_names() {
      let operations = operations_in(MAIN);
      let search = &operations["SearchTimeline"];
      assert_eq!(search.id, "ph2fARFabkwfxqmSKQ1OPw");
      assert_eq!(search.names, vec![
         "rweb_video_screen_enabled",
         "rweb_conversational_replies_downvote_enabled",
         "articles_preview_enabled",
      ]);
      assert!(operations["UserByScreenName"].names.is_empty());
   }

   #[test]
   fn features_take_the_account_config_in_web_order() {
      let values = feature_values(HOME).unwrap();
      let names = operations_in(MAIN).remove("SearchTimeline").unwrap().names;
      assert_eq!(
         features_json(&names, &values),
         r#"{"rweb_video_screen_enabled":false,"rweb_conversational_replies_downvote_enabled":false,"articles_preview_enabled":true}"#
      );
   }

   #[test]
   fn finds_the_main_bundle_and_chunk_urls() {
      assert_eq!(
         MAIN_BUNDLE_RE.find(HOME).unwrap().as_str(),
         "https://abs.twimg.com/responsive-web/client-web/main.53d8c7f0078e1404a.js"
      );
      let chunks = chunk_urls(HOME);
      assert_eq!(
         chunks["59924"],
         "https://abs.twimg.com/responsive-web/client-web/ondemand.s.26791a5c30a3bd19a.js"
      );
      assert_eq!(
         chunks["573"],
         "https://abs.twimg.com/responsive-web/client-web/573.a8b840f60117a2f3a.js"
      );
   }

   #[tokio::test]
   async fn resolve_swaps_in_the_live_id() {
      let operations = Operations {
         book: Arc::new(RwLock::new(Book::default())),
         path: "unused".into(),
      };
      assert!(
         operations
            .resolve("hyPfJYJ_XAtDYoslQc-Rgg/SearchTimeline")
            .await
            .is_none()
      );
      operations.book.write().await.operations.insert(
         "SearchTimeline".into(),
         Operation {
            id: "ph2fARFabkwfxqmSKQ1OPw".into(),
            names: Vec::new(),
            features: "{}".into(),
            chunk: None,
         },
      );
      let (endpoint, features) = operations
         .resolve("hyPfJYJ_XAtDYoslQc-Rgg/SearchTimeline")
         .await
         .unwrap();
      assert_eq!(endpoint, "ph2fARFabkwfxqmSKQ1OPw/SearchTimeline");
      assert_eq!(features, "{}");
   }
}
