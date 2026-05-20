/// Helper to generate a unique spectrum visualizer seed for a given track ID.
/// This seed is used to vary the speeds, phases, and frequencies of the simulated visualizer bars
/// so that every song has a completely unique visual signature.
pub fn get_spectrum_seed(track_id: &str) -> f32 {
    let mut hash = 5381u32;
    for b in track_id.bytes() {
        hash = ((hash << 5).wrapping_add(hash)).wrapping_add(b as u32);
    }
    // Map hash to a value between 0.6 and 1.8 for distinct frequencies
    0.6 + (hash % 1200) as f32 / 1000.0
}
