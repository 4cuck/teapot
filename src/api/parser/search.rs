use super::user::parse_user_object;
use crate::{
   api::schema::{
      InstructionType,
      ListData,
      ListMembersData,
      RetweetersData,
      SearchTimelineData,
   },
   types::{
      List,
      PaginatedResult,
      Query,
      User,
   },
};

/// Parse user search results.
pub fn parse_user_search(data: &SearchTimelineData) -> PaginatedResult<User> {
   let raw_instructions = data.instructions();

   let mut users = Vec::new();
   let mut bottom_cursor = None;

   for instruction in raw_instructions {
      if instruction.instruction_type != Some(InstructionType::TimelineAddEntries) {
         continue;
      }

      for entry in instruction.entries.as_deref().unwrap_or_default() {
         let entry_id = entry.entry_id_str();

         if entry_id.starts_with("user-") {
            if let Some(user_result) = entry.user_result()
               && let Ok(user) = parse_user_object(user_result)
            {
               users.push(user);
            }
         } else if entry_id.starts_with("cursor-bottom-") {
            bottom_cursor = entry.cursor_value().map(str::to_owned);
         }
      }
   }

   PaginatedResult {
      content:   users,
      top:       None,
      bottom:    bottom_cursor,
      beginning: false,
      query:     Query::default(),
   }
}

/// Parse a list from typed `ListData`.
pub fn parse_list(raw: &ListData) -> List {
   let id = raw
      .id_str
      .as_deref()
      .or(raw.rest_id.as_deref())
      .unwrap_or_default()
      .to_owned();
   let name = raw.name.clone().unwrap_or_default();
   let description = raw.description.clone().unwrap_or_default();
   let members = raw.member_count;
   let subscribers = raw.subscriber_count;

   // Extract user info from nested user_results
   let owner = raw
      .user_results
      .as_ref()
      .and_then(|nr| nr.result.as_ref())
      .and_then(|user_data| parse_user_object(user_data).ok());
   let (user_id, username) = owner
      .as_ref()
      .map(|user| (user.id.clone(), user.username.clone()))
      .unwrap_or_default();

   let banner = raw
      .custom_banner_media
      .clone()
      .or_else(|| raw.default_banner_media.clone())
      .or_else(|| raw.banner_url.clone())
      .unwrap_or_default();
   // Only a list's own banner is the search thumbnail. The stock default
   // banner is a shared pattern, and X draws a colored icon in its place.
   let cover = raw.custom_banner_media.clone().unwrap_or_default();

   // Facepile files are the tiny `_mini` cut. The row only needs a slightly
   // larger version of the same photo.
   let pictures: Vec<String> = raw
      .facepile_urls
      .iter()
      .filter(|url| !url.is_empty())
      .take(3)
      .map(|url| url.replace("_mini", "_normal"))
      .collect();

   List {
      id,
      name,
      user_id,
      username,
      description,
      members,
      subscribers,
      banner,
      cover,
      members_text: raw.members_context.clone(),
      followers: raw.followers_context.clone(),
      pictures,
   }
}

fn push_list(lists: &mut Vec<List>, raw: &ListData) {
   let parsed = parse_list(raw);
   if !parsed.id.is_empty() {
      lists.push(parsed);
   }
}

/// Parse the Lists tab of search (`product=Lists`).
///
/// Hits arrive as a module (`list-search-*` with `items`), and a single list
/// can also sit directly on the entry.
pub fn parse_list_search(data: &SearchTimelineData) -> PaginatedResult<List> {
   let raw_instructions = data.instructions();

   let mut lists = Vec::new();
   let mut bottom_cursor = None;

   for instruction in raw_instructions {
      if instruction.instruction_type != Some(InstructionType::TimelineAddEntries) {
         continue;
      }

      for entry in instruction.entries.as_deref().unwrap_or_default() {
         let entry_id = entry.entry_id_str();

         if let Some(list) = entry.list_result() {
            push_list(&mut lists, list);
         }
         for item in entry.items() {
            if let Some(list) = item.list_result() {
               push_list(&mut lists, list);
            }
         }
         if entry_id.starts_with("cursor-bottom-") {
            bottom_cursor = entry.cursor_value().map(str::to_owned);
         }
      }
   }

   PaginatedResult {
      content:   lists,
      top:       None,
      bottom:    bottom_cursor,
      beginning: false,
      query:     Query::default(),
   }
}

/// Parse list members from API response.
pub fn parse_list_members(data: &ListMembersData) -> PaginatedResult<User> {
   let raw_instructions = data.instructions();

   let mut users = Vec::new();
   let mut top_cursor = None;
   let mut bottom_cursor = None;

   for instruction in raw_instructions {
      if instruction.instruction_type != Some(InstructionType::TimelineAddEntries) {
         continue;
      }

      for entry in instruction.entries.as_deref().unwrap_or_default() {
         let entry_id = entry.entry_id_str();

         if entry_id.starts_with("user-") {
            if let Some(user_result) = entry.user_result()
               && let Ok(user) = parse_user_object(user_result)
            {
               users.push(user);
            }
         } else if entry_id.starts_with("cursor-bottom-") {
            bottom_cursor = entry.cursor_value().map(str::to_owned);
         } else if entry_id.starts_with("cursor-top-") {
            top_cursor = entry.cursor_value().map(str::to_owned);
         }
      }
   }

   PaginatedResult {
      content:   users,
      top:       top_cursor,
      bottom:    bottom_cursor,
      beginning: false,
      query:     Query::default(),
   }
}

/// Parse retweeters from API response (same structure as list members).
pub fn parse_retweeters(data: &RetweetersData) -> PaginatedResult<User> {
   let raw_instructions = data.instructions();

   let mut users = Vec::new();
   let mut top_cursor = None;
   let mut bottom_cursor = None;

   for instruction in raw_instructions {
      if instruction.instruction_type != Some(InstructionType::TimelineAddEntries) {
         continue;
      }

      for entry in instruction.entries.as_deref().unwrap_or_default() {
         let entry_id = entry.entry_id_str();

         if entry_id.starts_with("user-") {
            if let Some(user_result) = entry.user_result()
               && let Ok(user) = parse_user_object(user_result)
            {
               users.push(user);
            }
         } else if entry_id.starts_with("cursor-bottom-") {
            bottom_cursor = entry.cursor_value().map(str::to_owned);
         } else if entry_id.starts_with("cursor-top-") {
            top_cursor = entry.cursor_value().map(str::to_owned);
         }
      }
   }

   PaginatedResult {
      content:   users,
      top:       top_cursor,
      bottom:    bottom_cursor,
      beginning: false,
      query:     Query::default(),
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn list_search_reads_a_timeline_twitter_list() {
      let raw = r#"{
         "search_by_raw_query": {
            "search_timeline": {
               "timeline": {
                  "instructions": [{
                     "type": "TimelineAddEntries",
                     "entries": [{
                        "entryId": "list-search-0",
                        "content": {
                           "items": [{
                              "entryId": "list-42",
                              "item": {
                                 "itemContent": {
                                    "list": {
                                       "id_str": "42",
                                       "name": "News",
                                       "description": "Headlines",
                                 "member_count": 12,
                                 "subscriber_count": 3,
                                 "facepile_urls": [
                                    "https://pbs.twimg.com/profile_images/1/a_mini.jpg",
                                    "https://pbs.twimg.com/profile_images/2/b_mini.jpg"
                                 ],
                                       "custom_banner_media": {
                                          "media_info": { "original_img_url": "https://pbs.twimg.com/list_banner.jpg" }
                                       },
                                       "user_results": {
                                          "result": {
                                             "rest_id": "7",
                                             "legacy": { "screen_name": "alice", "name": "Alice" }
                                          }
                                       }
                                    }
                                 }
                              }
                           }]
                        }
                     }, {
                        "entryId": "cursor-bottom-0",
                        "content": { "value": "next" }
                     }]
                  }]
               }
            }
         }
      }"#;
      let data: SearchTimelineData = serde_json::from_str(raw).unwrap();
      let page = parse_list_search(&data);
      assert_eq!(page.content.len(), 1);
      assert_eq!(page.content[0].id, "42");
      assert_eq!(page.content[0].name, "News");
      assert_eq!(page.content[0].username, "alice");
      assert_eq!(page.content[0].members, 12);
      assert_eq!(page.content[0].subscribers, 3);
      assert_eq!(page.content[0].banner, "https://pbs.twimg.com/list_banner.jpg");
      assert_eq!(
         page.content[0].pictures,
         vec![
            "https://pbs.twimg.com/profile_images/1/a_normal.jpg".to_owned(),
            "https://pbs.twimg.com/profile_images/2/b_normal.jpg".to_owned(),
         ]
      );
      assert_eq!(page.bottom.as_deref(), Some("next"));
   }
}
