//! Process listing via the device's `top`.

use super::DeviceAdb;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub time: String,
    pub res_bytes: u64,
    pub user: String,
    /// Package the process belongs to, derived from its name.
    pub package: Option<String>,
}

const TOP_COMMAND: &str = "top -b -n 1 -o PID -o %CPU -o TIME+ -o RES -o USER -o ARGS";

/// "45M", "1.2G", "512K" or a bare KiB count.
pub fn parse_human_size(text: &str) -> u64 {
    let t = text.trim();
    let (num, mult) = match t.chars().last() {
        Some('K') | Some('k') => (&t[..t.len() - 1], 1024.0),
        Some('M') | Some('m') => (&t[..t.len() - 1], 1024.0 * 1024.0),
        Some('G') | Some('g') => (&t[..t.len() - 1], 1024.0 * 1024.0 * 1024.0),
        Some('T') | Some('t') => (&t[..t.len() - 1], 1024.0_f64.powi(4)),
        _ => (t, 1024.0),
    };
    num.parse::<f64>().map(|n| (n * mult) as u64).unwrap_or(0)
}

/// The package a process name refers to: the leading dotted identifier
/// (`com.foo:remote` -> `com.foo`).
pub fn package_of(process_name: &str) -> Option<String> {
    let pkg: String = process_name.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.').collect();
    (pkg.contains('.') && !pkg.starts_with('.')).then_some(pkg)
}

pub fn parse_top(text: &str) -> Vec<ProcessInfo> {
    let mut lines = text.lines();
    let header = loop {
        match lines.next() {
            Some(l) if l.trim_start().starts_with("PID") => break l,
            Some(_) => continue,
            None => return Vec::new(),
        }
    };
    let columns: Vec<String> = header.split_whitespace().map(|c| c.to_ascii_lowercase()).collect();
    let idx = |names: &[&str]| columns.iter().position(|c| names.contains(&c.as_str()));
    let (pid_i, cpu_i, time_i, res_i, user_i, args_i, name_i) = (
        idx(&["pid"]),
        idx(&["%cpu", "cpu%"]),
        idx(&["time+"]),
        idx(&["res", "rss"]),
        idx(&["user", "uid"]),
        idx(&["args", "command"]),
        idx(&["name"]),
    );
    let Some(pid_i) = pid_i else { return Vec::new() };

    lines
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let n = columns.len();
            // The last column (ARGS) keeps its internal spaces.
            let fields: Vec<&str> = {
                let mut out = Vec::with_capacity(n);
                let mut rest = line;
                for _ in 0..n - 1 {
                    rest = rest.trim_start();
                    match rest.find(char::is_whitespace) {
                        Some(p) => {
                            out.push(&rest[..p]);
                            rest = &rest[p..];
                        }
                        None => {
                            out.push(rest);
                            rest = "";
                        }
                    }
                }
                out.push(rest.trim());
                out
            };
            let get = |i: Option<usize>| i.and_then(|i| fields.get(i).copied()).unwrap_or("");
            let pid: u32 = fields.get(pid_i)?.parse().ok()?;
            let args = get(args_i);
            if args.starts_with("top -b") {
                return None;
            }
            let mut name = args.split_whitespace().next().unwrap_or("").to_string();
            if name.is_empty() {
                name = get(name_i).to_string();
            }
            let package = package_of(&name);
            Some(ProcessInfo {
                pid,
                package,
                name,
                cpu: get(cpu_i).parse().unwrap_or(0.0),
                time: get(time_i).to_string(),
                res_bytes: parse_human_size(get(res_i)),
                user: get(user_i).to_string(),
            })
        })
        .collect()
}

impl DeviceAdb {
    pub async fn processes(&self) -> Result<Vec<ProcessInfo>> {
        let text = self.shell(TOP_COMMAND).await?;
        Ok(parse_top(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: &str = "Tasks: 612 total,   1 running, 611 sleeping\n\
        Mem:   7.4G total,  6.8G used,  600M free\n\
        800%cpu   9%user   0%nice   4%sys 786%idle\n\
        \x20 PID %CPU     TIME+  RES USER         ARGS\n\
        \x2012345  3.0   0:12.34  145M u0_a123     com.example.app:remote\n\
        \x20  987  0.0   1:00.00  12M  system       /system/bin/surfaceflinger\n\
        \x20 4242  0.0   0:00.00  1.5G root         [kworker/0:1]\n\
        \x2025555  1.0   0:00.01  3.2M shell        top -b -n 1 -o PID -o %CPU\n";

    #[test]
    fn parses_columns_by_header_and_skips_the_probe_itself() {
        let procs = parse_top(TOP);
        assert_eq!(procs.len(), 3);
        let app = &procs[0];
        assert_eq!(app.pid, 12345);
        assert_eq!(app.name, "com.example.app:remote");
        assert_eq!(app.package.as_deref(), Some("com.example.app"));
        assert_eq!(app.cpu, 3.0);
        assert_eq!(app.res_bytes, 145 * 1024 * 1024);
        assert_eq!(app.user, "u0_a123");
        assert_eq!(procs[1].package, None);
        assert_eq!(procs[2].res_bytes, (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
    }

    #[test]
    fn garbage_without_header_yields_nothing() {
        assert!(parse_top("").is_empty());
        assert!(parse_top("top: not found").is_empty());
    }

    #[test]
    fn sizes_and_packages() {
        assert_eq!(parse_human_size("512K"), 512 * 1024);
        assert_eq!(parse_human_size("2048"), 2048 * 1024);
        assert_eq!(parse_human_size("x"), 0);
        assert_eq!(package_of("com.foo.bar:svc").as_deref(), Some("com.foo.bar"));
        assert_eq!(package_of("surfaceflinger"), None);
        assert_eq!(package_of("[kworker/0:1]"), None);
    }
}
