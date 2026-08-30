//! Static pages served by teapawt itself.

use axum::{
   Router,
   extract::State,
   response::{
      Html,
      IntoResponse,
      Redirect,
   },
   routing::get,
};
use axum_extra::extract::CookieJar;
use maud::{
   PreEscaped,
   html,
};

use crate::{
   AppState,
   types::Prefs,
   views::{
      layout::PageLayout,
      search as search_view,
   },
};

const HOME_DESCRIPTION: &str = "nitter.cf is a public Nitter replacement — a privacy-focused Twitter/X frontend. nitter.net shut down; this instance is a drop-in successor.";
const ABOUT_DESCRIPTION: &str = "About nitter.cf, a public Nitter replacement. After nitter.net shut down, this teapawt instance lets you browse Twitter/X without JavaScript or tracking.";
const HOME_JSON_LD: &str = r#"{"@context":"https://schema.org","@type":"WebSite","name":"nitter.cf","alternateName":["Nitter","teapawt","xitter.cf"],"url":"https://nitter.cf/","description":"Public Nitter replacement. nitter.net shut down; nitter.cf is a privacy-focused Twitter/X frontend.","sameAs":["https://xitter.cf"]}"#;

/// The pages teapawt serves itself rather than fetching from X.
pub fn router() -> Router<AppState> {
   Router::new()
      .route("/", get(home))
      .route("/about", get(about))
      .route("/explore", get(|| async { Redirect::to("/about") }))
      .route("/help", get(|| async { Redirect::to("/about") }))
}

async fn home(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
   let prefs = Prefs::from_cookies(&jar, &state.config);
   let content = search_view::render_search_page();
   let head = html! {
       link rel="canonical" href="https://nitter.cf/";
       meta property="og:type" content="website";
       meta property="og:url" content="https://nitter.cf/";
       script type="application/ld+json" {
           (PreEscaped(HOME_JSON_LD))
       }
   };

   let markup = PageLayout::new(&state.config, "nitter.cf — Nitter replacement", content)
      .description(HOME_DESCRIPTION)
      .prefs(&prefs)
      .head_extra(&head)
      .render();
   Html(markup.into_string())
}

async fn about(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
   let prefs = Prefs::from_cookies(&jar, &state.config);
   let head = html! {
       link rel="canonical" href="https://nitter.cf/about";
       meta property="og:type" content="website";
       meta property="og:url" content="https://nitter.cf/about";
   };
   let content = html! {
       div class="overlay-panel" {
           h1 { "About nitter.cf" }

           p {
               strong { "nitter.cf" }
               " is a public "
               a href="https://github.com/zedeus/nitter" { "Nitter" }
               " replacement — a privacy-focused Twitter/X frontend. "
               "nitter.net shut down; this instance is meant as a drop-in successor. "
               "Same URL style ("
               code { "/username" }
               ", "
               code { "/username/status/id" }
               "), no JavaScript required, and Twitter never sees your IP or fingerprint. "
               "Also available at "
               a href="https://xitter.cf" { "xitter.cf" }
               "."
           }

           p {
               "This site runs "
               a href="https://github.com/4cuck/teapot" { "teapawt" }
               ", a privacy-focused Twitter/X frontend forked from "
               a href="https://github.com/amaanq/teapot" { "teapot" }
               "."
           }

           ul {
               li { "No third-party JavaScript or ads" }
               li { "All requests go through the backend, client never talks to Twitter" }
               li { "Prevents Twitter from tracking your IP or JavaScript fingerprint" }
               li { "Uses Twitter's unofficial API (no developer account required)" }
               li { "Lightweight" }
               li { "RSS feeds" }
               li { "Themes" }
               li { "Mobile support (responsive design)" }
               li { "AGPLv3 licensed, no proprietary instances permitted" }
           }

           p {
               "Upstream teapot's GitHub wiki contains "
               a href="https://github.com/amaanq/teapot/wiki/Instances" { "instances" }
               " and "
               a href="https://github.com/amaanq/teapot/wiki/Extensions" { "browser extensions" }
               " maintained by the community."
           }

           h2 { "Why use teapawt?" }

           p {
               "It's impossible to use Twitter without JavaScript enabled, and as of 2024 you need to sign up. "
               "For privacy-minded folks, preventing JavaScript analytics and IP-based tracking is important, "
               "but apart from using a VPN and uBlock/uMatrix, it's impossible. Despite being behind a VPN and "
               "using heavy-duty adblockers, you can get accurately tracked with your "
               a href="https://restoreprivacy.com/browser-fingerprinting/" { "browser's fingerprint" }
               ", "
               a href="https://noscriptfingerprint.com/" { "no JavaScript required" }
               ". This all became particularly important after Twitter "
               a href="https://www.eff.org/deeplinks/2020/04/twitter-removes-privacy-option-and-shows-why-we-need-strong-privacy-laws" { "removed the ability" }
               " for users to control whether their data gets sent to advertisers."
           }

           p {
               "Using an instance of teapawt (hosted on a VPS for example), you can browse Twitter without "
               "JavaScript while retaining your privacy. In addition to respecting your privacy, teapawt is on "
               "average around 15 times lighter than Twitter, and in most cases serves pages faster "
               "(eg. timelines load 2-4x faster)."
           }

           h2 { "Instance info" }
           p {
               "Version: teapawt " (env!("CARGO_PKG_VERSION"))
           }
       }
   };

   let markup = PageLayout::new(&state.config, "About nitter.cf", content)
      .description(ABOUT_DESCRIPTION)
      .prefs(&prefs)
      .head_extra(&head)
      .render();
   Html(markup.into_string())
}
