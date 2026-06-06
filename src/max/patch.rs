//! Generation of starter Max patch (.maxpat) JSON.

use serde_json::{Value, json};

/// The kind of M4L device a patch is destined to become.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchKind {
    AudioEffect,
    MidiEffect,
    Instrument,
}

/// Build a minimal starter patcher for the given device kind, with the
/// correct I/O objects already placed and connected.
pub fn starter_patch(kind: PatchKind) -> Value {
    let (inlet, outlet) = match kind {
        PatchKind::AudioEffect => ("plugin~", "plugout~"),
        PatchKind::MidiEffect => ("midiin", "midiout"),
        PatchKind::Instrument => ("midiin", "plugout~"),
    };

    let boxes = json!([
        {
            "box": {
                "id": "obj-1",
                "maxclass": "newobj",
                "text": inlet,
                "numinlets": if inlet == "plugin~" { 1 } else { 0 },
                "numoutlets": 2,
                "patching_rect": [50.0, 50.0, 100.0, 22.0]
            }
        },
        {
            "box": {
                "id": "obj-2",
                "maxclass": "newobj",
                "text": outlet,
                "numinlets": 2,
                "numoutlets": if outlet == "plugout~" { 2 } else { 0 },
                "patching_rect": [50.0, 150.0, 100.0, 22.0]
            }
        }
    ]);

    // Wire left (and for signal pairs, right) connections.
    let lines = match kind {
        PatchKind::AudioEffect => json!([
            { "patchline": { "source": ["obj-1", 0], "destination": ["obj-2", 0] } },
            { "patchline": { "source": ["obj-1", 1], "destination": ["obj-2", 1] } }
        ]),
        PatchKind::MidiEffect => json!([
            { "patchline": { "source": ["obj-1", 0], "destination": ["obj-2", 0] } }
        ]),
        PatchKind::Instrument => json!([]),
    };

    json!({
        "patcher": {
            "fileversion": 1,
            "appversion": {
                "major": 8,
                "minor": 6,
                "revision": 0,
                "architecture": "x64",
                "modernui": 1
            },
            "classnamespace": "box",
            "rect": [100.0, 100.0, 640.0, 480.0],
            "openinpresentation": 1,
            "boxes": boxes,
            "lines": lines
        }
    })
}
