// Two interface languages: English, and Russian when Windows itself is in Russian.
// AGENT_MONITOR_LANG=en|ru overrides the choice.
use std::sync::OnceLock;
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;

const LANG_RUSSIAN: u16 = 0x19;

pub fn is_ru() -> bool {
    static RU: OnceLock<bool> = OnceLock::new();
    *RU.get_or_init(|| match std::env::var("AGENT_MONITOR_LANG").ok().as_deref() {
        Some("ru") => true,
        Some("en") => false,
        // The low 10 bits of a language id are the primary language.
        _ => (unsafe { GetUserDefaultUILanguage() } & 0x3ff) == LANG_RUSSIAN,
    })
}

pub fn lang() -> &'static str {
    if is_ru() { "ru" } else { "en" }
}

pub fn tr(en: &'static str, ru: &'static str) -> &'static str {
    if is_ru() { ru } else { en }
}
