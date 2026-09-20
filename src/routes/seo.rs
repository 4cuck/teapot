//! robots.txt and sitemaps. Every public path is crawlable.

use std::collections::BTreeSet;

use axum::{
   Router,
   extract::State,
   http::{
      HeaderMap,
      header,
   },
   response::{
      IntoResponse,
      Response,
   },
   routing::get,
};
use time::OffsetDateTime;

use crate::{
   AppState,
   config::Config,
};

/// Well-known public accounts so crawlers have many entry points on day one.
const SEED_PROFILES: &[&str] = &[
   "elonmusk",
   "nasa",
   "spacex",
   "tesla",
   "openai",
   "google",
   "microsoft",
   "apple",
   "meta",
   "amazon",
   "nvidia",
   "intel",
   "ibm",
   "samsung",
   "sony",
   "nintendo",
   "playstation",
   "xbox",
   "steam",
   "github",
   "gitlab",
   "linux",
   "ubuntu",
   "debian",
   "archlinux",
   "mozilla",
   "firefox",
   "torproject",
   "wikimedia",
   "wikipedia",
   "internetarchive",
   "eff",
   "fsf",
   "signalapp",
   "protonprivacy",
   "mullvadnet",
   "cloudflare",
   "fastly",
   "awscloud",
   "azure",
   "googlecloud",
   "vercel",
   "netlify",
   "digitalocean",
   "youtube",
   "twitch",
   "discord",
   "reddit",
   "instagram",
   "tiktok",
   "linkedin",
   "pinterest",
   "snapchat",
   "whatsapp",
   "telegram",
   "spotify",
   "netflix",
   "disney",
   "hulu",
   "bbc",
   "bbcworld",
   "cnn",
   "nytimes",
   "washingtonpost",
   "wsj",
   "reuters",
   "AP",
   "Bloomberg",
   "theeconomist",
   "guardian",
   "npr",
   "pbs",
   "aljazeera",
   "dwnews",
   "france24",
   "abc",
   "cbsnews",
   "nbcnews",
   "foxnews",
   "politico",
   "axios",
   "thehill",
   "TIME",
   "Newsweek",
   "Forbes",
   "wired",
   "techcrunch",
   "verge",
   "engadget",
   "arstechnica",
   "mashable",
   "gizmodo",
   "cnet",
   "zdnet",
   "venturebeat",
   "ProductHunt",
   "ycombinator",
   "paulg",
   "sama",
   "gdb",
   "karpathy",
   "ylecun",
   "fchollet",
   "jeffgeerling",
   "nixcraft",
   "rustlang",
   "golang",
   "Python",
   "nodejs",
   "typescript",
   "reactjs",
   "vuejs",
   "angular",
   "kubernetesio",
   "hashicorp",
   "anthropicai",
   "midjourney",
   "StabilityAI",
   "huggingface",
   "kaggle",
   "AndrewYNg",
   "lexfridman",
   "joerogan",
   "MKBHD",
   "MrBeast",
   "PewDiePie",
   "xQc",
   "Ninja",
   "shroud",
   "pokimane",
   "HasanAbi",
   "asmongold",
   "moistcr1tikal",
   "penguinz0",
   "Markiplier",
   "jacksepticeye",
   "jack",
   "ev",
   "biz",
   "naval",
   "pmarca",
   "balajis",
   "VitalikButerin",
   "cz_binance",
   "saylor",
   "aantonop",
   "DocumentingBTC",
   "whale_alert",
   "coinbase",
   "binance",
   "krakenfx",
   "POTUS",
   "WhiteHouse",
   "UN",
   "WHO",
   "UNICEF",
   "RedCross",
   "Amnesty",
   "HRW",
   "WorldBank",
   "IMFNews",
   "NATO",
   "EU_Commission",
   "govuk",
   "NASAJPL",
   "esa",
   "ISRO",
   "BlueOrigin",
   "VirginGalactic",
   "HubbleSite",
   "NASAWebb",
   "CERN",
   "Fermilab",
   "NSF",
   "NIH",
   "CDCgov",
   "FDA",
   "USDA",
   "NOAA",
   "NWS",
   "USGS",
   "NPS",
   "NatGeo",
   "Discovery",
   "WWF",
   "Greenpeace",
   "NRDC",
   "SierraClub",
   "Oceana",
   "TeamSeas",
   "TeamTrees",
   "MarkRober",
   "veritasium",
   "smartereveryday",
   "minutephysics",
   "3blue1brown",
   "numberphile",
   "standupmaths",
   "tomscott",
   "cgpgrey",
   "Vsauce",
   "SciShow",
   "crashcourse",
   "TEDTalks",
   "TED",
   "KhanAcademy",
   "Coursera",
   "edXonline",
   "Udacity",
   "Udemy",
   "Skillshare",
   "Duolingo",
   "arXiv",
   "Nature",
   "Science",
   "TheLancet",
   "NEJM",
   "BMJ_latest",
   "JAMA_current",
   "WellcomeTrust",
   "gatesfoundation",
   "BarackObama",
   "MichelleObama",
   "BernieSanders",
   "AOC",
   "SenWarren",
   "TheDemocrats",
   "GOP",
   "UKLabour",
   "Conservatives",
   "LibDems",
   "theSNP",
   "SkyNews",
   "ITVNews",
   "Channel4News",
   "FinancialTimes",
   "TheTimes",
   "Telegraph",
   "NewScientist",
   "sciam",
   "PopSci",
   "SPACEcom",
   "UniverseToday",
   "SkyandTelescope",
   "SETIInstitute",
   "JAXA_en",
   "UKSpaceAgency",
   "DLR_en",
   "CNES",
   "NintendoAmerica",
   "XboxP3",
   "ValveSoftware",
   "EpicGames",
   "Fortnite",
   "RiotGames",
   "LeagueOfLegends",
   "VALORANT",
   "PlayApex",
   "CallofDuty",
   "Battlefield",
   "NBA",
   "NFL",
   "MLB",
   "NHL",
   "MLS",
   "FIFA",
   "UEFA",
   "PremierLeague",
   "LaLigaEN",
   "SerieA_EN",
   "F1",
   "MotoGP",
   "INDYCAR",
   "NASCAR",
   "WWE",
   "UFC",
   "ESPN",
   "SportsCenter",
   "BleacherReport",
   "SkySports",
   "CBSSports",
   "FOXSports",
   "NBCSports",
   "TheAthletic",
   "complex",
   "Pitchfork",
   "RollingStone",
   "Billboard",
   "NME",
   "Consequence",
   "AppleMusic",
   "YouTubeMusic",
   "SoundCloud",
   "Bandcamp",
   "Tidal",
   "Deezer",
   "Pandora",
   "iHeartRadio",
   "BBCRadio1",
   "BBCRadio2",
   "NPRMusic",
   "KEXP",
   "ColbertLateShow",
   "jimmyfallon",
   "JimmyKimmelLive",
   "sethmeyers",
   "ConanOBrien",
   "SNL",
   "NBC",
   "ABCnetwork",
   "CBSTV",
   "DisneyPlus",
   "Max",
   "ParamountPlus",
   "AppleTV",
   "Crunchyroll",
   "VIZMedia",
   "StudioGhibli",
   "Pixar",
   "DisneyAnimation",
   "Marvel",
   "MarvelStudios",
   "DCComics",
   "WarnerBros",
   "UniversalPics",
   "ParamountPics",
   "SonyPictures",
   "Lionsgate",
   "A24",
   "Criterion",
   "MUBI",
   "Letterboxd",
   "RottenTomatoes",
   "IMDb",
   "Metacritic",
   "Variety",
   "THR",
   "DEADLINE",
   "EW",
   "People",
   "TMZ",
   "PopCrave",
   "PopBase",
   "BillboardCharts",
   "OfficialCharts",
   "ORICON",
   "SpotifyCharts",
];

/// Only the profile root. Tab URLs (`/media`, `/with_replies`, …) are linked
/// from the profile itself; listing them here 4×’d the file until Cloudflare
/// 520’d Googlebot.
const PROFILE_PATHS: &[&str] = &[""];
const XML_CONTENT_TYPE: &str = "text/xml; charset=utf-8";

pub fn router() -> Router<AppState> {
   Router::new()
      .route("/robots.txt", get(robots_txt))
      .route("/sitemap.xml", get(sitemap_index))
      .route("/sitemap-static.xml", get(sitemap_static))
      .route("/sitemap-profiles.xml", get(sitemap_profiles))
}

/// Sitemap and robots loc URLs always use the configured public host
/// (`nitter.cf`) so www / xitter responses don't split Google's property.
fn public_origin(_headers: &HeaderMap, config: &Config) -> String {
   config.url_prefix().trim_end_matches('/').to_owned()
}

fn text_response(body: String, content_type: &'static str) -> Response {
   (
      [
         (header::CONTENT_TYPE, content_type),
         (
            header::CACHE_CONTROL,
            "public, max-age=300, s-maxage=86400, stale-while-revalidate=604800",
         ),
         (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
      ],
      body,
   )
      .into_response()
}

async fn robots_txt(State(state): State<AppState>, headers: HeaderMap) -> Response {
   let origin = public_origin(&headers, &state.config);
   let body = format!(
      "\
# nitter.cf / xitter.cf — public Twitter/X frontend. Crawl everything.
# Google, Bing, Yandex, DuckDuckGo, Baidu, and every other crawler are welcome.

User-agent: *
Allow: /

User-agent: Googlebot
Allow: /

User-agent: Googlebot-Image
Allow: /

User-agent: Googlebot-News
Allow: /

User-agent: Googlebot-Video
Allow: /

User-agent: AdsBot-Google
Allow: /

User-agent: APIs-Google
Allow: /

User-agent: Mediapartners-Google
Allow: /

User-agent: Bingbot
Allow: /

User-agent: bingbot
Allow: /

User-agent: BingPreview
Allow: /

User-agent: msnbot
Allow: /

User-agent: AdIdxBot
Allow: /

User-agent: Yandex
Allow: /

User-agent: YandexBot
Allow: /

User-agent: YandexImages
Allow: /

User-agent: YandexVideo
Allow: /

User-agent: YandexMedia
Allow: /

User-agent: YandexNews
Allow: /

User-agent: DuckDuckBot
Allow: /

User-agent: DuckAssistBot
Allow: /

User-agent: Slurp
Allow: /

User-agent: Yahoo
Allow: /

User-agent: Baiduspider
Allow: /

User-agent: Baiduspider-image
Allow: /

User-agent: Baiduspider-video
Allow: /

User-agent: Sogou
Allow: /

User-agent: Sogou web spider
Allow: /

User-agent: Applebot
Allow: /

User-agent: Applebot-Extended
Allow: /

User-agent: FacebookBot
Allow: /

User-agent: facebookexternalhit
Allow: /

User-agent: Facebot
Allow: /

User-agent: Twitterbot
Allow: /

User-agent: LinkedInBot
Allow: /

User-agent: Pinterest
Allow: /

User-agent: Pinterestbot
Allow: /

User-agent: ia_archiver
Allow: /

User-agent: archive.org_bot
Allow: /

User-agent: ia_archiver-web.archive.org
Allow: /

User-agent: Qwantify
Allow: /

User-agent: SeznamBot
Allow: /

User-agent: MojeekBot
Allow: /

User-agent: PetalBot
Allow: /

User-agent: Bytespider
Allow: /

User-agent: GPTBot
Allow: /

User-agent: ChatGPT-User
Allow: /

User-agent: ClaudeBot
Allow: /

User-agent: anthropic-ai
Allow: /

User-agent: PerplexityBot
Allow: /

User-agent: YouBot
Allow: /

Sitemap: {origin}/sitemap.xml
"
   );
   text_response(body, "text/plain; charset=utf-8")
}

fn today() -> String {
   OffsetDateTime::now_utc()
      .date()
      .to_string()
}

fn url_entry(loc: &str, lastmod: &str, changefreq: &str, priority: &str) -> String {
   format!(
      "  <url>\n    <loc>{loc}</loc>\n    <lastmod>{lastmod}</lastmod>\n    \
       <changefreq>{changefreq}</changefreq>\n    <priority>{priority}</priority>\n  </url>\n"
   )
}

fn xml_escape(value: &str) -> String {
   value
      .replace('&', "&amp;")
      .replace('<', "&lt;")
      .replace('>', "&gt;")
      .replace('"', "&quot;")
}

async fn sitemap_index(State(state): State<AppState>, headers: HeaderMap) -> Response {
   let origin = public_origin(&headers, &state.config);
   let lastmod = today();
   let body = format!(
      "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">
  <sitemap>
    <loc>{origin}/sitemap-static.xml</loc>
    <lastmod>{lastmod}</lastmod>
  </sitemap>
  <sitemap>
    <loc>{origin}/sitemap-profiles.xml</loc>
    <lastmod>{lastmod}</lastmod>
  </sitemap>
</sitemapindex>
"
   );
   text_response(body, XML_CONTENT_TYPE)
}

async fn sitemap_static(State(state): State<AppState>, headers: HeaderMap) -> Response {
   let origin = public_origin(&headers, &state.config);
   let lastmod = today();
   let static_paths = [
      ("/", "daily", "1.0"),
      ("/about", "weekly", "0.9"),
      ("/search", "hourly", "0.8"),
   ];
   let mut body =
      String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
   for (path, changefreq, priority) in static_paths {
      body.push_str(&url_entry(
         &format!("{origin}{path}"),
         &lastmod,
         changefreq,
         priority,
      ));
   }
   body.push_str("</urlset>\n");
   text_response(body, XML_CONTENT_TYPE)
}

fn collect_profile_usernames(state: &AppState) -> BTreeSet<String> {
   let mut names = BTreeSet::new();
   for handle in SEED_PROFILES {
      if let Some(clean) = sanitize_handle(handle) {
         names.insert(clean);
      }
   }
   for key in state.cache.keys_with_prefix("p:") {
      if let Some(name) = key.strip_prefix("p:")
         && let Some(clean) = sanitize_handle(name)
      {
         names.insert(clean);
      }
   }
   for key in state.cache.keys_with_prefix("u:") {
      if let Some(name) = key.strip_prefix("u:")
         && let Some(clean) = sanitize_handle(name)
      {
         names.insert(clean);
      }
   }
   for key in state.cache.keys_with_prefix("unid:") {
      if let Some(name) = key.strip_prefix("unid:")
         && let Some(clean) = sanitize_handle(name)
      {
         names.insert(clean);
      }
   }
   names
}

fn sanitize_handle(raw: &str) -> Option<String> {
   let handle = raw.trim().trim_start_matches('@').to_ascii_lowercase();
   if handle.is_empty() || handle.len() > 15 {
      return None;
   }
   handle
      .chars()
      .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
      .then_some(handle)
}

async fn sitemap_profiles(State(state): State<AppState>, headers: HeaderMap) -> Response {
   let origin = public_origin(&headers, &state.config);
   let lastmod = today();
   let names = collect_profile_usernames(&state);
   let mut body =
      String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
   // Stay under the 50_000 URL sitemap limit.
   let mut count = 0usize;
   'outer: for name in names {
      for suffix in PROFILE_PATHS {
         if count >= 49_000 {
            break 'outer;
         }
         let loc = xml_escape(&format!("{origin}/{name}{suffix}"));
         let priority = if suffix.is_empty() { "0.8" } else { "0.6" };
         body.push_str(&url_entry(&loc, &lastmod, "hourly", priority));
         count += 1;
      }
   }
   body.push_str("</urlset>\n");
   text_response(body, XML_CONTENT_TYPE)
}
