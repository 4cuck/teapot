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
