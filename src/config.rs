use std::collections::BTreeSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    Udp,
    Tcp,
    Tls,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub protocol: Option<Protocol>,
    pub host: Option<String>,
    pub ip: IpAddr,
    pub port: Option<u16>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    Chn,
    Gfw,
    None,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetName {
    pub family: String,
    pub table: String,
    pub set: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPair {
    pub v4: Option<SetName>,
    pub v6: Option<SetName>,
}
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub dnl: Vec<String>,
    pub upstream: Vec<Upstream>,
    pub ipset: Option<SetPair>,
}
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind_addr: Vec<Ipv4Addr>,
    pub bind_port: Vec<u16>,
    pub china_dns: Vec<Upstream>,
    pub trust_dns: Vec<Upstream>,
    pub ipset_name4: Option<SetName>,
    pub ipset_name6: Option<SetName>,
    pub gfwlist_file: Vec<String>,
    pub chnlist_file: Vec<String>,
    pub default_tag: Option<Tag>,
    pub add_taggfw_ip: Option<SetPair>,
    pub groups: Vec<Group>,
    pub no_ipv6: usize,
    pub filter_qtype: BTreeSet<u16>,
    pub cache: Option<u64>,
    pub cache_stale: Option<u64>,
    pub verdict_cache: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub file: String,
    pub line: usize,
    pub message: String,
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}
impl std::error::Error for ConfigError {}

fn port(s: &str) -> Result<u16, String> {
    if s.contains('@') {
        return Err("bind-port@proto 属于 P1，未实现".into());
    }
    let n: u16 = s.parse().map_err(|_| "非法端口")?;
    if n == 0 {
        return Err("非法端口".into());
    }
    Ok(n)
}
fn list(s: &str) -> Result<Vec<&str>, String> {
    let values: Vec<_> = s.split(',').collect();
    if values.iter().any(|x| x.is_empty()) {
        return Err("空列表项".into());
    }
    Ok(values)
}
fn upstreams(s: &str) -> Result<Vec<Upstream>, String> {
    list(s)?
        .into_iter()
        .map(|item| {
            let (protocol, addr) = if let Some((p, addr)) = item.split_once("://") {
                (
                    Some(match p {
                        "udp" => Protocol::Udp,
                        "tcp" => Protocol::Tcp,
                        "tls" => Protocol::Tls,
                        _ => return Err("未知上游协议".into()),
                    }),
                    addr,
                )
            } else {
                (None, item)
            };
            let (host, addr) = match addr.split_once('@') {
                Some((h, a)) if !h.is_empty() => (Some(h.to_owned()), a),
                Some(_) => return Err("空主机名".into()),
                None => (None, addr),
            };
            let (ip, port_num) = match addr.split_once('#') {
                Some((ip, p)) => (ip, Some(port(p)?)),
                None => (addr, None),
            };
            Ok(Upstream {
                protocol,
                host,
                ip: ip.parse().map_err(|_| "非法上游 IP")?,
                port: port_num,
            })
        })
        .collect()
}
fn set_name(s: &str) -> Result<Option<SetName>, String> {
    if s == "null" {
        return Ok(None);
    }
    let parts: Vec<_> = s.split('@').collect();
    if parts.len() != 3
        || !matches!(parts[0], "inet" | "ip" | "ip6")
        || parts.iter().any(|x| x.is_empty())
    {
        return Err("非法集合名".into());
    }
    Ok(Some(SetName {
        family: parts[0].into(),
        table: parts[1].into(),
        set: parts[2].into(),
    }))
}
fn pair(s: &str) -> Result<SetPair, String> {
    let parts = list(s)?;
    if parts.len() != 2 {
        return Err("集合对必须包含两个值".into());
    }
    Ok(SetPair {
        v4: set_name(parts[0])?,
        v6: set_name(parts[1])?,
    })
}
fn paths(s: &str) -> Result<Vec<String>, String> {
    Ok(list(s)?.into_iter().map(str::to_owned).collect())
}
fn number(s: &str) -> Result<u64, String> {
    if !s.bytes().all(|b| b.is_ascii_digit()) || s.is_empty() {
        return Err("非法非负整数".into());
    }
    s.parse().map_err(|_| "非法非负整数".into())
}
impl Config {
    pub fn parse(file: &str, input: &[u8]) -> Result<Self, ConfigError> {
        let text = std::str::from_utf8(input).map_err(|e| ConfigError {
            file: file.into(),
            line: input[..e.valid_up_to()]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1,
            message: "配置不是 UTF-8".into(),
        })?;
        let mut config = Self::default();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let result = config.parse_line(line);
            if let Err(message) = result {
                return Err(ConfigError {
                    file: file.into(),
                    line: index + 1,
                    message,
                });
            }
        }
        Ok(config)
    }
    pub fn read(path: &Path) -> Result<Self, ConfigError> {
        let file = path.display().to_string();
        let bytes = std::fs::read(path).map_err(|e| ConfigError {
            file: file.clone(),
            line: 1,
            message: e.to_string(),
        })?;
        Self::parse(&file, &bytes)
    }
    pub fn validate_startup(&self) -> Result<(), &'static str> {
        if self
            .china_dns
            .iter()
            .chain(&self.trust_dns)
            .chain(self.groups.iter().flat_map(|g| &g.upstream))
            .any(|u| u.protocol == Some(Protocol::Tls))
        {
            return Err("tls:// P0-conditional 未实现");
        }
        Ok(())
    }
    fn parse_line(&mut self, line: &str) -> Result<(), String> {
        let mut fields = line.split_whitespace();
        let key = fields.next().ok_or("空指令")?;
        let value = fields.next();
        if fields.next().is_some() {
            return Err("不支持多余参数或行尾注释".into());
        }
        if key == "no-ipv6" {
            if value.is_some() {
                return Err("no-ipv6 规则未实现".into());
            }
            self.no_ipv6 += 1;
            return Ok(());
        }
        let value = value.ok_or("缺少值")?;
        match key {
            "bind-addr" => self
                .bind_addr
                .push(value.parse().map_err(|_| "非法 IPv4 bind 地址")?),
            "bind-port" => self.bind_port.push(port(value)?),
            "china-dns" => self.china_dns.extend(upstreams(value)?),
            "trust-dns" => self.trust_dns.extend(upstreams(value)?),
            "ipset-name4" => self.ipset_name4 = set_name(value)?,
            "ipset-name6" => self.ipset_name6 = set_name(value)?,
            "gfwlist-file" => self.gfwlist_file.extend(paths(value)?),
            "chnlist-file" => self.chnlist_file.extend(paths(value)?),
            "default-tag" => {
                self.default_tag = Some(match value {
                    "chn" => Tag::Chn,
                    "gfw" => Tag::Gfw,
                    "none" => Tag::None,
                    _ => return Err("非法 default-tag".into()),
                })
            }
            "add-taggfw-ip" => self.add_taggfw_ip = Some(pair(value)?),
            "group" => self.groups.push(Group {
                name: value.into(),
                ..Group::default()
            }),
            "group-dnl" => self
                .groups
                .last_mut()
                .ok_or("group 尚未声明")?
                .dnl
                .extend(paths(value)?),
            "group-upstream" => self
                .groups
                .last_mut()
                .ok_or("group 尚未声明")?
                .upstream
                .extend(upstreams(value)?),
            "group-ipset" => {
                self.groups.last_mut().ok_or("group 尚未声明")?.ipset = Some(pair(value)?)
            }
            "filter-qtype" => {
                for v in list(value)? {
                    self.filter_qtype
                        .insert(v.parse().map_err(|_| "非法 qtype")?);
                }
            }
            "cache" => self.cache = Some(number(value)?),
            "cache-stale" => self.cache_stale = Some(number(value)?),
            "verdict-cache" => self.verdict_cache = Some(number(value)?),
            _ => return Err(format!("未知指令 {key}")),
        }
        Ok(())
    }
}
