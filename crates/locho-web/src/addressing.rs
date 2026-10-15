//! Browser relay URL compatibility at the address-lookup boundary.

use iroh::{
    address_lookup::{
        AddressLookup, AddressLookupBuilder, AddressLookupBuilderError, EndpointData, EndpointInfo,
        Error, Item, PkarrResolver,
    },
    Endpoint, EndpointId, RelayUrl, TransportAddr,
};
use n0_future::{boxed::BoxStream, StreamExt};

pub fn relay_url(relay: RelayUrl) -> RelayUrl {
    let mut url = url::Url::from(relay);
    if let Some(host) = url.domain().and_then(|host| host.strip_suffix('.')) {
        let host = host.to_owned();
        // Removing a DNS root dot preserves the host and browser TLS validation.
        url.set_host(Some(&host)).expect("valid DNS hostname");
    }
    RelayUrl::from(url)
}

#[derive(Debug)]
pub struct BrowserLookup;

impl AddressLookupBuilder for BrowserLookup {
    fn into_address_lookup(
        self,
        endpoint: &Endpoint,
    ) -> Result<impl AddressLookup, AddressLookupBuilderError> {
        let resolver = PkarrResolver::n0_dns().into_address_lookup(endpoint)?;
        Ok(NormalizedLookup(resolver))
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

#[cfg(test)]
mod tests {
    use super::*;

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
