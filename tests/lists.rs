#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[path = "../src/lists.rs"]
mod lists;

use config::{Config, Group, Tag};
use lists::{RouteTag, resolve};
use std::io::Write;
use std::path::Path;

fn path(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/lists")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

fn standard() -> Config {
    Config {
        gfwlist_file: vec![path("gfw.txt")],
        chnlist_file: vec![path("chn.txt")],
        groups: ["uproxy", "udirect", "boot"]
            .into_iter()
            .map(|name| Group {
                name: name.into(),
                dnl: vec![path(&format!("{name}.txt"))],
                ..Group::default()
            })
            .collect(),
        ..Config::default()
    }
}

fn one_file(name: &str) -> Config {
    Config {
        gfwlist_file: vec![path(name)],
        ..Config::default()
    }
}

macro_rules! lookup_cases {
    ($($name:ident => $body:block),+ $(,)?) => {
        $(#[test]
        fn $name() $body)+

        #[test]
        fn lookup_summary() {
            let cases: &[fn()] = &[$($name),+];
            let mut passed = 0;
            for case in cases {
                case();
                passed += 1;
            }
            // Write directly so the required summary is visible with default capture.
            writeln!(std::io::stdout().lock(), "{passed}/{} lookup cases passed", cases.len()).unwrap();
        }
    };
}

lookup_cases! {
    exact_gfw => {
        assert_eq!(resolve("example.com", &standard()), RouteTag::Gfw);
    },
    exact_chn => {
        assert_eq!(resolve("china.test", &standard()), RouteTag::Chn);
    },
    subdomain => {
        assert_eq!(resolve("a.b.example.com", &standard()), RouteTag::Gfw);
    },
    label_boundary => {
        assert_eq!(resolve("notexample.com", &standard()), RouteTag::None);
    },
    non_suffix => {
        assert_eq!(resolve("example.com.other", &standard()), RouteTag::None);
    },
    longest_beats_group_priority => {
        assert_eq!(resolve("x.specific.long.test", &standard()), RouteTag::Gfw);
    },
    longest_beats_gfw_priority => {
        let mut c = standard();
        c.gfwlist_file = vec![path("uproxy.txt")];
        assert_eq!(resolve("narrow.custom.test", &c), RouteTag::Chn);
    },
    gfw_before_chn => {
        assert_eq!(resolve("shared.test", &standard()), RouteTag::Gfw);
    },
    custom_before_builtin => {
        assert_eq!(resolve("custom.test", &standard()), RouteTag::Group(0));
    },
    last_group_wins => {
        assert_eq!(resolve("x.both.test", &standard()), RouteTag::Group(2));
    },
    second_group_index => {
        let mut c = standard();
        c.groups.pop();
        assert_eq!(resolve("both.test", &c), RouteTag::Group(1));
    },
    declaration_not_group_name => {
        let mut c = standard();
        c.groups.swap(0, 2);
        assert_eq!(resolve("custom.test", &c), RouteTag::Group(2));
    },
    default_absent => {
        assert_eq!(resolve("unlisted.test", &standard()), RouteTag::None);
    },
    default_none => {
        let c = Config { default_tag: Some(Tag::None), ..Config::default() };
        assert_eq!(resolve("unlisted.test", &c), RouteTag::None);
    },
    default_chn => {
        let c = Config { default_tag: Some(Tag::Chn), ..Config::default() };
        assert_eq!(resolve("unlisted.test", &c), RouteTag::Chn);
    },
    default_gfw => {
        let c = Config { default_tag: Some(Tag::Gfw), ..Config::default() };
        assert_eq!(resolve("unlisted.test", &c), RouteTag::Gfw);
    },
    match_overrides_default => {
        let mut c = standard();
        c.default_tag = Some(Tag::Chn);
        assert_eq!(resolve("example.com", &c), RouteTag::Gfw);
    },
    case_sensitive => {
        assert_eq!(resolve("Case.Example", &standard()), RouteTag::Gfw);
        assert_eq!(resolve("case.example", &standard()), RouteTag::None);
        assert_eq!(resolve("EXAMPLE.COM", &standard()), RouteTag::None);
    },
    whitespace_tokens => {
        let c = one_file("tokens.txt");
        for name in ["first.test", "second.test", "third.test", "fourth.test", "repeat.test"] {
            assert_eq!(resolve(name, &c), RouteTag::Gfw, "{name}");
        }
    },
    ignored_boundaries => {
        let c = one_file("tokens.txt");
        for name in ["ignored.test", ".start.test", "start.test", "end.test", "end.test.", "", ".", "#"] {
            assert_eq!(resolve(name, &c), RouteTag::None, "{name}");
        }
    },
    gfw_files_append => {
        let c = Config::parse("lists.conf", format!("gfwlist-file {}\ngfwlist-file {},{}\n", path("gfw.txt"), path("tokens.txt"), path("ten.txt")).as_bytes()).unwrap();
        for name in ["example.com", "first.test", "a.b.c.d.e.f.g.test"] {
            assert_eq!(resolve(name, &c), RouteTag::Gfw, "{name}");
        }
    },
    chn_files_append => {
        let c = Config::parse("lists.conf", format!("chnlist-file {},{}\nchnlist-file {}\n", path("chn.txt"), path("tokens.txt"), path("apple.txt")).as_bytes()).unwrap();
        for name in ["china.test", "third.test", "ls.apple.com.edgekey.net.globalredir.akadns.net"] {
            assert_eq!(resolve(name, &c), RouteTag::Chn, "{name}");
        }
    },
    group_files_append => {
        let c = Config::parse("lists.conf", format!("group uproxy\ngroup-dnl {}\ngroup-dnl {},{}\n", path("uproxy.txt"), path("tokens.txt"), path("apple.txt")).as_bytes()).unwrap();
        for name in ["custom.test", "second.test", "ls.apple.com.edgekey.net.globalredir.akadns.net"] {
            assert_eq!(resolve(name, &c), RouteTag::Group(0), "{name}");
        }
    },
    apple_nine_to_eight => {
        assert_eq!(resolve("ls.apple.com.edgekey.net.globalredir.akadns.net", &one_file("apple.txt")), RouteTag::Gfw);
    },
    apple_nine_label_query => {
        assert_eq!(resolve("gsp4-cn.ls.apple.com.edgekey.net.globalredir.akadns.net", &one_file("apple.txt")), RouteTag::Gfw);
    },
    apple_seven_labels_miss => {
        assert_eq!(resolve("apple.com.edgekey.net.globalredir.akadns.net", &one_file("apple.txt")), RouteTag::None);
    },
    ten_label_token => {
        let c = one_file("ten.txt");
        for name in ["a.b.c.d.e.f.g.test", "also.a.b.c.d.e.f.g.test", "discard.also.a.b.c.d.e.f.g.test", "different.a.b.c.d.e.f.g.test"] {
            assert_eq!(resolve(name, &c), RouteTag::Gfw, "{name}");
        }
        assert_eq!(resolve("b.c.d.e.f.g.test", &c), RouteTag::None);
    },
    retained_label_validation => {
        let c = one_file("validation.txt");
        assert_eq!(resolve(&format!("{}.bad.test", "a".repeat(64)), &c), RouteTag::None);
        assert_eq!(resolve("a..bad.test", &c), RouteTag::None);
        assert_eq!(resolve(&format!("{}.limit.test", "a".repeat(63)), &c), RouteTag::Gfw);
    },
    discarded_labels_not_validated => {
        let c = one_file("validation.txt");
        assert_eq!(resolve("a.b.c.d.e.f.g.valid", &c), RouteTag::Gfw);
        assert_eq!(resolve("a.b.c.d.e.f.g.empty", &c), RouteTag::Gfw);
    },
    boundary_check_before_truncation => {
        assert_eq!(resolve("a.b.c.d.e.f.g.test", &one_file("validation.txt")), RouteTag::None);
    },
    unreadable_file_skipped => {
        let mut c = one_file("missing.txt");
        c.gfwlist_file.push(path("gfw.txt"));
        assert_eq!(resolve("example.com", &c), RouteTag::Gfw);
        assert_eq!(resolve("missing.test", &c), RouteTag::None);
    },
}
