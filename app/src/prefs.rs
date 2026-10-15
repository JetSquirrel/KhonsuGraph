//! Window preferences (language, interface size, ...), kept in khonsu-core's
//! settings.json beside the recent graphs. DuckLocal kept these in its
//! history database; the call sites read the same.

pub fn get(key: &str) -> anyhow::Result<Option<String>> {
    Ok(khonsu_core::settings::Settings::get_value(key))
}

pub fn set(key: &str, value: &str) -> anyhow::Result<()> {
    Ok(khonsu_core::settings::Settings::set_value(key, value)?)
}
