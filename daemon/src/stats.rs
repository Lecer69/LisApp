//! This PC's figures for the phone.

use lis_core::protocol::PcStats;
use std::fs;

#[derive(Default)]
pub struct Sampler {
    last: Option<(u64, u64)>,
}

impl Sampler {
    pub fn sample(&mut self) -> PcStats {
        let (memory_total, memory_available) = memory();
        PcStats {
            hostname: hostname(),
            cpu_percent: self.cpu(),
            memory_used: memory_total.saturating_sub(memory_available),
            memory_total,
            uptime_secs: uptime(),
        }
    }

    fn cpu(&mut self) -> f32 {
        let Some((total, idle)) = cpu_times() else { return 0.0 };
        let percent = match self.last {
            Some((last_total, last_idle)) if total > last_total => {
                let busy = (total - last_total).saturating_sub(idle.saturating_sub(last_idle));
                busy as f32 * 100.0 / (total - last_total) as f32
            }
            _ => 0.0,
        };
        self.last = Some((total, idle));
        percent
    }
}

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname").map(|name| name.trim().to_string()).unwrap_or_else(|_| "PC".to_string())
}

fn cpu_times() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let fields: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|field| field.parse().ok()).collect();
    let idle = fields.get(3)? + fields.get(4).unwrap_or(&0);
    Some((fields.iter().take(8).sum(), idle))
}

/// Bytes: total and available.
fn memory() -> (u64, u64) {
    let info = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        info.lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<u64>().ok())
            .map_or(0, |kb| kb * 1024)
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

fn uptime() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0, |seconds| seconds as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_real_figures() {
        let mut sampler = Sampler::default();
        let first = sampler.sample();
        assert!(!first.hostname.is_empty());
        assert!(first.memory_total > 0 && first.memory_used <= first.memory_total);
        assert!(first.uptime_secs > 0);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let second = sampler.sample();
        assert!((0.0..=100.0).contains(&second.cpu_percent));
    }
}
