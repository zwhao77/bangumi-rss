# bangumi-rss HTTP API Contract

Target contract for the HTTP API. The implementation may lag behind this
document — see [§7 Diff from current implementation](#7-diff-from-current-implementation)
for the migration checklist.

## 1. Conventions

- Base path: `/api`. Static UI assets (`/`, `/style.css`) live outside `/api`.
- Content: JSON (`application/json`, UTF-8) for API endpoints; errors use
  `application/problem+json` (see §3). File streaming is binary.
  Request bodies must send `Content-Type: application/json`.
- Field naming: `snake_case` throughout.
- Identifiers: feed IDs are UUID strings; infohashes are hex strings.
- Auth applies to **every** request, including UI assets (see §2).
- No URL versioning (`/api/v1/...`): this is a self-hosted single-user tool;
  `/api` is the namespace.

## 2. Authentication

HTTP Basic Auth, configured via `AUTH_USERNAME` / `AUTH_PASSWORD` env vars.
An empty `AUTH_USERNAME` disables auth entirely.

- Credentials are compared in **constant time** (no early-exit string equality).
- Rejection: `401` with `WWW-Authenticate: Basic realm="bangumi-rss"` and a
  Problem Details body (media type `application/problem+json`):

  ```json
  {
    "type": "urn:bangumi-rss:problems:unauthorized",
    "title": "Unauthorized",
    "status": 401,
    "detail": "missing or invalid credentials"
  }
  ```

- The plaintext password travels on every request — run behind a TLS reverse
  proxy when exposed beyond localhost.

## 3. Response format

**Success and failure are distinguished exclusively by the HTTP status code.**
There is no `success` field in any response body.

### Success (2xx)

- Body: `{"data": <T>}`. Fire-and-forget actions (`202`) additionally carry a
  `"message"` for human-readable context.
- Every success carries `data` (possibly `null`) — no empty-body successes.

### Failure (non-2xx)

- RFC 9457 (Problem Details for HTTP APIs), media type
  `application/problem+json`.
- Standard members:

  | Member | Type | Requirement | Meaning |
  |---|---|---|---|
  | `type` | string (URI) | required | stable problem-class identifier, see §6 registry |
  | `title` | string | required | short, stable summary of the problem class |
  | `status` | int | required | HTTP status code (informational — the HTTP status line is authoritative) |
  | `detail` | string | required | per-occurrence, human-readable specifics |
  | `instance` | string (URI) | optional | occurrence identifier for log correlation |

- Extension members are allowed (e.g. a validation-details array).

- Example:

  ```json
  {
    "type": "urn:bangumi-rss:problems:invalid-filter",
    "title": "Invalid feed filter",
    "status": 400,
    "detail": "invalid regex: regex parse error at ... unclosed group",
    "instance": "/api/requests/8f3a1c2e"
  }
  ```

## 4. Status codes

| Code | Meaning | Used by |
|---|---|---|
| `200` | OK | reads, lists, updates, deletes |
| `201` | Created | `POST /api/feeds` |
| `202` | Accepted (fire-and-forget) | `refresh` / `poll` / `notify/test` |
| `206` | Partial Content | `GET /api/files/{infohash}` with a `Range` header |
| `400` | Bad request (bad JSON, URL, filter, or query param) | all JSON endpoints |
| `401` | Unauthorized | auth gate |
| `404` | Unknown route / feed / subject / episode — Problem Details body | all endpoints |
| `405` | Method not allowed, with `Allow` header | router |
| `500` | Internal error / upstream failure | preview, bangumi |
| `503` | Downloader unavailable / logic thread busy | health, api queries |

`2xx` = success, `4xx` = client error, `5xx` = server error. Clients branch on
the HTTP status code (`resp.ok` in browsers), never on body fields.

## 5. Endpoints

### `POST /api/feeds` — create feed

Request:

```json
{
  "url": "https://example.com/rss/anime.xml",
  "name": "示例番剧",
  "season": 3,
  "bangumi_info": null,
  "filter": {"include": ["catchplay"], "exclude": ["sample"], "regex": "S03E0[1-6]"}
}
```

`url`, `name`, `season` required; `bangumi_info`/`filter` optional (default:
empty filter = accept all). The filter is validated before creation.

- `201` → `{"data": "<feed uuid>"}`
- `400` → invalid URL, missing name, or invalid filter (Problem Details)

### `POST /api/feeds/preview` — structured RSS preview

Request: `{"url": "<rss url>"}`

- `200` → `{"data": FeedPreview}` — see §6 schemas
- `400` → invalid URL; `500` → RSS fetch / Bangumi lookup failed

### `POST /api/feeds/refresh` — trigger RSS refresh for all feeds

- `202` → `{"data": null, "message": "RSS refresh triggered"}`

### `GET /api/feeds` — list feeds

- `200` → `{"data": [FeedInfo]}` — see §6 schemas

### `PUT /api/feeds/{id}` — update feed

Request body mirrors create; **omitting `filter` keeps the current filter**.

- `200` → `{"data": "updated"}`
- `400` → invalid filter; `404` → unknown feed id

### `DELETE /api/feeds/{id}` — remove feed

- `200` → `{"data": "feed <uuid> removed"}`
- `404` → unknown feed id

### `GET /api/files/{infohash}` — stream episode file

Binary response with `Range` support (`Accept-Ranges`, `Content-Range`,
`206 Partial Content` for ranged requests).

- `200` / `206` → file bytes
- `404` → unknown infohash (Problem Details)

### `GET /api/downloads` — list downloads

- `200` → `{"data": [DownloadInfo]}` — see §6 schemas

### `POST /api/downloads/refresh` / `POST /api/downloads/poll`

Fire-and-forget downloader actions.

- `202` → `{"data": null, "message": "..."}`

### `GET /api/bangumi/subjects/{id}` — Bangumi subject detail

- `200` → `{"data": BangumiInfo}`
- `404` → not found; `500` → upstream error

### `GET /api/bangumi/search?name=` — Bangumi search + detail

- `200` → `{"data": BangumiInfo}`
- `400` → missing `name`; `404` → no detail; `500` → upstream error

### `GET /api/health` — health check (probes downloader)

- `200` → `{"data": null}`
- `503` → downloader unavailable (Problem Details)

### `POST /api/notify/test` — send test notifications

- `202` → `{"data": null, "message": "test notifications sent"}`

## 6. Schemas

```text
FeedFilter   { include: [string], exclude: [string], regex: string? }
             // include = AND (all words, case-insensitive substring);
             // exclude = OR (any word rejects); regex = advanced (must match)

FeedInfo     { id: uuid, name: string, url: string, season: int,
               bangumi_info: BangumiInfo?, filter: FeedFilter }

FeedPreview  { suggested_name: string, suggested_season: int,
               latest_episode: int?, group: string?,
               sample_titles: [string], bangumi_info: BangumiInfo? }

BangumiInfo  { bangumi_id: int, name_cn: string, name: string,
               summary: string, eps_count: int?, rating: float?,
               score_count: int?, air_date: string, image_url: string,
               rank: int?, air_weekday: int? }

DownloadInfo { infohash: string, feed_name: string, season: int,
               state: "downloading"|"seeding"|"waiting"|"paused"|
                      "checking"|"completed"|"failed"|"removed",
               progress: float, speed: int, size: int, name: string }
```

### Problem type registry

`type` values used by the server. `about:blank` is acceptable when no class
fits, but the registry below is preferred.

| `type` | HTTP | `title` |
|---|---|---|
| `urn:bangumi-rss:problems:invalid-request` | 400 | Invalid request |
| `urn:bangumi-rss:problems:invalid-filter` | 400 | Invalid feed filter |
| `urn:bangumi-rss:problems:unauthorized` | 401 | Unauthorized |
| `urn:bangumi-rss:problems:not-found` | 404 | Not found |
| `urn:bangumi-rss:problems:method-not-allowed` | 405 | Method not allowed |
| `urn:bangumi-rss:problems:range-not-satisfiable` | 416 | Range Not Satisfiable |
| `urn:bangumi-rss:problems:internal` | 500 | Internal error |
| `urn:bangumi-rss:problems:upstream-error` | 500 | Upstream error |
| `urn:bangumi-rss:problems:service-unavailable` | 503 | Service unavailable |

## 7. Diff from current implementation

Migration checklist — each item is a deviation the code must be aligned to:

1. **Remove the `success` field** from every response. Success bodies become
   `{"data": <T>}` (actions add `message`).
2. **Errors become RFC 9457 Problem Details** (`application/problem+json`,
   `type`/`title`/`status`/`detail`, §6 registry) — replaces
   `{success: false, code, message}` and the current plain-text `401`.
3. **Frontend**: `api()` helper branches on `resp.ok` (no `success` reads);
   update the four direct `success` call sites (subscribe result, bangumi
   enrich/modal ×2, health, notify/test) and remove the `resp` scope bug in
   `previewFeed`.
4. **`201` for `POST /api/feeds`** (currently `200`).
5. **`202` for refresh / poll / notify-test** (currently `200`).
6. **JSON Problem Details `404`** for unknown routes (currently empty 404).
7. **`405` + `Allow`** for wrong methods (currently falls through to 404).
8. **Auth hardening**: constant-time credential comparison; `401` body becomes
   Problem Details (currently plain text).
9. **`e2e.sh`**: parse `{"data": ...}` and Problem Details; assert status codes
   (currently assumes a bare array / `message`-carried feed id).
10. **`ApiResult` type** is removed from the response path (or reduced to an
    internal type) — the `success`/`code`/`message` serializer no longer
    matches any wire format.

## 8. Explicitly out of scope

- HATEOAS, OAuth2, API versioning, pagination, idempotency keys — not
  warranted for a self-hosted single-user tool.
- Cookie/session auth — rejected in favour of Basic Auth.
- A `success` flag in the envelope — explicitly removed: HTTP status codes are
  the single source of truth.
