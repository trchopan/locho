//! Opt-in independent DNS tier racing for short iroh lookup deadlines.
use iroh::dns::{BoxIter, Builder, DnsError, DnsResolver, Resolver, TxtRecordData, DNS_TIMEOUT};
use std::{
    future::Future,
    net::{Ipv4Addr, Ipv6Addr},
    sync::{Arc, OnceLock},
};

pub(crate) fn resilient_resolver() -> DnsResolver {
    racing_resolver(
        DnsResolver::builder()
            .with_system_defaults()
            .disable_fallback(),
        DnsResolver::builder(),
    )
}

pub(crate) fn racing_resolver(system: Builder, public: Builder) -> DnsResolver {
    DnsResolver::custom(RacingResolver::new(system, public))
}

#[derive(Debug, Clone)]
struct RacingResolver {
    system_builder: Builder,
    public_builder: Builder,
    system: Arc<OnceLock<DnsResolver>>,
    public: Arc<OnceLock<DnsResolver>>,
}

impl RacingResolver {
    fn new(system_builder: Builder, public_builder: Builder) -> Self {
        Self {
            system_builder,
            public_builder,
            system: Arc::new(OnceLock::new()),
            public: Arc::new(OnceLock::new()),
        }
    }

    fn resolvers(&self) -> (DnsResolver, DnsResolver) {
        (
            self.system
                .get_or_init(|| self.system_builder.clone().build())
                .clone(),
            self.public
                .get_or_init(|| self.public_builder.clone().build())
                .clone(),
        )
    }
}

/// Prefer records over NODATA. If both tiers fail, report the system error
/// consistently rather than making diagnostics depend on response timing.
async fn first_records<T, E>(
    system: impl Future<Output = Result<Vec<T>, E>>,
    public: impl Future<Output = Result<Vec<T>, E>>,
) -> Result<Vec<T>, E> {
    tokio::pin!(system, public);
    let (system, public) = tokio::select! {
        result = &mut system => {
            if matches!(&result, Ok(values) if !values.is_empty()) { return result; }
            (result, public.await)
        },
        result = &mut public => {
            if matches!(&result, Ok(values) if !values.is_empty()) { return result; }
            (system.await, result)
        },
    };
    match (system, public) {
        (Ok(values), _) if !values.is_empty() => Ok(values),
        (_, Ok(values)) | (Ok(values), Err(_)) => Ok(values),
        (Err(error), Err(_)) => Err(error),
    }
}

impl Resolver for RacingResolver {
    fn lookup_ipv4(
        &self,
        host: String,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<BoxIter<Ipv4Addr>, DnsError>> + Send>> {
        let this = self.clone();
        Box::pin(async move {
            let (system, public) = this.resolvers();
            let values = first_records(
                async {
                    system
                        .lookup_ipv4(&host, DNS_TIMEOUT)
                        .await
                        .map(|values| values.collect())
                },
                async {
                    public
                        .lookup_ipv4(&host, DNS_TIMEOUT)
                        .await
                        .map(|values| values.collect())
                },
            )
            .await?;
            let values: Vec<_> = values
                .into_iter()
                .filter_map(|value| match value {
                    std::net::IpAddr::V4(value) => Some(value),
                    _ => None,
                })
                .collect();
            Ok(Box::new(values.into_iter()) as BoxIter<Ipv4Addr>)
        })
    }

    fn lookup_ipv6(
        &self,
        host: String,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<BoxIter<Ipv6Addr>, DnsError>> + Send>> {
        let this = self.clone();
        Box::pin(async move {
            let (system, public) = this.resolvers();
            let values = first_records(
                async {
                    system
                        .lookup_ipv6(&host, DNS_TIMEOUT)
                        .await
                        .map(|values| values.collect())
                },
                async {
                    public
                        .lookup_ipv6(&host, DNS_TIMEOUT)
                        .await
                        .map(|values| values.collect())
                },
            )
            .await?;
            let values: Vec<_> = values
                .into_iter()
                .filter_map(|value| match value {
                    std::net::IpAddr::V6(value) => Some(value),
                    _ => None,
                })
                .collect();
            Ok(Box::new(values.into_iter()) as BoxIter<Ipv6Addr>)
        })
    }

    fn lookup_txt(
        &self,
        host: String,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<BoxIter<TxtRecordData>, DnsError>> + Send>>
    {
        let this = self.clone();
        Box::pin(async move {
            let (system, public) = this.resolvers();
            let values = first_records(
                async {
                    system
                        .lookup_txt(&host, DNS_TIMEOUT)
                        .await
                        .map(|values| values.collect())
                },
                async {
                    public
                        .lookup_txt(&host, DNS_TIMEOUT)
                        .await
                        .map(|values| values.collect())
                },
            )
            .await?;
            Ok(Box::new(values.into_iter()) as BoxIter<TxtRecordData>)
        })
    }

    fn clear_cache(&self) {
        if let Some(system) = self.system.get() {
            system.clear_cache();
        }
        if let Some(public) = self.public.get() {
            public.clear_cache();
        }
    }

    fn reset(&self) -> Box<dyn Resolver> {
        // Do no I/O here: fresh caches and system configuration are built lazily
        // by the first lookup after a network change, not on the monitor thread.
        Box::new(Self::new(
            self.system_builder.clone(),
            self.public_builder.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_tier_failure_does_not_cancel_the_other() {
        assert_eq!(
            first_records(std::future::ready(Err::<Vec<i32>, _>("system")), async {
                tokio::task::yield_now().await;
                Ok(vec![42])
            })
            .await,
            Ok(vec![42])
        );
        assert_eq!(
            first_records(
                async {
                    tokio::task::yield_now().await;
                    Ok(vec![42])
                },
                std::future::ready(Err::<Vec<i32>, _>("public"))
            )
            .await,
            Ok(vec![42])
        );
    }

    async fn delayed(values: Result<Vec<i32>, &'static str>) -> Result<Vec<i32>, &'static str> {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        values
    }

    #[tokio::test]
    async fn empty_answers_wait_for_records_in_either_direction() {
        use std::future::ready;
        assert_eq!(
            first_records(ready(Ok(vec![])), delayed(Ok(vec![42]))).await,
            Ok(vec![42])
        );
        assert_eq!(
            first_records(delayed(Ok(vec![42])), ready(Ok(vec![]))).await,
            Ok(vec![42])
        );
    }

    #[tokio::test]
    async fn negative_results_and_errors_have_consistent_semantics() {
        use std::future::ready;
        assert_eq!(
            first_records(ready(Ok(vec![])), delayed(Ok(vec![]))).await,
            Ok(vec![])
        );
        assert_eq!(
            first_records(ready(Ok(vec![])), delayed(Err("public"))).await,
            Ok(vec![])
        );
        assert_eq!(
            first_records(delayed(Err("system")), ready(Ok(vec![]))).await,
            Ok(vec![])
        );
        assert_eq!(
            first_records(ready(Err("system")), delayed(Err("public"))).await,
            Err("system")
        );
        assert_eq!(
            first_records(delayed(Err("system")), ready(Err("public"))).await,
            Err("system")
        );
    }

    #[tokio::test]
    async fn conflicting_answers_use_the_first_nonempty_tier() {
        use std::future::ready;
        assert_eq!(
            first_records(ready(Ok(vec![1])), delayed(Ok(vec![2]))).await,
            Ok(vec![1])
        );
        assert_eq!(
            first_records(delayed(Ok(vec![1])), ready(Ok(vec![2]))).await,
            Ok(vec![2])
        );
    }

    #[tokio::test]
    async fn outer_deadline_cancels_a_pending_tier() {
        struct Dropped(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = Dropped(cancelled.clone());
        let pending = async move {
            let _guard = guard;
            std::future::pending::<Result<Vec<i32>, &'static str>>().await
        };
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            first_records(std::future::ready(Ok(vec![])), pending),
        )
        .await
        .is_err());
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    }

    // Use real local DNS packets to exercise NODATA and record conversion through
    // all three Resolver methods, without querying public or system DNS.
    async fn dns_tier(
        empty: bool,
        delay: std::time::Duration,
    ) -> (Builder, tokio::task::JoinHandle<()>) {
        use iroh::dns::NameserverConfig;
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let mut packet = [0u8; 512];
                let (size, peer) = socket.recv_from(&mut packet).await.unwrap();
                let mut end = 12;
                while packet[end] != 0 {
                    end += 1 + packet[end] as usize;
                }
                end += 5;
                assert!(end <= size);
                let kind = u16::from_be_bytes([packet[end - 4], packet[end - 3]]);
                let mut response = packet[..end].to_vec();
                response[2..12].copy_from_slice(&[
                    0x81,
                    0x80,
                    0,
                    1,
                    0,
                    u8::from(!empty),
                    0,
                    0,
                    0,
                    0,
                ]);
                if !empty {
                    let data = match kind {
                        1 => vec![127, 0, 0, 42],
                        28 => Ipv6Addr::LOCALHOST.octets().to_vec(),
                        16 => b"\x07fixture".to_vec(),
                        _ => panic!("unexpected record type"),
                    };
                    response.extend_from_slice(&[0xc0, 0x0c]);
                    response.extend_from_slice(&kind.to_be_bytes());
                    response.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
                    response.extend_from_slice(&(data.len() as u16).to_be_bytes());
                    response.extend_from_slice(&data);
                }
                tokio::time::sleep(delay).await;
                socket.send_to(&response, peer).await.unwrap();
            }
        });
        (
            DnsResolver::builder()
                .add_nameserver_config(
                    NameserverConfig::udp(address.ip()).with_port(address.port()),
                )
                .disable_fallback(),
            task,
        )
    }

    #[tokio::test]
    async fn nodata_does_not_suppress_a_aaaa_or_txt_records() {
        use std::time::Duration;
        for system_empty in [true, false] {
            let (system, system_task) = dns_tier(
                system_empty,
                if system_empty {
                    Duration::ZERO
                } else {
                    Duration::from_millis(30)
                },
            )
            .await;
            let (public, public_task) = dns_tier(
                !system_empty,
                if system_empty {
                    Duration::from_millis(30)
                } else {
                    Duration::ZERO
                },
            )
            .await;
            let resolver = racing_resolver(system, public);
            assert_eq!(
                resolver
                    .lookup_ipv4("fixture.test.", Duration::from_secs(1))
                    .await
                    .unwrap()
                    .collect::<Vec<_>>(),
                vec![std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 42))]
            );
            assert_eq!(
                resolver
                    .lookup_ipv6("fixture.test.", Duration::from_secs(1))
                    .await
                    .unwrap()
                    .collect::<Vec<_>>(),
                vec![std::net::IpAddr::V6(Ipv6Addr::LOCALHOST)]
            );
            assert_eq!(
                resolver
                    .lookup_txt("fixture.test.", Duration::from_secs(1))
                    .await
                    .unwrap()
                    .map(|record| record.to_string())
                    .collect::<Vec<_>>(),
                vec!["fixture"]
            );
            system_task.abort();
            public_task.abort();
            let _ = system_task.await;
            let _ = public_task.await;
        }
    }

    #[test]
    fn reset_does_not_initialize_resolvers() {
        let original = RacingResolver::new(DnsResolver::builder(), DnsResolver::builder());
        let _replacement = original.reset();
        assert!(original.system.get().is_none());
        assert!(original.public.get().is_none());
    }
}
