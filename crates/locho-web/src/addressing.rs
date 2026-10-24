//! Browser relay URL compatibility at the address-lookup boundary.

use iroh::{
    address_lookup::{
        AddressLookup, AddressLookupBuilder, AddressLookupBuilderError, EndpointData, EndpointInfo,
        Error, Item, PkarrResolver,
    },
    Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr,
};
use n0_future::{boxed::BoxStream, Stream, StreamExt};
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

/// Treat supplied relays as cached hints, not as authoritative addressing.
/// When discovery succeeds, keep a dead cached hint out of the initial dial's
/// path candidates. Failed/slow lookup retains the hint for offline discovery.
pub async fn initial_target(endpoint: &Endpoint, hint: EndpointAddr) -> EndpointAddr {
    if hint.relay_urls().next().is_none() {
        return hint;
    }
    let Ok(lookup) = endpoint.address_lookup() else {
        return hint;
    };
    let results = lookup
        .resolve(hint.id)
        .filter_map(|result| result.ok().and_then(Result::ok));
    prefer_resolved_relay(hint, results, Duration::from_secs(2)).await
}

async fn prefer_resolved_relay(
    hint: EndpointAddr,
    results: impl Stream<Item = Item>,
    budget: Duration,
) -> EndpointAddr {
    let resolve = async {
        n0_future::pin!(results);
        while let Some(item) = results.next().await {
            // Browser transports need a relay; direct-only or foreign records
            // must not discard a usable hint or change the peer identity.
            if item.endpoint_id() == hint.id && item.relay_urls().next().is_some() {
                return Some(item.into_endpoint_addr());
            }
        }
        None
    };
    n0_future::time::timeout(budget, resolve)
        .await
        .ok()
        .flatten()
        .unwrap_or(hint)
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

    #[tokio::test]
    async fn resolved_relay_replaces_stale_hint_without_changing_identity() {
        let id: EndpointId = "f24993f3f0b8fdc751f622fece94da6687e0c30b93301abbbbd28c7508118a0b"
            .parse()
            .unwrap();
        let hint = EndpointAddr::new(id).with_relay_url("https://127.0.0.1:1/".parse().unwrap());
        let resolved =
            EndpointInfo::new(id).with_relay_url("https://relay.example.org/".parse().unwrap());
        let stream = n0_future::stream::iter([Item::new(resolved.clone(), "fixture", Some(42))]);
        let target = prefer_resolved_relay(hint, stream, Duration::from_millis(100)).await;
        assert_eq!(target, resolved.into_endpoint_addr());
    }

    #[tokio::test]
    async fn unusable_or_slow_discovery_retains_explicit_hint() {
        let id: EndpointId = "f24993f3f0b8fdc751f622fece94da6687e0c30b93301abbbbd28c7508118a0b"
            .parse()
            .unwrap();
        let other: EndpointId = "1111111111111111111111111111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let hint =
            EndpointAddr::new(id).with_relay_url("https://hint.example.org/".parse().unwrap());
        let stream = n0_future::stream::iter([
            Item::new(
                EndpointInfo::new(other)
                    .with_relay_url("https://other.example.org/".parse().unwrap()),
                "fixture",
                None,
            ),
            Item::new(
                EndpointInfo::new(id).with_ip_addrs(vec!["127.0.0.1:1234".parse().unwrap()]),
                "fixture",
                None,
            ),
        ]);
        assert_eq!(
            prefer_resolved_relay(hint.clone(), stream, Duration::from_millis(100)).await,
            hint
        );
        assert_eq!(
            prefer_resolved_relay(
                hint.clone(),
                n0_future::stream::pending(),
                Duration::from_millis(10)
            )
            .await,
            hint
        );
        assert_eq!(
            prefer_resolved_relay(
                hint.clone(),
                n0_future::stream::empty(),
                Duration::from_millis(100)
            )
            .await,
            hint
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
