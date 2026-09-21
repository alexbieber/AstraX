use toml_edit::{value, Item, TableLike};

/// Match the official API exactly. A proxy that forwards DeepSeek may support
/// WebSockets independently, so model names and partial host matches are unsafe.
pub(crate) fn is_deepseek_http_endpoint(base_url: &str) -> bool {
    reqwest::Url::parse(base_url.trim()).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https") && url.host_str() == Some("api.deepseek.com")
    })
}

/// A provider's WebSocket capability belongs to its endpoint, not the common
/// Codex config inherited when creating a provider. Call before replacing URL.
pub(super) fn configure_third_party_transport(
    table: &mut dyn TableLike,
    base_url: &str,
    own_template: bool,
) {
    let same_endpoint = table
        .get("base_url")
        .and_then(Item::as_str)
        .is_some_and(|old| {
            super::store::canonical_provider_base_url(old)
                == super::store::canonical_provider_base_url(base_url)
        });
    let preserve_explicit = own_template
        && same_endpoint
        && !is_deepseek_http_endpoint(base_url)
        && table
            .get("supports_websockets")
            .and_then(Item::as_bool)
            .is_some();
    if !preserve_explicit {
        // Explicit false also overrides Codex's built-in provider defaults if a
        // user-chosen provider ID happens to shadow a built-in provider.
        let decor = table
            .get("supports_websockets")
            .and_then(Item::as_value)
            .map(|value| value.decor().clone());
        let mut replacement = value(false);
        if let Some(decor) = decor {
            *replacement
                .as_value_mut()
                .expect("boolean value")
                .decor_mut() = decor;
        }
        table.insert("supports_websockets", replacement);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::Table;

    #[test]
    fn deepseek_host_matching_does_not_disable_proxies() {
        assert!(is_deepseek_http_endpoint("https://api.deepseek.com/v1/"));
        assert!(is_deepseek_http_endpoint("HTTPS://API.DEEPSEEK.COM"));
        for endpoint in [
            "https://deepseek.proxy.example/v1",
            "https://api.deepseek.com.proxy.example/v1",
            "https://api.deepseek.com@proxy.example/v1",
            "https://proxy.example/api.deepseek.com",
            "invalid api.deepseek.com",
        ] {
            assert!(!is_deepseek_http_endpoint(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn endpoint_ownership_controls_websocket_inheritance() {
        for (source, destination, own, expected) in [
            (
                "https://proxy.example/v1",
                "https://proxy.example/v1/",
                true,
                true,
            ),
            (
                "https://proxy.example/v1",
                "https://proxy.example/v1",
                false,
                false,
            ),
            (
                "https://old.example/v1",
                "https://new.example/v1",
                true,
                false,
            ),
            (
                "https://api.deepseek.com",
                "https://api.deepseek.com",
                true,
                false,
            ),
        ] {
            let mut table = Table::new();
            table["base_url"] = value(source);
            table["supports_websockets"] = value(true);
            table["request_max_retries"] = value(9);
            configure_third_party_transport(&mut table, destination, own);
            assert_eq!(table["supports_websockets"].as_bool(), Some(expected));
            assert_eq!(table["request_max_retries"].as_integer(), Some(9));
        }
    }
}
