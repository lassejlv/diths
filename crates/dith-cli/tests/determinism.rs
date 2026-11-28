//! Cross-platform determinism: a fixed set of renders must hash to the checked-in manifest.
//!
//! CI runs this on x86-64 Linux, arm64 Linux and macOS, so a pass on all three means the same
//! inputs and flags give byte-identical files everywhere. It covers every preset, every algorithm
//! in every color space, the tone and resize flags, every generator, animation, inflate and
//! compare, from two small PNG fixtures (SVG input is left out because resvg rasterizes with the
//! platform's libm).
//!
//! After an intentional output change, regenerate the manifest with:
//!
//! ```text
//! DITH_UPDATE_DETERMINISM=1 cargo test --test determinism
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const UPDATE_ENV: &str = "DITH_UPDATE_DETERMINISM";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/determinism")
}

fn cases() -> Vec<(String, Vec<String>)> {
    let dir = fixtures();
    let photo = dir.join("photo.png").display().to_string();
    let alpha = dir.join("alpha.png").display().to_string();
    let presets: Vec<String> = serde_json::from_slice::<serde_json::Value>(
        &Command::new(env!("CARGO_BIN_EXE_dith"))
            .args(["list", "presets", "--json"])
            .output()
            .expect("dith list presets")
            .stdout,
    )
    .expect("preset list is JSON")
    .as_array()
    .expect("preset list is an array")
    .iter()
    .map(|p| p["id"].as_str().expect("preset id").to_string())
    .collect();

    let mut cases: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |name: String, argv: &[&str]| {
        cases.push((name, argv.iter().map(|a| (*a).to_string()).collect()));
    };

    for preset in &presets {
        add(
            format!("preset/{preset}"),
            &["apply", &photo, "-P", preset, "-o", "{O}/out.png"],
        );
        add(
            format!("preset/{preset}/alpha"),
            &["apply", &alpha, "-P", preset, "-o", "{O}/out.png"],
        );
        add(
            format!("preset/{preset}/sphere"),
            &[
                "generate",
                "sphere",
                "--canvas-width",
                "160",
                "--canvas-height",
                "120",
                "-P",
                preset,
                "-o",
                "{O}/out.png",
            ],
        );
    }
    for format in ["webp", "gif", "bmp", "tiff", "jpg", "svg", "txt"] {
        add(
            format!("format/{format}"),
            &[
                "apply",
                &photo,
                "-P",
                "risograph",
                "-o",
                &format!("{{O}}/out.{format}"),
            ],
        );
    }
    for algorithm in [
        "floyd-steinberg",
        "atkinson",
        "jarvis-judice-ninke",
        "stucki",
        "burkes",
        "sierra",
        "sierra-two-row",
        "sierra-lite",
        "ostromoukhov",
        "bayer2",
        "bayer4",
        "bayer8",
        "bayer16",
        "halftone",
        "clustered-dot",
        "diagonal-lines",
        "horizontal-lines",
        "vertical-lines",
        "crosshatch",
        "blue-noise",
        "ign",
        "riemersma",
        "random",
        "threshold",
    ] {
        for space in ["oklab", "linear", "srgb"] {
            add(
                format!("algorithm/{algorithm}/{space}"),
                &[
                    "apply",
                    &photo,
                    "-a",
                    algorithm,
                    "-p",
                    "pico8",
                    "--color-space",
                    space,
                    "-o",
                    "{O}/out.png",
                ],
            );
        }
        add(
            format!("algorithm/{algorithm}/auto8"),
            &[
                "apply",
                &photo,
                "-a",
                algorithm,
                "--auto-palette",
                "8",
                "-o",
                "{O}/out.png",
            ],
        );
    }
    let flags: &[(&str, &[&str])] = &[
        ("gamma-0.6", &["--gamma", "0.6"]),
        ("gamma-1.8", &["--gamma", "1.8"]),
        ("saturation-1.6", &["--saturation", "1.6"]),
        ("saturation-0.3", &["--saturation", "0.3"]),
        ("grayscale-invert", &["--grayscale", "--invert"]),
        (
            "contrast-brightness",
            &["--contrast", "0.3", "--brightness=-0.1"],
        ),
        ("blur", &["--blur", "1.8"]),
        ("sharpen", &["--sharpen", "1.2"]),
        ("auto-levels", &["--auto-levels"]),
        ("downscale", &["--width", "151"]),
        ("upscale", &["--scale", "1.7"]),
        (
            "cover",
            &["--width", "120", "--height", "120", "--fit", "cover"],
        ),
        ("pixel-size", &["--pixel-size", "3"]),
        (
            "ink",
            &[
                "--ink-below",
                "0.2",
                "--paper-above",
                "0.85",
                "--ink-lines",
                "0.5",
            ],
        ),
        ("noise-seed", &["--noise", "0.3", "--seed", "7"]),
    ];
    for (label, extra) in flags {
        let mut argv = vec![
            "apply",
            photo.as_str(),
            "-a",
            "floyd-steinberg",
            "-p",
            "pico8",
        ];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["-o", "{O}/out.png"]);
        add(format!("flags/{label}"), &argv);
    }
    for mode in ["preserve", "flatten", "threshold", "dither"] {
        add(
            format!("alpha/{mode}"),
            &[
                "apply",
                &alpha,
                "-p",
                "gameboy",
                "--alpha-mode",
                mode,
                "-o",
                "{O}/out.png",
            ],
        );
    }
    for pattern in [
        "linear-gradient",
        "radial-gradient",
        "mesh",
        "sphere",
        "waves",
        "noise",
        "rings",
        "plasma",
        "checkerboard",
    ] {
        add(
            format!("generate/{pattern}"),
            &[
                "generate",
                pattern,
                "--canvas-width",
                "200",
                "--canvas-height",
                "140",
                "-o",
                "{O}/out.png",
            ],
        );
        add(
            format!("generate/{pattern}/fs-pico8"),
            &[
                "generate",
                pattern,
                "--canvas-width",
                "200",
                "--canvas-height",
                "140",
                "--angle",
                "30",
                "--frequency",
                "2.5",
                "-a",
                "floyd-steinberg",
                "-p",
                "pico8",
                "-o",
                "{O}/out.png",
            ],
        );
        add(
            format!("animate/{pattern}"),
            &[
                "animate",
                pattern,
                "--frames",
                "6",
                "--canvas-width",
                "120",
                "--canvas-height",
                "90",
                "--easing",
                "ease-in-out",
                "-o",
                "{O}/out.gif",
            ],
        );
    }
    add(
        "animate/sphere/apng-pulse".into(),
        &[
            "animate",
            "--pattern",
            "sphere",
            "--frames",
            "5",
            "--canvas-width",
            "120",
            "--canvas-height",
            "90",
            "--sweep-gamma",
            "0.6..1.4",
            "--sweep-mode",
            "pulse",
            "--anim-format",
            "apng",
            "-o",
            "{O}/out.png",
        ],
    );
    add(
        "animate/photo-boil".into(),
        &[
            "animate",
            &photo,
            "--frames",
            "4",
            "--temporal",
            "boil",
            "-P",
            "manga",
            "-o",
            "{O}/out.gif",
        ],
    );
    add(
        "inflate".into(),
        &[
            "generate",
            "inflate",
            "--shape",
            &alpha,
            "--canvas-width",
            "200",
            "--canvas-height",
            "138",
            "-o",
            "{O}/out.png",
        ],
    );
    add(
        "compare".into(),
        &[
            "compare",
            &photo,
            "--algorithms",
            "floyd-steinberg,bayer8,blue-noise",
            "--tile-max-dim",
            "120",
            "-o",
            "{O}/sheet.png",
        ],
    );
    cases
}

/// FNV-1a, 64 bit: enough to catch any changed byte in a render.
fn fnv1a(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

fn render(name: &str, argv: &[String], root: &Path) -> Vec<(String, String)> {
    let out = root.join(name.replace('/', "__"));
    std::fs::create_dir_all(&out).unwrap();
    let argv: Vec<String> = argv
        .iter()
        .map(|a| a.replace("{O}", &out.display().to_string()))
        .collect();
    let run = Command::new(env!("CARGO_BIN_EXE_dith"))
        .args(&argv)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let mut files: Vec<PathBuf> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{name}: no output");
    files
        .iter()
        .map(|path| {
            let file = path.file_name().unwrap().to_string_lossy();
            (
                format!("{name}/{file}"),
                fnv1a(&std::fs::read(path).unwrap()),
            )
        })
        .collect()
}

#[test]
fn renders_match_the_cross_platform_manifest() {
    let root = tempfile::TempDir::new().unwrap();
    let cases = cases();
    let threads = std::thread::available_parallelism()
        .map_or(2, usize::from)
        .min(8);
    let got: BTreeMap<String, String> = std::thread::scope(|scope| {
        let chunks: Vec<_> = cases
            .chunks(cases.len().div_ceil(threads))
            .map(|chunk| {
                let root = root.path();
                scope.spawn(move || {
                    chunk
                        .iter()
                        .flat_map(|(name, argv)| render(name, argv, root))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        chunks.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });

    let manifest_path = fixtures().join("manifest.json");
    if std::env::var_os(UPDATE_ENV).is_some() {
        let json = serde_json::to_string_pretty(&got).unwrap() + "\n";
        std::fs::write(&manifest_path, json).unwrap();
        return;
    }
    let expected: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let mut report = String::new();
    for (key, hash) in &got {
        match expected.get(key) {
            Some(want) if want == hash => {}
            Some(want) => writeln!(report, "changed  {key}: {want} -> {hash}").unwrap(),
            None => writeln!(report, "new      {key}: {hash}").unwrap(),
        }
    }
    for key in expected.keys().filter(|k| !got.contains_key(*k)) {
        writeln!(report, "missing  {key}").unwrap();
    }
    assert!(
        report.is_empty(),
        "{} of {} renders differ from tests/determinism/manifest.json (set {UPDATE_ENV}=1 to regenerate after an intended change):\n{report}",
        report.lines().count(),
        got.len()
    );
}
