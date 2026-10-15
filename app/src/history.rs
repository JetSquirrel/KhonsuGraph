//! DuckLocal's settings calls, kept so the code carried over from it reads
//! the same; KhonsuGraph keeps them in settings.json (see `prefs`).

pub fn get_setting(key: &str) -> anyhow::Result<Option<String>> {
    crate::prefs::get(key)
}

pub fn set_setting(key: &str, value: &str) -> anyhow::Result<()> {
    crate::prefs::set(key, value)
}
