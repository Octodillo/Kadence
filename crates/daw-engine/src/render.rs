use std::path::Path;

use daw_core::event::EventList;

use crate::compile::CompileError;
use crate::engine::{EngineConfig, EngineController};
use crate::graph::GraphDesc;

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub sample_rate: f64,
    /// Block size the graph is driven with.
    pub block_size: u32,
    /// Length of the render in frames.
    pub frames: usize,
    /// Starts the transport before rendering.
    pub play: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            block_size: 256,
            frames: 48_000,
            play: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error("wav: {0}")]
    Wav(#[from] hound::Error),
}

/// Renders `graph` offline and returns the master output interleaved.
pub fn render(graph: GraphDesc, opts: &RenderOptions) -> Result<Vec<f32>, RenderError> {
    let config = EngineConfig {
        sample_rate: opts.sample_rate,
        max_frames: opts.block_size,
        ..Default::default()
    };
    let (mut ctl, mut proc) = EngineController::new(config);
    let compiled = ctl.compile(graph)?;
    let channels = compiled.output_channels();
    let _ = ctl.send_graph(Box::new(compiled));
    if opts.play {
        // A fresh queue has room for the graph and this command.
        let _ = ctl.play();
    }

    let block = opts.block_size as usize;
    let mut bufs = vec![vec![0.0f32; block]; channels];
    let events = EventList::with_capacity(0);
    let mut out = Vec::with_capacity(opts.frames * channels);
    let mut done = 0;
    while done < opts.frames {
        let n = block.min(opts.frames - done);
        let mut outs: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..n]).collect();
        proc.process(n, &[], &mut outs, &events);
        for i in 0..n {
            out.extend(bufs.iter().map(|b| b[i]));
        }
        done += n;
    }
    drop(proc);
    ctl.collect_garbage();
    Ok(out)
}

/// Renders `graph` offline into a 32-bit float WAV file.
pub fn render_to_wav(
    graph: GraphDesc,
    opts: &RenderOptions,
    path: &Path,
) -> Result<(), RenderError> {
    let channels = graph_output_channels(&graph);
    let samples = render(graph, opts)?;
    let spec = hound::WavSpec {
        channels,
        sample_rate: opts.sample_rate.round() as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}

fn graph_output_channels(graph: &GraphDesc) -> u16 {
    graph.nodes[graph.master.0 as usize].node.ports().audio_out
}
