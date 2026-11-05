//! Domain-list classification only; no upstream selection or network I/O.

use crate::config::{Config, Tag};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTag {
    Chn,
    Gfw,
    None,
    /// Zero-based index into `Config::groups` (declaration order).
    Group(usize),
}

const MAX_NAME_LEVEL: usize = 8;

fn last_eight_labels(name: &[u8]) -> &[u8] {
    let len = name
        .rsplit(|&b| b == b'.')
        .take(MAX_NAME_LEVEL)
        .map(|label| label.len() + 1)
        .sum::<usize>()
        - 1;
    &name[name.len() - len..]
}

fn check_name(token: &[u8]) -> Option<&[u8]> {
    if token.is_empty() || matches!(token[0], b'#' | b'.') || token.last() == Some(&b'.') {
        return None;
    }
    // Match upstream check_name: discard left-hand labels before validating them.
    let name = last_eight_labels(token);
    name.split(|&b| b == b'.')
        .all(|label| !label.is_empty() && label.len() <= 63)
        .then_some(name)
}

fn load(paths: &[String], tag: RouteTag, names: &mut HashMap<Vec<u8>, RouteTag>) {
    for path in paths {
        // Upstream load_list warns and continues when a file cannot be opened.
        // Do not interpret special paths (including "-") as stdin or other inputs.
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        for token in bytes.split(u8::is_ascii_whitespace) {
            if let Some(name) = check_name(token) {
                // dnl_init inserts high-priority tags first; duplicates keep that tag.
                names.entry(name.to_vec()).or_insert(tag);
            }
        }
    }
}

/// Resolve a case-sensitive, dot-separated qname using only files named in config.
///
/// Priority breaks ties for the same suffix: later custom groups, then gfw, then
/// chn. A longer suffix always wins, even over a higher-priority tag. As in
/// upstream dnl_get_tag, qname is the presentation name without a final root dot.
/// Files that cannot be read are skipped, like upstream load_list. Lists are read
/// per call; loading once at startup / wiring into the server is outside M2-01.
#[allow(dead_code)] // M2-01 compatibility API; the server uses the load-once Router.
pub fn resolve(qname: &str, config: &Config) -> RouteTag {
    Router::load(config).resolve(qname)
}

pub struct Router {
    names: HashMap<Vec<u8>, RouteTag>,
    default: RouteTag,
}

impl Router {
    pub fn load(config: &Config) -> Self {
        let mut names = HashMap::new();
        for (index, group) in config.groups.iter().enumerate().rev() {
            load(&group.dnl, RouteTag::Group(index), &mut names);
        }
        load(&config.gfwlist_file, RouteTag::Gfw, &mut names);
        load(&config.chnlist_file, RouteTag::Chn, &mut names);

        let default = match config.default_tag {
            Some(Tag::Chn) => RouteTag::Chn,
            Some(Tag::Gfw) => RouteTag::Gfw,
            Some(Tag::None) | None => RouteTag::None,
        };
        Self { names, default }
    }

    pub fn resolve(&self, qname: &str) -> RouteTag {
        let mut suffix = last_eight_labels(qname.as_bytes());
        loop {
            if let Some(&tag) = self.names.get(suffix) {
                return tag;
            }
            match suffix.iter().position(|&b| b == b'.') {
                Some(dot) => suffix = &suffix[dot + 1..],
                None => break,
            }
        }
        self.default
    }
}
