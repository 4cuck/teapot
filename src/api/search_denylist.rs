//! Queries X's Search Content Control tool has already rejected.
//!
//! The list is learned from `QueryDenylistedFailure` and kept on disk, so the
//! next search for the same string is rewritten before it is sent.

use std::{
   collections::HashSet,
   fs,
   path::{
      Path,
      PathBuf,
   },
   sync::{
      Arc,
      RwLock,
   },
};

/// `include:nativeretweets` is what Shitter prepends so the raw query is no
/// longer the string on X's list. Result rows are deduped when rendered.
pub fn bypass_denylist(query: &str) -> String {
   let query = query.trim();
   if query.is_empty() || query.contains("include:nativeretweets") {
      return query.to_owned();
   }
   format!("include:nativeretweets {query}")
}

#[derive(Clone)]
pub struct SearchDenylist {
   path:  Arc<PathBuf>,
   inner: Arc<RwLock<HashSet<String>>>,
}

impl SearchDenylist {
   pub fn load(path: impl Into<PathBuf>) -> Self {
      let path = path.into();
      let inner = fs::read_to_string(&path)
         .ok()
         .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
         .unwrap_or_default()
         .into_iter()
         .map(|query| query.trim().to_owned())
         .filter(|query| !query.is_empty())
         .collect();
      Self {
         path:  Arc::new(path),
         inner: Arc::new(RwLock::new(inner)),
      }
   }

   pub fn contains(&self, query: &str) -> bool {
      self.inner
         .read()
         .is_ok_and(|set| set.contains(query.trim()))
   }

   /// The string to send. A known-bad query is rewritten unless that rewrite
   /// has also been rejected.
   pub fn prepare(&self, query: &str) -> String {
      let query = query.trim();
      if !self.contains(query) {
         return query.to_owned();
      }
      let rewritten = bypass_denylist(query);
      if rewritten != query && !self.contains(&rewritten) {
         rewritten
      } else {
         query.to_owned()
      }
   }

   pub fn remember(&self, query: &str) {
      let query = query.trim();
      if query.is_empty() {
         return;
      }
      let Ok(mut set) = self.inner.write() else {
         return;
      };
      if !set.insert(query.to_owned()) {
         return;
      }
      if self.path.as_os_str().is_empty() {
         return;
      }
      let mut listed = set.iter().cloned().collect::<Vec<_>>();
      drop(set);
      listed.sort();
      let Ok(body) = serde_json::to_string_pretty(&listed) else {
         return;
      };
      let temporary = Path::new(&*self.path).with_extension("json.tmp");
      if fs::write(&temporary, body).is_ok() {
         let _ = fs::rename(&temporary, &*self.path);
      }
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn a_rejected_query_is_rewritten_on_the_next_search_and_saved() {
      let path = std::env::temp_dir().join(format!(
         "teapot-search-denylist-{}-{}.json",
         std::process::id(),
         "remember"
      ));
      let _ = fs::remove_file(&path);
      let list = SearchDenylist::load(&path);
      assert_eq!(list.prepare("teen"), "teen");

      list.remember("teen");
      assert_eq!(list.prepare("teen"), "include:nativeretweets teen");

      let loaded = SearchDenylist::load(&path);
      assert!(loaded.contains("teen"));
      assert_eq!(loaded.prepare("teen"), "include:nativeretweets teen");
      let _ = fs::remove_file(&path);
   }

   #[test]
   fn a_rewrite_that_was_also_rejected_is_not_sent_again() {
      let list = SearchDenylist::load(PathBuf::new());
      list.remember("teen");
      list.remember("include:nativeretweets teen");
      assert_eq!(list.prepare("teen"), "teen");
   }
}
