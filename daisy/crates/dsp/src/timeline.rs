//! Parser for the visualizer's `<song>.timeline.json` sidecar files.
//!
//! Runs unchanged on the Mac host (where it's called against bytes read
//! from disk) and on Daisy firmware (where the same bytes will come from
//! SD card or QSPI flash). Uses `serde-json-core` + `heapless::Vec` so
//! no allocator is required at this layer — though the rest of `dsp`
//! does use `alloc`, so this could be widened to `alloc::Vec` later if
//! we hit the keypoint cap.

use heapless::Vec;
use serde::Deserialize;

/// Cap on the number of keypoints any single lane can contain. Bumping
/// this costs RAM proportional to N (each `Keypoint` is 8 bytes), so
/// keep it generous but bounded.
pub const MAX_KEYPOINTS: usize = 256;

/// One automation keypoint. The JSON also carries a `curve` field which
/// we ignore here — all current consumers do linear interp via [`bpm_at`].
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct Keypoint {
    pub t: f32,
    pub v: f32,
}

#[derive(Deserialize)]
struct Lanes {
    #[serde(default)]
    bpm: Vec<Keypoint, MAX_KEYPOINTS>,
}

#[derive(Deserialize)]
struct TimelineRaw {
    lanes: Lanes,
}

/// Parse the BPM lane out of a timeline JSON blob. Returns an empty Vec
/// if the lane is absent. `None` is only returned on outright JSON parse
/// errors (malformed input, exceeded keypoint cap).
pub fn parse_bpm(bytes: &[u8]) -> Option<Vec<Keypoint, MAX_KEYPOINTS>> {
    let (raw, _consumed): (TimelineRaw, _) = serde_json_core::from_slice(bytes).ok()?;
    let mut kps = raw.lanes.bpm;
    // JSON authoring order isn't guaranteed — sort by time so `bpm_at` can
    // assume monotonic input.
    kps.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(core::cmp::Ordering::Equal));
    Some(kps)
}

/// Linear interpolation between BPM keypoints. Clamps at the ends.
/// Caller is responsible for the wrap (e.g. `t = elapsed % loop_seconds`).
pub fn bpm_at(keypoints: &[Keypoint], t: f32) -> f32 {
    if keypoints.is_empty() {
        return 0.0;
    }
    if t <= keypoints[0].t {
        return keypoints[0].v;
    }
    for w in keypoints.windows(2) {
        let (k0, k1) = (w[0], w[1]);
        if t <= k1.t {
            let span = k1.t - k0.t;
            let alpha = if span > 0.0 { (t - k0.t) / span } else { 0.0 };
            return k0.v + alpha * (k1.v - k0.v);
        }
    }
    keypoints.last().unwrap().v
}
