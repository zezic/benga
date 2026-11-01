//! Throwaway full-resolution GPU ribbon experiment.
//! cargo run --release --features gpu-ribbon-prototype --example gpu_ribbon_prototype
fn main() -> anyhow::Result<()> {
    benga::visualizer::gpu_ribbon_prototype::run()
}
