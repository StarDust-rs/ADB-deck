//! `getprop` parsing and device naming.

use std::collections::HashMap;

/// Parses `getprop` output lines of the form `[key]: [value]`.
pub fn parse_getprop(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, rest) = line.strip_prefix('[')?.split_once("]: [")?;
            let value = rest.strip_suffix(']')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Vendor properties that hold a consumer-facing model name, in the order
/// they are tried.
const MARKET_NAME_KEYS: [&str; 6] = [
    "ro.oppo.market.name",
    "ro.config.marketing_name",
    "ro.vendor.oplus.market.enname",
    "ro.vivo.market.name",
    "ro.product.marketname",
    "ro.asus.product.mkt_name",
];

pub fn market_name(props: &HashMap<String, String>) -> Option<String> {
    MARKET_NAME_KEYS.iter().filter_map(|k| props.get(*k)).map(|v| v.trim()).find(|v| !v.is_empty()).map(str::to_string)
}

/// Display name: vendor market name, else "manufacturer model".
pub fn display_name(props: &HashMap<String, String>) -> String {
    if let Some(name) = market_name(props) {
        return name;
    }
    let get = |k: &str| props.get(k).map(|v| v.trim()).unwrap_or("");
    let joined = format!("{} {}", get("ro.product.manufacturer"), get("ro.product.model"));
    let joined = joined.trim();
    if joined.is_empty() {
        let fallback = get("ro.product.name");
        if fallback.is_empty() {
            "Unknown device".into()
        } else {
            fallback.into()
        }
    } else {
        joined.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[ro.product.manufacturer]: [Google]\n\
        [ro.product.model]: [Pixel 7]\n\
        [ro.build.version.release]: [14]\n\
        [ro.build.version.sdk]: [34]\n\
        [ro.serialno]: [28131FDH]\n\
        [empty.value]: []\n\
        garbage line\n";

    #[test]
    fn parses_properties_and_ignores_noise() {
        let p = parse_getprop(SAMPLE);
        assert_eq!(p["ro.product.model"], "Pixel 7");
        assert_eq!(p["empty.value"], "");
        assert_eq!(p.len(), 6);
    }

    #[test]
    fn values_containing_brackets_survive() {
        let p = parse_getprop("[a.b]: [x [y] z]\n");
        assert_eq!(p["a.b"], "x [y] z");
    }

    #[test]
    fn name_prefers_market_name_then_manufacturer_model() {
        let mut p = parse_getprop(SAMPLE);
        assert_eq!(display_name(&p), "Google Pixel 7");
        p.insert("ro.product.marketname".into(), "Redmi Note".into());
        assert_eq!(display_name(&p), "Redmi Note");
    }

    #[test]
    fn name_falls_back_gracefully() {
        let p = HashMap::new();
        assert_eq!(display_name(&p), "Unknown device");
        let p = parse_getprop("[ro.product.name]: [sdk_gphone]\n");
        assert_eq!(display_name(&p), "sdk_gphone");
    }
}
