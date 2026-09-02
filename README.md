# teapawt

A privacy-focused Twitter/X frontend written in Rust. This is a
[teapot](https://github.com/amaanq/teapot) fork with its own identity —
same engine, different name and logo.

Public instance: [nitter.cf](https://nitter.cf) (also [xitter.cf](https://xitter.cf)).

## Features

- **Privacy-focused**: No third-party JavaScript, tracking, ads, or remote fonts
- **RSS feeds**: Subscribe to any user's tweets
- **SOCKS5 exits for GraphQL**: Pin each cookie session to one proxy port so
  its apparent IP stays stable
- **In-process cache**: First-page search, profiles, and stale-while-revalidate
  so repeats do not wait on X
- **FxEmbed-style Discord embeds**:
  - Multiple images displayed in carousel on mobile
  - Videos play directly in the embed
  - Proper image dimensions for better layout
  - ActivityPub JSON endpoint for rich embeds

## Requirements

- Rust 1.96.1+
- Twitter/X session tokens for API access
- Optional: SOCKS5 endpoints if X blocks the server's own IP

## Building

```bash
cargo build --release
```

The crate and binary stay named `teapot` so existing configs and process
managers keep working.

## Configuration

1. Copy the example config:

```bash
cp config/teapot.example.toml config/teapot.toml
```

2. Edit `config/teapot.toml`. Set `title = "teapawt"` (or whatever you want
   shown in the navbar).

3. Behind nginx or another TLS terminator, set `https = true` and
   `publicPort = 443` so prefs bookmarks, RSS, and `og:image` links omit the
   listen port (`8471` or whatever `port` is).

4. Create a sessions file with your Twitter/X credentials:

```bash
cp sessions.example.jsonl sessions.jsonl
# Edit sessions.jsonl with your auth_token and ct0 from browser cookies
```

Cookie sessions use `auth_token` and `ct0`. OAuth sessions use
`oauth_token` and `oauth_secret`. See `sessions.example.jsonl` for the JSONL
shape.

**Never commit** a real `sessions.jsonl`, `config/teapot.toml`, `proxies.txt`,
or `session-proxies.json`. Those paths are gitignored.

### Proxies

X GraphQL (search, profiles, tweets) can go out through SOCKS5. Media (`/pic`,
`/video`, `/gif`) stays on the optional HTTP `proxy` and does not use the
SOCKS list.

```bash
cp proxies.example.txt proxies.txt
# one host:port:user:pass per line
```

```toml
# config/teapot.toml
socksProxiesFile   = "proxies.txt"
sessionProxiesFile = "session-proxies.json"
```

All lines must share one hostname (different ports are the rotating exits).
On startup teapawt DNS-probes that host, keeps the lowest-ping A record, and
pins each session id to one port. Pins are rewritten to `sessionProxiesFile`
as `{ "sessionId": port }` only — credentials stay in `proxies.txt`.

`apiProxy` is an HTTP/HTTPS proxy for GraphQL when you are not using SOCKS.
It is ignored when `socksProxiesFile` is set.

Tunnels to `x.com` are HTTP/1.1 over TLS with keep-alive, `TCP_NODELAY`, and
rustls session resumption. Idle tunnels are reused for a short window so a
second search does not pay a new SOCKS + TLS handshake.

### Rate limits and cache

- `maxConcurrentReqs` is **per cookie session**, not global.
- `clientBudget` caps how fast one visitor IP can spend uncached X calls.
  Cache hits, media, and static files are free. SearchTimeline costs 2 tokens.
- First-page search is cached for 60 seconds, then served stale for 4 more
  minutes while a refresh runs.
- Profiles are cached for 5 minutes, then served stale for 20 more minutes.
- Search result pages seed user ids and prefetch a few profiles so a click
  often hits cache.

Empty HTTP 404s from SearchTimeline are treated as a brief X/WAF blip
(`TransientUpstream`) and retried on another session. Dead cookies are 401,
or 403 with code 326 — not those empty 404s.

Some queries fail with X error 214 (`Query is denylisted in Search Content
Control tool`). That is X's platform denylist, not a teapawt setting.

## Running

```bash
# Development
cargo run

# Production
./target/release/teapot
```

The server listens on `http://localhost:8080` by default (`address` / `port`
in the config).

## Getting Twitter Sessions

To use the Twitter API, you need session tokens from a logged-in Twitter account:

1. Log into Twitter/X in your browser
2. Open Developer Tools (F12) → Application → Cookies
3. Copy the values of `auth_token` and `ct0`
4. Add them to `sessions.jsonl`

## Project Structure

```
teapawt/
├── src/
│   ├── main.rs          # Entry point
│   ├── config.rs        # Configuration
│   ├── error.rs         # Error types
│   ├── types/           # Data structures (User, Tweet, Timeline, etc.)
│   ├── api/             # Twitter API client, SOCKS5 pool, OAuth, parsing
│   ├── cache/           # In-process cache (TTL + stale-while-revalidate)
│   ├── routes/          # HTTP route handlers
│   ├── views/           # Maud HTML templates
│   └── utils/           # Utilities (HMAC, formatters)
├── public/              # Static assets (CSS, JS, fonts, logo)
├── config/              # teapot.example.toml only
├── proxies.example.txt  # Dummy SOCKS5 list
├── sessions.example.jsonl
└── Cargo.toml
```

## Embed Improvements

This rewrite includes FxEmbed-style improvements for Discord embeds:

### Multiple Images

Discord can display all images from a tweet in a carousel. This works via an ActivityPub JSON endpoint at `/users/{username}/statuses/{id}` that returns media attachments.

### Video Playback

Videos can be played directly in Discord embeds using `twitter:player` meta tags:

- `twitter:player` - Embed URL
- `twitter:player:stream` - Direct MP4 URL
- `twitter:player:width/height` - Dimensions

### Image Dimensions

All images include `og:image:width` and `og:image:height` meta tags for proper layout.

## License

[GNU Affero General Public License v3.0](LICENSE)
