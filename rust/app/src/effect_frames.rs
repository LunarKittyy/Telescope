//! `telescope-rs effect PLUGIN W H [--config JSON | --config-lines FILE]`: a dev tool for the site's clip tiles. Reads raw RGBA frames
//! of W x H on stdin, pushes each through the real plugin's picture change and writes the changed frames to stdout. A lines file has one
//! JSON settings object per frame (the last one holds on). Works for plugins with a frame step (chroma_key, desk) and for colour
//! grading, whose change is a colour stage.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::Arc;
use telescope_core::frame::{Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::media::{Media, StagedStep};
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;

struct NoContext;

impl FrameContext for NoContext {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn make(name: &str) -> Result<Box<dyn Plugin>, String> {
    Ok(match name {
        "chroma_key" => Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::with_reader(telescope_effects::shared::picture_reader(Arc::new(telescope_media::FfmpegMedia)))),
        "desk" => Box::new(telescope_effects::desk::DeskPlugin::new()),
        "colour_grading" => Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
        other => return Err(format!("no effect called {other} (chroma_key, desk, colour_grading)")),
    })
}

pub fn run(words: &[String]) -> Result<(), String> {
    let value = |flag: &str| words.iter().position(|w| w == flag).and_then(|i| words.get(i + 1)).cloned();
    let (name, w, h) = match words {
        [n, w, h, ..] => (n.clone(), w.parse::<u32>().map_err(|_| "bad width")?, h.parse::<u32>().map_err(|_| "bad height")?),
        _ => return Err("usage: telescope-rs effect PLUGIN W H [--config JSON | --config-lines FILE] < frames.rgba > out.rgba".into()),
    };
    let parse = |t: &str| serde_json::from_str::<Value>(t).map_err(|e| format!("settings aren't JSON: {e}"));
    let configs: Vec<Value> = match (value("--config-lines"), value("--config")) {
        (Some(f), _) => std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?.lines().filter(|l| !l.trim().is_empty()).map(parse).collect::<Result<_, _>>()?,
        (None, Some(j)) => vec![parse(&j)?],
        _ => vec![json!({})],
    };
    if configs.is_empty() {
        return Err("no settings".into());
    }
    let mut plugin = make(&name)?;
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    let mut pipeline = telescope_media::FfmpegMedia.pipeline()?;
    let size = (w * h * 4) as usize;
    let (mut stdin, mut stdout) = (std::io::stdin().lock(), std::io::BufWriter::new(std::io::stdout().lock()));
    let mut buf = vec![0u8; size];
    let mut last: Option<usize> = None;
    let mut grade: Option<Arc<dyn Fn(&mut [f32]) + Send + Sync>> = None;
    for n in 0.. {
        if stdin.read_exact(&mut buf).is_err() {
            break;
        }
        let which = n.min(configs.len() - 1);
        if last != Some(which) {
            last = Some(which);
            let mut cfg = configs[which].clone();
            if name != "colour_grading" {
                cfg["on"] = json!(true);
            }
            plugin.set_config(&cfg).map_err(|_| "the plugin turned the settings down".to_string())?;
            if name == "colour_grading" {
                // the plugin tells the pipeline about a grade it loaded quietly on the next event it hears
                plugin.on_event(&mut host, &telescope_core::event::Event::PhonesChanged { count: 1 });
                grade = host.colour_stages.iter().rev().find_map(|(_, s)| s.as_ref().and_then(|s| s.eval.0.clone()));
            } else {
                let step: FrameStep = plugin.frame_step().ok_or("the plugin has no picture change for these settings")?;
                pipeline.set_steps(vec![StagedStep { owner: name.clone(), stage: FrameStage::Background, step }]);
            }
        }
        if name == "colour_grading" {
            if let Some(g) = &grade {
                for px in buf.chunks_exact_mut(4) {
                    let mut rgb = [f32::from(px[0]) / 255.0, f32::from(px[1]) / 255.0, f32::from(px[2]) / 255.0];
                    g(&mut rgb);
                    for c in 0..3 {
                        px[c] = (rgb[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                }
            }
            stdout.write_all(&buf).map_err(|e| e.to_string())?;
        } else {
            let out = pipeline.process(Frame::new(w, h, PixelFormat::Rgba, buf.clone()), &NoContext);
            stdout.write_all(&out.data).map_err(|e| e.to_string())?;
        }
    }
    stdout.flush().map_err(|e| e.to_string())
}
