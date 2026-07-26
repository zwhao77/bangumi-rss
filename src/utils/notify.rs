//! Template-based notification rendering — pure functions, no I/O.
//!
//! Flow: Notification → fields map → template replacement → (body, content_type)

use std::str::FromStr;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::types::Notification;

/// Template type — determines Content-Type and escape strategy.
#[derive(Debug, Clone)]
pub enum TemplateType {
    Json(String),
    Form(String),
}

/// Resolved webhook configuration — ready for the executor to use.
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub url: String,
    pub template: TemplateType,
    /// Optional separate template for `Failed` notifications.
    /// Falls back to built-in `render_failed` if `None`.
    pub error_template: Option<TemplateType>,
}

// ── Built-in presets ──

/// Named notification format presets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Preset {
    Bark,
    Gotify,
    Serverchan,
}

impl Preset {
    fn template(self) -> TemplateType {
        match self {
            Self::Bark => TemplateType::Json(
                r#"{"title":"{{anime_name}} 第{{episode}}集","body":"{{summary}}","icon":"{{image_url}}","group":"bangumi-rss"}"#
                    .into(),
            ),
            Self::Gotify => TemplateType::Json(
                r#"{"title":"{{anime_name}} 第{{episode}}集","message":"{{summary}}","priority":5}"#.into(),
            ),
            Self::Serverchan => {
                TemplateType::Form("title={{anime_name}} 第{{episode}}集 下载完成&desp={{summary}}".into())
            }
        }
    }

    /// Built-in error template per preset — uses service-specific features.
    pub fn error_template(self) -> TemplateType {
        match self {
            Self::Bark => TemplateType::Json(
                r#"{"title":"⚠️ {{title}}","body":"{{message}}","icon":"https://cdn.jsdelivr.net/gh/twitter/twemoji@14.0.2/assets/72x72/26a0.png","group":"bangumi-rss-errors"}"#
                    .into(),
            ),
            Self::Gotify => TemplateType::Json(
                r#"{"title":"{{title}}","message":"{{message}}","priority":8}"#.into(),
            ),
            Self::Serverchan => {
                TemplateType::Form("title={{title}}&desp=❌ {{message}}".into())
            }
        }
    }
}

impl FromStr for Preset {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "bark" => Ok(Self::Bark),
            "gotify" => Ok(Self::Gotify),
            "serverchan" => Ok(Self::Serverchan),
            _ => Err(()),
        }
    }
}

const DEFAULT_TEMPLATE: &str =
    r#"{"title":"{{anime_name}} 第{{episode}}集","body":"{{summary}}","image":"{{image_url}}"}"#;

/// Determine the final template from format preset + custom override.
///
/// Priority: preset > custom > default
/// Resolve template from preset + custom override, with optional error template.
///
/// Priority for main template: preset > custom > default
/// Priority for error template: custom_error > preset_error > None (built-in fallback)
pub fn resolve_webhook(
    url: &str,
    preset: Option<Preset>,
    custom_template: Option<&str>,
    custom_error_template: Option<&str>,
) -> Option<WebhookConfig> {
    if url.is_empty() {
        return None;
    }
    let template = resolve_template(preset, custom_template);
    let error_template = resolve_error_template(preset, custom_error_template);
    Some(WebhookConfig {
        url: url.into(),
        template,
        error_template,
    })
}

pub(crate) fn resolve_template(preset: Option<Preset>, custom: Option<&str>) -> TemplateType {
    if let Some(p) = preset {
        return p.template();
    }
    if let Some(t) = custom
        && !t.is_empty()
    {
        return if t.trim().starts_with('{') || t.trim().starts_with('[') {
            TemplateType::Json(t.to_string())
        } else {
            TemplateType::Form(t.to_string())
        };
    }
    TemplateType::Json(DEFAULT_TEMPLATE.into())
}

fn resolve_error_template(preset: Option<Preset>, custom: Option<&str>) -> Option<TemplateType> {
    if let Some(t) = custom
        && !t.is_empty()
    {
        Some(if t.trim().starts_with('{') || t.trim().starts_with('[') {
            TemplateType::Json(t.to_string())
        } else {
            TemplateType::Form(t.to_string())
        })
    } else {
        preset.map(|p| p.error_template())
    }
}

/// Render a notification using the given template.
/// Returns `(body_string, content_type)`.
/// For `Failed` notifications, use `render_failed` instead — it uses a built-in
/// error template matching the same Content-Type.
pub fn render(template: &TemplateType, notification: &Notification) -> (String, &'static str) {
    let (tpl, content_type, is_json) = match template {
        TemplateType::Json(t) => (t.as_str(), "application/json", true),
        TemplateType::Form(t) => (t.as_str(), "application/x-www-form-urlencoded", false),
    };

    let mut result = tpl.to_string();
    for (key, val) in notification_to_fields(notification) {
        let encoded = if is_json {
            json_escape(&val)
        } else {
            url_encode(&val)
        };
        result = result.replace(&format!("{{{{{key}}}}}"), &encoded);
    }

    (result, content_type)
}

/// Built-in error templates — includes both `body` (Bark-compatible) and `message` (Gotify-compatible).
const FAILED_JSON: &str = r#"{"title":"{{title}}","body":"{{message}}","message":"{{message}}"}"#;
const FAILED_FORM: &str = "title={{title}}&desp={{message}}";

/// Render a `Failed` notification using a built-in template that matches the
/// same Content-Type as the user's configured template.
pub fn render_failed(
    template_type: &TemplateType,
    failed: &crate::types::FailedData,
) -> (String, &'static str) {
    let tpl = match template_type {
        TemplateType::Json(_) => TemplateType::Json(FAILED_JSON.into()),
        TemplateType::Form(_) => TemplateType::Form(FAILED_FORM.into()),
    };
    let notification = crate::types::Notification::Failed(failed.clone());
    render(&tpl, &notification)
}

// ── Field mapping ──

fn notification_to_fields(n: &Notification) -> Vec<(&'static str, String)> {
    match n {
        Notification::EpisodeDownloaded(d) => vec![
            ("type", "episode_downloaded".into()),
            ("anime_name", d.anime_name.clone()),
            ("season", d.season.to_string()),
            ("episode", d.episode.to_string()),
            ("library_path", d.library_path.clone()),
            ("name_cn", d.name_cn.clone().unwrap_or_default()),
            ("name_original", d.name_original.clone().unwrap_or_default()),
            ("summary", d.summary.clone().unwrap_or_default()),
            (
                "rating",
                d.rating.map(|r| r.to_string()).unwrap_or_default(),
            ),
            ("image_url", d.image_url.clone().unwrap_or_default()),
            (
                "eps_count",
                d.eps_count.map(|c| c.to_string()).unwrap_or_default(),
            ),
            (
                "message",
                format!("{} 第{}集 下载完成", d.anime_name, d.episode),
            ),
        ],
        Notification::Failed(f) => vec![
            ("type", "failed".into()),
            ("context", f.title.clone()),
            ("error", f.message.clone()),
            ("title", f.title.clone()),
            ("message", format!("[失败] {}: {}", f.title, f.message)),
        ],
    }
}

// ── Escape helpers ──

fn json_escape(s: &str) -> String {
    let quoted = serde_json::to_string(s).unwrap();
    quoted[1..quoted.len() - 1].to_string()
}

fn url_encode(s: &str) -> String {
    // form-urlencoded: space → +, special chars → %XX
    utf8_percent_encode(s, NON_ALPHANUMERIC)
        .to_string()
        .replace("%20", "+")
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EpisodeDownloadedData, FailedData};

    fn sample_downloaded() -> Notification {
        Notification::EpisodeDownloaded(EpisodeDownloadedData {
            anime_name: "通知模块想要你测试".into(),
            season: 1,
            episode: 2,
            library_path: "/anime/通知模块想要你测试/S01/通知模块想要你测试 - 02.mp4".into(),
            image_url: Some("https://img.example.com/cover.jpg".into()),
            name_cn: None,
            name_original: None,
            summary: Some("这是一个测试通知".into()),
            rating: Some(10.0),
            eps_count: Some(2),
        })
    }

    fn sample_failed() -> Notification {
        Notification::Failed(FailedData {
            title: "通知模块想要你测试".into(),
            message: "请求被拒绝：原因是测试需要".into(),
        })
    }

    #[test]
    fn preset_bark() {
        let t = Preset::Bark.template();
        let (body, ct) = render(&t, &sample_downloaded());
        assert_eq!(ct, "application/json");
        assert!(body.contains(r#""title":"通知模块想要你测试 第2集""#));
        assert!(body.contains(r#""body":"这是一个测试通知""#));
        assert!(body.contains(r#""group":"bangumi-rss""#));
        assert!(body.contains("https://img.example.com/cover.jpg"));
    }

    #[test]
    fn preset_gotify() {
        let t = Preset::Gotify.template();
        let (body, ct) = render(&t, &sample_downloaded());
        assert_eq!(ct, "application/json");
        assert!(body.contains(r#""priority":5"#));
        assert!(body.contains(r#""message":"这是一个测试通知""#));
        assert!(!body.is_empty());
    }

    #[test]
    fn preset_serverchan() {
        let t = Preset::Serverchan.template();
        let (body, ct) = render(&t, &sample_downloaded());
        assert_eq!(ct, "application/x-www-form-urlencoded");
        assert!(body.starts_with("title="), "body: {body}");
        assert!(body.contains("desp="), "body: {body}");
        // Content is URL-encoded; verify decoded form
        assert!(
            body.contains("%E6%B5%8B%E8%AF%95"),
            "should contain URL-encoded '测试': {body}"
        );
        assert!(
            !body.contains("{{"),
            "template keys should be replaced in: {body}"
        );
    }

    #[test]
    fn custom_json_template() {
        let t = resolve_template(None, Some(r#"{"anime":"{{anime_name}}"}"#));
        let (body, ct) = render(&t, &sample_downloaded());
        assert_eq!(ct, "application/json");
        assert_eq!(body, r#"{"anime":"通知模块想要你测试"}"#);
    }

    #[test]
    fn custom_form_template() {
        let t = resolve_template(None, Some("anime={{anime_name}}&ep={{episode}}"));
        let (body, ct) = render(&t, &sample_downloaded());
        assert_eq!(ct, "application/x-www-form-urlencoded");
        assert!(body.contains("anime="));
        assert!(body.contains("&ep=2"));
    }

    #[test]
    fn default_template() {
        let t = resolve_template(None, None);
        let (body, ct) = render(&t, &sample_downloaded());
        assert_eq!(ct, "application/json");
        assert!(body.contains(r#""title":"通知模块想要你测试 第2集""#));
        assert!(body.contains(r#""body":"这是一个测试通知""#));
    }

    #[test]
    fn failed_notification() {
        let t = resolve_template(None, Some(r#"{"error":"{{error}}"}"#));
        let (body, ct) = render(&t, &sample_failed());
        assert_eq!(ct, "application/json");
        assert!(body.contains("请求被拒绝"));
    }

    #[test]
    fn json_escapes_special_chars() {
        assert_eq!(json_escape(r#"say "hello""#), r#"say \"hello\""#);
        assert_eq!(json_escape("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn url_encodes_special_chars() {
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("你好"), "%E4%BD%A0%E5%A5%BD");
    }

    #[test]
    fn notification_to_fields_episode_downloaded() {
        let fields = super::notification_to_fields(&sample_downloaded());
        let map: std::collections::HashMap<&str, &str> =
            fields.iter().map(|(k, v)| (*k, v.as_str())).collect();

        assert_eq!(map.get("type"), Some(&"episode_downloaded"));
        assert_eq!(map.get("anime_name"), Some(&"通知模块想要你测试"));
        assert_eq!(map.get("season"), Some(&"1"));
        assert_eq!(map.get("episode"), Some(&"2"));
        assert_eq!(
            map.get("library_path"),
            Some(&"/anime/通知模块想要你测试/S01/通知模块想要你测试 - 02.mp4")
        );
        assert_eq!(
            map.get("image_url"),
            Some(&"https://img.example.com/cover.jpg")
        );
        assert_eq!(map.get("summary"), Some(&"这是一个测试通知"));
        assert_eq!(map.get("rating"), Some(&"10"));
        assert_eq!(map.get("eps_count"), Some(&"2"));
        assert_eq!(
            map.get("message"),
            Some(&"通知模块想要你测试 第2集 下载完成")
        );
        // Optional fields that were None
        assert_eq!(map.get("name_cn"), Some(&""));
        assert_eq!(map.get("name_original"), Some(&""));
    }

    #[test]
    fn notification_to_fields_failed() {
        let fields = super::notification_to_fields(&sample_failed());
        let map: std::collections::HashMap<&str, &str> =
            fields.iter().map(|(k, v)| (*k, v.as_str())).collect();

        assert_eq!(map.get("type"), Some(&"failed"));
        assert_eq!(map.get("context"), Some(&"通知模块想要你测试"));
        assert_eq!(map.get("error"), Some(&"请求被拒绝：原因是测试需要"));
        assert_eq!(map.get("title"), Some(&"通知模块想要你测试"));
        assert_eq!(
            map.get("message"),
            Some(&"[失败] 通知模块想要你测试: 请求被拒绝：原因是测试需要")
        );
    }

    #[test]
    fn render_failed_json() {
        let failed = FailedData {
            title: "测试番剧".into(),
            message: "连接超时".into(),
        };
        let t = TemplateType::Json(r#"{"dummy":"true"}"#.into());
        let (body, ct) = render_failed(&t, &failed);
        assert_eq!(ct, "application/json");
        assert!(body.contains(r#""title":"测试番剧""#));
        assert!(body.contains("[失败] 测试番剧: 连接超时"));
        assert!(body.contains("[失败] 测试番剧: 连接超时"));
    }

    #[test]
    fn render_failed_form() {
        let failed = FailedData {
            title: "测试 RSS 源".into(),
            message: "DNS 解析失败".into(),
        };
        let t = TemplateType::Form("dummy=true".into());
        let (body, ct) = render_failed(&t, &failed);
        assert_eq!(ct, "application/x-www-form-urlencoded");
        assert!(body.starts_with("title="));
        assert!(
            body.contains("DNS+%E8%A7%A3%E6%9E%90%E5%A4%B1%E8%B4%A5"),
            "body: {body}"
        );
    }

    #[test]
    fn resolve_empty_uses_default() {
        let t = resolve_template(None, None);
        assert!(matches!(t, TemplateType::Json(_)));
    }
}
