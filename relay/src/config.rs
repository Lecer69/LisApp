//! The relay's settings, from a TOML file. Every key has a default, so a file needs only the password.

use serde::Deserialize;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

pub const EXAMPLE: &str = include_str!("../deploy/config.toml");

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub enum Log {
    Quiet,
    Info,
    Debug,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub address: IpAddr,
    pub port: u16,
    pub log: Log,
}

impl Default for Server {
    fn default() -> Self {
        Self { address: IpAddr::from([0u16; 8]), port: lis_core::relay::DEFAULT_RELAY_PORT, log: Log::Info }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Auth {
    pub password: String,
    pub password_file: Option<String>,
    pub failure_delay_ms: u64,
    pub max_failures: u32,
    pub failure_window_minutes: u64,
    pub ban_minutes: u64,
}

impl Default for Auth {
    fn default() -> Self {
        Self { password: String::new(), password_file: None, failure_delay_ms: 2000, max_failures: 10, failure_window_minutes: 10, ban_minutes: 30 }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_connections: usize,
    pub max_connections_per_ip: usize,
    pub max_pcs: usize,
    pub max_tunnels: usize,
    pub max_tunnels_per_pc: usize,
    pub tunnel_kbps: u64,
    pub total_kbps: u64,
    pub daily_mb_per_pc: u64,
    pub max_tunnel_hours: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 512,
            max_connections_per_ip: 32,
            max_pcs: 64,
            max_tunnels: 128,
            max_tunnels_per_pc: 4,
            tunnel_kbps: 512,
            total_kbps: 0,
            daily_mb_per_pc: 500,
            max_tunnel_hours: 0,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Timeouts {
    pub request_secs: u64,
    pub answer_secs: u64,
    pub idle_secs: u64,
    pub listener_ping_secs: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { request_secs: 10, answer_secs: 15, idle_secs: 120, listener_ping_secs: 30 }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Access {
    pub allow_pcs: Vec<String>,
    pub allow_ips: Vec<String>,
    pub deny_ips: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub auth: Auth,
    pub limits: Limits,
    pub timeouts: Timeouts,
    pub access: Access,
}

/// An address, or a range of them as `address/bits`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Net {
    address: IpAddr,
    bits: u8,
}

impl Net {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (address, bits) = match text.split_once('/') {
            Some((address, bits)) => (address, Some(bits)),
            None => (text, None),
        };
        let address: IpAddr = address.trim().parse().map_err(|_| format!("{text:?} isn't an address"))?;
        let address = address.to_canonical();
        let max = if address.is_ipv4() { 32 } else { 128 };
        let bits = match bits {
            Some(bits) => bits.trim().parse::<u8>().ok().filter(|bits| *bits <= max).ok_or_else(|| format!("{text:?} has a bad range"))?,
            None => max,
        };
        Ok(Self { address, bits })
    }

    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.address, address.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(address)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(self.bits)).unwrap_or(0);
                u32::from(net) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(address)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(self.bits)).unwrap_or(0);
                u128::from(net) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

/// What the server runs with: the file, checked and with the password read.
#[derive(Debug, Clone)]
pub struct Settings {
    pub config: Config,
    pub password: String,
    pub allow_ips: Vec<Net>,
    pub deny_ips: Vec<Net>,
    pub allow_pcs: Vec<String>,
}

fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

impl Settings {
    pub fn from_config(config: Config) -> Result<Self, String> {
        let password = match &config.auth.password_file {
            Some(path) => std::fs::read_to_string(path).map_err(|error| format!("can't read {path}: {error}"))?.trim().to_string(),
            None => config.auth.password.clone(),
        };
        if password.chars().count() < 8 {
            return Err("auth.password needs 8 characters or more".into());
        }
        let nets = |list: &[String]| list.iter().map(|net| Net::parse(net)).collect::<Result<Vec<_>, _>>();
        let allow_pcs = config
            .access
            .allow_pcs
            .iter()
            .map(|pc| {
                let pc: String = pc.chars().filter(|c| !c.is_whitespace() && *c != ':').collect::<String>().to_lowercase();
                if pc.len() == 64 && pc.bytes().all(|b| b.is_ascii_hexdigit()) {
                    Ok(pc)
                } else {
                    Err(format!("access.allow_pcs: {pc:?} isn't a fingerprint (64 hex digits)"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        let limits = &config.limits;
        let timeouts = &config.timeouts;
        for (name, value) in [
            ("limits.max_connections", limits.max_connections),
            ("limits.max_connections_per_ip", limits.max_connections_per_ip),
            ("limits.max_pcs", limits.max_pcs),
            ("limits.max_tunnels", limits.max_tunnels),
            ("limits.max_tunnels_per_pc", limits.max_tunnels_per_pc),
        ] {
            if value == 0 {
                return Err(format!("{name} has to be 1 or more"));
            }
        }
        for (name, value) in [
            ("timeouts.request_secs", timeouts.request_secs),
            ("timeouts.answer_secs", timeouts.answer_secs),
            ("timeouts.listener_ping_secs", timeouts.listener_ping_secs),
        ] {
            if value == 0 {
                return Err(format!("{name} has to be 1 or more"));
            }
        }
        // The devices ping every 25 s
        if timeouts.idle_secs < 30 {
            return Err("timeouts.idle_secs has to be 30 or more, as the devices only ping every 25 s".into());
        }
        if timeouts.listener_ping_secs >= 75 {
            return Err("timeouts.listener_ping_secs has to be under 75, or PCs think the relay is gone".into());
        }

        Ok(Self {
            allow_ips: nets(&config.access.allow_ips).map_err(|error| format!("access.allow_ips: {error}"))?,
            deny_ips: nets(&config.access.deny_ips).map_err(|error| format!("access.deny_ips: {error}"))?,
            allow_pcs,
            password,
            config,
        })
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Config = toml::from_str(text).map_err(|error| error.to_string())?;
        Self::from_config(config)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|error| format!("can't read {}: {error}", path.display()))?;
        Self::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Everything at its default, for tests.
    pub fn with_password(password: &str) -> Self {
        let mut config = Config::default();
        config.auth.password = password.to_string();
        Self::from_config(config).expect("a valid password")
    }

    pub fn ip_allowed(&self, address: IpAddr) -> bool {
        let allowed = self.allow_ips.is_empty() || self.allow_ips.iter().any(|net| net.contains(address));
        allowed && !self.deny_ips.iter().any(|net| net.contains(address))
    }

    pub fn pc_allowed(&self, pc: &str) -> bool {
        self.allow_pcs.is_empty() || self.allow_pcs.iter().any(|allowed| allowed == pc)
    }

    pub fn failure_delay(&self) -> Duration {
        Duration::from_millis(self.config.auth.failure_delay_ms)
    }

    pub fn request_limit(&self) -> Duration {
        secs(self.config.timeouts.request_secs)
    }

    pub fn answer_limit(&self) -> Duration {
        secs(self.config.timeouts.answer_secs)
    }

    pub fn idle_limit(&self) -> Duration {
        secs(self.config.timeouts.idle_secs)
    }

    pub fn listener_ping(&self) -> Duration {
        secs(self.config.timeouts.listener_ping_secs)
    }

    /// None for no limit.
    pub fn tunnel_length(&self) -> Option<Duration> {
        let hours = self.config.limits.max_tunnel_hours;
        (hours > 0).then(|| secs(hours * 3600))
    }

    pub fn daily_bytes_per_pc(&self) -> Option<u64> {
        let mb = self.config.limits.daily_mb_per_pc;
        (mb > 0).then(|| mb * 1024 * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_is_valid_once_it_has_a_password() {
        let text = EXAMPLE.replace("password = \"\"", "password = \"a-long-password\"");
        let settings = Settings::parse(&text).unwrap();
        assert_eq!(settings.config.server.port, 47421);
        assert_eq!(settings.config.limits.tunnel_kbps, 512);
        assert!(Settings::parse(EXAMPLE).is_err(), "no password in the example");
    }

    #[test]
    fn only_the_password_is_needed() {
        let settings = Settings::parse("[auth]\npassword = \"12345678\"").unwrap();
        assert_eq!(settings.config.limits.max_tunnels_per_pc, 4);
        assert_eq!(settings.config.server.log, Log::Info);
    }

    #[test]
    fn typos_and_bad_values_are_errors() {
        for text in [
            "[auth]\npassword = \"short\"",
            "[auth]\npassword = \"12345678\"\npasword = \"x\"",
            "[auth]\npassword = \"12345678\"\n[limits]\nmax_pcs = 0",
            "[auth]\npassword = \"12345678\"\n[timeouts]\nidle_secs = 10",
            "[auth]\npassword = \"12345678\"\n[access]\ndeny_ips = [\"10.0.0.0/40\"]",
            "[auth]\npassword = \"12345678\"\n[access]\nallow_pcs = [\"abc\"]",
            "[server]\nlog = \"loud\"\n[auth]\npassword = \"12345678\"",
        ] {
            assert!(Settings::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn ranges_match_both_families() {
        let net = Net::parse("192.168.0.0/16").unwrap();
        assert!(net.contains("192.168.4.2".parse().unwrap()));
        assert!(net.contains("::ffff:192.168.4.2".parse().unwrap()), "IPv4 seen through IPv6");
        assert!(!net.contains("10.0.0.1".parse().unwrap()));
        assert!(Net::parse("0.0.0.0/0").unwrap().contains("8.8.8.8".parse().unwrap()));
        assert!(Net::parse("2001:db8::/32").unwrap().contains("2001:db8::1".parse().unwrap()));
        assert!(!Net::parse("2001:db8::/32").unwrap().contains("192.168.0.1".parse().unwrap()));
        assert!(Net::parse("10.0.0.5").unwrap().contains("10.0.0.5".parse().unwrap()));
    }

    #[test]
    fn deny_wins_over_allow() {
        let text = "[auth]\npassword = \"12345678\"\n[access]\nallow_ips = [\"10.0.0.0/8\"]\ndeny_ips = [\"10.0.0.9\"]";
        let settings = Settings::parse(text).unwrap();
        assert!(settings.ip_allowed("10.1.2.3".parse().unwrap()));
        assert!(!settings.ip_allowed("10.0.0.9".parse().unwrap()));
        assert!(!settings.ip_allowed("11.0.0.1".parse().unwrap()));
    }

    #[test]
    fn fingerprints_can_be_pasted_with_spaces_and_colons() {
        let fp = "ab".repeat(32);
        let spaced: String = fp.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
        let text = format!("[auth]\npassword = \"12345678\"\n[access]\nallow_pcs = [\"{}\"]", spaced.to_uppercase());
        let settings = Settings::parse(&text).unwrap();
        assert!(settings.pc_allowed(&fp));
        assert!(!settings.pc_allowed(&"cd".repeat(32)));
    }
}
