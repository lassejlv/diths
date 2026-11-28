//! Native side of the dith-wasm parity and speed checks. It runs the same `dith_wasm::engine`
//! calls the `.wasm` runs, natively, so `scripts/parity.mjs` and `scripts/bench.mjs` can compare.
//!
//! ```text
//! native fixture <image> <out.rgba>          decode an image to raw RGBA, print {"width","height"}
//! native render <cases.json> <in.rgba> <w> <h> <out-dir>
//!                                            write <id>.rgba and <id>.<format> (or the error code
//!                                            in <id>.<format>.error) for every case and every
//!                                            format this build encodes
//! native bench <cases.json> <in.rgba> <w> <h> <runs>
//!                                            print the median milliseconds of every case as JSON
//! ```
//!
//! A case is `{"id", "recipe"}` (dither the input) or `{"id", "generate", "recipe"?}` (draw a
//! generator, dithering it when a recipe is given), with recipes and options as JSON values.

use std::path::Path;
use std::time::Instant;

use dith_core::image::RgbaImage;
use dith_core::output::ExportOptions;
use dith_wasm::engine;
use serde_json::{Value, json};

fn spec(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), ToString::to_string)
}

fn run(case: &Value, input: &RgbaImage) -> RgbaImage {
    let id = &case["id"];
    let recipe = case
        .get("recipe")
        .filter(|r| !r.is_null())
        .map(|r| engine::recipe(&spec(r)).unwrap_or_else(|e| panic!("{id}: {}", e.message)));
    let source = match case.get("generate") {
        Some(options) => engine::generate(
            &engine::generate_options(&spec(options))
                .unwrap_or_else(|e| panic!("{id}: {}", e.message)),
        ),
        None => input.clone(),
    };
    match recipe {
        Some(recipe) => {
            engine::dither(source, &recipe)
                .unwrap_or_else(|e| panic!("{id}: {}", e.message))
                .image
        }
        None => source,
    }
}

fn load(args: &[String]) -> (Vec<Value>, RgbaImage) {
    let cases: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(&args[0]).expect("reading cases"))
            .expect("parsing cases");
    let raw = std::fs::read(&args[1]).expect("reading input RGBA");
    let input = RgbaImage::from_raw(
        args[2].parse().expect("width"),
        args[3].parse().expect("height"),
        raw,
    )
    .expect("input RGBA matches width x height");
    (cases, input)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fixture") => {
            let img = dith_core::image::open(&args[1])
                .expect("decoding fixture")
                .into_rgba8();
            std::fs::write(&args[2], img.as_raw()).expect("writing RGBA");
            println!(
                "{}",
                json!({ "width": img.width(), "height": img.height() })
            );
        }
        Some("render") => {
            let (cases, input) = load(&args[1..5]);
            let dir = Path::new(&args[5]);
            std::fs::create_dir_all(dir).expect("creating output dir");
            for case in &cases {
                let id = case["id"].as_str().expect("case id");
                let img = run(case, &input);
                std::fs::write(dir.join(format!("{id}.rgba")), img.as_raw()).expect("writing");
                for format in engine::FORMATS {
                    let (name, bytes) =
                        match engine::encode(&img, format, &ExportOptions::default()) {
                            Ok(bytes) => (format!("{id}.{format}"), bytes),
                            Err(e) => (format!("{id}.{format}.error"), e.code.as_bytes().to_vec()),
                        };
                    std::fs::write(dir.join(name), bytes).expect("writing");
                }
                std::fs::write(
                    dir.join(format!("{id}.json")),
                    json!({ "width": img.width(), "height": img.height() }).to_string(),
                )
                .expect("writing");
            }
        }
        Some("bench") => {
            let (cases, input) = load(&args[1..5]);
            let runs: usize = args[5].parse().expect("runs");
            let mut report = serde_json::Map::new();
            for case in &cases {
                let id = case["id"].as_str().expect("case id");
                std::hint::black_box(run(case, &input));
                let mut times: Vec<f64> = (0..runs)
                    .map(|_| {
                        let started = Instant::now();
                        std::hint::black_box(run(case, &input));
                        started.elapsed().as_secs_f64() * 1e3
                    })
                    .collect();
                times.sort_by(f64::total_cmp);
                report.insert(id.to_string(), json!(times[times.len() / 2]));
            }
            println!("{}", Value::Object(report));
        }
        _ => {
            eprintln!("usage: native fixture|render|bench ... (see the module docs)");
            std::process::exit(2);
        }
    }
}
