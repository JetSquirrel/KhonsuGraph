//! Column names that say "latitude" or "longitude", from DuckLocal's map.

/// The latitude and longitude columns names alone suggest, as `detect` finds
/// them but without the values to check: what `ducklocal check` can say about
/// a `map` plot that leaves `lat` or `lng` out.
pub fn guess_coordinate_columns<'a>(
    names: impl Iterator<Item = &'a str> + Clone,
) -> (Option<usize>, Option<usize>) {
    let lat = names.clone().position(is_lat_name);
    let lng = names
        .enumerate()
        .find(|(ix, name)| Some(*ix) != lat && is_lng_name(name))
        .map(|(ix, _)| ix);
    (lat, lng)
}

/// The words of a column name, lowercased: split at anything not a letter or
/// digit, and at a lower-to-upper case change, so `geo_lat`, `geo-lat` and
/// `geoLat` all hold the word `lat`.
fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        prev_lower = c.is_lowercase();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

pub fn is_lat_name(name: &str) -> bool {
    name_words(name)
        .iter()
        .any(|w| matches!(w.as_str(), "lat" | "latitude" | "纬度"))
}

pub fn is_lng_name(name: &str) -> bool {
    name_words(name)
        .iter()
        .any(|w| matches!(w.as_str(), "lng" | "lon" | "long" | "longitude" | "经度"))
}
