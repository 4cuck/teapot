use super::{
   GalleryPhoto,
   PhotoRail,
   Query,
   Tweet,
   User,
};

/// Thumbnails shown in a profile header.
const PHOTO_RAIL_LEN: usize = 10;

pub type Tweets = Vec<Tweet>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineKind {
   #[default]
   Tweets,
   Replies,
   Media,
   Search,
}

/// Generic paginated result.
#[derive(Debug, Clone, Default)]
pub struct PaginatedResult<T> {
   pub content:   Vec<T>,
   pub top:       Option<String>,
   pub bottom:    Option<String>,
   pub beginning: bool,
   pub query:     Query,
}

/// A chain of tweets (for conversation threads).
#[derive(Debug, Clone, Default)]
pub struct Chain {
   pub content:  Tweets,
   pub has_more: bool,
   pub cursor:   Option<String>,
}

impl Chain {
   pub fn contains(&self, tweet: &Tweet) -> bool {
      self.content.iter().any(|entry| entry.id == tweet.id)
   }
}

/// A conversation view (tweet + context + replies).
#[derive(Debug, Clone, Default)]
pub struct Conversation {
   pub tweet:   Tweet,
   pub before:  Chain,
   pub after:   Chain,
   pub replies: PaginatedResult<Chain>,
}

/// User timeline.
pub type Timeline = PaginatedResult<Tweets>;

impl Timeline {
   /// Drop groups whose first post is a reply to someone else.
   ///
   /// Used when the profile timeline is served from `UserTweetsAndReplies`,
   /// which mixes those replies into the posts tab.
   pub fn keep_posts(&mut self) {
      self.content
         .retain(|group| group.first().is_none_or(|tweet| !tweet.replies_to_someone_else()));
   }

   /// One thumbnail per post of a media timeline: first photo, else the video,
   /// GIF or card image.
   #[must_use]
   pub fn photo_rail(&self) -> PhotoRail {
      let mut photos = Vec::new();
      for tweet in self.content.iter().flatten() {
         let url = tweet
            .photos
            .first()
            .map(|photo| photo.url.as_str())
            .or_else(|| tweet.video.as_ref().map(|video| video.thumb.as_str()))
            .or_else(|| tweet.gifs.first().map(|gif| gif.thumb.as_str()))
            .or_else(|| tweet.card.as_ref().map(|card| card.image.as_str()))
            .filter(|url| !url.is_empty());
         if let Some(url) = url {
            photos.push(GalleryPhoto {
               url:      url.to_owned(),
               tweet_id: tweet.id.to_string(),
               color:    String::new(),
            });
            if photos.len() >= PHOTO_RAIL_LEN {
               break;
            }
         }
      }
      photos
   }
}

/// User profile with tweets and photo rail.
#[derive(Debug, Clone, Default)]
pub struct Profile {
   pub user:       User,
   pub photo_rail: PhotoRail,
   pub pinned:     Option<Tweet>,
   pub tweets:     Timeline,
   /// The media page the rail was cut from, when this profile had to fetch
   /// one. Handed to the caller so the media tab can be served from it; not
   /// part of the cached profile.
   pub media:      Option<Timeline>,
}

/// Edit history for a tweet.
#[derive(Debug, Clone, Default)]
pub struct EditHistory {
   pub latest:  Tweet,
   pub history: Tweets,
}

#[cfg(test)]
mod tests {
   use super::*;

   fn tweet(author: &str, reply_to: &str) -> Tweet {
      Tweet {
         user: User {
            username: author.to_owned(),
            ..User::default()
         },
         reply: (!reply_to.is_empty()).then(|| vec![reply_to.to_owned()]).unwrap_or_default(),
         text: "x".to_owned(),
         ..Tweet::default()
      }
   }

   #[test]
   fn keep_posts_drops_replies_to_other_people() {
      let mut timeline = Timeline {
         content: vec![
            vec![tweet("alice", "")],
            vec![tweet("alice", "alice")],
            vec![tweet("alice", "bob")],
         ],
         ..Timeline::default()
      };
      timeline.keep_posts();
      assert_eq!(timeline.content.len(), 2);
      assert!(timeline.content[0][0].reply.is_empty());
      assert_eq!(timeline.content[1][0].reply[0], "alice");
   }
}

/// Twitter list.
#[derive(Debug, Clone, Default)]
pub struct List {
   pub id:          String,
   pub name:        String,
   pub user_id:     String,
   pub username:    String,
   pub description: String,
   pub members:     i32,
   pub banner:      String,
}
