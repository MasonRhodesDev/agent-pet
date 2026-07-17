//! Render the mascot + speech bubble offline for placement verification:
//!
//!   cargo run -p pet-render --example preview_bubble -- assets/default-pet out.png [elapsed_ms] [--no-bubble]
//!
//! `--no-bubble` renders the same waiting sprite with the bubble absent —
//! the dismissed-alert visual (mascot track unchanged, nag collapsed).

use pet_proto::AgentState;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_bubble = args.iter().any(|a| a == "--no-bubble");
    let mut positional = args.iter().filter(|a| !a.starts_with("--"));
    let pet_dir = positional.next().cloned().unwrap_or_else(|| "assets/default-pet".into());
    let out = positional.next().cloned().unwrap_or_else(|| "preview.png".into());
    let elapsed: u64 = positional.next().and_then(|s| s.parse().ok()).unwrap_or(60_000);

    let body = (!no_bubble).then_some("Approve the deploy to staging? This needs your OK.");
    let (rgba, w, h) = pet_render::preview::render_scene(
        std::path::Path::new(&pet_dir),
        AgentState::Waiting,
        body,
        elapsed,
    )?;
    image::RgbaImage::from_raw(w, h, rgba)
        .expect("buffer size")
        .save(&out)?;
    println!("wrote {out} ({w}x{h})");
    Ok(())
}
