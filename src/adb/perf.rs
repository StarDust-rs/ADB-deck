//! Performance sampling: CPU, memory, battery, temperature, FPS.

use std::collections::BTreeMap;
use std::time::Instant;

use super::overview::parse_meminfo;
use super::{sh_quote, DeviceAdb};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuTimes {
    pub busy: u64,
    pub idle: u64,
}

/// Per-core times from `/proc/stat`; the aggregate `cpu ` line is skipped.
pub fn parse_cpu_times(text: &str) -> Vec<CpuTimes> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let label = parts.next()?;
            if !label.starts_with("cpu") || label == "cpu" {
                return None;
            }
            let v: Vec<u64> = parts.filter_map(|p| p.parse().ok()).collect();
            if v.len() < 7 {
                return None;
            }
            // user nice system idle iowait irq softirq [steal]
            let idle = v[3] + v[4];
            let busy = v[0] + v[1] + v[2] + v[5] + v[6] + v.get(7).copied().unwrap_or(0);
            Some(CpuTimes { busy, idle })
        })
        .collect()
}

/// Load in 0.0..=1.0 between two samples of the same core.
pub fn cpu_load(prev: CpuTimes, now: CpuTimes) -> f32 {
    let busy = now.busy.saturating_sub(prev.busy);
    let total = busy + now.idle.saturating_sub(prev.idle);
    if total == 0 {
        0.0
    } else {
        busy as f32 / total as f32
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Battery {
    pub level: u32,
    pub temperature_c: f32,
    pub voltage_v: f32,
}

pub fn parse_battery(text: &str) -> Battery {
    let field = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.trim().split_once(':')?;
            (k.trim() == key).then(|| v.trim().parse::<f32>().ok()).flatten()
        })
    };
    Battery {
        level: field("level").unwrap_or(0.0) as u32,
        temperature_c: field("temperature").unwrap_or(0.0) / 10.0,
        voltage_v: field("voltage").unwrap_or(0.0) / 1000.0,
    }
}

/// Average of all `CPU*` temperatures in `dumpsys thermalservice` output.
pub fn parse_cpu_temperature(text: &str) -> Option<f32> {
    let temps: Vec<f32> = text
        .lines()
        .filter_map(|line| {
            let value = line.split("mValue=").nth(1)?.split(',').next()?.trim().parse::<f32>().ok()?;
            let name = line.split("mName=").nth(1)?.split([',', '}']).next()?.trim();
            name.starts_with("CPU").then_some(value)
        })
        .collect();
    (!temps.is_empty()).then(|| temps.iter().sum::<f32>() / temps.len() as f32)
}

/// cpu index -> MHz from lines like `/sys/devices/system/cpu/cpu3/cpufreq/scaling_cur_freq 1804800`.
pub fn parse_cpu_freqs(text: &str) -> BTreeMap<usize, u32> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let path = parts.next()?;
            let khz: u64 = parts.next()?.parse().ok()?;
            let idx = path.split('/').find_map(|seg| seg.strip_prefix("cpu")?.parse::<usize>().ok())?;
            Some((idx, (khz / 1000) as u32))
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopActivity {
    pub package: String,
    pub activity: String,
    pub pid: u32,
}

/// Last `ACTIVITY pkg/act hash pid=N` line of `dumpsys activity top`.
pub fn parse_top_activity(text: &str) -> TopActivity {
    let Some(line) = text.lines().rev().find(|l| l.trim_start().starts_with("ACTIVITY ")) else {
        return TopActivity::default();
    };
    let mut parts = line.split_whitespace().skip(1);
    let component = parts.next().unwrap_or("");
    let pid = line.split("pid=").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|p| p.parse().ok()).unwrap_or(0);
    match component.split_once('/') {
        Some((pkg, act)) => TopActivity { package: pkg.into(), activity: act.into(), pid },
        None => TopActivity { package: component.into(), activity: String::new(), pid },
    }
}

pub fn parse_flips(text: &str) -> Option<u64> {
    text.split("flips=").nth(1)?.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

/// Turns successive page-flip counters into frames per second.
#[derive(Debug, Default)]
pub struct FpsTracker {
    last: Option<(u64, Instant)>,
}

impl FpsTracker {
    pub fn update(&mut self, flips: u64, at: Instant) -> u32 {
        let fps = match self.last {
            Some((prev, t)) if flips >= prev => {
                let secs = at.duration_since(t).as_secs_f64();
                if secs > 0.0 {
                    ((flips - prev) as f64 / secs).round() as u32
                } else {
                    0
                }
            }
            _ => 0,
        };
        self.last = Some((flips, at));
        fps
    }
}

/// FPS from `dumpsys SurfaceFlinger --latency` output (nanosecond columns).
pub fn fps_from_latency(text: &str) -> u32 {
    let mut stamps: Vec<f64> = text
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            (cols.len() == 3).then(|| cols[1].parse::<f64>().ok()).flatten()
        })
        // Unused slots are reported as i64::MAX.
        .filter(|v| *v < 9.0e18)
        .map(|v| v / 1e9)
        .collect();
    stamps.pop();
    match (stamps.first(), stamps.last()) {
        (Some(first), Some(last)) if stamps.len() > 1 && last > first => ((stamps.len() - 1) as f64 / (last - first)).round() as u32,
        _ => 0,
    }
}

#[derive(Debug, Clone, Default)]
pub struct PerfSample {
    pub uptime_ms: u64,
    pub cpus: Vec<CpuTimes>,
    pub freqs_mhz: BTreeMap<usize, u32>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub battery: Battery,
    pub cpu_temp_c: Option<f32>,
    pub flips: Option<u64>,
    pub top: TopActivity,
}

const SAMPLE_SCRIPTS: [&str; 8] = [
    "cat /proc/uptime",
    "grep '^cpu' /proc/stat",
    "for f in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq; do echo \"$f $(cat $f 2>/dev/null)\"; done",
    "grep -E '^(MemTotal|MemAvailable|MemFree)' /proc/meminfo",
    "dumpsys battery",
    "dumpsys thermalservice | grep -E 'mName=CPU'",
    "dumpsys SurfaceFlinger | grep -m1 flips=",
    "dumpsys activity top | grep ACTIVITY",
];

impl DeviceAdb {
    /// One round trip collecting everything the performance view needs.
    pub async fn perf_sample(&self) -> Result<PerfSample> {
        let p = self.shell_batch(&SAMPLE_SCRIPTS).await?;
        let uptime = p[0].split_whitespace().next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let (mem_total, mem_free) = parse_meminfo(&p[3]);
        Ok(PerfSample {
            uptime_ms: (uptime * 1000.0) as u64,
            cpus: parse_cpu_times(&p[1]),
            freqs_mhz: parse_cpu_freqs(&p[2]),
            mem_total,
            mem_used: mem_total.saturating_sub(mem_free),
            battery: parse_battery(&p[4]),
            cpu_temp_c: parse_cpu_temperature(&p[5]),
            flips: parse_flips(&p[6]),
            top: parse_top_activity(&p[7]),
        })
    }

    /// Fallback for devices without a `flips=` counter: measure the frame
    /// cadence of the foreground app's layers.
    pub async fn fps_by_latency(&self, package: &str) -> Result<u32> {
        if package.is_empty() {
            return Ok(0);
        }
        let list = self.shell(&format!("dumpsys SurfaceFlinger --list | grep -F {}", sh_quote(package))).await?;
        let layers: Vec<&str> = list
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter(|l| !["ActivityRecordInputSink", "Background for SurfaceView", "Bounds for -"].iter().any(|m| l.contains(m)))
            .take(6)
            .collect();
        if layers.is_empty() {
            return Ok(0);
        }
        let scripts: Vec<String> = layers.iter().map(|l| format!("dumpsys SurfaceFlinger --latency {}", sh_quote(l))).collect();
        let refs: Vec<&str> = scripts.iter().map(String::as_str).collect();
        let outputs = self.shell_batch(&refs).await?;
        Ok(outputs.iter().map(|o| fps_from_latency(o)).max().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cpu_times_skip_aggregate_and_compute_load() {
        let a = parse_cpu_times("cpu  10 0 10 80 0 0 0 0\ncpu0 10 0 10 80 0 0 0 0\ncpu1 0 0 0 100 0 0 0\nintr 1 2 3\n");
        assert_eq!(a.len(), 2);
        let b = parse_cpu_times("cpu0 30 0 30 140 0 0 0 0\ncpu1 0 0 0 200 0 0 0\n");
        assert!((cpu_load(a[0], b[0]) - 0.4).abs() < 1e-6);
        assert_eq!(cpu_load(a[1], b[1]), 0.0);
        assert_eq!(cpu_load(a[0], a[0]), 0.0);
    }

    #[test]
    fn battery_fields() {
        let b = parse_battery("Current Battery Service state:\n  level: 87\n  voltage: 4123\n  temperature: 251\n");
        assert_eq!(b, Battery { level: 87, temperature_c: 25.1, voltage_v: 4.123 });
        assert_eq!(parse_battery(""), Battery::default());
    }

    #[test]
    fn cpu_temperature_averages_cpu_sensors_only() {
        let text = "Temperature{mValue=40.0, mType=0, mName=CPU0, mStatus=0}\n\
                    Temperature{mValue=50.0, mType=0, mName=CPU1, mStatus=0}\n\
                    Temperature{mValue=30.0, mType=2, mName=BAT, mStatus=0}\n";
        assert_eq!(parse_cpu_temperature(text), Some(45.0));
        assert_eq!(parse_cpu_temperature("none"), None);
    }

    #[test]
    fn frequencies_are_mapped_by_core_index() {
        let f = parse_cpu_freqs("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq 1804800\n/sys/devices/system/cpu/cpu10/cpufreq/scaling_cur_freq 500000\n/sys/x/cpu2/cpufreq/scaling_cur_freq \n");
        assert_eq!(f[&0], 1804);
        assert_eq!(f[&10], 500);
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn top_activity_uses_the_last_entry() {
        let text = "  ACTIVITY com.android.launcher3/.Launcher 1a2b pid=1000\n  ACTIVITY com.example/.Main 3c4d pid=2345\n";
        assert_eq!(parse_top_activity(text), TopActivity { package: "com.example".into(), activity: ".Main".into(), pid: 2345 });
        assert_eq!(parse_top_activity(""), TopActivity::default());
    }

    #[test]
    fn fps_from_flip_counter() {
        let mut t = FpsTracker::default();
        let start = Instant::now();
        assert_eq!(t.update(1000, start), 0);
        assert_eq!(t.update(1060, start + Duration::from_secs(1)), 60);
        assert_eq!(t.update(10, start + Duration::from_secs(2)), 0, "counter reset");
        assert_eq!(parse_flips("  flips=12345 (x)"), Some(12345));
        assert_eq!(parse_flips("nothing"), None);
    }

    #[test]
    fn fps_from_latency_dump() {
        let mut text = String::from("16666667\n");
        for i in 0..61u64 {
            let t = 1_000_000_000_000u64 + i * 16_666_667;
            text.push_str(&format!("{t}\t{}\t{}\n", t + 1000, t + 2000));
        }
        text.push_str("0\t9223372036854775807\t0\n");
        assert_eq!(fps_from_latency(&text), 60);
        assert_eq!(fps_from_latency(""), 0);
    }
}
