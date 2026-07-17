//! AgentState -> animation track, degrading gracefully when a pet lacks a
//! semantic track.

use pet_proto::AgentState;

use super::pet_json::PetDef;

pub fn track_for(state: AgentState, pet: &PetDef) -> &'static str {
    let track = state.track();
    if pet.animations.contains_key(track) {
        track
    } else {
        "idle"
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::pet_json::{Animation, Frame};
    use super::*;

    fn pet_with(tracks: &[&str]) -> PetDef {
        PetDef {
            id: "t".into(),
            spritesheet_path: PathBuf::new(),
            frame_width: 1,
            frame_height: 1,
            columns: 8,
            rows: 9,
            animations: tracks
                .iter()
                .map(|name| {
                    (
                        name.to_string(),
                        Animation {
                            frames: vec![Frame {
                                sprite_index: 0,
                                duration_ms: 100,
                            }],
                            loop_start: Some(0),
                            fallback: "idle".into(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn maps_states_to_their_tracks() {
        let pet = pet_with(&["idle", "running", "waiting", "review", "failed"]);
        assert_eq!(track_for(AgentState::Running, &pet), "running");
        assert_eq!(track_for(AgentState::Waiting, &pet), "waiting");
        assert_eq!(track_for(AgentState::Ready, &pet), "review");
        assert_eq!(track_for(AgentState::Failed, &pet), "failed");
        assert_eq!(track_for(AgentState::Idle, &pet), "idle");
        assert_eq!(track_for(AgentState::Gone, &pet), "idle");
    }

    #[test]
    fn missing_track_degrades_to_idle() {
        let pet = pet_with(&["idle", "running"]);
        assert_eq!(track_for(AgentState::Waiting, &pet), "idle");
        assert_eq!(track_for(AgentState::Running, &pet), "running");
    }
}
