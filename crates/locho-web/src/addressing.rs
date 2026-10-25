//! Browser relay URL compatibility at the address-lookup boundary.

use iroh::{
    address_lookup::{
        AddressLookup, AddressLookupBuilder, AddressLookupBuilderError, EndpointData, EndpointInfo,
        Error, Item, PkarrResolver,
    },
    Endpoint, EndpointId, RelayUrl, TransportAddr,
};
use n0_future::{boxed::BoxStream, StreamExt};
use std::{sync::Arc, time::Duration};

pub fn relay_url(relay: RelayUrl) -> RelayUrl {
    let mut url = url::Url::from(relay);
    if let Some(host) = url.domain().and_then(|host| host.strip_suffix('.')) {
        let host = host.to_owned();
        // Removing a DNS root dot preserves the host and browser TLS validation.
        url.set_host(Some(&host)).expect("valid DNS hostname");
    }
    RelayUrl::from(url)
}

/// Investigation candidate: client policy must use the same URL spelling as
/// peer hints/discovery, or one physical relay can acquire two actor keys.
#[cfg(feature = "browser-repro")]
pub fn canonical_default_relay_mode() -> iroh::RelayMode {
    canonical_relay_mode(iroh::endpoint::default_relay_mode())
}

#[cfg(feature = "browser-repro")]
fn canonical_relay_mode(mode: iroh::RelayMode) -> iroh::RelayMode {
    if matches!(mode, iroh::RelayMode::Disabled) {
        return mode;
    }
    let relays = mode
        .relay_map()
        .relays::<Vec<_>>()
        .into_iter()
        .map(|config| {
            let mut config = config.as_ref().clone();
            config.url = relay_url(config.url);
            config
        })
        .collect();
    iroh::RelayMode::Custom(relays)
}

#[derive(Debug)]
pub struct BrowserLookup;

impl AddressLookupBuilder for BrowserLookup {
    fn into_address_lookup(
        self,
        endpoint: &Endpoint,
    ) -> Result<impl AddressLookup, AddressLookupBuilderError> {
        let resolver = PkarrResolver::n0_dns().into_address_lookup(endpoint)?;
        Ok(NormalizedLookup(RetryingLookup(Arc::new(resolver))))
    }
}

#[derive(Debug)]
struct NormalizedLookup<T>(T);

impl<T: AddressLookup> AddressLookup for NormalizedLookup<T> {
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        let stream = self.0.resolve(endpoint_id)?;
        Some(Box::pin(stream.map(|result| {
            result.map(|item| {
                let addrs = item
                    .addrs()
                    .map(|addr| match addr {
                        TransportAddr::Relay(relay) => {
                            TransportAddr::Relay(relay_url(relay.clone()))
                        }
                        other => other.clone(),
                    })
                    .collect();
                let mut data = EndpointData::new(addrs);
                data.set_user_data(item.user_data());
                Item::new(
                    EndpointInfo::from_parts(item.endpoint_id(), data),
                    item.provenance(),
                    item.last_updated(),
                )
            })
        })))
    }
}

/// Retry read-only discovery, not QUIC connections or application requests.
/// A stale hint otherwise consumes the full dial timeout before lookup retries,
/// leaving very little of the browser's startup budget for a healthy relay.
#[derive(Debug)]
struct RetryingLookup<T>(Arc<T>);

impl<T: AddressLookup + 'static> AddressLookup for RetryingLookup<T> {
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        let lookup = self.0.clone();
        let first = lookup.resolve(endpoint_id)?;
        Some(Box::pin(n0_future::stream::unfold(
            (lookup, first, 0u8),
            move |(lookup, mut stream, mut retries)| async move {
                loop {
                    let result = stream.next().await?;
                    if result.is_err() && retries < 2 {
                        retries += 1;
                        n0_future::time::sleep(Duration::from_millis(250 * u64::from(retries)))
                            .await;
                        if let Some(next) = lookup.resolve(endpoint_id) {
                            stream = next;
                            continue;
                        }
                    }
                    return Some((result, (lookup, stream, retries)));
                }
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "browser-repro")]
    #[test]
    fn canonical_client_policy_matches_peer_keys_and_preserves_relay_options() {
        for mode in [iroh::RelayMode::Default, iroh::RelayMode::Staging] {
            let original = mode.relay_map();
            let normalized = canonical_relay_mode(mode).relay_map();
            assert_eq!(original.len(), normalized.len());
            for config in original.relays::<Vec<_>>() {
                let url = relay_url(config.url.clone());
                let result = normalized
                    .get(&url)
                    .expect("peer-normalized key must be in client policy");
                assert_eq!(result.url, url);
                assert_eq!(result.quic, config.quic);
                assert_eq!(result.auth_token, config.auth_token);
            }
        }
        assert_eq!(
            canonical_relay_mode(iroh::RelayMode::Disabled),
            iroh::RelayMode::Disabled
        );
    }

    #[test]
    fn normalizes_dns_root_dot_without_changing_port_or_path() {
        for (input, expected) in [
            (
                "https://relay.example.org.:444/",
                "https://relay.example.org:444/",
            ),
            ("https://relay.example.org/", "https://relay.example.org/"),
            ("https://127.0.0.1:444/", "https://127.0.0.1:444/"),
            ("https://[::1]:444/", "https://[::1]:444/"),
        ] {
            assert_eq!(relay_url(input.parse().unwrap()).as_str(), expected);
        }
    }

    #[derive(Debug)]
    struct FixedLookup(Item);

    impl AddressLookup for FixedLookup {
        fn resolve(&self, _: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
            Some(Box::pin(n0_future::stream::once_future(
                std::future::ready(Ok(self.0.clone())),
            )))
        }
    }

    #[derive(Debug)]
    struct FailingLookup {
        attempts: std::sync::atomic::AtomicUsize,
        failures: usize,
        item: Item,
    }

    impl AddressLookup for FailingLookup {
        fn resolve(&self, _: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
            let attempt = self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let result = if attempt < self.failures {
                Err(Error::from_err(
                    "fixture",
                    std::io::Error::other("temporary lookup failure"),
                ))
            } else {
                Ok(self.item.clone())
            };
            Some(Box::pin(n0_future::stream::once_future(
                std::future::ready(result),
            )))
        }
    }

    #[tokio::test]
    async fn discovery_retries_are_bounded_and_preserve_verified_items() {
        let id: EndpointId = "f24993f3f0b8fdc751f622fece94da6687e0c30b93301abbbbd28c7508118a0b"
            .parse()
            .unwrap();
        for failures in [0, 2, 3] {
            let inner = Arc::new(FailingLookup {
                attempts: std::sync::atomic::AtomicUsize::new(0),
                failures,
                item: Item::new(
                    EndpointInfo::new(id)
                        .with_relay_url("https://relay.example.org./".parse().unwrap()),
                    "fixture",
                    Some(42),
                ),
            });
            let lookup = NormalizedLookup(RetryingLookup(inner.clone()));
            let mut stream = lookup.resolve(id).unwrap();
            let result = stream.next().await.unwrap();
            if failures == 3 {
                assert!(result.is_err());
            } else {
                let item = result.unwrap();
                assert_eq!(item.endpoint_id(), id);
                assert_eq!(item.provenance(), "fixture");
                assert_eq!(item.last_updated(), Some(42));
                assert_eq!(
                    item.relay_urls().next().unwrap().as_str(),
                    "https://relay.example.org/"
                );
            }
            assert!(stream.next().await.is_none());
            assert_eq!(
                inner.attempts.load(std::sync::atomic::Ordering::SeqCst),
                (failures + 1).min(3)
            );
        }
    }

    #[tokio::test]
    async fn lookup_preserves_identity_metadata_and_non_relay_addresses() {
        let id: EndpointId = "f24993f3f0b8fdc751f622fece94da6687e0c30b93301abbbbd28c7508118a0b"
            .parse()
            .unwrap();
        let address = "127.0.0.1:12345".parse().unwrap();
        let info = EndpointInfo::new(id)
            .with_relay_url("https://relay.example.org./".parse().unwrap())
            .with_ip_addrs(vec![address]);
        let lookup = NormalizedLookup(FixedLookup(Item::new(info, "fixture", Some(42))));
        let item = lookup.resolve(id).unwrap().next().await.unwrap().unwrap();
        assert_eq!(item.endpoint_id(), id);
        assert_eq!(item.provenance(), "fixture");
        assert_eq!(item.last_updated(), Some(42));
        assert_eq!(item.ip_addrs().copied().collect::<Vec<_>>(), vec![address]);
        assert_eq!(
            item.relay_urls().next().unwrap().as_str(),
            "https://relay.example.org/"
        );
    }
}
