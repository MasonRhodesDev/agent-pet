//! Render the mascot + speech bubble offline for placement verification:
//!
//!   cargo run -p pet-render --example preview_bubble -- assets/default-pet out.png [elapsed_ms]

use pet_proto::AgentState;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let pet_dir = args.next().unwrap_or_else(|| "assets/default-pet".into());
    let out = args.next().unwrap_or_else(|| "preview.png".into());
    let elapsed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60_000);

    let (rgba, w, h) = pet_render::preview::render_scene(
        std::path::Path::new(&pet_dir),
        AgentState::Waiting,
        Some("Approve the deploy to staging? This needs your OK."),
        elapsed,
    )?;
    image::RgbaImage::from_raw(w, h, rgba)
        .expect("buffer size")
        .save(&out)?;
    println!("wrote {out} ({w}x{h})");
    Ok(())
}
