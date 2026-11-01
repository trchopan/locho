//! Read-only public DNS transport probe. Does not load identities or capabilities.
//! The final probe uses the dependency's merged eager mode, not the CLI's
//! independent-tier racing resolver. Results depend on local DNS/network state;
//! see docs/dns-recovery.md for the deterministic local-fixture regression.
use iroh::dns::{DnsResolver, FallbackMode, NameserverConfig};
use std::{net::IpAddr, time::Duration};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("n0_dns_resolver=debug,iroh_dns=debug")
        .with_ansi(false)
        .init();
    let addr: IpAddr = "1.1.1.1".parse().unwrap();
    for (name, builder) in [
        (
            "Cloudflare UDP",
            DnsResolver::builder()
                .add_nameserver_config(NameserverConfig::udp(addr))
                .disable_fallback(),
        ),
        (
            "Cloudflare TCP",
            DnsResolver::builder()
                .add_nameserver_config(NameserverConfig::tcp(addr))
                .disable_fallback(),
        ),
        (
            "Cloudflare HTTPS",
            DnsResolver::builder()
                .add_nameserver_config(NameserverConfig::https(addr))
                .disable_fallback(),
        ),
        (
            "System and eager fallback",
            DnsResolver::builder()
                .with_system_defaults()
                .with_fallback_mode(FallbackMode::Eager),
        ),
    ] {
        let resolver = builder.build();
        match resolver
            .lookup_ipv4("dns.iroh.link.", Duration::from_secs(3))
            .await
        {
            Ok(values) => println!("{name}: {:?}", values.collect::<Vec<_>>()),
            Err(error) => println!("{name}: {error:#?}"),
        }
    }
}
