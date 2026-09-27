//! English / Portuguese (Brazil) / Spanish texts. The default comes from the
//! build (`--features pt` for the Portuguese installer); the `language` setting
//! in config.toml overrides it at runtime.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En = 0,
    Pt = 1,
    Es = 2,
}

pub const DEFAULT: Lang = if cfg!(feature = "pt") { Lang::Pt } else { Lang::En };

static CURRENT: AtomicU8 = AtomicU8::new(DEFAULT as u8);

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Pt => "pt",
            Lang::Es => "es",
        }
    }

    pub fn parse(s: &str) -> Lang {
        match s.trim().to_ascii_lowercase().as_str() {
            "pt" | "pt-br" | "pt_br" => Lang::Pt,
            "en" => Lang::En,
            s if s == "es" || s.starts_with("es-") || s.starts_with("es_") => Lang::Es,
            _ => DEFAULT,
        }
    }
}

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn get() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::Pt,
        2 => Lang::Es,
        _ => Lang::En,
    }
}

/// Title of the OBS dock, per language. The installer recognises both.
pub fn dock_title(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Dynamic Delay",
        Lang::Pt => "Delay dinâmico",
        Lang::Es => "Delay dinámico",
    }
}

/// `t!("english {x}", "português {x}", "español {x}")` formats the text of the
/// current language. The Spanish text is optional: without it Spanish shows
/// the English one.
#[macro_export]
macro_rules! t {
    ($en:literal, $pt:literal, $es:literal $(,)?) => {
        match $crate::i18n::get() {
            $crate::i18n::Lang::En => format!($en),
            $crate::i18n::Lang::Pt => format!($pt),
            $crate::i18n::Lang::Es => format!($es),
        }
    };
    ($en:literal, $pt:literal, $es:literal, $($arg:tt)+) => {
        match $crate::i18n::get() {
            $crate::i18n::Lang::En => format!($en, $($arg)+),
            $crate::i18n::Lang::Pt => format!($pt, $($arg)+),
            $crate::i18n::Lang::Es => format!($es, $($arg)+),
        }
    };
    ($en:literal, $pt:literal $(,)?) => {
        $crate::t!($en, $pt, $en)
    };
    ($en:literal, $pt:literal, $($arg:tt)+) => {
        $crate::t!($en, $pt, $en, $($arg)+)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_language() {
        let n = 3;
        set(Lang::Pt);
        assert_eq!(crate::t!("{n} files", "{n} arquivos"), "3 arquivos");
        set(Lang::En);
        assert_eq!(crate::t!("{n} files", "{n} arquivos"), "3 files");
        assert_eq!(Lang::parse("PT-BR"), Lang::Pt);
        // one test: the language is global and tests run in parallel
        for code in ["es", "ES", " es-419 ", "es_MX", "es-ES"] {
            assert_eq!(Lang::parse(code), Lang::Es, "{code}");
        }
        assert_eq!(Lang::Es.code(), "es");
        assert_eq!(Lang::parse("fr"), DEFAULT);
        set(Lang::Es);
        assert_eq!(get(), Lang::Es);
        assert_eq!(crate::t!("{n} files", "{n} arquivos", "{n} archivos"), "3 archivos");
        assert_eq!(crate::t!("{} files", "{} arquivos", "{} archivos", n + 1), "4 archivos");
        // no Spanish text: English is shown
        assert_eq!(crate::t!("{n} files", "{n} arquivos"), "3 files");
        assert_eq!(crate::t!("{} files", "{} arquivos", n), "3 files");
        assert_eq!(dock_title(Lang::Es), "Delay dinámico");
        set(DEFAULT);
    }
}
