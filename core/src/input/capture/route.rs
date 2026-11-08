//! Which protocol a UDP datagram is: rules over its ports, addresses and
//! VLAN tags (doc/protocol-definitions.md → "Matching").
//!
//! A rule names only what it cares about; the rest matches anything.
//! Written as text (CLI, settings): `3490`, `30000-30100`, or
//! `port=3490,src=10.0.0.0/8,dst=fe80::1,sport=50000,vlan=5`; `*` matches
//! every datagram. A datagram goes to the protocol of the most specific rule
//! that matches it (more conditions, then longer address prefixes); of
//! equally specific rules, the user's win over defaults, then the protocol
//! listed first (DLT, SOME/IP, syslog, then loaded ones). A port range
//! counts as one condition, as a single port does.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// What the transport knows about a UDP datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Datagram {
    /// IPv4 addresses in the first 4 bytes.
    pub src: [u8; 16],
    pub dst: [u8; 16],
    pub v6: bool,
    pub src_port: u16,
    pub dst_port: u16,
    /// VLAN ids of its tags, outer first; 0: none.
    pub vlan: [u16; 2],
}

/// An address and how many of its leading bits count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    addr: [u8; 16],
    v6: bool,
    bits: u8,
}

impl Prefix {
    fn matches(&self, addr: &[u8; 16], v6: bool) -> bool {
        if self.v6 != v6 {
            return false;
        }
        let full = usize::from(self.bits / 8);
        if self.addr[..full] != addr[..full] {
            return false;
        }
        let rest = self.bits % 8;
        rest == 0 || (self.addr[full] ^ addr[full]) & (0xFF << (8 - rest)) == 0
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let max = if self.v6 { 128 } else { 32 };
        if self.v6 {
            write!(f, "{}", Ipv6Addr::from(self.addr))?;
        } else {
            write!(f, "{}", Ipv4Addr::new(self.addr[0], self.addr[1], self.addr[2], self.addr[3]))?;
        }
        if self.bits != max {
            write!(f, "/{}", self.bits)?;
        }
        Ok(())
    }
}

impl FromStr for Prefix {
    type Err = String;

    fn from_str(s: &str) -> Result<Prefix, String> {
        let (addr, bits) = match s.split_once('/') {
            Some((a, b)) => (a, Some(b.parse::<u8>().map_err(|_| format!("`{b}` isn't a prefix length"))?)),
            None => (s, None),
        };
        let mut out = [0u8; 16];
        let v6 = if let Ok(a) = Ipv4Addr::from_str(addr) {
            out[..4].copy_from_slice(&a.octets());
            false
        } else if let Ok(a) = Ipv6Addr::from_str(addr) {
            out = a.octets();
            true
        } else {
            return Err(format!("`{addr}` isn't an IP address"));
        };
        let max = if v6 { 128 } else { 32 };
        let bits = bits.unwrap_or(max);
        if bits > max {
            return Err(format!("/{bits} is longer than the address ({max} bits)"));
        }
        Ok(Prefix { addr: out, v6, bits })
    }
}

/// A rule: every condition it gives must hold.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Match {
    /// UDP destination ports (inclusive range).
    pub port: Option<(u16, u16)>,
    /// UDP source ports.
    pub sport: Option<(u16, u16)>,
    pub src: Option<Prefix>,
    pub dst: Option<Prefix>,
    /// One of the datagram's VLAN tags.
    pub vlan: Option<u16>,
}

impl Match {
    /// A rule on the destination port only.
    pub fn port(port: u16) -> Match {
        Match { port: Some((port, port)), ..Match::default() }
    }

    pub fn matches(&self, d: &Datagram) -> bool {
        let in_range = |r: Option<(u16, u16)>, p: u16| r.is_none_or(|(a, b)| (a..=b).contains(&p));
        in_range(self.port, d.dst_port)
            && in_range(self.sport, d.src_port)
            && self.src.is_none_or(|p| p.matches(&d.src, d.v6))
            && self.dst.is_none_or(|p| p.matches(&d.dst, d.v6))
            && self.vlan.is_none_or(|v| d.vlan.contains(&v) && v != 0)
    }

    /// How much a rule says: conditions, then address prefix bits. Larger
    /// wins.
    pub fn specificity(&self) -> (usize, u32) {
        let conditions = usize::from(self.port.is_some())
            + usize::from(self.sport.is_some())
            + usize::from(self.src.is_some())
            + usize::from(self.dst.is_some())
            + usize::from(self.vlan.is_some());
        let bits = self.src.map_or(0, |p| u32::from(p.bits)) + self.dst.map_or(0, |p| u32::from(p.bits));
        (conditions, bits)
    }

    /// The rule as it is written, given one condition at a time
    /// (`key=value`; `port` alone may be a bare number or range).
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let value = value.trim();
        match key.trim() {
            "port" => self.port = Some(range(value)?),
            "sport" => self.sport = Some(range(value)?),
            "src" => self.src = Some(value.parse()?),
            "dst" => self.dst = Some(value.parse()?),
            "vlan" => {
                let v: u16 = value.parse().map_err(|_| format!("`{value}` isn't a VLAN id"))?;
                if !(1..=4094).contains(&v) {
                    return Err(format!("VLAN id {v} isn't 1 to 4094"));
                }
                self.vlan = Some(v);
            }
            other => return Err(format!("`{other}` isn't a rule key (port, sport, src, dst, vlan)")),
        }
        Ok(())
    }
}

fn range(s: &str) -> Result<(u16, u16), String> {
    let port = |p: &str| p.trim().parse::<u16>().map_err(|_| format!("`{}` isn't a port", p.trim()));
    let (a, b) = match s.split_once('-') {
        Some((a, b)) => (port(a)?, port(b)?),
        None => {
            let p = port(s)?;
            (p, p)
        }
    };
    if a > b {
        return Err(format!("{a}-{b}: the first port is larger"));
    }
    Ok((a, b))
}

impl FromStr for Match {
    type Err = String;

    fn from_str(s: &str) -> Result<Match, String> {
        let s = s.trim();
        let mut m = Match::default();
        if s == "*" {
            return Ok(m);
        }
        if s.is_empty() {
            return Err("an empty rule (write `*` for every datagram)".into());
        }
        for part in s.split(',') {
            match part.split_once('=') {
                Some((k, v)) => m.set(k, v)?,
                None => m.set("port", part)?,
            }
        }
        Ok(m)
    }
}

impl fmt::Display for Match {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        let range = |(a, b): (u16, u16)| if a == b { a.to_string() } else { format!("{a}-{b}") };
        let only_port = self.sport.is_none() && self.src.is_none() && self.dst.is_none() && self.vlan.is_none();
        if let Some(p) = self.port {
            parts.push(if only_port { range(p) } else { format!("port={}", range(p)) });
        }
        if let Some(p) = self.sport {
            parts.push(format!("sport={}", range(p)));
        }
        if let Some(p) = self.src {
            parts.push(format!("src={p}"));
        }
        if let Some(p) = self.dst {
            parts.push(format!("dst={p}"));
        }
        if let Some(v) = self.vlan {
            parts.push(format!("vlan={v}"));
        }
        if parts.is_empty() { f.write_str("*") } else { f.write_str(&parts.join(",")) }
    }
}

/// A rule for a protocol by its name (`DLT`, `SOMEIP`, `syslog`, a
/// definition's name; case doesn't matter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub protocol: String,
    pub rule: Match,
}

impl FromStr for Route {
    type Err = String;

    /// `NAME:RULE`, e.g. `DLT:3490` or `MYPROTO:port=40000,src=10.0.0.0/8`.
    fn from_str(s: &str) -> Result<Route, String> {
        let (name, rule) = s.split_once(':').ok_or_else(|| format!("`{s}`: write NAME:RULE"))?;
        if name.trim().is_empty() {
            return Err(format!("`{s}`: no protocol name"));
        }
        Ok(Route { protocol: name.trim().to_owned(), rule: rule.parse()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dg(src: &str, dst: &str, sport: u16, port: u16, vlan: [u16; 2]) -> Datagram {
        let (s, d): (Prefix, Prefix) = (src.parse().unwrap(), dst.parse().unwrap());
        Datagram { src: s.addr, dst: d.addr, v6: s.v6, src_port: sport, dst_port: port, vlan }
    }

    #[test]
    fn rules_read_and_write_back() {
        for text in ["3490", "30000-30100", "*", "port=3490,src=10.0.0.0/8", "sport=50000,dst=fe80::1,vlan=5"] {
            assert_eq!(text.parse::<Match>().unwrap().to_string(), text);
        }
        assert_eq!("port=40000".parse::<Match>().unwrap().to_string(), "40000");
        assert_eq!(" 1 , vlan = 7 ".parse::<Match>().unwrap().to_string(), "port=1,vlan=7");
        let r: Route = "DLTK:port=3491,dst=10.0.0.2".parse().unwrap();
        assert_eq!((r.protocol.as_str(), r.rule.to_string().as_str()), ("DLTK", "port=3491,dst=10.0.0.2"));
    }

    #[test]
    fn bad_rules_say_why() {
        let e = |s: &str| s.parse::<Match>().unwrap_err();
        assert_eq!(e("http"), "`http` isn't a port");
        assert_eq!(e("port=9-3"), "9-3: the first port is larger");
        assert_eq!(e("src=10.0.0.300"), "`10.0.0.300` isn't an IP address");
        assert_eq!(e("dst=10.0.0.0/33"), "/33 is longer than the address (32 bits)");
        assert_eq!(e("vlan=5000"), "VLAN id 5000 isn't 1 to 4094");
        assert_eq!(e("host=x"), "`host` isn't a rule key (port, sport, src, dst, vlan)");
        assert_eq!(e(""), "an empty rule (write `*` for every datagram)");
        assert_eq!("3490".parse::<Route>().unwrap_err(), "`3490`: write NAME:RULE");
    }

    #[test]
    fn what_matches() {
        let d = dg("10.1.2.3", "192.168.0.9", 50000, 3490, [5, 0]);
        let m = |s: &str| s.parse::<Match>().unwrap().matches(&d);
        assert!(m("3490") && m("3000-4000") && m("*"));
        assert!(!m("3491"));
        assert!(m("src=10.0.0.0/8") && m("src=10.1.2.3") && !m("src=10.1.2.4") && !m("src=11.0.0.0/8"));
        assert!(m("dst=192.168.0.0/23") && !m("dst=192.168.2.0/23"));
        assert!(m("sport=50000") && !m("sport=1-100"));
        assert!(m("vlan=5") && !m("vlan=6"));
        assert!(!m("src=::1"), "IPv4 doesn't match an IPv6 rule");
        let v6 = dg("fe80::1", "fe80::2", 1, 2, [0, 0]);
        assert!("src=fe80::/10".parse::<Match>().unwrap().matches(&v6));
        assert!(!"vlan=1".parse::<Match>().unwrap().matches(&v6));
    }

    #[test]
    fn more_said_is_more_specific() {
        let s = |t: &str| t.parse::<Match>().unwrap().specificity();
        assert!(s("3490,src=10.0.0.0/8") > s("3490"));
        assert!(s("3490,src=10.0.0.0/16") > s("3490,src=10.0.0.0/8"));
        assert!(s("3490") > s("*"));
    }
}
