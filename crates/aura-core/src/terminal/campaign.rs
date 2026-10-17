//! Test-only state-machine campaign; no production inspection API is added.
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{
    Config, FileFailurePersistence, RngAlgorithm, TestError, TestRng, TestRunner,
};
use std::{env, fs, io::Read, path::PathBuf, time::Instant};

#[derive(Clone, Debug)]
enum Action {
    Bytes(Vec<u8>),
    Resize(usize, usize),
    View(i16),
    Limit(usize),
    Select(usize, usize, usize, usize),
    Clear,
    Edit(u8, usize, usize, usize),
}

#[derive(Clone, Debug)]
struct Settings {
    profile: String,
    seed: u64,
    algorithm: RngAlgorithm,
    cases: u32,
    actions: usize,
    width: usize,
    height: usize,
    history: usize,
    shrink: u32,
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> T {
    env::var(name).map_or(default, |value| {
        value.parse().unwrap_or_else(|_| panic!("invalid {name}"))
    })
}

impl Settings {
    fn read() -> Self {
        let profile = env::var("AURA_PROFILE").unwrap_or_else(|_| "quick".into());
        let (cases, actions, width, height, history, shrink) = match profile.as_str() {
            "quick" => (64, 32, 12, 6, 32, 1024),
            "extended" => (2048, 96, 24, 12, 128, 4096),
            "exhaustive" => (4096, 128, 32, 16, 256, 8192),
            _ => panic!("unknown AURA_PROFILE={profile}"),
        };
        let s = Self {
            profile,
            seed: number("AURA_SEED", 0x4155_5241),
            algorithm: match env::var("AURA_RNG").as_deref().unwrap_or("chacha") {
                "chacha" => RngAlgorithm::ChaCha,
                "xorshift" => RngAlgorithm::XorShift,
                _ => panic!("AURA_RNG must be chacha or xorshift"),
            },
            cases: number("AURA_CASES", cases),
            actions: number("AURA_ACTIONS", actions),
            width: number("AURA_WIDTH", width),
            height: number("AURA_HEIGHT", height),
            history: number("AURA_HISTORY", history),
            shrink: number("AURA_SHRINK", shrink),
        };
        assert!((1..=65536).contains(&s.cases) && (1..=512).contains(&s.actions));
        assert!((1..=128).contains(&s.width) && (1..=64).contains(&s.height) && s.history <= 4096);
        assert!(s.shrink <= 65536);
        s
    }

    fn runner(&self) -> TestRunner {
        // Expand the user seed in little-endian words; this mapping is part of
        // the replay contract, rather than a dependency's unspecified default.
        let seed: Vec<u8> = (0..4u64)
            .flat_map(|n| self.seed.wrapping_add(n).to_le_bytes())
            .collect();
        let len = if self.algorithm == RngAlgorithm::XorShift {
            16
        } else {
            32
        };
        TestRunner::new_with_rng(
            Config {
                cases: self.cases,
                max_shrink_iters: self.shrink,
                max_shrink_time: 0,
                source_file: Some(file!()),
                failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
                    "proptest-regressions",
                ))),
                ..Config::default()
            },
            TestRng::from_seed(self.algorithm, &seed[..len]),
        )
    }
}

fn corpus() -> Vec<Vec<u8>> {
    [
        "A",
        " \u{a0}\u{2009}\u{3000}",
        "\t\r\n\x08",
        "\u{301}",
        "e\u{301}\u{308}",
        "界中",
        "👩\u{200d}💻👍🏽🇪🇸❤\u{fe0f}",
        "각",
        "العربية",
        "क्षि",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[?47h",
        "\x1b[?47l",
        "\x1bc",
        "\x1b7",
        "\x1b8",
        "\x1b[?6h",
        "\x1b[?6l",
        "\x1b[?7l",
        "\x1b[?7h",
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[2;3r",
        "\x1b[r",
        "\x1b[31;44m",
        "\x1b[0m",
        "\x1b[6n",
        "\x1b]0;ÜМ\x1b\\",
        "\x1b]8;;https://example.org\x1b\\",
        "\x1b]8;;\x1b\\",
        "\x1b]52;c;YQ==\x07",
        "\x1b]133;A\x07",
        "\x1bPignored\x1b\\",
    ]
    .into_iter()
    .map(|s| s.as_bytes().to_vec())
    .chain([
        vec![0xff, 0xe2, 0x82],
        vec![0x84, 0x85, 0x88, 0x8d, 0x9b, b'H'],
    ])
    .collect()
}

fn actions(s: &Settings) -> BoxedStrategy<Vec<Action>> {
    let vt = (
        1u8..=12,
        prop::sample::select(vec![
            'A', 'B', 'C', 'D', 'G', 'H', 'J', 'K', 'X', '@', 'P', 'L', 'M', 'S', 'T',
        ]),
    )
        .prop_map(|(n, op)| Action::Bytes(format!("\x1b[{n}{op}").into_bytes()));
    let action = prop_oneof![
        7 => prop::sample::select(corpus()).prop_map(Action::Bytes),
        3 => vt,
        2 => prop::collection::vec(any::<u8>(), 0..33).prop_map(Action::Bytes),
        2 => any::<u8>().prop_map(|p| Action::Bytes(char::from_u32(0x2800 + u32::from(p)).unwrap().to_string().into_bytes())),
        2 => (0..=s.width, 0..=s.height).prop_map(|(w,h)| Action::Resize(w,h)),
        1 => (-32i16..=32).prop_map(Action::View),
        1 => (0..=s.history).prop_map(Action::Limit),
        1 => (0..s.height, 0..s.width, 0..s.height, 0..s.width).prop_map(|(a,b,c,d)| Action::Select(a,b,c,d)),
        1 => Just(Action::Clear),
    ];
    prop::collection::vec(action, 1..=s.actions).boxed()
}

fn apply(t: &mut Terminal, a: &Action, fragment: usize) -> Vec<u8> {
    match a {
        Action::Bytes(bytes) => {
            let mut replies = Vec::new();
            if fragment == 0 {
                replies.extend(t.process(bytes));
            } else {
                t.process(&[]);
                for chunk in bytes.chunks(fragment) {
                    replies.extend(t.process(chunk));
                    t.process(&[]);
                }
            }
            replies
        }
        Action::Resize(w, h) => {
            if *w == 0 || *h == 0 {
                let before = t.clone();
                assert!(t.resize(*w, *h).is_err());
                assert!(*t == before, "invalid resize mutated state");
            } else {
                t.resize(*w, *h).unwrap();
            }
            Vec::new()
        }
        Action::View(n) => {
            if *n < 0 {
                t.scroll_view_down(n.unsigned_abs() as usize);
            } else {
                t.scroll_view_up(*n as usize);
            }
            Vec::new()
        }
        Action::Limit(n) => {
            t.set_scrollback_limit(*n);
            Vec::new()
        }
        Action::Select(ar, ac, br, bc) => {
            // Coordinates are relative to the current viewport and clamp to
            // its edges, even when shrinking changes a generated precondition.
            let h = t.active_grid().height();
            let w = t.active_grid().width();
            t.set_selection(
                SelectionPoint {
                    line: t.viewport_line((*ar).min(h - 1)).unwrap(),
                    column: (*ac).min(w - 1),
                },
                SelectionPoint {
                    line: t.viewport_line((*br).min(h - 1)).unwrap(),
                    column: (*bc).min(w - 1),
                },
            );
            Vec::new()
        }
        Action::Clear => {
            t.clear_selection();
            Vec::new()
        }
        Action::Edit(op, r, c, n) => {
            let w = t.active_grid().width();
            let h = t.active_grid().height();
            let (r, c) = (*r % h, *c % w);
            let suffix = match op {
                0 => char::from(b'a' + (*n % 26) as u8).to_string(),
                1 => format!("\x1b[{}X", n + 1),
                2 => "\x1b[K".into(),
                3 => format!("\x1b[{}@", n + 1),
                4 => format!("\x1b[{}P", n + 1),
                5 => format!("\x1b[{}S", n + 1),
                6 => format!("\x1b[{}T", n + 1),
                7 => format!("\x1b[{}L", n + 1),
                8 => format!("\x1b[{}M", n + 1),
                _ => unreachable!(),
            };
            t.process(format!("\x1b[{};{}H{suffix}", r + 1, c + 1).as_bytes())
        }
    }
}

fn invariants(t: &Terminal) {
    let w = t.active_grid().width();
    let h = t.active_grid().height();
    assert!(t.top_margin <= t.bottom_margin && t.bottom_margin < h);
    assert_eq!(t.tabs.len(), w);
    assert!(
        t.hyperlinks.len() <= 4096
            && t.clipboard_requests.len() <= 16
            && t.shell_markers.len() <= 1024
    );
    for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
        let g = t.grid.screen(screen);
        g.assert_sidecar_ownership(&t.hyperlinks);
        assert_eq!((g.width(), g.height()), (w, h));
        assert!(g.cursor().column < w && g.cursor().row < h);
        assert!(g.scrollback_len() <= g.scrollback_limit());
        assert!(g.viewport_offset() <= g.scrollback_len());
        for line in g.first_line()..=g.last_line() {
            let row = g.row_at_line(line).unwrap();
            for (col, cell) in row.cells().iter().enumerate() {
                let flags = cell.flags();
                if flags.contains(CellFlags::WIDE_LEADING) {
                    assert!(
                        col + 1 < w
                            && row.cells()[col + 1]
                                .flags()
                                .contains(CellFlags::WIDE_SPACER),
                        "orphan leading screen={screen:?} line={line} col={col}"
                    );
                }
                if flags.contains(CellFlags::WIDE_SPACER) {
                    assert!(
                        col > 0
                            && row.cells()[col - 1]
                                .flags()
                                .contains(CellFlags::WIDE_LEADING),
                        "orphan spacer screen={screen:?} line={line} col={col}"
                    );
                    assert_eq!(cell.content, 0);
                }
                if let Some(text) = row.stored_grapheme_at(col) {
                    assert!(
                        text.len() <= 256
                            && !flags.contains(CellFlags::WIDE_SPACER)
                            && cell.content != 0
                    );
                    assert_eq!(text.chars().next().unwrap() as u32, cell.content);
                }
            }
        }
    }
    if let Some((a, b)) = t.selection() {
        let g = t.active_grid();
        for p in [a, b] {
            assert!((g.first_line()..=g.last_line()).contains(&p.line) && p.column < w);
        }
    }
    if let Some((a, b)) = t.selection_cell_range() {
        assert!(a <= b && b < (w * h) as u32);
    }
}

// A deliberately narrow oracle: default-colour ASCII, explicit CUP before
// each operation, full-screen margins, and no wrapping/reflow in this model.
// Vec insertion/removal is independent of Grid's circular rows and cell copies.
fn model_step(model: &mut [Vec<u32>], a: &Action) {
    let Action::Edit(op, r, c, n) = a else {
        panic!("non-model action");
    };
    let h = model.len();
    let w = model[0].len();
    let (r, c) = (*r % h, *c % w);
    let count = n + 1;
    match op {
        0 => model[r][c] = u32::from(b'a') + (*n % 26) as u32,
        1 | 2 => {
            let end = if *op == 2 { w } else { (c + count).min(w) };
            for cell in &mut model[r][c..end] {
                *cell = 0;
            }
        }
        3 => {
            for _ in 0..count.min(w - c) {
                model[r].insert(c, 0);
                model[r].pop();
            }
        }
        4 => {
            for _ in 0..count.min(w - c) {
                model[r].remove(c);
                model[r].push(0);
            }
        }
        5..=8 => {
            let start = if *op < 7 { 0 } else { r };
            let mut rows = model.to_vec();
            for _ in 0..count.min(h - start) {
                if *op == 5 || *op == 8 {
                    rows.remove(start);
                    rows.push(vec![0; w]);
                } else {
                    rows.insert(start, vec![0; w]);
                    rows.pop();
                }
            }
            model.clone_from_slice(&rows);
        }
        _ => unreachable!(),
    }
}

fn check(property: &str, s: &Settings, steps: &[Action]) -> Result<(), String> {
    let mut current_step = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut whole = Terminal::new(s.width, s.height).unwrap();
        whole.set_scrollback_limit(s.history);
        let mut bytes = Terminal::new(s.width, s.height).unwrap();
        bytes.set_scrollback_limit(s.history);
        let mut variable = Terminal::new(s.width, s.height).unwrap();
        variable.set_scrollback_limit(s.history);
        let mut model = vec![vec![0; s.width]; s.height];
        let mut reference = Terminal::new(s.width, s.height).unwrap();
        reference.set_scrollback_limit(s.history);
        if property == "combining" {
            assert_eq!(steps.len(), 3);
            apply(&mut reference, &steps[0], 0);
            apply(&mut reference, &steps[2], 0);
            apply(&mut reference, &steps[1], 0);
        }
        for (i, a) in steps.iter().enumerate() {
            current_step = i;
            let replies = apply(&mut whole, a, 0);
            invariants(&whole);
            if property == "model" {
                model_step(&mut model, a);
                for (r, row) in model.iter().enumerate() {
                    for (c, expected) in row.iter().enumerate() {
                        assert_eq!(
                            whole.active_grid().cell(r, c).unwrap().content,
                            *expected,
                            "model row={r} col={c}"
                        );
                    }
                }
            } else {
                assert_eq!(replies, apply(&mut bytes, a, 1), "byte replies");
                assert_eq!(
                    replies,
                    apply(&mut variable, a, 1 + i % 7),
                    "variable replies"
                );
                assert!(whole == bytes, "byte fragmentation");
                assert!(whole == variable, "variable fragmentation");
            }
        }
        if property == "combining" {
            assert!(
                whole.active_grid() == reference.active_grid(),
                "combining mark detached during resize"
            );
            assert_eq!(whole.cursor(), reference.cursor());
        }
        if property != "model" {
            for suffix in [
                b"\x18\x1b[0m\x1b[6n".as_slice(),
                "界e\u{301}\x1b[?1049h\x1b[?1049l".as_bytes(),
            ] {
                let r = whole.process(suffix);
                assert_eq!(r, bytes.process(suffix));
                assert_eq!(r, variable.process(suffix));
                assert!(whole == bytes, "continuation");
                assert!(whole == variable);
            }
            assert_eq!(
                whole.take_clipboard_requests(),
                bytes.take_clipboard_requests()
            );
            assert_eq!(whole.take_shell_markers(), bytes.take_shell_markers());
        }
    }));
    result.map_err(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "panic".into());
        format!(
            "property={property} first_step={current_step} action={:?}: {message}",
            steps.get(current_step)
        )
    })
}

fn encode(property: &str, s: &Settings, actions: &[Action]) -> String {
    let mut text = format!(
        "AURA-CASE 1\nproperty {property}\nsize {} {} {}\n",
        s.width, s.height, s.history
    );
    for a in actions {
        text.push_str(&match a {
            Action::Bytes(b) => format!(
                "bytes {}",
                if b.is_empty() {
                    "-".into()
                } else {
                    b.iter().map(|v| format!("{v:02x}")).collect::<String>()
                }
            ),
            Action::Resize(w, h) => format!("resize {w} {h}"),
            Action::Limit(n) => format!("limit {n}"),
            Action::View(n) => format!("view {n}"),
            Action::Clear => "clear".into(),
            Action::Select(a, b, c, d) => format!("select {a} {b} {c} {d}"),
            Action::Edit(op, r, c, n) => format!("edit {op} {r} {c} {n}"),
        });
        text.push('\n');
    }
    text
}

fn decode(text: &str) -> (String, Settings, Vec<Action>) {
    assert!(text.len() <= 512 * 1024, "case exceeds artifact budget");
    let mut lines = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    assert_eq!(lines.next(), Some("AURA-CASE 1"));
    let property = lines
        .next()
        .unwrap()
        .strip_prefix("property ")
        .unwrap()
        .to_owned();
    assert!(matches!(property.as_str(), "state" | "model" | "combining"));
    let mut s = Settings::read();
    let size: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    assert_eq!(size.len(), 4);
    assert_eq!(size[0], "size");
    s.width = size[1].parse().unwrap();
    s.height = size[2].parse().unwrap();
    s.history = size[3].parse().unwrap();
    assert!((1..=128).contains(&s.width) && (1..=64).contains(&s.height) && s.history <= 4096);
    let mut actions = Vec::new();
    for line in lines {
        let v: Vec<_> = line.split_whitespace().collect();
        let n = |i: usize| v[i].parse::<usize>().unwrap();
        let a = match v[0] {
            "bytes" => {
                assert_eq!(v.len(), 2);
                let hex = v[1];
                assert!(hex == "-" || (hex.len() % 2 == 0 && hex.len() <= 16384));
                Action::Bytes(if hex == "-" {
                    Vec::new()
                } else {
                    hex.as_bytes()
                        .chunks(2)
                        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
                        .collect()
                })
            }
            "resize" => {
                assert_eq!(v.len(), 3);
                assert!(n(1) <= 128 && n(2) <= 64);
                Action::Resize(n(1), n(2))
            }
            "limit" => {
                assert_eq!(v.len(), 2);
                assert!(n(1) <= 4096);
                Action::Limit(n(1))
            }
            "view" => {
                assert_eq!(v.len(), 2);
                Action::View(v[1].parse().unwrap())
            }
            "select" => {
                assert_eq!(v.len(), 5);
                Action::Select(n(1), n(2), n(3), n(4))
            }
            "clear" => {
                assert_eq!(v.len(), 1);
                Action::Clear
            }
            "edit" => {
                assert_eq!(v.len(), 5);
                assert!(n(1) < 9 && n(4) < 128);
                Action::Edit(n(1) as u8, n(2), n(3), n(4))
            }
            _ => panic!("unknown case action: {line}"),
        };
        actions.push(a);
        assert!(actions.len() <= 512);
    }
    (property, s, actions)
}

fn run(property: &str, strategy: BoxedStrategy<Vec<Action>>, s: &Settings) {
    let start = Instant::now();
    eprintln!(
        "campaign property={property} profile={} settings={s:?} unicode-width={:?} segmentation={:?}",
        s.profile,
        unicode_width::UNICODE_VERSION,
        unicode_segmentation::UNICODE_VERSION
    );
    let result = s.runner().run(&strategy, |steps| {
        check(property, s, &steps).map_err(TestCaseError::fail)
    });
    if let Err(error) = result {
        if let TestError::Fail(_, case) = &error {
            let root = PathBuf::from(
                env::var("AURA_ARTIFACTS").unwrap_or_else(|_| "target/campaign".into()),
            );
            fs::create_dir_all(&root).unwrap();
            let path = root.join(format!("{property}-{}-minimal.case", s.seed));
            let text = encode(property, s, case);
            fs::write(&path, &text).unwrap();
            let (p, settings, actions) = decode(&text);
            assert!(
                check(&p, &settings, &actions).is_err(),
                "saved replay did not reproduce"
            );
            eprintln!(
                "MINIMAL_CASE={}\n{}",
                path.display(),
                check(property, s, case).unwrap_err()
            );
        }
        panic!("{error}; settings={s:?}");
    }
    eprintln!(
        "COMPLETE property={property} cases={} elapsed={:?}",
        s.cases,
        start.elapsed()
    );
}

#[test]
fn generated_state_and_fragmentation() {
    let s = Settings::read();
    run("state", actions(&s), &s);
}

#[test]
fn generated_combining_resize() {
    let s = Settings::read();
    let height = s.height;
    let strategy = (1..=s.width * 2, 1..=s.width)
        .prop_map(move |(n, width)| {
            vec![
                Action::Bytes(vec![b'a'; n]),
                Action::Resize(width, height),
                Action::Bytes("\u{301}".as_bytes().to_vec()),
            ]
        })
        .boxed();
    run("combining", strategy, &s);
}

#[test]
fn generated_independent_ascii_model() {
    let s = Settings::read();
    let strategy = prop::collection::vec(
        (0u8..9, 0..s.height, 0..s.width, 0usize..16)
            .prop_map(|(a, b, c, d)| Action::Edit(a, b, c, d)),
        1..=s.actions,
    )
    .boxed();
    run("model", strategy, &s);
}

fn read_case(path: &std::path::Path) -> String {
    // Limit the read itself; checking decoded length alone would allocate an
    // arbitrarily large user-supplied file before enforcing the case budget.
    let mut text = String::new();
    fs::File::open(path)
        .unwrap()
        .take(512 * 1024 + 1)
        .read_to_string(&mut text)
        .unwrap();
    assert!(text.len() <= 512 * 1024, "case exceeds artifact budget");
    text
}

#[test]
fn replay_cases() {
    if let Ok(path) = env::var("AURA_REPLAY") {
        let text = read_case(std::path::Path::new(&path));
        let (p, s, a) = decode(&text);
        check(&p, &s, &a).unwrap_or_else(|e| panic!("replay={path}: {e}"));
    } else {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/campaign/cases");
        let mut paths: Vec<_> = fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "case"))
            .collect();
        paths.sort();
        for path in paths {
            let (p, s, a) = decode(&read_case(&path));
            check(&p, &s, &a).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
    }
}

#[test]
fn all_short_partitions_and_directed_limits() {
    let mut samples = corpus();
    samples.extend([
        vec![b'\x1b', b']', b'0', b';']
            .into_iter()
            .chain(vec![b'x'; 4097])
            .chain([7])
            .collect(),
        format!("e{}", "\u{301}".repeat(140)).into_bytes(),
    ]);
    for input in samples {
        let mut expected = Terminal::new(8, 3).unwrap();
        let replies = expected.process(&input);
        if input.len() <= 8 {
            for mask in 0..1usize << input.len().saturating_sub(1) {
                let mut t = Terminal::new(8, 3).unwrap();
                let mut actual = Vec::new();
                let mut start = 0;
                for end in 1..=input.len() {
                    if end == input.len() || mask & (1 << (end - 1)) != 0 {
                        actual.extend(t.process(&input[start..end]));
                        t.process(&[]);
                        start = end;
                    }
                }
                assert_eq!(t, expected, "partition={mask} bytes={input:?}");
                assert_eq!(actual, replies);
            }
        } else {
            for split in [1, input.len() / 2, input.len() - 1] {
                let mut t = Terminal::new(8, 3).unwrap();
                let mut r = t.process(&input[..split]);
                r.extend(t.process(&input[split..]));
                assert_eq!(t, expected);
                assert_eq!(r, replies);
            }
        }
        invariants(&expected);
    }
}

#[test]
fn enumerate_braille_pairs_and_optional_triples() {
    let triples = env::var("AURA_BRAILLE_TRIPLES").as_deref() == Ok("1");
    let count = if triples {
        256u32.pow(3)
    } else {
        256u32.pow(2)
    };
    let start = Instant::now();
    let mut t = Terminal::new(3, 1).unwrap();
    let mut utf8 = [0; 4];
    for index in 0..count {
        t.process(b"\r");
        let digits = if triples { 3 } else { 2 };
        for column in 0..digits {
            let code = 0x2800 + ((index >> (column * 8)) & 255);
            t.process(
                char::from_u32(code)
                    .unwrap()
                    .encode_utf8(&mut utf8)
                    .as_bytes(),
            );
            assert_eq!(
                t.active_grid().cell(0, column as usize).unwrap().content,
                code,
                "index={index} column={column}"
            );
        }
        assert_eq!(t.cursor().row, 0);
    }
    eprintln!(
        "COMPLETE braille {} count={count} elapsed={:?}",
        if triples { "triples" } else { "pairs" },
        start.elapsed()
    );
}
