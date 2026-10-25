//! 本地化目录。
//!
//! 目录以 JSON 内嵌在二进制中。当前支持 `zh-CN` 与 `en-US`。
//! 缺失的键回退到 `en-US`，仍然缺失时返回键名本身。

use std::collections::HashMap;
use std::fmt;

const ZH_CN: &str = include_str!("../locales/zh-CN.json");
const EN_US: &str = include_str!("../locales/en-US.json");

/// 支持的语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    /// 简体中文。
    ZhCn,
    /// 美式英语。
    EnUs,
}

impl Locale {
    /// 全部支持的语言。
    pub const ALL: [Self; 2] = [Self::ZhCn, Self::EnUs];

    /// BCP 47 标签。
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }

    /// 从标签解析，未知返回 `None`。
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        let normalized = tag.trim().replace('_', "-").to_ascii_lowercase();
        if normalized == "zh" || normalized.starts_with("zh-") {
            Some(Self::ZhCn)
        } else if normalized == "en" || normalized.starts_with("en-") {
            Some(Self::EnUs)
        } else {
            None
        }
    }

    /// 按 `--lang`、`ARKMCL_LANG`、`LC_ALL`、`LC_MESSAGES`、`LANG` 的顺序检测。
    #[must_use]
    pub fn detect(override_tag: Option<&str>) -> Self {
        if let Some(tag) = override_tag {
            if let Some(locale) = Self::from_tag(tag) {
                return locale;
            }
        }
        for key in ["ARKMCL_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(key) {
                if let Some(locale) = Self::from_tag(&value) {
                    return locale;
                }
            }
        }
        Self::EnUs
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.tag())
    }
}

/// 目录加载错误。
#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// 本地化目录。
#[derive(Debug)]
pub struct Catalog {
    locale: Locale,
    primary: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

impl Catalog {
    /// 加载指定语言的目录。
    ///
    /// # Errors
    ///
    /// 内嵌目录不是合法 JSON 时返回错误。
    pub fn load(locale: Locale) -> Result<Self, Error> {
        let primary = parse(match locale {
            Locale::ZhCn => ZH_CN,
            Locale::EnUs => EN_US,
        })?;
        let fallback = if locale == Locale::EnUs {
            HashMap::new()
        } else {
            parse(EN_US)?
        };
        Ok(Self {
            locale,
            primary,
            fallback,
        })
    }

    /// 当前语言。
    #[must_use]
    pub const fn locale(&self) -> Locale {
        self.locale
    }

    /// 查询文本；缺失时回退到 `en-US`，再缺失则返回键名。
    #[must_use]
    pub fn text<'a>(&'a self, key: &'a str) -> &'a str {
        self.primary
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }

    /// 查询文本并替换 `{name}` 占位符。
    #[must_use]
    pub fn format(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut output = self.text(key).to_owned();
        for (name, value) in args {
            output = output.replace(&format!("{{{name}}}"), value);
        }
        output
    }
}

fn parse(source: &str) -> Result<HashMap<String, String>, Error> {
    serde_json::from_str(source).map_err(|error| Error(format!("invalid locale catalog: {error}")))
}

#[cfg(test)]
mod tests {
    use super::{Catalog, Locale};

    #[test]
    fn parses_language_tags() {
        assert_eq!(Locale::from_tag("zh-CN"), Some(Locale::ZhCn));
        assert_eq!(Locale::from_tag("zh_CN.UTF-8"), Some(Locale::ZhCn));
        assert_eq!(Locale::from_tag("en-US"), Some(Locale::EnUs));
        assert_eq!(Locale::from_tag("en"), Some(Locale::EnUs));
        assert_eq!(Locale::from_tag("de-DE"), None);
    }

    #[test]
    fn translates_and_formats() {
        let en = Catalog::load(Locale::EnUs).expect("catalog");
        let zh = Catalog::load(Locale::ZhCn).expect("catalog");

        assert_eq!(en.text("cli.stop"), "Stopping service...");
        assert_eq!(zh.text("cli.stop"), "正在停止服务……");
        assert!(zh.text("cli.help").contains("用法"));
        assert!(
            en.format(
                "cli.status",
                &[("pid", "42"), ("locale", "en-US"), ("socket", "/tmp/x")]
            )
            .contains("pid 42")
        );
    }

    #[test]
    fn falls_back_to_english() {
        let zh = Catalog::load(Locale::ZhCn).expect("catalog");
        assert_eq!(zh.text("rpc.internal"), "内部错误");
        assert_eq!(zh.text("missing.key"), "missing.key");
    }
}
