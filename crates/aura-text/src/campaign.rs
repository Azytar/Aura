//! Controlled-font shaping/cache properties, including unsupported-glyph paths.
use super::*;
use proptest::{
    prelude::*,
    test_runner::{Config, FileFailurePersistence, RngAlgorithm, TestError, TestRng, TestRunner},
};
use std::{cell::RefCell, env, io::Read, path::PathBuf, time::Instant};
type Case = (String, bool, bool, u8);
fn check(font: &mut Font, case: &Case) -> Result<(), TestCaseError> {
    let (text, bold, italic, size) = case;
    font.set_size([12.0, 17.0, 23.0][usize::from(*size)])
        .map_err(TestCaseError::fail)?;
    let first = match font.glyph_text_with_context(text, "ل", "ا", *bold, *italic) {
        Err(error) if error == CACHE_FULL => {
            font.invalidate_atlas();
            font.glyph_text_with_context(text, "ل", "ا", *bold, *italic)
        }
        result => result,
    }
    .map_err(TestCaseError::fail)?;
    let second = font
        .glyph_text_with_context(text, "ل", "ا", *bold, *italic)
        .map_err(TestCaseError::fail)?;
    prop_assert_eq!(first.0, second.0);
    prop_assert_eq!(first.1, second.1);
    prop_assert!(second.2.is_none());
    let info = first.1;
    prop_assert!(
        info.uv_min
            .iter()
            .chain(&info.uv_max)
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
    );
    prop_assert!(
        info.size
            .iter()
            .all(|v| v.is_finite() && (0.0..=512.0).contains(v))
    );
    prop_assert!(info.bearing.iter().all(|v| v.is_finite()) && info.advance.is_finite());
    prop_assert!(font.glyphs.len() <= MAX_GLYPHS && font.fallback_count() <= 8);
    if let Some(upload) = first.2 {
        prop_assert!(upload.pixels.len() <= 1024 * 1024);
        prop_assert_eq!(
            upload.pixels.len(),
            (upload.width * upload.height) as usize * if upload.color { 4 } else { 1 }
        );
    }
    Ok(())
}
#[test]
fn generated_shaping_and_cache_contract() {
    let profile = env::var("AURA_PROFILE").unwrap_or_else(|_| "quick".into());
    let defaults = match profile.as_str() {
        "quick" => (64, 1024),
        "extended" => (2048, 4096),
        "exhaustive" => (4096, 8192),
        _ => panic!("unknown profile"),
    };
    let cases = env::var("AURA_CASES").map_or(defaults.0, |s| s.parse::<u32>().unwrap());
    let shrink = env::var("AURA_SHRINK").map_or(defaults.1, |s| s.parse::<u32>().unwrap());
    assert!((1..=65536).contains(&cases) && shrink <= 65536);
    let seed = env::var("AURA_SEED").map_or(0x4155_5241u64, |s| s.parse().unwrap());
    let algorithm = match env::var("AURA_RNG").as_deref().unwrap_or("chacha") {
        "chacha" => RngAlgorithm::ChaCha,
        "xorshift" => RngAlgorithm::XorShift,
        _ => panic!("unknown RNG"),
    };
    let seed_bytes: Vec<u8> = (0..4u64)
        .flat_map(|i| seed.wrapping_add(i).to_le_bytes())
        .collect();
    let font = match Font::load(
        Some(Path::new(
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        )),
        17.0,
    ) {
        Ok(font) => RefCell::new(font),
        Err(error) => {
            assert!(
                env::var("AURA_REQUIRE_GPU").as_deref() != Ok("1"),
                "required controlled DejaVu font: {error}"
            );
            eprintln!("SKIP controlled DejaVu shaping font: {error}");
            return;
        }
    };
    if let Ok(path) = env::var("AURA_TEXT_REPLAY") {
        let mut text = String::new();
        fs::File::open(path)
            .unwrap()
            .take(1025)
            .read_to_string(&mut text)
            .unwrap();
        assert!(text.len() <= 1024);
        let v: Vec<_> = text.split_whitespace().collect();
        assert_eq!(v.len(), 5);
        assert_eq!(v[0], "AURA-TEXT-1");
        assert!(v[4].len() <= 512 && v[4].len() % 2 == 0);
        assert!(matches!(v[2], "0" | "1") && matches!(v[3], "0" | "1"));
        assert!(v[1].parse::<u8>().unwrap() < 3);
        let bytes: Vec<u8> = v[4]
            .as_bytes()
            .chunks(2)
            .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
            .collect();
        let case = (
            String::from_utf8(bytes).unwrap(),
            v[2] == "1",
            v[3] == "1",
            v[1].parse().unwrap(),
        );
        check(&mut font.borrow_mut(), &case).unwrap();
        return;
    }
    let texts = vec![
        " ",
        "\u{a0}",
        "\u{2009}",
        "\u{3000}",
        "\u{301}",
        "e\u{301}\u{308}",
        "界",
        "👩\u{200d}💻",
        "👍🏽",
        "🇪🇸",
        "❤\u{fe0f}",
        "각",
        "ب",
        "क्षि",
        "\u{200d}",
        "\u{fe0f}",
    ];
    let strategy = (
        prop::sample::select(texts).prop_map(str::to_owned),
        any::<bool>(),
        any::<bool>(),
        0u8..3,
    );
    let config = Config {
        cases,
        max_shrink_iters: shrink,
        max_shrink_time: 0,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..Config::default()
    };
    let mut runner = TestRunner::new_with_rng(
        config,
        TestRng::from_seed(
            algorithm,
            &seed_bytes[..if algorithm == RngAlgorithm::ChaCha {
                32
            } else {
                16
            }],
        ),
    );
    let start = Instant::now();
    let result = runner.run(&strategy, |case| check(&mut font.borrow_mut(), &case));
    if let Err(error) = result {
        if let TestError::Fail(_, case) = &error {
            let root = PathBuf::from(
                env::var("AURA_ARTIFACTS").unwrap_or_else(|_| "target/campaign".into()),
            );
            fs::create_dir_all(&root).unwrap();
            fs::write(
                root.join("text-minimal.case"),
                format!(
                    "AURA-TEXT-1 {} {} {} {}\n",
                    case.3,
                    u8::from(case.1),
                    u8::from(case.2),
                    case.0
                        .as_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                ),
            )
            .unwrap();
        }
        panic!("{error}; seed={seed} algorithm={algorithm:?} cases={cases} shrink={shrink}");
    }
    assert!(font.borrow_mut().glyph_text("", false, false).is_err());
    assert!(
        font.borrow_mut()
            .glyph_text(&"a".repeat(257), false, false)
            .is_err()
    );
    assert!(
        font.borrow_mut()
            .glyph_text_with_context("a", &"a".repeat(129), "", false, false)
            .is_err()
    );
    eprintln!(
        "COMPLETE text profile={profile} cases={cases} seed={seed} algorithm={algorithm:?} shrink={shrink} elapsed={:?}",
        start.elapsed()
    );
}
