// Shared by every scene. The loader prepends this file to each scene's WGSL, so scenes get
// the `Frame` uniform, the fullscreen vertex stage and a few helpers for free.
//
// Layout mirrors `onset_render::uniforms::FrameUniforms` field for field (std140):
// every scalar is an f32 so packing is predictable; arrays are vec4 groups.

struct Frame {
    resolution: vec2<f32>,      // output size in pixels
    time: f32,                  // seconds since start
    playhead: f32,              // seconds into the track

    beat_phase: f32,            // 0..1 within the current beat
    bar_phase: f32,             // 0..1 within the current 4-beat bar
    phrase_phase: f32,          // 0..1 within the current phrase
    intensity: f32,             // director output 0..1

    bpm: f32,                   // tempo being played
    playing: f32,               // 1 when playing
    phrase_kind: f32,           // 0 none, 1 intro, 2 verse, 3 bridge, 4 chorus, 5 outro, 6 up, 7 down
    next_phrase_kind: f32,      // same encoding

    beats_to_next: f32,         // beats until the next phrase, -1 unknown
    drop_countdown: f32,        // beats until the next drop, -1 unknown
    rms: f32,                   // 0..1 loudness
    onset: f32,                 // 1 on an onset hop

    bands: array<vec4<f32>, 6>, // 24 log-spaced bands, 30 Hz .. 16 kHz
    theme: array<vec4<f32>, 5>, // background, text, accent1, accent2, accent3 (linear RGB, a unused)
};

@group(0) @binding(0) var<uniform> frame: Frame;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle covering the screen; uv runs 0..1 with y down like an image.
@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    var out: VsOut;
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i & 2u) * 2 - 1);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x, -y) * 0.5 + vec2<f32>(0.5, 0.5);
    return out;
}

fn band(i: u32) -> f32 {
    return frame.bands[i / 4u][i % 4u];
}

fn theme(i: u32) -> vec3<f32> {
    return frame.theme[i].rgb;
}

// Aspect-corrected coordinates centred on the screen, y up, x in [-aspect, aspect].
fn centred(uv: vec2<f32>) -> vec2<f32> {
    let aspect = frame.resolution.x / max(frame.resolution.y, 1.0);
    return vec2<f32>((uv.x - 0.5) * 2.0 * aspect, (0.5 - uv.y) * 2.0);
}

// Integer hash of the cell containing p. Integer maths keeps the noise stable however far
// the coordinates drift over a long set; a fractional hash collapses to a few values once
// f32 precision runs out (around 10^4).
fn hash21(p: vec2<f32>) -> f32 {
    let c = vec2<i32>(floor(p));
    var n = bitcast<u32>(c.x) * 1597334677u ^ bitcast<u32>(c.y) * 3812015801u;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = n ^ (n >> 16u);
    return f32(n) / 4294967295.0;
}

fn noise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p_in: vec2<f32>) -> f32 {
    var p = p_in;
    var v = 0.0;
    var a = 0.5;
    for (var k = 0; k < 4; k = k + 1) {
        v = v + a * noise2(p);
        p = p * 2.03 + vec2<f32>(17.1, 9.2);
        a = a * 0.5;
    }
    return v;
}

// Sharp-then-decaying pulse from a 0..1 phase: 1 at the beat, fading over the beat.
fn beat_pulse(phase: f32, sharpness: f32) -> f32 {
    return pow(clamp(1.0 - phase, 0.0, 1.0), sharpness);
}
