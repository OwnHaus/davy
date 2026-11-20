//! What HTTP checks and alert deliveries share when they reach a URL a
//! user typed: the client, the addresses it may connect to, and the error
//! text that never holds the URL.

use reqwest::{
    ClientBuilder, Method, RequestBuilder, Url,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect::Policy,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use url::Host;

/// Sent with every check and delivery.
pub const USER_AGENT: &str = concat!("Davy/", env!("CARGO_PKG_VERSION"));
pub(crate) const MAX_ERROR_CHARS: usize = 200;

/// IPv4 networks that are not public, from the IANA special-purpose
/// registry, plus multicast.
const NOT_PUBLIC_V4: [(Ipv4Addr, u8); 14] = [
    (Ipv4Addr::UNSPECIFIED, 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];
/// Every public IPv6 address is in here, apart from those that carry an
/// IPv4 address.
const GLOBAL_UNICAST: (Ipv6Addr, u8) = (Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3);
/// The parts of `GLOBAL_UNICAST` that are not public.
const NOT_PUBLIC_V6: [(Ipv6Addr, u8); 4] = [
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32),
    (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),
    (Ipv6Addr::new(0x5f00, 0, 0, 0, 0, 0, 0, 0), 16),
];
const MAPPED: (Ipv6Addr, u8) = (Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96);
const NAT64: (Ipv6Addr, u8) = (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96);
const SIX_TO_FOUR: (Ipv6Addr, u8) = (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16);

/// The addresses a check or delivery may connect to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Any address. A self-hosted server watches internal services.
    Any,
    /// Public addresses only, so a cloud tenant cannot reach the network
    /// the service runs in.
    Public,
}

impl Reach {
    #[must_use]
    pub fn allows(self, ip: IpAddr) -> bool {
        self == Self::Any || is_public(ip)
    }

    /// False when `url`'s host is an address this refuses. A host name is
    /// checked when it resolves.
    fn allows_url(self, url: &Url) -> bool {
        let ip = match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
            Some(Host::Domain(_)) | None => return true,
        };
        self.allows(ip)
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match carried_v4(ip) {
            Some(v4) => is_public_v4(v4),
            None => in_v6(ip, GLOBAL_UNICAST) && !NOT_PUBLIC_V6.iter().any(|&n| in_v6(ip, n)),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    !NOT_PUBLIC_V4.iter().any(|&(net, len)| {
        let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
        ip.to_bits() & mask == net.to_bits()
    })
}

fn in_v6(ip: Ipv6Addr, (net, len): (Ipv6Addr, u8)) -> bool {
    let mask = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
    ip.to_bits() & mask == net.to_bits()
}

/// The IPv4 address an IPv4-mapped, NAT64 or 6to4 address reaches.
fn carried_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let o = ip.octets();
    if in_v6(ip, MAPPED) || in_v6(ip, NAT64) {
        Some(Ipv4Addr::new(o[12], o[13], o[14], o[15]))
    } else if in_v6(ip, SIX_TO_FOUR) {
        Some(Ipv4Addr::new(o[2], o[3], o[4], o[5]))
    } else {
        None
    }
}

/// The error of a request to an address `Reach::Public` refuses.
#[derive(Debug)]
pub struct NotPublic;

impl std::fmt::Display for NotPublic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("address is not public")
    }
}

impl std::error::Error for NotPublic {}

/// Resolves a name to its public addresses only. The connection goes to
/// what this returns, so a second DNS answer cannot swap in another address.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let public: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|addr| is_public(addr.ip()))
                .collect();
            if public.is_empty() {
                return Err(NotPublic.into());
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

/// The HTTP client of checks and deliveries, with the reach it enforces.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    reach: Reach,
}

impl Client {
    /// Follows up to `redirects` redirects. With 0, a redirect is the answer.
    pub fn new(reach: Reach, redirects: usize) -> reqwest::Result<Self> {
        Self::build(reach, redirects, |builder| builder)
    }

    /// Like `new`, with `name` resolving to `addr` without the reach check,
    /// so a test can stand in a public host for a local server.
    #[cfg(test)]
    pub(crate) fn resolving(
        reach: Reach,
        redirects: usize,
        name: &str,
        addr: SocketAddr,
    ) -> reqwest::Result<Self> {
        Self::build(reach, redirects, |builder| builder.resolve(name, addr))
    }

    fn build(
        reach: Reach,
        redirects: usize,
        extra: impl FnOnce(ClientBuilder) -> ClientBuilder,
    ) -> reqwest::Result<Self> {
        let policy = if redirects == 0 {
            Policy::none()
        } else {
            let limited = Policy::limited(redirects);
            Policy::custom(move |attempt| {
                if reach.allows_url(attempt.url()) {
                    limited.redirect(attempt)
                } else {
                    attempt.error(NotPublic)
                }
            })
        };
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            // A redirect target must not learn the URL it came from, which
            // can hold a token.
            .referer(false)
            .redirect(policy);
        if reach == Reach::Public {
            // A proxy would resolve the name itself, past the check.
            builder = builder.dns_resolver(PublicOnly).no_proxy();
        }
        Ok(Self {
            http: extra(builder).build()?,
            reach,
        })
    }

    /// A request to `url`, refused at once when its host is an address the
    /// reach refuses.
    pub fn request(&self, method: Method, url: &str) -> Result<RequestBuilder, NotPublic> {
        match Url::parse(url) {
            Ok(parsed) if !self.reach.allows_url(&parsed) => Err(NotPublic),
            _ => Ok(self.http.request(method, url)),
        }
    }
}

fn causes(e: &reqwest::Error) -> impl Iterator<Item = &(dyn std::error::Error + 'static)> {
    std::iter::successors(Some(e as &(dyn std::error::Error + 'static)), |e| {
        e.source()
    })
}

/// True when the reach refused an address on the way, in DNS or on a
/// redirect.
#[must_use]
pub fn not_public(e: &reqwest::Error) -> bool {
    causes(e).any(<dyn std::error::Error>::is::<NotPublic>)
}

/// Why a request failed, with its causes, cut to 200 characters. It never
/// holds the URL, because a delivery URL is a credential.
#[must_use]
pub fn error_text(e: reqwest::Error) -> String {
    causes(&e.without_url())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
        .chars()
        .take(MAX_ERROR_CHARS)
        .collect()
}
